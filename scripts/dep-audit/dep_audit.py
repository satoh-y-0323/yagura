#!/usr/bin/env python3
"""Audit the Rust and npm dependencies for known vulnerabilities.

The same script is run by hand before a push and by CI, so the failure
criteria live here and nowhere else. See README.md for how to call it and
how to handle a failure.

Standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Dict, List, Optional, Sequence, Set, Tuple

REPO_ROOT = Path(__file__).resolve().parents[2]
GUI_DIR = REPO_ROOT / "gui"

# The bundled pseudoconsole: the files, the hashes recorded next to them, and the
# hashes the daemon pins. All three must agree.
CONPTY_DIR = REPO_ROOT / "vendor" / "conpty"
CONPTY_FILES = {"conpty.dll": "x64/conpty.dll", "OpenConsole.exe": "x64/OpenConsole.exe"}
CONPTY_PINS = REPO_ROOT / "crates" / "yagura-os" / "src" / "bundled_conpty_pins.rs"

CARGO_AUDIT_VERSION = "0.22.2"
CARGO_AUDIT_INSTALL = f"cargo install cargo-audit --version {CARGO_AUDIT_VERSION} --locked"

PASSED = "passed"
FAILED = "failed"
NOT_STARTED = "could not start"

Run = Callable[[List[str], Path], int]
Which = Callable[[str], Optional[str]]


@dataclass
class Check:
    name: str
    executable: str
    args: List[str]
    cwd: Path
    # Executables that must be found before the check is started, each with the
    # hint printed when it is missing.
    required: List[tuple]


def _npm_executable(platform: str) -> str:
    # npm is a batch file on Windows; without a shell only the full name starts it.
    return "npm.cmd" if platform == "win32" else "npm"


def _checks(platform: str) -> dict:
    npm = _npm_executable(platform)
    return {
        # cargo-audit's default exit code already leaves warnings
        # (unmaintained, unsound, yanked) green and vulnerabilities red.
        "cargo": Check(
            name="cargo",
            executable="cargo",
            args=["audit"],
            cwd=REPO_ROOT,
            required=[
                ("cargo", "install Rust (https://rustup.rs)"),
                # Without cargo-audit, `cargo audit` exits non-zero just like a
                # vulnerability would, so its absence is detected up front.
                ("cargo-audit", f"install it with: {CARGO_AUDIT_INSTALL}"),
            ],
        ),
        # All dependencies, not just production ones: the dev/prod split does
        # not say what ends up in the bundle (the svelte runtime is a devDependency).
        "npm": Check(
            name="npm",
            executable=npm,
            args=["audit", "--audit-level=high"],
            cwd=GUI_DIR,
            required=[(npm, "install Node.js (https://nodejs.org), which ships npm")],
        ),
    }


def _recorded_conpty_hashes(readme: str, pins: str) -> dict:
    """Maps each bundled file name to (README hash, pinned-in-code hash); None when absent."""
    found = {}
    for name, relpath in CONPTY_FILES.items():
        in_readme = re.search(rf"`{re.escape(relpath)}`\s*\|\s*`([0-9a-f]{{64}})`", readme)
        in_pins = re.search(rf'name:\s*"{re.escape(name)}",\s*sha256:\s*"([0-9a-f]{{64}})"', pins)
        found[name] = (
            in_readme.group(1) if in_readme else None,
            in_pins.group(1) if in_pins else None,
        )
    return found


def _check_vendor_conpty() -> str:
    """Checks vendor/conpty against the recorded and pinned hashes, in process."""
    try:
        readme = (CONPTY_DIR / "README.md").read_text(encoding="utf-8")
        pins = CONPTY_PINS.read_text(encoding="utf-8")
        recorded = _recorded_conpty_hashes(readme, pins)
        problems = []
        for name, relpath in CONPTY_FILES.items():
            actual = hashlib.sha256((CONPTY_DIR / relpath).read_bytes()).hexdigest()
            in_readme, in_pins = recorded[name]
            if in_readme is None or in_pins is None:
                problems.append(f"{name}: no recorded hash found (README: {in_readme}, pins: {in_pins})")
            elif not (actual == in_readme == in_pins):
                problems.append(
                    f"{name}: file {actual}, README {in_readme}, pinned in code {in_pins}"
                )
    except OSError as exc:
        print(f"[vendor] could not read the bundled ConPTY: {exc}", file=sys.stderr)
        return NOT_STARTED
    for problem in problems:
        print(f"[vendor] {problem}", file=sys.stderr)
    return FAILED if problems else PASSED


# ---------------------------------------------------------------------------
# Advisory databases beyond cargo audit / npm audit
#
# cargo audit reads RustSec alone and npm audit reads GitHub's reviewed
# advisories alone. An advisory a project publishes in its own repository is
# missing from both until GitHub reviews it, which can take weeks, and a
# reviewed GitHub advisory for a crate may never reach RustSec. So each check
# also reads OSV (RustSec and reviewed GitHub advisories together, for crates)
# and the published advisories of every GitHub repository a locked package
# comes from.
# ---------------------------------------------------------------------------

USER_AGENT = "yagura-dep-audit"
OSV_BATCH_URL = "https://api.osv.dev/v1/querybatch"
OSV_VULN_URL = "https://api.osv.dev/v1/vulns/"
GITHUB_API = "https://api.github.com"
OSV_BATCH_MAX = 1000
REPO_FETCH_WORKERS = 8

# Severities that are printed but do not fail each check. npm keeps the threshold its
# own `npm audit --audit-level=high` uses; a severity that is missing fails.
PASSING_SEVERITIES = {"cargo": {"low"}, "npm": {"low", "medium"}}

Fetch = Callable[[str, Optional[dict], Optional[str]], Tuple[object, Dict[str, str]]]


class AdvisoryError(Exception):
    """An advisory source could not be read; the check cannot pass."""


@dataclass
class Inventory:
    """The locked packages of one ecosystem and the GitHub repositories they come from."""

    versions: Dict[str, Set[str]] = field(default_factory=dict)
    repos: Set[Tuple[str, str]] = field(default_factory=set)

    def add(self, name: str, version: str, repository: Optional[str]) -> None:
        self.versions.setdefault(name, set()).add(version)
        repo = github_repo(repository)
        if repo is not None:
            self.repos.add(repo)


@dataclass
class Finding:
    package: str
    version: str
    advisory: str
    severity: str
    summary: str

    def fails(self, check_name: str) -> bool:
        return self.severity not in PASSING_SEVERITIES[check_name]


_GITHUB_REPO = re.compile(r"github\.com[/:]([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(?:\.git)?(?:[/#?]|$)")


def github_repo(repository: Optional[str]) -> Optional[Tuple[str, str]]:
    """(owner, repo) of a repository URL or npm shorthand, lowercased; None when not on GitHub."""
    if not repository:
        return None
    text = repository.strip()
    shorthand = re.fullmatch(r"(?:github:)?([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)", text)
    match = shorthand or _GITHUB_REPO.search(text)
    if match is None:
        return None
    return match.group(1).lower(), match.group(2).lower()


def _version_key(version: str) -> Tuple:
    """Orders semver-like versions: numeric parts, then a release after its pre-releases."""
    core, _, pre = version.strip().lstrip("v").partition("-")
    core = core.split("+", 1)[0]
    numbers = tuple(int(part) if part.isdigit() else 0 for part in core.split("."))
    numbers = numbers + (0,) * (3 - len(numbers))
    if not pre:
        return numbers + ((1,),)
    pre_parts = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    return numbers + ((0,) + pre_parts,)


_CONSTRAINT = re.compile(r"(>=|<=|>|<|=)\s*v?([0-9][^\s,]*)")


def in_range(version: str, vulnerable_range: str) -> bool:
    """Whether `version` satisfies every constraint of a GitHub `vulnerable_version_range`."""
    constraints = _CONSTRAINT.findall(vulnerable_range or "")
    if not constraints:
        return False
    key = _version_key(version)
    for op, bound in constraints:
        other = _version_key(bound)
        if not {
            ">=": key >= other,
            "<=": key <= other,
            ">": key > other,
            "<": key < other,
            "=": key == other,
        }[op]:
            return False
    return True


def _default_fetch(url: str, body: Optional[dict], token: Optional[str]):
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
        headers["X-GitHub-Api-Version"] = "2022-11-28"
    request = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read().decode("utf-8")), dict(response.headers)


def _github_token(capture: Callable[[List[str], Path], str]) -> Optional[str]:
    for name in ("GITHUB_TOKEN", "GH_TOKEN"):
        value = os.environ.get(name, "").strip()
        if value:
            return value
    try:
        value = capture(["gh", "auth", "token"], REPO_ROOT).strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    return value or None


def _default_capture(cmd: List[str], cwd: Path) -> str:
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True, encoding="utf-8").stdout


def cargo_inventory(capture: Callable[[List[str], Path], str]) -> Inventory:
    """Every registry package `cargo metadata` resolves from Cargo.lock."""
    try:
        metadata = json.loads(capture(["cargo", "metadata", "--format-version", "1", "--locked"], REPO_ROOT))
    except (OSError, subprocess.CalledProcessError, ValueError) as exc:
        raise AdvisoryError(f"cargo metadata failed: {exc}") from exc
    inventory = Inventory()
    for package in metadata.get("packages", []):
        if package.get("source") is None:
            continue
        inventory.add(package["name"], package["version"], package.get("repository"))
    return inventory


def npm_inventory(gui_dir: Path = GUI_DIR) -> Inventory:
    """Every package in package-lock.json, with the repository its installed package.json names."""
    try:
        lock = json.loads((gui_dir / "package-lock.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise AdvisoryError(f"could not read package-lock.json: {exc}") from exc
    inventory = Inventory()
    for path, entry in lock.get("packages", {}).items():
        if not path or entry.get("link"):
            continue
        name = path.rsplit("node_modules/", 1)[-1]
        version = entry.get("version")
        if not version:
            continue
        manifest = gui_dir / path / "package.json"
        try:
            repository = json.loads(manifest.read_text(encoding="utf-8")).get("repository")
        except OSError as exc:
            # An optional package for another platform is never installed here.
            if entry.get("optional"):
                inventory.add(name, version, None)
                continue
            raise AdvisoryError(f"{manifest} is missing; run `npm ci` in gui/ first") from exc
        except ValueError:
            repository = None
        if isinstance(repository, dict):
            repository = repository.get("url")
        inventory.add(name, version, repository if isinstance(repository, str) else None)
    return inventory


def _osv_severity(vuln: dict) -> str:
    severity = (vuln.get("database_specific") or {}).get("severity")
    if isinstance(severity, str) and severity:
        return {"moderate": "medium"}.get(severity.lower(), severity.lower())
    return "unknown"


def _informational(vuln: dict) -> bool:
    """A RustSec notice (unmaintained, unsound), which cargo audit reports as a warning only."""
    return any(
        (affected.get("database_specific") or {}).get("informational")
        for affected in vuln.get("affected") or []
    )


def osv_findings(ecosystem: str, inventory: Inventory, fetch: Fetch) -> List[Finding]:
    queries = [
        {"package": {"ecosystem": ecosystem, "name": name}, "version": version}
        for name, versions in sorted(inventory.versions.items())
        for version in sorted(versions)
    ]
    hits: List[Tuple[dict, str]] = []
    for start in range(0, len(queries), OSV_BATCH_MAX):
        batch = queries[start : start + OSV_BATCH_MAX]
        try:
            answer, _ = fetch(OSV_BATCH_URL, {"queries": batch}, None)
        except (OSError, urllib.error.URLError, ValueError) as exc:
            raise AdvisoryError(f"OSV query failed: {exc}") from exc
        for query, result in zip(batch, answer.get("results", [])):
            for vuln in result.get("vulns", []) or []:
                hits.append((query, vuln["id"]))
    details = []
    for query, vuln_id in hits:
        try:
            vuln, _ = fetch(OSV_VULN_URL + urllib.parse.quote(vuln_id), None, None)
        except (OSError, urllib.error.URLError, ValueError) as exc:
            raise AdvisoryError(f"OSV lookup of {vuln_id} failed: {exc}") from exc
        details.append((query, vuln_id, vuln))
    # A GitHub copy of a RustSec notice carries the notice's id as an alias.
    notices = {vuln_id for _, vuln_id, vuln in details if _informational(vuln)}
    findings = []
    for query, vuln_id, vuln in details:
        if vuln.get("withdrawn") or vuln_id in notices:
            continue
        if notices.intersection(vuln.get("aliases") or ()):
            continue
        findings.append(
            Finding(
                query["package"]["name"],
                query["version"],
                vuln_id,
                _osv_severity(vuln),
                vuln.get("summary") or "",
            )
        )
    return findings


def _repo_advisories(repo: Tuple[str, str], fetch: Fetch, token: str) -> Optional[List[dict]]:
    """Published advisories of one repository; None when the repository cannot be read."""
    url: Optional[str] = (
        f"{GITHUB_API}/repos/{repo[0]}/{repo[1]}/security-advisories?state=published&per_page=100"
    )
    advisories: List[dict] = []
    while url:
        try:
            page, headers = fetch(url, None, token)
        except urllib.error.HTTPError as exc:
            remaining = (exc.headers or {}).get("X-RateLimit-Remaining") if exc.headers else None
            if exc.code == 404 or (exc.code == 403 and remaining not in ("0",)):
                return None
            raise AdvisoryError(f"GitHub advisories of {repo[0]}/{repo[1]}: {exc}") from exc
        except (OSError, urllib.error.URLError, ValueError) as exc:
            raise AdvisoryError(f"GitHub advisories of {repo[0]}/{repo[1]}: {exc}") from exc
        advisories.extend(page if isinstance(page, list) else [])
        link = {k.lower(): v for k, v in headers.items()}.get("link", "")
        found = re.search(r'<([^>]+)>;\s*rel="next"', link)
        url = found.group(1) if found else None
    return advisories


def _hits(vulnerability: dict, github_ecosystem: str, inventory: Inventory) -> bool:
    package = vulnerability.get("package") or {}
    if package.get("ecosystem") != github_ecosystem:
        return False
    return any(
        in_range(version, vulnerability.get("vulnerable_version_range", ""))
        for version in inventory.versions.get(package.get("name"), ())
    )


def _reviewed_vulnerabilities(advisory: dict, fetch: Fetch, token: str) -> Optional[List[dict]]:
    """The ranges GitHub settled on when it reviewed the advisory; None while it is unreviewed.

    A repository's own advisory is written by the project and its ranges can be loose; the
    reviewed copy in the global database corrects them.
    """
    ghsa = advisory.get("ghsa_id")
    if not ghsa:
        return None
    try:
        reviewed, _ = fetch(f"{GITHUB_API}/advisories/{urllib.parse.quote(ghsa)}", None, token)
    except urllib.error.HTTPError as exc:
        # A workflow token is refused (403) a global advisory it may not read, where a
        # user's token gets 404; either way there is no reviewed copy to read. A spent
        # rate limit is not that, and stops the check.
        remaining = (exc.headers or {}).get("X-RateLimit-Remaining") if exc.headers else None
        if exc.code == 404 or (exc.code == 403 and remaining not in ("0",)):
            return None
        raise AdvisoryError(f"GitHub advisory {ghsa}: {exc}") from exc
    except (OSError, urllib.error.URLError, ValueError) as exc:
        raise AdvisoryError(f"GitHub advisory {ghsa}: {exc}") from exc
    vulnerabilities = reviewed.get("vulnerabilities") if isinstance(reviewed, dict) else None
    return vulnerabilities or None


def repo_findings(
    github_ecosystem: str, inventory: Inventory, fetch: Fetch, token: str
) -> Tuple[List[Finding], List[str]]:
    """Findings from the repositories' own advisories, and the repositories that could not be read."""
    repos = sorted(inventory.repos)
    with ThreadPoolExecutor(max_workers=REPO_FETCH_WORKERS) as pool:
        pages = list(pool.map(lambda repo: _repo_advisories(repo, fetch, token), repos))
    findings: List[Finding] = []
    unreadable = []
    for repo, advisories in zip(repos, pages):
        if advisories is None:
            unreadable.append(f"{repo[0]}/{repo[1]}")
            continue
        for advisory in advisories:
            if advisory.get("withdrawn_at"):
                continue
            vulnerabilities = advisory.get("vulnerabilities") or []
            if any(_hits(v, github_ecosystem, inventory) for v in vulnerabilities):
                vulnerabilities = _reviewed_vulnerabilities(advisory, fetch, token) or vulnerabilities
            for vulnerability in vulnerabilities:
                package = vulnerability.get("package") or {}
                if package.get("ecosystem") != github_ecosystem:
                    continue
                name = package.get("name")
                for version in sorted(inventory.versions.get(name, ())):
                    if in_range(version, vulnerability.get("vulnerable_version_range", "")):
                        findings.append(
                            Finding(
                                name,
                                version,
                                advisory.get("ghsa_id", "?"),
                                (advisory.get("severity") or "unknown").lower(),
                                advisory.get("summary") or "",
                            )
                        )
    return findings, unreadable


