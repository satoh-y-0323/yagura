#!/usr/bin/env python3
"""Load benchmark for the yagura daemon (Windows only).

`run` starts an isolated daemon instance, opens pseudo-child windows that
flood output, and measures throughput, input-to-screen latency, CPU time and
resident memory of a named set of processes, repeated R times.  `compare`
judges whether a second result is worse than a baseline.  See README.md.

Standard library only.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import platform
import random
import shutil
import statistics
import struct
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from pathlib import Path
from typing import Dict, List, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
RESULT_FORMAT = 2

DEFAULT_PROCS = ("yagura-daemon.exe", "yagura-gui.exe", "yagura-holder.exe")
FIXTURE = "hex_echo.exe"
READY_TEXT = "CTRL-READY"
FLOODED_TEXT = "QUERIES-FLOODED"
FLOOD_LINE = b"x" * 78 + b"\r\n"
VIEW_SIZE = {"cols": 120, "rows": 30}

# name: (unit, higher_is_better, absolute floor below which a difference is not judged).
# The floors are derived in README.md section 4.
METRICS = {
    "throughput_mib_s": ("MiB/s", True, 0.2),
    "latency_loaded_ms": ("ms", False, 0.02),
    "latency_idle_ms": ("ms", False, 0.25),
    "cpu_flood_s": ("s", False, 4.0),
    "cpu_idle_pct": ("% of one core", False, 0.25),
    "mem_base_mib": ("MiB", False, 0.05),
    "mem_loaded_mib": ("MiB", False, 0.5),
    "mem_per_window_mib": ("MiB", False, 0.06),
}


class BenchError(Exception):
    pass


# --- Windows process statistics -------------------------------------------------

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True) if os.name == "nt" else None
psapi = ctypes.WinDLL("psapi", use_last_error=True) if os.name == "nt" else None

TH32CS_SNAPPROCESS = 0x00000002
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_VM_READ = 0x0010
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", wintypes.DWORD),
        ("cntThreads", wintypes.DWORD),
        ("th32ParentProcessID", wintypes.DWORD),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", wintypes.DWORD),
        ("szExeFile", ctypes.c_wchar * 260),
    ]


class PROCESS_MEMORY_COUNTERS_EX(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


if kernel32 is not None:
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel32.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD),
    ]
    psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(PROCESS_MEMORY_COUNTERS_EX),
        wintypes.DWORD,
    ]


def _list_processes() -> List[tuple]:
    snapshot = kernel32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snapshot in (None, INVALID_HANDLE_VALUE):
        raise BenchError(f"CreateToolhelp32Snapshot failed: {ctypes.get_last_error()}")
    try:
        entry = PROCESSENTRY32W()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32W)
        found = []
        ok = kernel32.Process32FirstW(snapshot, ctypes.byref(entry))
        while ok:
            found.append((entry.th32ProcessID, entry.szExeFile))
            ok = kernel32.Process32NextW(snapshot, ctypes.byref(entry))
        return found
    finally:
        kernel32.CloseHandle(snapshot)


def _filetime_seconds(value: wintypes.FILETIME) -> float:
    return ((value.dwHighDateTime << 32) | value.dwLowDateTime) / 1e7


def _process_stats(pid: int) -> Optional[dict]:
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, False, pid)
    if not handle:
        return None
    try:
        size = wintypes.DWORD(32768)
        path = ctypes.create_unicode_buffer(size.value)
        if not kernel32.QueryFullProcessImageNameW(handle, 0, path, ctypes.byref(size)):
            return None
        times = [wintypes.FILETIME() for _ in range(4)]
        if not kernel32.GetProcessTimes(handle, *[ctypes.byref(t) for t in times]):
            return None
        memory = PROCESS_MEMORY_COUNTERS_EX()
        memory.cb = ctypes.sizeof(memory)
        if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory), memory.cb):
            return None
        return {
            "path": path.value,
            "cpu_s": _filetime_seconds(times[2]) + _filetime_seconds(times[3]),
            "working_set": memory.WorkingSetSize,
            "private": memory.PrivateUsage,
        }
    finally:
        kernel32.CloseHandle(handle)


class ProcessSet:
    """The processes whose image name is in `names` and whose image lives in `bin_dir`.

    The directory filter keeps processes of another yagura (the user's resident
    daemon, an installed build) out of the measurement.
    """

    def __init__(self, names, bin_dir: Path):
        self.names = {name.lower() for name in names}
        self.bin_dir = os.path.normcase(str(bin_dir.resolve()))

    def sample(self) -> Dict[str, dict]:
        """Per image name: total CPU seconds, working set, private bytes and count."""
        totals: Dict[str, dict] = {}
        for pid, exe in _list_processes():
            stats = _process_stats(pid)
            if stats is None:
                continue
            if os.path.normcase(os.path.dirname(stats["path"])) != self.bin_dir:
                continue
            slot = totals.setdefault(
                exe.lower(), {"cpu_s": 0.0, "working_set": 0, "private": 0, "count": 0}
            )
            slot["cpu_s"] += stats["cpu_s"]
            slot["working_set"] += stats["working_set"]
            slot["private"] += stats["private"]
            slot["count"] += 1
        return totals

    def total(self, sample: Dict[str, dict], key: str) -> float:
        return sum(value[key] for name, value in sample.items() if name in self.names)


# --- IPC client --------------------------------------------------------------------


def current_user_sid() -> str:
    output = subprocess.run(
        ["whoami.exe", "/user", "/fo", "csv", "/nh"], capture_output=True, text=True, check=True
    ).stdout
    return output.strip().split(",")[-1].strip('"')


def endpoint_for(instance: str) -> str:
    return "\\\\.\\pipe\\yagura-" + instance + "-" + current_user_sid()


GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
OPEN_EXISTING = 3
FILE_FLAG_OVERLAPPED = 0x40000000
ERROR_BROKEN_PIPE = 109
ERROR_IO_PENDING = 997
WAIT_OBJECT_0 = 0
INFINITE = 0xFFFFFFFF


class OVERLAPPED(ctypes.Structure):
    _fields_ = [
        ("Internal", ctypes.c_void_p),
        ("InternalHigh", ctypes.c_void_p),
        ("Offset", wintypes.DWORD),
        ("OffsetHigh", wintypes.DWORD),
        ("hEvent", wintypes.HANDLE),
    ]


if kernel32 is not None:
    kernel32.CreateFileW.restype = wintypes.HANDLE
    kernel32.CreateFileW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.c_void_p,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    kernel32.CreateEventW.restype = wintypes.HANDLE
    kernel32.CreateEventW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.BOOL, wintypes.LPCWSTR]
    for _transfer_fn in (kernel32.ReadFile, kernel32.WriteFile):
        _transfer_fn.argtypes = [
            wintypes.HANDLE,
            ctypes.c_void_p,
            wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
            ctypes.POINTER(OVERLAPPED),
        ]
    kernel32.GetOverlappedResult.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(OVERLAPPED),
        ctypes.POINTER(wintypes.DWORD),
        wintypes.BOOL,
    ]
    kernel32.WaitForSingleObject.restype = wintypes.DWORD
    kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel32.CancelIoEx.argtypes = [wintypes.HANDLE, ctypes.POINTER(OVERLAPPED)]


class BenchTimeout(BenchError):
    pass


class Connection:
    """One length-prefixed JSON connection. The pipe is opened for overlapped
    I/O so a read gives up at its deadline even when the daemon goes silent;
    each connection is still driven by one thread."""

    def __init__(self, endpoint: str, timeout: float = 10.0):
        deadline = time.monotonic() + timeout
        while True:
            handle = kernel32.CreateFileW(
                endpoint,
                GENERIC_READ | GENERIC_WRITE,
                0,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
            if handle not in (None, INVALID_HANDLE_VALUE):
                break
            error = ctypes.get_last_error()
            if time.monotonic() > deadline:
                raise BenchError(f"cannot connect to {endpoint}: {ctypes.FormatError(error)}")
            time.sleep(0.05)
        self.handle = handle
        self._consumed = 0
        self.event = kernel32.CreateEventW(None, True, False, None)
        if not self.event:
            error = ctypes.get_last_error()
            kernel32.CloseHandle(self.handle)
            self.handle = None
            raise BenchError(f"CreateEventW failed: {ctypes.FormatError(error)}")

    def _transfer(self, operation, buffer, count: int, deadline: Optional[float]) -> int:
        """Runs one overlapped read or write; raises BenchTimeout at `deadline`."""
        if self.handle is None:
            raise BenchError("connection is closed")
        overlapped = OVERLAPPED()
        overlapped.hEvent = self.event
        done = wintypes.DWORD(0)
        if not operation(self.handle, buffer, count, ctypes.byref(done), ctypes.byref(overlapped)):
            error = ctypes.get_last_error()
            if error != ERROR_IO_PENDING:
                raise self._io_error(error)
            if deadline is None:
                wait = INFINITE
            else:
                wait = max(0, int((deadline - time.monotonic()) * 1000))
            if kernel32.WaitForSingleObject(self.event, wait) != WAIT_OBJECT_0:
                kernel32.CancelIoEx(self.handle, ctypes.byref(overlapped))
                # The operation may have completed before the cancel; keep what it moved.
                if kernel32.GetOverlappedResult(
                    self.handle, ctypes.byref(overlapped), ctypes.byref(done), True
                ):
                    return done.value
                raise BenchTimeout("timed out waiting for the daemon")
        if not kernel32.GetOverlappedResult(
            self.handle, ctypes.byref(overlapped), ctypes.byref(done), True
        ):
            raise self._io_error(ctypes.get_last_error())
        return done.value

    @staticmethod
    def _io_error(error: int) -> BenchError:
        if error == ERROR_BROKEN_PIPE:
            return BenchError("daemon closed the connection")
        return BenchError(f"pipe I/O failed: {ctypes.FormatError(error)}")

    def send(self, message: dict, timeout: float = 30.0) -> None:
        """Writes one message; a timeout leaves a partial message on the wire, so the
        connection is dropped."""
        body = json.dumps(message).encode("utf-8")
        data = struct.pack("<I", len(body)) + body
        deadline = time.monotonic() + timeout
        try:
            while data:
                buffer = ctypes.create_string_buffer(data, len(data))
                written = self._transfer(kernel32.WriteFile, buffer, len(data), deadline)
                data = data[written:]
        except BenchTimeout:
            self.close()
            raise BenchError("timed out sending to the daemon; connection dropped")

    def _read_exact(self, count: int, deadline: Optional[float]) -> bytes:
        chunks = []
        while count:
            buffer = ctypes.create_string_buffer(count)
            read = self._transfer(kernel32.ReadFile, buffer, count, deadline)
            if not read:
                raise BenchError("daemon closed the connection")
            chunks.append(buffer.raw[:read])
            self._consumed += read
            count -= read
        return b"".join(chunks)

    def receive(self, deadline: Optional[float] = None) -> dict:
        """Reads one message; raises BenchTimeout when no byte of one arrived by `deadline`.
        A timeout inside a message loses its start, so the connection is dropped."""
        before = self._consumed
        try:
            (length,) = struct.unpack("<I", self._read_exact(4, deadline))
            return json.loads(self._read_exact(length, deadline))
        except BenchTimeout:
            if self._consumed == before:
                raise
            self.close()
            raise BenchError("timed out inside a message; connection dropped")

    def request(self, message: dict, answer_type: str, timeout: float = 30.0) -> dict:
        deadline = time.monotonic() + timeout
        self.send(message)
        while True:
            reply = self.receive(deadline)
            if reply.get("type") == answer_type:
                return reply
            if reply.get("type") == "error":
                raise BenchError(f"{message['type']} refused: {reply}")

    def close(self) -> None:
        for name in ("handle", "event"):
            handle = getattr(self, name, None)
            if handle:
                kernel32.CloseHandle(handle)
                setattr(self, name, None)


class ScreenView:
    """An attached view that keeps the window's rows, acknowledging every frame."""

    def __init__(self, endpoint: str, window_id: str):
        self.conn = Connection(endpoint)
        self.rows: Dict[int, tuple] = {}
        self.conn.request(
            {"type": "attach_view", "window_id": window_id, "size": VIEW_SIZE}, "attached"
        )

    def _apply(self, frame: dict) -> None:
        if frame["reset"]:
            self.rows.clear()
        for line in frame["lines"]:
            text = "".join(cell for run in line["runs"] for cell in run["cells"])
            self.rows[line["row"]] = (text, line.get("wrapped", False))
        if frame["last"]:
            self.conn.send({"type": "frame_ack", "seq": frame["seq"]})

    def text(self) -> str:
        parts = []
        for row in sorted(self.rows):
            text, wrapped = self.rows[row]
            parts.append(text if wrapped else text + "\n")
        return "".join(parts)

    def wait_for(self, needle: str, deadline: float) -> float:
        """Reads frames until `needle` is on screen; returns the time it was seen."""
        while time.monotonic() < deadline:
            try:
                message = self.conn.receive(deadline)
            except BenchTimeout:
                break
            received = time.perf_counter()
            if message.get("type") != "frame":
                continue
            self._apply(message)
            if needle in self.text():
                return received
        raise BenchError(f"timed out waiting for {needle!r} on screen")


