#!/usr/bin/env python3
"""Flake reproduction harness for the yagura test suite (Windows only).

The harness repeats one measurement arm inside a single process, classifies
every iteration as pass / hang / fail / build-failure, captures thread and
process state *before* killing a hung target, and keeps the machine clean
between iterations.  See README.md for the calling conventions, the output
layout and the classification rules.

No third-party modules are used.  PowerShell is only invoked as
``powershell -NoProfile -Command <script>``; the harness never writes a .ps1.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_LOG_BASE = REPO_ROOT / "target" / "flake-logs"
DEFAULT_ITERATION_TIMEOUT = 120.0
DEFAULT_FULL_TIMEOUT = 2400.0
DEFAULT_BUDGET = 14400.0
DEFAULT_CARGO_ARGS = ("test", "--workspace", "--no-fail-fast")
DEFAULT_SAMPLE_INTERVAL_SECONDS = 3.0
SAMPLE_INTERVAL_ENV_VAR = "FLAKE_HARNESS_SAMPLE_INTERVAL_SECONDS"

# Windows pseudo processes that must never be targeted.
ALWAYS_PROTECTED = {0, 4}

ProcKey = Tuple[int, str]


class HarnessStop(Exception):
    """Raised when the harness cannot keep measuring and must fail loud."""

    def __init__(self, stage: str, message: str) -> None:
        super().__init__(message)
        self.stage = stage
        self.message = message


@dataclass
class ProcInfo:
    pid: int
    ppid: int
    name: str
    path: str
    cmd: str
    created: str

    @property
    def key(self) -> ProcKey:
        return (self.pid, self.created)

    def describe(self) -> str:
        return (
            f"pid={self.pid} ppid={self.ppid} name={self.name} "
            f"created={self.created or '?'} path={self.path or '?'} "
            f"cmd={self.cmd or '?'}"
        )


def now_stamp() -> str:
    return datetime.now().strftime("%Y%m%d-%H%M%S")


def now_iso() -> str:
    return datetime.now().isoformat(timespec="seconds")


def require_windows() -> None:
    if os.name != "nt":
        sys.stderr.write(
            "flake_harness.py supports Windows only "
            "(process snapshots and thread capture rely on PowerShell / CIM).\n"
        )
        raise SystemExit(1)


# --------------------------------------------------------------------------
# PowerShell plumbing
# --------------------------------------------------------------------------


def ps_literal(path: Path) -> str:
    """Quote a path for a PowerShell single-quoted string literal."""
    text = str(path)
    if "'" in text:
        raise HarnessStop("powershell", f"path contains a single quote: {text}")
    return f"'{text}'"


@dataclass
class PsResult:
    returncode: int
    stdout: str
    stderr: str


def run_powershell(
    script: str, timeout: float = 120.0, seen_pids: Optional[set] = None
) -> PsResult:
    proc = subprocess.Popen(
        ["powershell", "-NoProfile", "-Command", script],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        stdin=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if seen_pids is not None:
        # The helper is itself a new process; remembering it keeps the
        # per-iteration process diff free of the harness' own plumbing.
        seen_pids.add(proc.pid)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()
        raise
    return PsResult(proc.returncode, stdout, stderr)


def _scratch_file(directory: Optional[Path], prefix: str) -> Path:
    target_dir = directory if directory is not None else Path(tempfile.gettempdir())
    target_dir.mkdir(parents=True, exist_ok=True)
    handle, name = tempfile.mkstemp(prefix=prefix, suffix=".tmp", dir=str(target_dir))
    os.close(handle)
    return Path(name)


class ProcessProbe:
    """Process table snapshots, thread capture and termination."""

    def __init__(self, scratch_dir: Optional[Path]) -> None:
        self.scratch_dir = scratch_dir
        self.helper_pids: set = set()
        self.snapshot_calls = 0
        self.snapshot_seconds = 0.0

    def snapshot(self) -> Dict[int, ProcInfo]:
        out_file = _scratch_file(self.scratch_dir, "snapshot-")
        script = (
            "$ErrorActionPreference='Stop';"
            "$rows = Get-CimInstance Win32_Process | ForEach-Object {"
            " $c = ''; if ($_.CreationDate) { $c = $_.CreationDate.ToString('o') };"
            " [pscustomobject]@{ pid = [int]$_.ProcessId; ppid = [int]$_.ParentProcessId;"
            " name = [string]$_.Name; path = [string]$_.ExecutablePath;"
            " cmd = [string]$_.CommandLine; created = $c } };"
            "$json = $rows | ConvertTo-Json -Compress -Depth 3;"
            f"[IO.File]::WriteAllText({ps_literal(out_file)}, $json,"
            " (New-Object System.Text.UTF8Encoding($false)))"
        )
        started = time.monotonic()
        try:
            completed = run_powershell(script, seen_pids=self.helper_pids)
            if completed.returncode != 0:
                raise HarnessStop(
                    "snapshot",
                    "PowerShell process snapshot failed: " + completed.stderr.strip(),
                )
            raw = out_file.read_text(encoding="utf-8", errors="replace")
        except subprocess.TimeoutExpired as exc:
            raise HarnessStop("snapshot", f"PowerShell process snapshot timed out: {exc}")
        finally:
            out_file.unlink(missing_ok=True)
            self.snapshot_calls += 1
            self.snapshot_seconds += time.monotonic() - started

        try:
            parsed = json.loads(raw) if raw.strip() else []
        except json.JSONDecodeError as exc:
            raise HarnessStop("snapshot", f"process snapshot is not valid JSON: {exc}")
        if isinstance(parsed, dict):
            parsed = [parsed]

        table: Dict[int, ProcInfo] = {}
        for row in parsed:
            try:
                pid = int(row.get("pid") or 0)
            except (TypeError, ValueError):
                continue
            table[pid] = ProcInfo(
                pid=pid,
                ppid=int(row.get("ppid") or 0),
                name=str(row.get("name") or ""),
                path=str(row.get("path") or ""),
                cmd=str(row.get("cmd") or ""),
                created=str(row.get("created") or ""),
            )
        return table

    def capture_threads(self, pids: Sequence[int]) -> str:
        """Return the thread table (Id / ThreadState / WaitReason / CPU) of `pids`."""
        if not pids:
            return "(no live process to inspect)\n"
        out_file = _scratch_file(self.scratch_dir, "threads-")
        id_list = ",".join(str(int(p)) for p in pids)
        script = (
            "$sb = New-Object System.Text.StringBuilder;"
            f"foreach ($id in @({id_list})) {{"
            " $p = $null;"
            " try { $p = Get-Process -Id $id -ErrorAction Stop } catch { $p = $null };"
            " if ($null -eq $p) { [void]$sb.AppendLine(\"pid $id : not running\"); continue };"
            " [void]$sb.AppendLine(\"pid $id : $($p.ProcessName)\");"
            " [void]$sb.AppendLine('  Id ThreadState WaitReason TotalProcessorTime');"
            " $threads = @();"
            " try { $threads = @($p.Threads) } catch"
            " { [void]$sb.AppendLine('  <threads unavailable>') };"
            " foreach ($t in $threads) {"
            "  $ts = 'n/a'; $wr = 'n/a'; $cpu = 'n/a';"
            "  try { $ts = [string]$t.ThreadState } catch { $ts = 'n/a' };"
            "  try { $wr = [string]$t.WaitReason } catch { $wr = 'n/a' };"
            "  try { $cpu = [string]$t.TotalProcessorTime } catch { $cpu = 'n/a' };"
            "  [void]$sb.AppendLine(('  {0} {1} {2} {3}' -f $t.Id, $ts, $wr, $cpu)) } };"
            f"[IO.File]::WriteAllText({ps_literal(out_file)}, $sb.ToString(),"
            " (New-Object System.Text.UTF8Encoding($false)))"
        )
        try:
            completed = run_powershell(script, seen_pids=self.helper_pids)
            if completed.returncode != 0:
                return (
                    "<thread capture failed: "
                    + completed.stderr.strip().replace("\n", " ")
                    + ">\n"
                )
            return out_file.read_text(encoding="utf-8", errors="replace")
        except subprocess.TimeoutExpired:
            return "<thread capture timed out>\n"
        finally:
            out_file.unlink(missing_ok=True)

    def stop_processes(self, pids: Sequence[int]) -> None:
        if not pids:
            return
        id_list = ",".join(str(int(p)) for p in pids)
        script = (
            f"foreach ($id in @({id_list})) {{"
            " try { Stop-Process -Id $id -Force -ErrorAction Stop } catch { } }"
        )
        try:
            run_powershell(script, seen_pids=self.helper_pids)
        except subprocess.TimeoutExpired:
            raise HarnessStop("kill", f"Stop-Process timed out for pids {id_list}")


def descendants_of(root: int, table: Dict[int, ProcInfo]) -> List[int]:
    """Return every descendant pid of `root` following recorded parent links."""
    children: Dict[int, List[int]] = {}
    for info in table.values():
        if info.pid == info.ppid:
            continue
        parent = table.get(info.ppid)
        # A recycled parent pid keeps pointing at an unrelated (newer) process;
        # only follow the link when the parent is older than the child.
        if parent is None:
            continue
        if parent.created and info.created and parent.created > info.created:
            continue
        children.setdefault(info.ppid, []).append(info.pid)

    found: List[int] = []
    stack = list(children.get(root, []))
    seen = {root}
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        found.append(pid)
        stack.extend(children.get(pid, []))
    return sorted(found)


def ancestors_of(pid: int, table: Dict[int, ProcInfo]) -> List[int]:
    chain: List[int] = []
    seen = {pid}
    current = table.get(pid)
    while current is not None and current.ppid not in seen:
        parent = table.get(current.ppid)
        if parent is None:
            break
        if parent.created and current.created and parent.created > current.created:
            break
        chain.append(parent.pid)
        seen.add(parent.pid)
        current = parent
    return chain


# --------------------------------------------------------------------------
# libtest / cargo log parsing
# --------------------------------------------------------------------------

RUNNING_RE = re.compile(r"^\s*(?:Running|Doc-tests)\s+(.+?)\s*$")
RESULT_RE = re.compile(
    r"^test result:\s+(\w+)\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored;"
    r"\s+(\d+) measured;\s+(\d+) filtered out"
)
FAILED_RE = re.compile(r"^test (\S+) \.\.\. FAILED\s*$")
OUTCOME_RE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)")
SLOW_RE = re.compile(r"^test (\S+) has been running for over \d+ seconds\s*$")
ERROR_RE = re.compile(r"^error(\[[^\]]+\])?:", re.MULTILINE)


@dataclass
class LogSummary:
    suites: List[Tuple[str, Optional[Dict[str, int]]]] = field(default_factory=list)
    failed_names: List[str] = field(default_factory=list)
    slow_without_result: List[str] = field(default_factory=list)
    suites_without_result: List[str] = field(default_factory=list)
    passed: int = 0
    failed: int = 0
    ignored: int = 0

    @property
    def suite_count(self) -> int:
        return sum(1 for _, result in self.suites if result is not None)


def parse_log(text: str) -> LogSummary:
    summary = LogSummary()
    current: Optional[str] = None
    finished: set = set()
    slow: List[str] = []

    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        running = RUNNING_RE.match(line)
        if running is not None:
            if current is not None:
                summary.suites.append((current, None))
            current = running.group(1)
            continue
        result = RESULT_RE.match(line.strip())
        if result is not None:
            parsed = {
                "passed": int(result.group(2)),
                "failed": int(result.group(3)),
                "ignored": int(result.group(4)),
            }
            summary.suites.append((current if current is not None else "(direct)", parsed))
            summary.passed += parsed["passed"]
            summary.failed += parsed["failed"]
            summary.ignored += parsed["ignored"]
            current = None
            continue
        failed = FAILED_RE.match(line.strip())
        if failed is not None:
            summary.failed_names.append(failed.group(1))
        outcome = OUTCOME_RE.match(line.strip())
        if outcome is not None:
            finished.add(outcome.group(1))
        slow_line = SLOW_RE.match(line.strip())
        if slow_line is not None:
            slow.append(slow_line.group(1))

    if current is not None:
        summary.suites.append((current, None))

    summary.slow_without_result = sorted({name for name in slow if name not in finished})
    summary.suites_without_result = [
        label for label, result in summary.suites if result is None
    ]
    return summary


def has_result_line(text: str) -> bool:
    return "test result:" in text


def has_error_line(text: str) -> bool:
    return ERROR_RE.search(text) is not None


# --------------------------------------------------------------------------
# Run context
# --------------------------------------------------------------------------


@dataclass
class IterationRecord:
    index: int
    classification: str
    returncode: Optional[int]
    seconds: float
    log_path: Path
    capture_path: Optional[Path]
    log_summary: LogSummary


class Harness:
    def __init__(self, args: argparse.Namespace, run_dir: Path) -> None:
        self.args = args
        self.run_dir = run_dir
        self.probe = ProcessProbe(run_dir)
        self.progress_path = run_dir / "progress.jsonl"
        self.hygiene_path = run_dir / "hygiene.log"
        self.summary_path = run_dir / "summary.txt"
        self.cleanup_path = run_dir / "cleanup-final.txt"
        self.leftovers_path = run_dir / "leftovers.json"
        self.selfcheck_path = run_dir / "selfcheck.txt"
        self.records: List[IterationRecord] = []
        # Population (A): every pid ever observed (periodic sampling, see
        # `_sample_iteration`) as the iteration root or one of its
        # descendants.  Verified against a live snapshot before being
        # terminated; a pid that already exited is simply skipped.
        self.leftovers: Dict[ProcKey, ProcInfo] = {}
        self.saturation: List[subprocess.Popen] = []
        self.saturation_pids: List[int] = []
        self.unattributed: List[Tuple[int, ProcInfo]] = []
        self.hygiene_events = 0
        self.protected: set = set(ALWAYS_PROTECTED)
        self.resolved_exe: Optional[Path] = None
        self.iteration_command: List[str] = []
        self.started_at = time.monotonic()
        self.started_iso = now_iso()
        self.stop: Optional[HarnessStop] = None
        self.budget_hit = False
        self.target_roots = [REPO_ROOT / "target"]
        for extra in args.extra_target_dir or []:
            self.target_roots.append(Path(extra).resolve())
        if args.sample_interval_seconds is not None:
            self.sample_interval = args.sample_interval_seconds
        else:
            env_value = os.environ.get(SAMPLE_INTERVAL_ENV_VAR)
            self.sample_interval = (
                float(env_value) if env_value else DEFAULT_SAMPLE_INTERVAL_SECONDS
            )

    # -- reporting ---------------------------------------------------------

    def progress(self, event: str, **fields: object) -> None:
        payload = {"time": now_iso(), "event": event}
        payload.update(fields)
        line = json.dumps(payload, ensure_ascii=False)
        with self.progress_path.open("a", encoding="utf-8") as handle:
            handle.write(line + "\n")
        print(line, flush=True)

    def hygiene_note(self, text: str) -> None:
        with self.hygiene_path.open("a", encoding="utf-8") as handle:
            handle.write(f"[{now_iso()}] {text}\n")

    # -- self diagnosis -------------------------------------------------

    def self_check(self) -> None:
        lines = [f"self-check at {now_iso()}", f"python: {sys.version}"]
        try:
            table = self.probe.snapshot()
        except HarnessStop as stop:
            lines.append(f"FAIL snapshot: {stop.message}")
            self.selfcheck_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
            raise HarnessStop("self-check", stop.message)
        own = os.getpid()
        if own not in table:
            lines.append("FAIL snapshot does not contain the harness pid")
            self.selfcheck_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
            raise HarnessStop(
                "self-check", "PowerShell process snapshot does not list the harness pid"
            )
        lines.append(f"OK snapshot: {len(table)} processes, harness pid {own} present")

        threads = self.probe.capture_threads([own])
        if "ThreadState" not in threads:
            lines.append("FAIL thread capture: " + threads.strip()[:200])
            self.selfcheck_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
            raise HarnessStop(
                "self-check", "PowerShell thread capture did not return a thread table"
            )
        lines.append(f"OK thread capture: {len(threads.splitlines())} lines for pid {own}")

        self.protected.add(own)
        for pid in ancestors_of(own, table):
            self.protected.add(pid)
        lines.append("protected pids (harness + ancestors): " + str(sorted(self.protected)))
        self.selfcheck_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        self.progress("self-check", result="ok", processes=len(table))

    # -- target resolution --------------------------------------------------

    def resolve_target(self) -> None:
        args = self.args
        if args.target == "cargo" and args.form != "full":
            self.resolved_exe = self._cargo_resolve()
        elif args.target in ("exe", "command"):
            self.resolved_exe = self._locate_exe()
        self.iteration_command = self._build_iteration_command()
        self.progress(
            "target-resolved",
            command=self.iteration_command,
            resolved_executable=str(self.resolved_exe) if self.resolved_exe else None,
        )

    def _locate_exe(self) -> Path:
        raw = self.args.exe
        candidate = Path(raw)
        if candidate.exists():
            return candidate.resolve()
        found = shutil.which(raw)
        if found is None:
            raise HarnessStop("resolve", f"executable not found: {raw}")
        return Path(found).resolve()

    def _cargo_resolve(self) -> Path:
        log_path = self.run_dir / "resolve.log"
        command = [
            "cargo",
            "test",
            "-p",
            self.args.crate,
            "--test",
            self.args.test_file,
            "--no-run",
            "--message-format=json",
        ]
        self.progress("resolve-start", command=command, log=str(log_path))
        with log_path.open("wb") as handle:
            completed = subprocess.run(
                command,
                cwd=str(REPO_ROOT),
                stdout=handle,
                stderr=subprocess.STDOUT,
            )
        text = log_path.read_text(encoding="utf-8", errors="replace")
        if completed.returncode != 0:
            raise HarnessStop(
                "resolve",
                f"cargo target resolution failed with rc={completed.returncode}; "
                f"see {log_path}",
            )
        executable: Optional[str] = None
        for line in text.splitlines():
            if not line.startswith("{"):
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") != "compiler-artifact":
                continue
            target = message.get("target") or {}
            if "test" not in (target.get("kind") or []):
                continue
            if target.get("name") != self.args.test_file:
                continue
            if message.get("executable"):
                executable = message["executable"]
        if executable is None:
            raise HarnessStop(
                "resolve",
                f"cargo produced no test executable for --test {self.args.test_file}; "
                f"see {log_path}",
            )
        return Path(executable).resolve()

    def _build_iteration_command(self) -> List[str]:
        args = self.args
        if args.form == "full":
            cargo_args = args.cargo_arg or list(DEFAULT_CARGO_ARGS)
            return ["cargo"] + cargo_args
        assert self.resolved_exe is not None
        command = [str(self.resolved_exe)] + list(args.arg or [])
        if args.form == "single" and args.test_name:
            command += [args.test_name, "--exact", "--test-threads", "1"]
        return command

    @property
    def iteration_runs_cargo(self) -> bool:
        return self.args.form == "full"

    # -- population and hygiene -----------------------------------

    def _under_target_root(self, path: str) -> bool:
        if not path:
            return False
        try:
            resolved = Path(path).resolve()
        except OSError:
            return False
        for root in self.target_roots:
            try:
                if resolved.is_relative_to(root):
                    return True
            except ValueError:
                continue
        return False

    def collect_population(
        self, table: Dict[int, ProcInfo]
    ) -> List[Tuple[ProcInfo, str]]:
        """The population is exactly (A) union (B), nothing else.

        (A) observed-descendant - pids merged into `self.leftovers` by
        `_sample_iteration` while some iteration was running (periodic
        sampling of the root's descendant tree; see the module docstring
        of `_sample_iteration`).
        (B) target-dir - executables living under `target/` (or an
        `--extra-target-dir`), verified directly against the live snapshot.
        A survivor that matches neither is never touched here.
        """
        victims: Dict[ProcKey, Tuple[ProcInfo, str]] = {}

        def add(info: ProcInfo, reason: str) -> None:
            if info.pid in self.protected or info.pid in self.saturation_pids:
                return
            if info.pid in self.probe.helper_pids:
                return
            victims.setdefault(info.key, (info, reason))

        for key, info in self.leftovers.items():
            live = table.get(key[0])
            if live is not None and live.created == key[1]:
                add(live, "observed-descendant")
        for info in table.values():
            if self._under_target_root(info.path):
                add(info, "target-dir")
        return [victims[key] for key in sorted(victims)]

    def sweep(self, table: Dict[int, ProcInfo], phase: str) -> List[str]:
        """Terminate the cleanup population and verify it is gone.  Fail loud if not."""
        population = self.collect_population(table)
        notes: List[str] = []
        if not population:
            self.hygiene_note(f"{phase}: population empty, nothing to terminate")
            return notes
        for info, reason in population:
            note = f"{phase}: terminating [{reason}] {info.describe()}"
            self.hygiene_note(note)
            notes.append(note)
        self.probe.stop_processes([info.pid for info, _ in population])
        time.sleep(0.5)
        after = self.probe.snapshot()
        stubborn = []
        for info, reason in population:
            live = after.get(info.pid)
            if live is not None and live.created == info.created:
                stubborn.append(info)
                self.hygiene_note(f"{phase}: STILL ALIVE {info.describe()}")
            else:
                self.hygiene_note(f"{phase}: terminated pid={info.pid}")
        self.hygiene_events += len(population)
        self.progress(
            "hygiene",
            phase=phase,
            terminated=len(population) - len(stubborn),
            still_alive=len(stubborn),
            details=[f"{info.pid}:{info.name}:{reason}" for info, reason in population],
        )
        if stubborn:
            raise HarnessStop(
                "hygiene",
                "could not terminate "
                + "; ".join(info.describe() for info in stubborn),
            )
        for info, _ in population:
            self.leftovers.pop(info.key, None)
        return notes

    # -- periodic observation --------------------------------------

    @staticmethod
    def _root_and_descendants(
        root_pid: int, table: Dict[int, ProcInfo]
    ) -> Dict[ProcKey, ProcInfo]:
        found: Dict[ProcKey, ProcInfo] = {}
        root = table.get(root_pid)
        if root is not None:
            found[root.key] = root
        for pid in descendants_of(root_pid, table):
            info = table.get(pid)
            if info is not None:
                found[info.key] = info
        return found

    def _sample_iteration(self, root_pid: int) -> Dict[int, ProcInfo]:
        """Take one process snapshot and merge the root's live descendant
        tree into `self.leftovers` (population (A)).

        A pid observed here stays attributed even if its immediate parent
        exits before the next sample: the merge is additive across every
        sample taken during the iteration, so a chain that is only intact
        for one snapshot is enough to keep the whole branch attributed.
        Returns the snapshot table so callers needing it (capture, the
        pre/post iteration diff) do not have to take another one.
        """
        table = self.probe.snapshot()
        for key, info in self._root_and_descendants(root_pid, table).items():
            self.leftovers[key] = info
        return table

    # -- capture --------------------------------------------------------

    def capture(self, index: int, root_pid: int) -> Path:
        table = self._sample_iteration(root_pid)
        pids = [root_pid] + descendants_of(root_pid, table)
        live = [pid for pid in pids if pid in table]
        threads = self.probe.capture_threads(live)
        path = self.run_dir / f"capture-iter-{index:03d}.txt"
        lines = [
            f"# capture for iteration {index}",
            f"# taken at {now_iso()} before terminating the target",
            f"# root pid {root_pid}, {len(live)} live process(es) in the tree",
            "",
            "## process tree (root and descendants)",
        ]
        for pid in pids:
            info = table.get(pid)
            if info is None:
                lines.append(f"pid={pid} (gone before capture)")
                continue
            marker = "root" if pid == root_pid else "descendant"
            lines.append(f"[{marker}] {info.describe()}")
        lines += ["", "## thread state (Id / ThreadState / WaitReason / TotalProcessorTime)", ""]
        lines.append(threads.rstrip("\n"))
        lines.append("")
        path.write_text("\n".join(lines), encoding="utf-8")
        return path

    def kill_tree(self, root_pid: int) -> None:
        table = self.probe.snapshot()
        pids = [pid for pid in descendants_of(root_pid, table) if pid not in self.protected]
        self.probe.stop_processes(pids + [root_pid])

    # -- saturation ---------------------------------------------------------

    def start_saturation(self) -> None:
        if not self.args.saturate:
            return
        workers = self.args.saturate_workers or (os.cpu_count() or 1)
        for _ in range(workers):
            proc = subprocess.Popen(
                [sys.executable, "-c", "while True: pass"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                cwd=str(REPO_ROOT),
            )
            self.saturation.append(proc)
            self.saturation_pids.append(proc.pid)
        self.progress("saturation-start", workers=workers, pids=self.saturation_pids)

    def stop_saturation(self) -> List[str]:
        notes: List[str] = []
        for proc in self.saturation:
            try:
                proc.kill()
            except OSError:
                pass
        for proc in self.saturation:
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                notes.append(f"saturation pid {proc.pid} did not exit")
        if self.saturation_pids:
            self.progress("saturation-stop", pids=self.saturation_pids)
        return notes

    # -- main loop ----------------------------------------------------------

    def loop(self) -> None:
        timeout = self.args.timeout_seconds
        if timeout is None:
            timeout = DEFAULT_FULL_TIMEOUT if self.args.form == "full" else DEFAULT_ITERATION_TIMEOUT
        for index in range(1, self.args.iterations + 1):
            elapsed = time.monotonic() - self.started_at
            if elapsed >= self.args.budget_seconds:
                self.budget_hit = True
                self.progress(
                    "budget-stop", elapsed_seconds=round(elapsed, 1), next_iteration=index
                )
                break
            self.run_iteration(index, timeout)

    def run_iteration(self, index: int, timeout: float) -> None:
        before = self.probe.snapshot()
        terminated = self.sweep(before, f"pre-iteration {index}")
        # Re-baseline only when something was killed, so the diff of this
        # iteration is not polluted by the processes we just removed.
        baseline = self.probe.snapshot() if terminated else before

        log_path = self.run_dir / f"iter-{index:03d}.log"
        self.progress(
            "iteration-start",
            iteration=index,
            log=str(log_path),
            timeout_seconds=timeout,
            sample_interval_seconds=self.sample_interval,
        )
        started = time.monotonic()
        with log_path.open("wb") as handle:
            proc = subprocess.Popen(
                self.iteration_command,
                cwd=str(REPO_ROOT),
                stdout=handle,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
            )
            try:
                self._sample_iteration(proc.pid)
            except HarnessStop:
                proc.kill()
                proc.wait(timeout=30)
                raise
            last_sample = time.monotonic()
            timed_out = False
            deadline = started + timeout
            while True:
                if proc.poll() is not None:
                    break
                if time.monotonic() >= deadline:
                    timed_out = True
                    break
                now = time.monotonic()
                if now - last_sample >= self.sample_interval:
                    try:
                        self._sample_iteration(proc.pid)
                    except HarnessStop:
                        proc.kill()
                        proc.wait(timeout=30)
                        raise
                    last_sample = now
                time.sleep(0.2)

        capture_path: Optional[Path] = None
        if timed_out:
            capture_path = self.capture(index, proc.pid)
            self.kill_tree(proc.pid)
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                raise HarnessStop("kill", f"iteration {index} target pid {proc.pid} survived kill")
        seconds = time.monotonic() - started

        text = log_path.read_text(encoding="utf-8", errors="replace")
        summary = parse_log(text)
        if timed_out:
            classification = "hang"
            returncode: Optional[int] = None
        else:
            returncode = proc.returncode
            classification = self.classify(returncode, text)

        # Final sample: catches descendants still alive at process exit even
        # when their immediate parent already died (see `_sample_iteration`),
        # and doubles as the "after" table for the unattributed-survivor scan.
        after = self._sample_iteration(proc.pid)
        unattributed = self.find_unattributed(baseline, after)
        for info in unattributed:
            self.hygiene_note(
                f"iteration {index}: unattributed new survivor (not terminated) "
                f"{info.describe()}"
            )
        self.unattributed.extend((index, info) for info in unattributed)

        self.records.append(
            IterationRecord(
                index=index,
                classification=classification,
                returncode=returncode,
                seconds=seconds,
                log_path=log_path,
                capture_path=capture_path,
                log_summary=summary,
            )
        )
        self.progress(
            "iteration-end",
            iteration=index,
            classification=classification,
            returncode=returncode,
            seconds=round(seconds, 2),
            log=str(log_path),
            capture=str(capture_path) if capture_path else None,
            suites=summary.suite_count,
            passed=summary.passed,
            failed=summary.failed,
            ignored=summary.ignored,
            unattributed_survivor_pids=[info.pid for info in unattributed],
            completed_iterations=len(self.records),
        )

    def classify(self, returncode: int, text: str) -> str:
        if (
            self.iteration_runs_cargo
            and returncode != 0
            and not has_result_line(text)
            and has_error_line(text)
        ):
            return "build-failure"
        return "pass" if returncode == 0 else "fail"

    def find_unattributed(
        self, baseline: Dict[int, ProcInfo], after: Dict[int, ProcInfo]
    ) -> List[ProcInfo]:
        """Survivors new since `baseline` that belong to neither (A) nor
        (B) are reported only - never terminated (see the module docstring
        of `collect_population`).
        """
        found: List[ProcInfo] = []
        for pid, info in after.items():
            if pid in self.protected or pid in self.saturation_pids:
                continue
            if pid in self.probe.helper_pids:
                continue
            old = baseline.get(pid)
            if old is not None and old.created == info.created:
                continue  # not new since the iteration started
            if info.key in self.leftovers:
                continue  # (A) observed descendant
            if self._under_target_root(info.path):
                continue  # (B) target-dir executable
            found.append(info)
        found.sort(key=lambda info: info.pid)
        return found

    # -- shutdown -----------------------------------------------------------

    def finish(self) -> int:
        cleanup_lines = [f"# final cleanup at {now_iso()}"]
        for note in self.stop_saturation():
            cleanup_lines.append("WARN " + note)
        try:
            table = self.probe.snapshot()
        except HarnessStop as stop:
            self.stop = self.stop or stop
            table = {}
        cleanup_lines.append("## saturation workers started by this run")
        for pid in self.saturation_pids:
            state = "alive" if pid in table else "gone"
            cleanup_lines.append(f"pid={pid} {state}")
            if pid in table:
                self.probe.stop_processes([pid])
        self.saturation_pids = []

        cleanup_lines.append("## population at shutdown")
        population = self.collect_population(table) if table else []
        for info, reason in population:
            cleanup_lines.append(f"[{reason}] {info.describe()}")
        if population:
            self.probe.stop_processes([info.pid for info, _ in population])
            time.sleep(0.5)
        residual: List[ProcInfo] = []
        try:
            final_table = self.probe.snapshot()
        except HarnessStop as stop:
            self.stop = self.stop or stop
            final_table = {}
        for info, _ in population:
            live = final_table.get(info.pid)
            if live is not None and live.created == info.created:
                residual.append(info)
        cleanup_lines.append("## residual after cleanup")
        cleanup_lines.append(f"residual processes: {len(residual)}")
        for info in residual:
            cleanup_lines.append("STILL ALIVE " + info.describe())
        cleanup_lines.append("")
        self.cleanup_path.write_text("\n".join(cleanup_lines), encoding="utf-8")
        self.leftovers_path.write_text(
            json.dumps(
                [
                    {"pid": info.pid, "created": info.created, "name": info.name}
                    for info in self.leftovers.values()
                ],
                indent=2,
            ),
            encoding="utf-8",
        )
        if residual and self.stop is None:
            self.stop = HarnessStop(
                "cleanup",
                "processes survived the final cleanup: "
                + "; ".join(info.describe() for info in residual),
            )
        summary_text = self.render_summary(residual)
        self.summary_path.write_text(summary_text, encoding="utf-8")
        print(summary_text, flush=True)
        self.progress(
            "run-end" if self.stop is None else "run-stopped",
            completed_iterations=len(self.records),
            stage=self.stop.stage if self.stop else None,
        )
        return 1 if self.stop is not None else 0

    def render_summary(self, residual: Sequence[ProcInfo]) -> str:
        counts = {"pass": 0, "hang": 0, "fail": 0, "build-failure": 0}
        by_class: Dict[str, List[int]] = {key: [] for key in counts}
        for record in self.records:
            counts[record.classification] += 1
            by_class[record.classification].append(record.index)

        lines = [
            "=== flake harness summary ===",
            f"run id            : {self.run_dir.name}",
            f"run directory     : {self.run_dir}",
            f"started / ended   : {self.started_iso} / {now_iso()}",
            f"elapsed seconds   : {round(time.monotonic() - self.started_at, 1)}",
            f"axes              : target={self.args.target} form={self.args.form} "
            f"saturation={'on' if self.args.saturate else 'off'}",
            f"iteration command : {self.iteration_command}",
            f"resolved exe      : {self.resolved_exe}",
            f"iterations asked  : {self.args.iterations}",
            f"completed iterations: {len(self.records)}",
            "",
            "classification counts:",
            f"  pass          : {counts['pass']}  iterations {by_class['pass']}",
            f"  hang          : {counts['hang']}  iterations {by_class['hang']}",
            f"  fail          : {counts['fail']}  iterations {by_class['fail']}",
            f"  build-failure : {counts['build-failure']}  iterations {by_class['build-failure']}",
            "",
        ]
        if self.stop is not None:
            lines += [
                "STOPPED (fail-loud):",
                f"  stage   : {self.stop.stage}",
                f"  message : {self.stop.message}",
                f"  completed iterations: {len(self.records)}",
                "",
            ]
        if self.budget_hit:
            lines += [f"budget stop: time budget {self.args.budget_seconds}s reached", ""]

        lines.append("iterations:")
        for record in self.records:
            lines.append(
                f"  {record.index:03d} {record.classification:<13} "
                f"rc={record.returncode} {record.seconds:8.2f}s log={record.log_path}"
            )
            if record.capture_path is not None:
                lines.append(f"      capture: {record.capture_path}")
        lines.append("")

        captures = [r.capture_path for r in self.records if r.capture_path is not None]
        lines.append(f"capture files ({len(captures)}):")
        for path in captures:
            lines.append(f"  {path}")
        lines.append("")

        lines.append("libtest aggregation (Running / test result: pairing):")
        total_suites = 0
        total = {"passed": 0, "failed": 0, "ignored": 0}
        for record in self.records:
            s = record.log_summary
            total_suites += s.suite_count
            total["passed"] += s.passed
            total["failed"] += s.failed
            total["ignored"] += s.ignored
            lines.append(
                f"  iteration {record.index:03d}: suites={s.suite_count} "
                f"passed={s.passed} failed={s.failed} ignored={s.ignored}"
            )
        lines.append(
            f"  total: suites={total_suites} passed={total['passed']} "
            f"failed={total['failed']} ignored={total['ignored']}"
        )
        lines.append("")

        failed_names: Dict[str, int] = {}
        slow_names: Dict[str, int] = {}
        orphan_suites: Dict[str, int] = {}
        for record in self.records:
            for name in record.log_summary.failed_names:
                failed_names[name] = failed_names.get(name, 0) + 1
            for name in record.log_summary.slow_without_result:
                slow_names[name] = slow_names.get(name, 0) + 1
            for label in record.log_summary.suites_without_result:
                orphan_suites[label] = orphan_suites.get(label, 0) + 1
        lines.append("extraction:")
        lines.append(f"  FAILED test names ({len(failed_names)}):")
        for name, count in sorted(failed_names.items()):
            lines.append(f"    {name} x{count}")
        lines.append(f"  over-60s warned without result line ({len(slow_names)}):")
        for name, count in sorted(slow_names.items()):
            lines.append(f"    {name} x{count}")
        lines.append(f"  Running without test result line ({len(orphan_suites)}):")
        for label, count in sorted(orphan_suites.items()):
            lines.append(f"    {label} x{count}")
        lines.append("")

        lines += [
            "hygiene / cleanup:",
            f"  sample interval seconds: {self.sample_interval}",
            f"  pre-iteration hygiene terminations: {self.hygiene_events}",
            f"  hygiene log     : {self.hygiene_path}",
            f"  cleanup record  : {self.cleanup_path}",
            f"  leftover record : {self.leftovers_path}",
            f"  residual processes after cleanup: {len(residual)}",
        ]
        for info in residual:
            lines.append("    STILL ALIVE " + info.describe())
        lines.append(
            f"  unattributed new survivor processes (reported, not terminated): "
            f"{len(self.unattributed)}"
        )
        for iteration, info in self.unattributed:
            lines.append(f"    iteration {iteration:03d}: {info.describe()}")
        lines += [
            "",
            "files:",
            f"  progress  : {self.progress_path}",
            f"  summary   : {self.summary_path}",
            f"  self-check: {self.selfcheck_path}",
            f"  snapshots taken: {self.probe.snapshot_calls} "
            f"({round(self.probe.snapshot_seconds, 1)}s total)",
            "",
        ]
        return "\n".join(lines)


# --------------------------------------------------------------------------
# Subcommands
# --------------------------------------------------------------------------


def prepare_run_dir(args: argparse.Namespace) -> Path:
    base = Path(args.log_base).resolve() if args.log_base else DEFAULT_LOG_BASE
    run_id = args.run_id if args.run_id is not None else f"run-{now_stamp()}"
    run_dir = base / run_id
    if run_dir.exists() and any(run_dir.iterdir()):
        sys.stderr.write(f"run directory already exists and is not empty: {run_dir}\n")
        raise SystemExit(1)
    run_dir.mkdir(parents=True, exist_ok=True)
    return run_dir


RUN_ID_PATTERN = re.compile(r"[A-Za-z0-9._-]{1,64}")
WINDOWS_RESERVED_NAMES = frozenset(
    {"CON", "PRN", "AUX", "NUL"}
    | {f"COM{digit}" for digit in range(1, 10)}
    | {f"LPT{digit}" for digit in range(1, 10)}
)


def run_id_arg(value: str) -> str:
    """argparse ``type=`` for ``--run-id``: a single path component only.

    `prepare_run_dir` joins this straight onto the log base directory, so
    anything containing a separator or resolving to ``.``/``..`` could
    escape it (CWE-22); reject those here instead of at join time.
    Windows reserved device names (``CON``, ``NUL``, ``COM1`` ...) are
    rejected on every OS, so a run id stays portable across platforms.
    """
    # `fullmatch` rather than a `$`-anchored `match`: `$` also matches before a trailing newline.
    if not RUN_ID_PATTERN.fullmatch(value) or value in (".", ".."):
        raise argparse.ArgumentTypeError(
            "--run-id must match [A-Za-z0-9._-]{1,64} and must not be '.' or '..'"
        )
    # Windows keys the device name off the part before the *first* dot, not the extension.
    if value.split(".", 1)[0].upper() in WINDOWS_RESERVED_NAMES:
        raise argparse.ArgumentTypeError(
            "--run-id must not be a Windows reserved device name (CON/PRN/AUX/NUL/COM1-9/LPT1-9)"
        )
    return value


def validate_run_args(args: argparse.Namespace) -> None:
    def die(message: str) -> None:
        sys.stderr.write(f"invalid arguments: {message}\n")
        raise SystemExit(2)

    if args.target == "cargo":
        if args.form == "full":
            if args.crate or args.test_file:
                die("--target cargo --form full runs cargo directly; drop --crate/--test-file")
        else:
            if not args.crate or not args.test_file:
                die("--target cargo with --form single|binary needs --crate and --test-file")
        if args.exe or args.arg:
            die("--target cargo does not take --exe/--arg")
    else:
        if not args.exe:
            die(f"--target {args.target} needs --exe")
        if args.form == "full":
            die("--form full requires --target cargo")
    if args.target == "exe" and args.arg:
        die("--target exe does not take --arg; use --target command for free argument lists")
    if args.target == "command":
        if args.form != "single":
            die("--target command supports --form single only (one process per iteration)")
        if args.test_name:
            die("--target command does not take --test-name")
    if args.form == "single" and args.target != "command" and not args.test_name:
        die("--form single needs --test-name")
    if args.form in ("binary", "full") and args.test_name:
        die(f"--form {args.form} does not take --test-name")
    if args.form != "full" and args.cargo_arg:
        die("--cargo-arg applies to --form full only")
    if args.iterations < 1:
        die("--iterations must be >= 1")
    if args.sample_interval_seconds is not None and args.sample_interval_seconds <= 0:
        die("--sample-interval-seconds must be > 0")


def cmd_run(args: argparse.Namespace) -> int:
    validate_run_args(args)
    run_dir = prepare_run_dir(args)
    harness = Harness(args, run_dir)
    print(f"run directory: {run_dir}", flush=True)
    harness.progress(
        "run-start",
        run_dir=str(run_dir),
        target=args.target,
        form=args.form,
        saturation=bool(args.saturate),
        iterations=args.iterations,
    )
    try:
        harness.self_check()
        harness.resolve_target()
        harness.start_saturation()
        harness.loop()
    except HarnessStop as stop:
        harness.stop = stop
        harness.progress("stop", stage=stop.stage, message=stop.message)
    except KeyboardInterrupt:
        harness.stop = HarnessStop("interrupt", "interrupted by the caller")
    return harness.finish()


def cmd_sweep(args: argparse.Namespace) -> int:
    base = Path(args.log_base).resolve() if args.log_base else DEFAULT_LOG_BASE
    base.mkdir(parents=True, exist_ok=True)
    out_path = Path(args.out).resolve() if args.out else base / f"sweep-{now_stamp()}.txt"
    probe = ProcessProbe(base)
    lines = [f"# standalone sweep at {now_iso()}"]
    rc = 0
    try:
        table = probe.snapshot()
        own = os.getpid()
        protected = (
            set(ALWAYS_PROTECTED)
            | {own}
            | set(ancestors_of(own, table))
            | probe.helper_pids
        )
        lines.append(f"protected pids: {sorted(protected)}")

        roots = [REPO_ROOT / "target"] + [
            Path(p).resolve() for p in (args.extra_target_dir or [])
        ]
        wanted: Dict[int, Tuple[ProcInfo, str]] = {}
        for pid in args.pid or []:
            info = table.get(int(pid))
            if info is not None and int(pid) not in protected:
                wanted[int(pid)] = (info, "explicit --pid")
        for run_dir in args.from_run or []:
            record = Path(run_dir).resolve() / "leftovers.json"
            if not record.exists():
                lines.append(f"leftover record missing: {record}")
                continue
            for entry in json.loads(record.read_text(encoding="utf-8")):
                info = table.get(int(entry["pid"]))
                if info is not None and info.created == entry.get("created"):
                    if info.pid not in protected:
                        wanted[info.pid] = (info, f"leftover of {record}")
        for info in table.values():
            if info.pid in protected or not info.path:
                continue
            try:
                resolved = Path(info.path).resolve()
            except OSError:
                continue
            for root in roots:
                try:
                    if resolved.is_relative_to(root):
                        wanted[info.pid] = (info, f"executable under {root}")
                except ValueError:
                    continue

        lines.append(f"## population ({len(wanted)})")
        for pid in sorted(wanted):
            info, reason = wanted[pid]
            lines.append(f"[{reason}] {info.describe()}")
        if wanted:
            probe.stop_processes(sorted(wanted))
            time.sleep(0.5)
        after = probe.snapshot()
        residual = [
            info
            for pid, (info, _) in wanted.items()
            if pid in after and after[pid].created == info.created
        ]
        lines.append("## result")
        lines.append(f"terminated: {len(wanted) - len(residual)}")
        lines.append(f"residual processes: {len(residual)}")
        for info in residual:
            lines.append("STILL ALIVE " + info.describe())
            rc = 1
    except HarnessStop as stop:
        lines.append(f"STOPPED at stage {stop.stage}: {stop.message}")
        rc = 1
    lines.append("")
    out_path.write_text("\n".join(lines), encoding="utf-8")
    print("\n".join(lines), flush=True)
    print(f"sweep record: {out_path}", flush=True)
    return rc


def cmd_selfcheck(args: argparse.Namespace) -> int:
    base = Path(args.log_base).resolve() if args.log_base else DEFAULT_LOG_BASE
    base.mkdir(parents=True, exist_ok=True)
    probe = ProcessProbe(base)
    try:
        table = probe.snapshot()
        own = os.getpid()
        if own not in table:
            raise HarnessStop("self-check", "snapshot does not list the harness pid")
        threads = probe.capture_threads([own])
        if "ThreadState" not in threads:
            raise HarnessStop("self-check", "thread capture returned no table")
    except HarnessStop as stop:
        print(f"self-check FAILED at stage {stop.stage}: {stop.message}", flush=True)
        return 1
    print(f"self-check ok: {len(table)} processes, harness pid {own} present", flush=True)
    print(f"thread capture: {len(threads.splitlines())} lines", flush=True)
    print(f"protected pids (harness + ancestors): {sorted(ancestors_of(own, table) + [own])}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="flake_harness.py",
        description="Repeat one measurement arm and classify every iteration "
        "(Windows only). See README.md.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    def add_common(sub: argparse.ArgumentParser) -> None:
        sub.add_argument("--log-base", help=f"log root (default {DEFAULT_LOG_BASE})")
        sub.add_argument(
            "--extra-target-dir",
            action="append",
            help="additional CARGO_TARGET_DIR whose executables belong to the "
            "cleanup population (repeatable)",
        )

    run = subparsers.add_parser("run", help="run one measurement arm")
    add_common(run)
    run.add_argument(
        "--run-id",
        type=run_id_arg,
        help="run directory name, [A-Za-z0-9._-]{1,64} (default run-<timestamp>)",
    )
    run.add_argument(
        "--target",
        choices=("cargo", "exe", "command"),
        required=True,
        help="axis (A): how the iteration target is resolved",
    )
    run.add_argument(
        "--form",
        choices=("single", "binary", "full"),
        required=True,
        help="axis (B): execution form",
    )
    run.add_argument(
        "--saturate",
        action="store_true",
        help="axis (C): keep every logical core busy while iterating",
    )
    run.add_argument("--saturate-workers", type=int, help="busy loop count (default: cpu count)")
    run.add_argument("--crate", help="cargo package for --target cargo")
    run.add_argument("--test-file", help="integration test target name for --target cargo")
    run.add_argument("--exe", help="executable for --target exe|command")
    run.add_argument(
        "--arg",
        action="append",
        help="argument for --target command (repeatable, use --arg=-x for dash values)",
    )
    run.add_argument("--test-name", help="test name for --form single")
    run.add_argument(
        "--cargo-arg",
        action="append",
        help="replace the cargo argument list of --form full (repeatable, "
        "default: test --workspace --no-fail-fast)",
    )
    run.add_argument("--iterations", type=int, default=1)
    run.add_argument(
        "--sample-interval-seconds",
        type=float,
        help="periodic process-table sampling interval used to build the "
        f"observed-descendant population (default {DEFAULT_SAMPLE_INTERVAL_SECONDS:.0f}, "
        f"env {SAMPLE_INTERVAL_ENV_VAR})",
    )
    run.add_argument(
        "--timeout-seconds",
        type=float,
        help=f"per iteration deadline (default {DEFAULT_ITERATION_TIMEOUT:.0f}, "
        f"{DEFAULT_FULL_TIMEOUT:.0f} for --form full)",
    )
    run.add_argument(
        "--budget-seconds",
        type=float,
        default=DEFAULT_BUDGET,
        help=f"total time budget checked between iterations (default {DEFAULT_BUDGET:.0f})",
    )
    run.set_defaults(func=cmd_run)

    sweep = subparsers.add_parser("sweep", help="terminate leftover processes and report")
    add_common(sweep)
    sweep.add_argument("--pid", action="append", type=int, help="extra pid (repeatable)")
    sweep.add_argument(
        "--from-run",
        action="append",
        help="run directory whose leftovers.json is added to the population (repeatable)",
    )
    sweep.add_argument("--out", help="record file path (default <log base>/sweep-<stamp>.txt)")
    sweep.set_defaults(func=cmd_sweep)

    check = subparsers.add_parser("selfcheck", help="run the PowerShell self diagnosis only")
    add_common(check)
    check.set_defaults(func=cmd_selfcheck)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    require_windows()
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    sys.exit(main())