@dataclass
class AdvisorySources:
    """What the advisory part of a check reads; replaced in tests."""

    cargo_inventory: Callable[[], Inventory]
    npm_inventory: Callable[[], Inventory]
    fetch: Fetch
    token: Callable[[], Optional[str]]


def _default_sources() -> AdvisorySources:
    return AdvisorySources(
        cargo_inventory=lambda: cargo_inventory(_default_capture),
        npm_inventory=npm_inventory,
        fetch=_default_fetch,
        token=lambda: _github_token(_default_capture),
    )


def _check_advisories(check_name: str, sources: AdvisorySources) -> str:
    """OSV (crates only) and repository advisories for one check's ecosystem."""
    token = sources.token()
    if token is None:
        print(
            f"[{check_name}] no GitHub token: set GITHUB_TOKEN, or sign in with `gh auth login`",
            file=sys.stderr,
        )
        return NOT_STARTED
    try:
        if check_name == "cargo":
            inventory = sources.cargo_inventory()
            findings = osv_findings("crates.io", inventory, sources.fetch)
            repo_found, unreadable = repo_findings("rust", inventory, sources.fetch, token)
        else:
            inventory = sources.npm_inventory()
            findings = []
            repo_found, unreadable = repo_findings("npm", inventory, sources.fetch, token)
    except AdvisoryError as exc:
        print(f"[{check_name}] {exc}", file=sys.stderr)
        return NOT_STARTED
    seen = set()
    failed = False
    for finding in findings + repo_found:
        key = (finding.package, finding.version, finding.advisory)
        if key in seen:
            continue
        seen.add(key)
        fails = finding.fails(check_name)
        failed = failed or fails
        print(
            f"[{check_name}] {finding.advisory} ({finding.severity}) {finding.package} "
            f"{finding.version}: {finding.summary}",
            file=sys.stderr if fails else sys.stdout,
        )
    print(
        f"[{check_name}] advisories: {len(inventory.versions)} packages, "
        f"{len(inventory.repos)} GitHub repositories ({len(unreadable)} not readable)"
    )
    return FAILED if failed else PASSED