# --- daemon lifecycle --------------------------------------------------------------


def daemon_env(instance: str) -> dict:
    env = {key: value for key, value in os.environ.items() if not key.upper().startswith("YAGURA_")}
    env["YAGURA_INSTANCE"] = instance
    return env


def start_daemon(bin_dir: Path, instance: str, workdir: Path) -> subprocess.Popen:
    playbook = workdir / "playbook.toml"
    playbook.write_text('[approval]\nauto_approve = ["open_window", "close_window"]\n')
    log = open(workdir / f"{instance}.log", "ab")
    return subprocess.Popen(
        [str(bin_dir / "yagura-daemon.exe"), "--playbook", str(playbook)],
        env=daemon_env(instance),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
    )


def stop_daemon(bin_dir: Path, instance: str, daemon: subprocess.Popen) -> None:
    """Stops only the daemon this run started: `--shutdown` to its instance, then
    a kill of the PID this script holds if it does not exit."""
    subprocess.run(
        [str(bin_dir / "yagura-daemon.exe"), "--shutdown"],
        env=daemon_env(instance),
        capture_output=True,
        timeout=30,
    )
    try:
        daemon.wait(timeout=30)
    except subprocess.TimeoutExpired:
        daemon.kill()
        daemon.wait(timeout=10)


