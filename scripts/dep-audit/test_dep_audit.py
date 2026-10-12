"""Contract tests for scripts/dep-audit/dep_audit.py.

Run with: python -m pytest scripts/dep-audit/ -v

Public boundary these tests rely on:

  main(argv, run, *, platform=None, which=None) -> int
    run(cmd: list[str], cwd: pathlib.Path) -> int
        starts a subprocess and returns its exit code; raises FileNotFoundError
        when the tool cannot be started.
    platform: str
        the OS to decide the npm executable name for, in the form of
        sys.platform ("win32" means Windows). Defaults to sys.platform.
    which: Callable[[str], str | None]
        executable lookup with shutil.which semantics, consulted before a
        check is started. Defaults to shutil.which.

No test here starts a real cargo or npm process.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import dep_audit

REPO_ROOT = Path(__file__).resolve().parents[2]
CARGO_AUDIT_INSTALL = "cargo install cargo-audit --version 0.22.2 --locked"
ALL_TOOLS = frozenset({"cargo", "cargo-audit", "npm", "npm.cmd"})


def _tool_of(cmd: list[str]) -> str:
    stem = Path(cmd[0]).stem.lower()
    if stem == "cargo":
        return "cargo"
    if stem == "npm":
        return "npm"
    raise AssertionError(f"unexpected command started: {cmd!r}")


class Recorder:
    """Stands in for `run`: records each call and answers per tool."""

    def __init__(self, codes=None, raises=()):
        self.codes = {"cargo": 0, "npm": 0, **(codes or {})}
        self.raises = set(raises)
        self.calls: list[tuple[list[str], Path]] = []

    def __call__(self, cmd, cwd):
        cmd = list(cmd)
        self.calls.append((cmd, Path(cwd)))
        tool = _tool_of(cmd)
        if tool in self.raises:
            raise FileNotFoundError(cmd[0])
        return self.codes[tool]

    def tools(self) -> list[str]:
        return [_tool_of(cmd) for cmd, _ in self.calls]

    def call_for(self, tool: str) -> tuple[list[str], Path]:
        matches = [c for c in self.calls if _tool_of(c[0]) == tool]
        assert len(matches) == 1, f"expected exactly one {tool} call, got {self.calls!r}"
        return matches[0]


def fake_which(present=ALL_TOOLS):
    present = frozenset(present)

    def which(name):
        return f"/fake/bin/{name}" if name in present else None

    return which


def quiet_sources():
    """Advisory sources that find nothing and touch no network."""
    return sources_with(FakeFetch({}))


def sources_with(fetch, *, cargo=None, npm=None, credential="fake-credential"):
    return dep_audit.AdvisorySources(
        cargo_inventory=lambda: cargo if cargo is not None else dep_audit.Inventory(),
        npm_inventory=lambda: npm if npm is not None else dep_audit.Inventory(),
        fetch=fetch,
        token=lambda: credential,
    )


def run_main(argv, run, *, platform="linux", present=ALL_TOOLS, sources=None):
    return dep_audit.main(
        argv,
        run,
        platform=platform,
        which=fake_which(present),
        sources=sources if sources is not None else quiet_sources(),
    )


def assert_failed(rc):
    assert isinstance(rc, int) and not isinstance(rc, bool)
    assert rc != 0


# ---------------------------------------------------------------------------
# Both checks run by default
# ---------------------------------------------------------------------------


def test_no_arguments_runs_cargo_and_npm_and_succeeds_when_both_pass():
    rec = Recorder()
    rc = run_main([], rec)
    assert sorted(rec.tools()) == ["cargo", "npm"]
    assert rc == 0
    assert isinstance(rc, int) and not isinstance(rc, bool)


# ---------------------------------------------------------------------------
# One failing check does not skip the other
# ---------------------------------------------------------------------------


def test_failing_cargo_still_runs_npm_and_fails_overall():
    rec = Recorder(codes={"cargo": 1})
    rc = run_main([], rec)
    assert sorted(rec.tools()) == ["cargo", "npm"]
    assert_failed(rc)


def test_failing_npm_still_runs_cargo_and_fails_overall():
    rec = Recorder(codes={"npm": 1})
    rc = run_main([], rec)
    assert sorted(rec.tools()) == ["cargo", "npm"]
    assert_failed(rc)


# ---------------------------------------------------------------------------
# Commands and working directories
# ---------------------------------------------------------------------------


def test_repo_root_used_by_tests_is_the_repository_root():
    assert (REPO_ROOT / "Cargo.toml").is_file()
    assert (REPO_ROOT / "gui" / "package.json").is_file()


def test_cargo_runs_audit_in_repo_root_regardless_of_process_cwd(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    rec = Recorder()
    run_main(["--only", "cargo"], rec)
    cmd, cwd = rec.call_for("cargo")
    assert cmd[1:] == ["audit"]
    assert cwd.resolve() == REPO_ROOT


def test_npm_runs_high_level_audit_over_all_dependencies_in_gui(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    rec = Recorder()
    run_main(["--only", "npm"], rec)
    cmd, cwd = rec.call_for("npm")
    assert cmd[1:] == ["audit", "--audit-level=high"]
    assert not any(arg.startswith("--omit") for arg in cmd)
    assert cwd.resolve() == REPO_ROOT / "gui"


def test_commands_are_the_same_when_both_checks_run(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    rec = Recorder()
    run_main([], rec)
    cargo_cmd, cargo_cwd = rec.call_for("cargo")
    npm_cmd, npm_cwd = rec.call_for("npm")
    assert cargo_cmd[1:] == ["audit"]
    assert cargo_cwd.resolve() == REPO_ROOT
    assert npm_cmd[1:] == ["audit", "--audit-level=high"]
    assert npm_cwd.resolve() == REPO_ROOT / "gui"


# ---------------------------------------------------------------------------
# A tool that cannot be started is a failure, not an escaping exception
# ---------------------------------------------------------------------------


def test_cargo_not_startable_fails_without_raising_and_npm_still_runs():
    rec = Recorder(raises={"cargo"})
    rc = run_main([], rec)
    assert sorted(rec.tools()) == ["cargo", "npm"]
    assert_failed(rc)


def test_npm_not_startable_fails_without_raising_and_cargo_still_runs():
    rec = Recorder(raises={"npm"})
    rc = run_main([], rec)
    assert sorted(rec.tools()) == ["cargo", "npm"]
    assert_failed(rc)


# ---------------------------------------------------------------------------
# --only selects one check
# ---------------------------------------------------------------------------


def test_only_cargo_runs_cargo_alone():
    rec = Recorder()
    rc = run_main(["--only", "cargo"], rec)
    assert rec.tools() == ["cargo"]
    assert rc == 0


def test_only_npm_runs_npm_alone():
    rec = Recorder()
    rc = run_main(["--only", "npm"], rec)
    assert rec.tools() == ["npm"]
    assert rc == 0


def test_only_with_unknown_value_fails_and_runs_nothing():
    rec = Recorder()
    try:
        rc = run_main(["--only", "pip"], rec)
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    assert_failed(rc)
    assert rec.calls == []


# ---------------------------------------------------------------------------
# npm executable name follows the target OS
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("platform", "expected"),
    [("win32", "npm.cmd"), ("linux", "npm"), ("darwin", "npm")],
)
def test_npm_executable_name_depends_on_platform(platform, expected):
    rec = Recorder()
    rc = run_main(["--only", "npm"], rec, platform=platform)
    cmd, _ = rec.call_for("npm")
    assert Path(cmd[0]).name == expected
    assert rc == 0


# ---------------------------------------------------------------------------
# Presence is checked before a check is started
# ---------------------------------------------------------------------------


def test_missing_cargo_audit_fails_without_running_cargo_and_shows_install_command(capsys):
    rec = Recorder()
    rc = run_main([], rec, present=ALL_TOOLS - {"cargo-audit"})
    out = capsys.readouterr()
    assert rec.tools() == ["npm"]
    assert_failed(rc)
    assert CARGO_AUDIT_INSTALL in out.out + out.err


def test_present_cargo_audit_does_not_show_install_command(capsys):
    rec = Recorder()
    rc = run_main([], rec)
    out = capsys.readouterr()
    assert rc == 0
    assert CARGO_AUDIT_INSTALL not in out.out + out.err


@pytest.mark.parametrize("platform", ["win32", "linux"])
def test_missing_npm_fails_without_running_npm_and_cargo_still_runs(platform):
    rec = Recorder()
    rc = run_main([], rec, platform=platform, present=ALL_TOOLS - {"npm", "npm.cmd"})
    assert rec.tools() == ["cargo"]
    assert_failed(rc)


# ---------------------------------------------------------------------------
# Advisories beyond cargo audit / npm audit: OSV and the repositories' own
# ---------------------------------------------------------------------------

GITHUB = "https://api.github.com"


def repo_url(owner, repo):
    return f"{GITHUB}/repos/{owner}/{repo}/security-advisories?state=published&per_page=100"


def osv_none(count):
    return {"results": [{} for _ in range(count)]}


def advisory(ghsa, severity, *vulns, withdrawn=None):
    return {
        "ghsa_id": ghsa,
        "severity": severity,
        "summary": f"summary of {ghsa}",
        "withdrawn_at": withdrawn,
        "vulnerabilities": [
            {"package": {"ecosystem": eco, "name": name}, "vulnerable_version_range": rng}
            for eco, name, rng in vulns
        ],
    }


class FakeFetch:
    """Answers by URL; an OSV batch is answered by `osv`, an unknown URL raises a 404."""

    def __init__(self, pages, osv=None):
        self.pages = pages
        self.osv = osv
        self.urls = []

    def __call__(self, url, body, credential):
        self.urls.append(url)
        if url == dep_audit.OSV_BATCH_URL:
            return (self.osv(body) if self.osv else osv_none(len(body["queries"]))), {}
        if url in self.pages:
            page = self.pages[url]
            if isinstance(page, Exception):
                raise page
            return page, {}
        raise dep_audit.urllib.error.HTTPError(url, 404, "Not Found", {}, None)


def inventory(*packages):
    inv = dep_audit.Inventory()
    for name, version, repo in packages:
        inv.add(name, version, repo)
    return inv


def audit_only(check, fetch, rec=None, **kw):
    return run_main(["--only", check], rec or Recorder(), sources=sources_with(fetch, **kw))


@pytest.mark.parametrize(
    ("version", "rng", "expected"),
    [
        ("2.11.5", ">= 2.0.0, <= 2.11.5", True),
        ("2.11.6", ">= 2.0.0, <= 2.11.5", False),
        ("2.5.1", ">= 2.0, <2.5.2", True),
        ("2.5.2", ">= 2.0, <2.5.2", False),
        ("1.4.0", "= 1.4.0", True),
        ("2.0.0-beta.19", ">= 2.0.0-beta.0, <= 2.0.0-beta.19", True),
        ("2.0.0", "<= 2.0.0-beta.19", False),
        ("0.11.6", "<0.11.7", True),
        ("1.0.0", "", False),
    ],
)
def test_in_range_follows_github_range_syntax(version, rng, expected):
    assert dep_audit.in_range(version, rng) is expected


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("https://github.com/tauri-apps/tauri", ("tauri-apps", "tauri")),
        ("git+https://github.com/tauri-apps/tauri.git", ("tauri-apps", "tauri")),
        ("https://github.com/unicode-org/icu4x/tree/main/utils/zerovec", ("unicode-org", "icu4x")),
        ("github:sveltejs/svelte", ("sveltejs", "svelte")),
        ("vitejs/vite", ("vitejs", "vite")),
        ("https://gitlab.com/a/b", None),
        (None, None),
    ],
)
def test_github_repo_is_read_from_urls_and_npm_shorthand(text, expected):
    assert dep_audit.github_repo(text) == expected


def test_an_unreviewed_repository_advisory_fails_the_cargo_check():
    fetch = FakeFetch(
        {
            repo_url("tauri-apps", "tauri"): [
                advisory("GHSA-a", "high", ("rust", "tauri", ">= 2.0.0, <= 2.11.5"))
            ]
        }
    )
    cargo = inventory(("tauri", "2.11.5", "https://github.com/tauri-apps/tauri"))
    assert_failed(audit_only("cargo", fetch, cargo=cargo))


def test_a_patched_version_passes_the_cargo_check():
    fetch = FakeFetch(
        {
            repo_url("tauri-apps", "tauri"): [
                advisory("GHSA-a", "high", ("rust", "tauri", ">= 2.0.0, <= 2.11.5"))
            ]
        }
    )
    cargo = inventory(("tauri", "2.11.6", "https://github.com/tauri-apps/tauri"))
    assert audit_only("cargo", fetch, cargo=cargo) == 0


def test_an_advisory_for_another_package_of_the_same_repository_is_matched_by_name():
    fetch = FakeFetch(
        {
            repo_url("tauri-apps", "plugins-workspace"): [
                advisory("GHSA-b", "medium", ("rust", "tauri-plugin-fs", ">= 2.0.0, < 2.5.2"))
            ]
        }
    )
    cargo = inventory(
        ("tauri-plugin-dialog", "2.7.2", "https://github.com/tauri-apps/plugins-workspace"),
        ("tauri-plugin-fs", "2.5.1", None),
    )
    assert_failed(audit_only("cargo", fetch, cargo=cargo))


def test_a_low_advisory_is_shown_but_passes(capsys):
    fetch = FakeFetch(
        {repo_url("contain-rs", "bit-vec"): [advisory("GHSA-c", "low", ("rust", "bit-vec", "<= 0.9.1"))]}
    )
    cargo = inventory(("bit-vec", "0.8.0", "https://github.com/contain-rs/bit-vec"))
    assert audit_only("cargo", fetch, cargo=cargo) == 0
    assert "GHSA-c" in capsys.readouterr().out


def forbidden(url, remaining):
    return dep_audit.urllib.error.HTTPError(
        url, 403, "Forbidden", {"X-RateLimit-Remaining": remaining}, None
    )


def test_a_reviewed_copy_the_token_may_not_read_leaves_the_repositorys_ranges(capsys):
    global_url = f"{GITHUB}/advisories/GHSA-c"
    fetch = FakeFetch(
        {
            repo_url("contain-rs", "bit-vec"): [
                advisory("GHSA-c", "low", ("rust", "bit-vec", "<= 0.9.1"))
            ],
            global_url: forbidden(global_url, "59"),
        }
    )
    cargo = inventory(("bit-vec", "0.8.0", "https://github.com/contain-rs/bit-vec"))
    assert audit_only("cargo", fetch, cargo=cargo) == 0
    assert "GHSA-c" in capsys.readouterr().out


def test_a_rate_limited_reviewed_copy_stops_the_cargo_check():
    global_url = f"{GITHUB}/advisories/GHSA-c"
    fetch = FakeFetch(
        {
            repo_url("contain-rs", "bit-vec"): [
                advisory("GHSA-c", "low", ("rust", "bit-vec", "<= 0.9.1"))
            ],
            global_url: forbidden(global_url, "0"),
        }
    )
    cargo = inventory(("bit-vec", "0.8.0", "https://github.com/contain-rs/bit-vec"))
    assert_failed(audit_only("cargo", fetch, cargo=cargo))


def test_npm_keeps_the_high_threshold_of_npm_audit():
    medium = FakeFetch(
        {repo_url("vitejs", "vite"): [advisory("GHSA-d", "medium", ("npm", "vite", "<= 8.2.3"))]}
    )
    high = FakeFetch(
        {repo_url("vitejs", "vite"): [advisory("GHSA-e", "high", ("npm", "vite", "<= 8.2.3"))]}
    )
    npm = inventory(("vite", "8.2.1", "vitejs/vite"))
    assert audit_only("npm", medium, npm=npm) == 0
    assert_failed(audit_only("npm", high, npm=npm))


def test_the_reviewed_ranges_replace_a_repositorys_loose_ones():
    fetch = FakeFetch(
        {
            repo_url("bytecodealliance", "rustix"): [
                advisory("GHSA-f", "medium", ("rust", "rustix", "> 0.35.11"))
            ],
            f"{GITHUB}/advisories/GHSA-f": {
                "vulnerabilities": [
                    {
                        "package": {"ecosystem": "rust", "name": "rustix"},
                        "vulnerable_version_range": ">= 0.38.0, < 0.38.19",
                    }
                ]
            },
        }
    )
    cargo = inventory(("rustix", "1.1.5", "https://github.com/bytecodealliance/rustix"))
    assert audit_only("cargo", fetch, cargo=cargo) == 0


def test_a_withdrawn_advisory_is_ignored():
    fetch = FakeFetch(
        {
            repo_url("tauri-apps", "tauri"): [
                advisory("GHSA-g", "high", ("rust", "tauri", "<= 2.11.5"), withdrawn="2026-01-01")
            ]
        }
    )
    cargo = inventory(("tauri", "2.11.5", "https://github.com/tauri-apps/tauri"))
    assert audit_only("cargo", fetch, cargo=cargo) == 0


def test_an_unreadable_repository_is_counted_not_failed(capsys):
    cargo = inventory(("gone", "1.0.0", "https://github.com/someone/gone"))
    assert audit_only("cargo", FakeFetch({}), cargo=cargo) == 0
    assert "1 not readable" in capsys.readouterr().out


def test_osv_reports_a_reviewed_advisory_cargo_audit_does_not_know(monkeypatch):
    def osv(body):
        return {
            "results": [
                {"vulns": [{"id": "GHSA-h"}]} if q["package"]["name"] == "tauri" else {}
                for q in body["queries"]
            ]
        }

    fetch = FakeFetch(
        {dep_audit.OSV_VULN_URL + "GHSA-h": {"id": "GHSA-h", "database_specific": {"severity": "MODERATE"}}},
        osv=osv,
    )
    cargo = inventory(("tauri", "2.11.0", None))
    assert_failed(audit_only("cargo", fetch, cargo=cargo))


def test_rustsec_notices_and_their_github_copies_stay_warnings():
    notice = {
        "id": "RUSTSEC-1",
        "affected": [{"database_specific": {"informational": "unsound"}}],
    }
    copy = {"id": "GHSA-i", "aliases": ["RUSTSEC-1"], "database_specific": {"severity": "MODERATE"}}

    def osv(body):
        return {"results": [{"vulns": [{"id": "RUSTSEC-1"}, {"id": "GHSA-i"}]} for _ in body["queries"]]}

    fetch = FakeFetch(
        {dep_audit.OSV_VULN_URL + "RUSTSEC-1": notice, dep_audit.OSV_VULN_URL + "GHSA-i": copy},
        osv=osv,
    )
    cargo = inventory(("glib", "0.18.5", None))
    assert audit_only("cargo", fetch, cargo=cargo) == 0


def test_without_a_github_credential_the_check_cannot_pass(capsys):
    rc = run_main(["--only", "cargo"], Recorder(), sources=sources_with(FakeFetch({}), credential=None))
    assert_failed(rc)
    assert "gh auth login" in capsys.readouterr().err


def test_an_unreachable_advisory_source_cannot_pass():
    def broken(url, body, credential):
        raise dep_audit.urllib.error.URLError("offline")

    cargo = inventory(("tauri", "2.11.6", None))
    assert_failed(audit_only("cargo", broken, cargo=cargo))


def test_npm_inventory_skips_an_uninstalled_optional_package_and_reads_repositories(tmp_path):
    (tmp_path / "node_modules" / "vite").mkdir(parents=True)
    (tmp_path / "node_modules" / "vite" / "package.json").write_text(
        '{"repository": {"url": "git+https://github.com/vitejs/vite.git"}}', encoding="utf-8"
    )
    (tmp_path / "package-lock.json").write_text(
        """{"packages": {
            "": {"name": "gui"},
            "node_modules/vite": {"version": "8.2.4"},
            "node_modules/@rolldown/binding-android-arm64": {"version": "1.0.0", "optional": true}
        }}""",
        encoding="utf-8",
    )
    inv = dep_audit.npm_inventory(tmp_path)
    assert inv.versions == {"vite": {"8.2.4"}, "@rolldown/binding-android-arm64": {"1.0.0"}}
    assert inv.repos == {("vitejs", "vite")}


def test_npm_inventory_needs_an_installed_tree(tmp_path):
    (tmp_path / "package-lock.json").write_text(
        '{"packages": {"node_modules/vite": {"version": "8.2.4"}}}', encoding="utf-8"
    )
    with pytest.raises(dep_audit.AdvisoryError, match="npm ci"):
        dep_audit.npm_inventory(tmp_path)