def _default_run(cmd: List[str], cwd: Path) -> int:
    return subprocess.run(cmd, cwd=cwd, check=False).returncode


def _run_check(check: Check, run: Run, which: Which) -> str:
    resolved = {}
    for executable, hint in check.required:
        path = which(executable)
        if path is None:
            print(f"[{check.name}] {executable} not found on PATH; {hint}", file=sys.stderr)
            return NOT_STARTED
        resolved[executable] = path

    cmd = [resolved.get(check.executable, check.executable), *check.args]
    print(f"[{check.name}] running `{check.executable} {' '.join(check.args)}` in {check.cwd}")
    sys.stdout.flush()
    try:
        code = run(cmd, check.cwd)
    except OSError as exc:
        print(f"[{check.name}] could not start {check.executable}: {exc}", file=sys.stderr)
        return NOT_STARTED
    return PASSED if code == 0 else FAILED


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="dep_audit.py",
        description="Audit the Rust and npm dependencies for known vulnerabilities.",
    )
    parser.add_argument(
        "--only",
        choices=("cargo", "npm", "vendor"),
        help="run only this check (default: both)",
    )
    return parser.parse_args(list(argv))


def main(
    argv: Sequence[str],
    run: Run = _default_run,
    *,
    platform: Optional[str] = None,
    which: Optional[Which] = None,
    sources: Optional[AdvisorySources] = None,
) -> int:
    args = _parse_args(argv)
    checks = _checks(platform if platform is not None else sys.platform)
    lookup = which if which is not None else shutil.which
    advisory_sources = sources if sources is not None else _default_sources()
    selected = [args.only] if args.only else ["cargo", "npm", "vendor"]

    # Every selected check runs even after a failure, so one run reports all of them.
    results = []
    for key in selected:
        if key == "vendor":
            result = _check_vendor_conpty()
            print(f"[vendor] {result}")
            results.append(("vendor", result))
            continue
        check = checks[key]
        result = _run_check(check, run, lookup)
        advisory_result = _check_advisories(check.name, advisory_sources)
        # The worse of the two: could not start > failed > passed.
        for worse in (NOT_STARTED, FAILED):
            if worse in (result, advisory_result):
                result = worse
                break
        print(f"[{check.name}] {result}")
        results.append((check.name, result))

    print("summary: " + ", ".join(f"{name} {result}" for name, result in results))
    return 0 if all(result == PASSED for _, result in results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