def wait_daemon_ready(endpoint: str, daemon: subprocess.Popen, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if daemon.poll() is not None:
            raise BenchError(f"daemon exited during start-up with code {daemon.returncode}")
        try:
            conn = Connection(endpoint, timeout=0.5)
        except BenchError:
            continue
        try:
            conn.request({"type": "ping"}, "pong")
            return
        finally:
            conn.close()
    raise BenchError("daemon did not answer ping")


# --- one repetition ----------------------------------------------------------------


def open_window(conn: Connection, command: List[str], name: str) -> str:
    reply = conn.request(
        {
            "type": "open_window",
            "command": command,
            "cwd": None,
            "name": name,
            "vendor": None,
            "detection": None,
        },
        "window_opened",
    )
    return reply["window_id"]


def latency_sample(view: ScreenView, serial: int, deadline: float) -> float:
    token = f"[{serial:06d}]"
    needle = token.encode("ascii").hex()
    started = time.perf_counter()
    view.conn.send({"type": "text", "text": token})
    return (view.wait_for(needle, deadline) - started) * 1000.0


class Build:
    """One build under test: its binaries, its own instance and its process set."""

    def __init__(self, label: str, bin_dir: Path, instance: str, procs: List[str]):
        self.label = label
        self.bin_dir = bin_dir
        # Builds get separate instances: a newer build may migrate the store to
        # a version an older one refuses to open.
        self.instance = instance
        self.procs = ProcessSet(procs, bin_dir)


def stage_build(source: Path, staged: Path) -> Path:
    """Copies the top-level files of `source` into `staged` and returns it.

    Identical binaries run from a cargo target directory measured 2-3% more CPU
    than from a plain copy (README.md section 4), so every build runs from a
    sibling directory of the same kind."""
    staged.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        if entry.is_file():
            shutil.copy2(entry, staged / entry.name)
    return staged


def one_rep(args, build: Build, rep: int, workdir: Path) -> dict:
    procs = build.procs
    endpoint = endpoint_for(build.instance)
    fixture = str(build.bin_dir / FIXTURE)
    flood_count = max(1, args.flood_mib * 1024 * 1024 // len(FLOOD_LINE))
    flood_windows = args.windows - 1
    serial = random.randrange(100000)
    daemon = start_daemon(build.bin_dir, build.instance, workdir)
    try:
        wait_daemon_ready(endpoint, daemon)
        time.sleep(args.settle)
        base = procs.sample()

        control = Connection(endpoint)
        probe_id = open_window(control, [fixture], "probe")
        probe = ScreenView(endpoint, probe_id)
        probe.wait_for(READY_TEXT, time.monotonic() + 30)

        done_at: Dict[int, float] = {}
        failures: List[str] = []

        def follow(index: int, window_id: str) -> None:
            try:
                view = ScreenView(endpoint, window_id)
                done_at[index] = view.wait_for(FLOODED_TEXT, time.monotonic() + args.timeout)
                view.conn.close()
            except Exception as error:  # reported by the main thread
                failures.append(f"flood window {index}: {error}")

        cpu_before = procs.total(procs.sample(), "cpu_s")
        started = time.perf_counter()
        followers = []
        for index in range(flood_windows):
            window_id = open_window(
                control, [fixture, "--flood", FLOOD_LINE.hex(), str(flood_count)], f"flood-{index}"
            )
            thread = threading.Thread(target=follow, args=(index, window_id), daemon=True)
            thread.start()
            followers.append(thread)
        control.close()

        loaded = []
        deadline = time.monotonic() + args.timeout
        while any(thread.is_alive() for thread in followers):
            loaded.append(latency_sample(probe, serial, deadline))
            serial += 1
            time.sleep(args.sample_gap)
        for thread in followers:
            thread.join()
        if failures:
            raise BenchError("; ".join(failures))
        finished = max(done_at.values()) if done_at else time.perf_counter()
        cpu_after = procs.total(procs.sample(), "cpu_s")

        time.sleep(args.settle)
        idle_start = procs.sample()
        idle_t0 = time.perf_counter()
        time.sleep(args.idle_seconds)
        idle_end = procs.sample()
        idle_elapsed = time.perf_counter() - idle_t0

        idle = []
        for _ in range(args.idle_samples):
            idle.append(latency_sample(probe, serial, time.monotonic() + 30))
            serial += 1
            time.sleep(args.sample_gap)
        probe.conn.close()

        mib = 1024 * 1024
        mem_base = procs.total(base, "working_set") / mib
        mem_loaded = procs.total(idle_end, "working_set") / mib
        flood_bytes = flood_windows * flood_count * len(FLOOD_LINE)
        return {
            "rep": rep,
            "throughput_mib_s": flood_bytes / mib / (finished - started),
            "latency_loaded_ms": statistics.median(loaded) if loaded else None,
            "latency_idle_ms": statistics.median(idle),
            "cpu_flood_s": cpu_after - cpu_before,
            "cpu_idle_pct": (procs.total(idle_end, "cpu_s") - procs.total(idle_start, "cpu_s"))
            / idle_elapsed
            * 100.0,
            "mem_base_mib": mem_base,
            "mem_loaded_mib": mem_loaded,
            "mem_per_window_mib": (mem_loaded - mem_base) / args.windows,
            "detail": {
                "flood_seconds": finished - started,
                "flood_bytes": flood_bytes,
                "loaded_samples_ms": loaded,
                "idle_samples_ms": idle,
                "private_base_mib": procs.total(base, "private") / mib,
                "private_loaded_mib": procs.total(idle_end, "private") / mib,
                "by_image_loaded": idle_end,
            },
        }
    finally:
        stop_daemon(build.bin_dir, build.instance, daemon)




# --- summary and comparison --------------------------------------------------------


def _mad(values: List[float]) -> float:
    median = statistics.median(values)
    return statistics.median(abs(value - median) for value in values)


def summarize(values: List[float]) -> dict:
    median = statistics.median(values)
    mad = _mad(values)
    return {
        "median": median,
        "min": min(values),
        "max": max(values),
        "mad": mad,
        # Robust coefficient of variation: 1.4826 * MAD estimates sigma for normal data.
        "rcv": (1.4826 * mad / abs(median)) if median else 0.0,
        "n": len(values),
    }


def _summaries(reps: List[dict]) -> dict:
    summary = {}
    for name in METRICS:
        values = [rep[name] for rep in reps if rep[name] is not None]
        if values:
            summary[name] = summarize(values)
    return summary


def _git_head() -> Optional[str]:
    try:
        return (
            subprocess.run(
                ["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"], capture_output=True, text=True
            ).stdout.strip()
            or None
        )
    except OSError:
        return None


def command_run(args) -> int:
    if os.name != "nt":
        print("bench.py run: Windows only (see README.md)", file=sys.stderr)
        return 1
    labels = args.label or []
    if len(labels) not in (0, len(args.bin_dir)):
        print("bench.py run: give one --label per --bin-dir, or none", file=sys.stderr)
        return 1
    if args.windows < 2:
        print("bench.py run: --windows must be at least 2 (one probe, one flood)", file=sys.stderr)
        return 1
    if not args.instance.startswith("bench-") or len(args.instance) + 1 + len(
        str(len(args.bin_dir) - 1)
    ) > 16:
        print(
            "bench.py run: --instance must start with 'bench-' and leave room for '-<index>' "
            "within the 16-byte instance identifier",
            file=sys.stderr,
        )
        return 1
    out = Path(args.out).resolve()
    workdir = out.parent / (out.stem + "-work")
    workdir.mkdir(parents=True, exist_ok=True)
    builds = []
    sources = []
    for index, raw in enumerate(args.bin_dir):
        source = Path(raw).resolve()
        if not (source / "yagura-daemon.exe").is_file() or not (source / FIXTURE).is_file():
            print(
                f"bench.py run: yagura-daemon.exe or {FIXTURE} missing in {source}",
                file=sys.stderr,
            )
            return 1
        label = labels[index] if labels else source.name
        staged = stage_build(source, workdir / f"bin-{index}")
        builds.append(Build(label, staged, f"{args.instance}-{index}", args.procs.split(",")))
        sources.append(source)

    # The first start of an executable measured a working set about twice the
    # later ones (README.md section 4), so each build runs once unrecorded.
    for _ in range(args.warmup):
        for build in builds:
            one_rep(args, build, -1, workdir)
            time.sleep(args.settle)

    results: List[List[dict]] = [[] for _ in builds]
    for rep in range(args.reps):
        # A B, then B A: alternating the order cancels a drift that is linear in time.
        order = list(range(len(builds)))
        if rep % 2:
            order.reverse()
        for index in order:
            result = one_rep(args, builds[index], rep, workdir)
            results[index].append(result)
            print(
                f"rep {rep} [{builds[index].label}]: "
                + ", ".join(
                    f"{name}={result[name]:.3f}" for name in METRICS if result[name] is not None
                ),
                flush=True,
            )
            time.sleep(args.settle)

    document = {
        "format": RESULT_FORMAT,
        "created": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "machine": {"platform": platform.platform(), "cpus": os.cpu_count()},
        "bench_git_head": _git_head(),
        "settings": {
            "reps": args.reps,
            "warmup": args.warmup,
            "windows": args.windows,
            "flood_mib": args.flood_mib,
            "procs": sorted(builds[0].procs.names),
            "idle_seconds": args.idle_seconds,
            "idle_samples": args.idle_samples,
            "sample_gap": args.sample_gap,
            "settle": args.settle,
        },
        "builds": [
            {
                "label": build.label,
                "source_dir": str(source),
                "bin_dir": str(build.bin_dir),
                "instance": build.instance,
                "summary": _summaries(reps),
                "reps": reps,
            }
            for build, source, reps in zip(builds, sources, results)
        ],
    }
    out.write_text(json.dumps(document, indent=2))
    print(f"wrote {out}")
    return 0


def judge_paired(base: dict, new: dict, name: str, z: float) -> Optional[dict]:
    """Judges `new` against `base` on the per-repetition differences of one run.

    Repetition i of both builds ran next to each other, so their difference
    carries no drift between sessions."""
    unit, higher_is_better, floor = METRICS[name]
    pairs = [
        (a[name], b[name])
        for a, b in zip(base["reps"], new["reps"])
        if a[name] is not None and b[name] is not None
    ]
    if not pairs:
        return None
    worse_steps = [(a - b) if higher_is_better else (b - a) for a, b in pairs]
    worse_by = statistics.median(worse_steps)
    sigma = 1.4826 * _mad(worse_steps)
    # Standard error of a median is about 1.2533 * sigma / sqrt(n).
    tolerance = max(floor, z * 1.2533 * sigma / len(pairs) ** 0.5)
    base_median = statistics.median(a for a, _ in pairs)
    new_median = statistics.median(b for _, b in pairs)
    return {
        "metric": name,
        "unit": unit,
        "base": base_median,
        "new": new_median,
        "ratio": (new_median / base_median) if base_median else None,
        "worse_by": worse_by,
        "tolerance": tolerance,
        "pairs": len(pairs),
        "worse": worse_by > tolerance,
    }


def _build(document: dict, label: Optional[str], fallback: int) -> dict:
    builds = document["builds"]
    if label is None:
        return builds[fallback]
    for build in builds:
        if build["label"] == label:
            return build
    raise BenchError(f"no build labelled {label!r}")


def command_compare(args) -> int:
    if len(args.results) == 2:
        # Two separate runs: drift between sessions is not cancelled, so the
        # medians are shown but nothing is judged (README.md section 4).
        base, new = (json.loads(Path(p).read_text())["builds"][0] for p in args.results)
        print("separate runs: not judged; measure both builds in one run to judge")
        for name in METRICS:
            if name in base["summary"] and name in new["summary"]:
                a, b = base["summary"][name]["median"], new["summary"][name]["median"]
                ratio = f"{b / a:.3f}" if a else "-"
                print(f"{name:<20} {a:>10.3f} {b:>10.3f} {ratio:>7}")
        return 3
    document = json.loads(Path(args.results[0]).read_text())
    if len(document["builds"]) < 2:
        print("compare: the result holds one build; run with two --bin-dir", file=sys.stderr)
        return 2
    base = _build(document, args.base, 0)
    new = _build(document, args.new, 1)
    verdicts = [v for v in (judge_paired(base, new, name, args.z) for name in METRICS) if v]
    print(f"base = {base['label']}, new = {new['label']}, z = {args.z}")
    print(
        f"{'metric':<20} {'base':>10} {'new':>10} {'ratio':>7} {'worse_by':>10} {'tol':>9}  verdict"
    )
    for v in verdicts:
        ratio = f"{v['ratio']:.3f}" if v["ratio"] is not None else "-"
        print(
            f"{v['metric']:<20} {v['base']:>10.3f} {v['new']:>10.3f} {ratio:>7} "
            f"{v['worse_by']:>10.3f} {v['tolerance']:>9.3f}  {'WORSE' if v['worse'] else 'ok'}"
        )
    if args.json:
        Path(args.json).write_text(json.dumps(verdicts, indent=2))
    return 1 if any(v["worse"] for v in verdicts) else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="measure one or more builds, interleaved")
    run.add_argument(
        "--bin-dir",
        action="append",
        required=True,
        help="directory with yagura-daemon.exe and hex_echo.exe; repeat for each build",
    )
    run.add_argument("--label", action="append", help="one label per --bin-dir")
    run.add_argument("--out", required=True, help="result JSON path")
    run.add_argument(
        "--instance", default="bench-t1051", help="instance prefix; build i uses <prefix>-<i>"
    )
    run.add_argument("--reps", type=int, default=10)
    run.add_argument("--warmup", type=int, default=1, help="unrecorded repetitions per build")
    run.add_argument("--windows", type=int, default=8)
    run.add_argument("--flood-mib", type=int, default=8, help="output each flood window writes")
    run.add_argument("--procs", default=",".join(DEFAULT_PROCS))
    run.add_argument("--idle-seconds", type=float, default=15.0)
    run.add_argument("--idle-samples", type=int, default=30)
    run.add_argument("--sample-gap", type=float, default=0.05)
    run.add_argument("--settle", type=float, default=2.0)
    run.add_argument("--timeout", type=float, default=180.0)
    run.set_defaults(func=command_run)

    compare = sub.add_parser("compare", help="judge whether the new build is worse than the base")
    compare.add_argument(
        "results", nargs="+", help="one result with two builds (judged), or two results (not judged)"
    )
    compare.add_argument("--base", help="label of the base build (default: the first)")
    compare.add_argument("--new", help="label of the new build (default: the second)")
    compare.add_argument("--z", type=float, default=3.0)
    compare.add_argument("--json", help="also write the verdicts to this path")
    compare.set_defaults(func=command_compare)

    args = parser.parse_args(argv)
    if args.command == "compare" and len(args.results) > 2:
        parser.error("compare takes one or two result files")
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
