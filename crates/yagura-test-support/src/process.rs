//! Child-process command fixtures, the running test binary's own path,
//! the wait on an output file's growth, and the wait on a process's end.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The size of `path` in bytes, or `0` when it cannot be read.
///
/// Absence reads as zero on purpose: the growth helpers below start polling
/// before the child has created its output file.
#[must_use]
pub fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Stage 1 of the grandchild observation: the descendant is demonstrably
/// alive, because the file it appends to keeps growing.
///
/// # Panics
///
/// Panics if the file has not grown within [`crate::HANG_GUARD`], which means
/// the fixture never established its precondition.
pub async fn wait_until_file_grows(path: &Path) {
    let deadline = tokio::time::Instant::now() + crate::HANG_GUARD;
    let start = file_len(path);
    loop {
        if file_len(path) > start {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "grandchild never appended to {} (fixture precondition failed)",
                path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether process `pid` is still running.
///
/// On Windows a process whose id still opens (a handle to it is held
/// elsewhere) but which has exited is not running. On Unix a process that has
/// exited but is not yet reaped by its parent still counts as running.
///
/// # Panics
///
/// Panics if `pid` is `0` or out of the platform's range, or if the probe
/// fails for a reason other than the process not existing: a probe that
/// cannot answer must not read as "gone".
#[must_use]
pub fn process_is_running(pid: u32) -> bool {
    assert!(pid != 0, "process id 0 names no single process");
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // The process object is signaled only once the process has fully
        // ended; its exit code is set earlier, while termination is under way.
        // SAFETY: the handle is opened for waiting only, waited on once with no
        // timeout on this thread and closed before returning.
        unsafe {
            match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => false,
                Err(error) => panic!("OpenProcess({pid}) failed unexpectedly: {error}"),
                Ok(handle) => {
                    let state = WaitForSingleObject(handle, 0);
                    let _ = CloseHandle(handle);
                    match state {
                        WAIT_TIMEOUT => true,
                        WAIT_OBJECT_0 => false,
                        other => {
                            panic!("WaitForSingleObject({pid}) failed unexpectedly: {other:?}")
                        }
                    }
                }
            }
        }
    }
    #[cfg(unix)]
    {
        let id = libc::pid_t::try_from(pid)
            .unwrap_or_else(|_| panic!("process id {pid} is out of range"));
        // SAFETY: signal 0 delivers nothing; it only checks that `id` names a
        // process this one may signal.
        if unsafe { libc::kill(id, 0) } == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => false,
            Some(libc::EPERM) => true,
            _ => panic!("kill({pid}, 0) failed unexpectedly: {error}"),
        }
    }
}

/// Returns once none of `pids` is a running process (see
/// [`process_is_running`]), checking again every 20 ms.
///
/// The wait runs on a blocking thread against the wall clock, so a test on
/// tokio's paused clock does not reach the guard at once.
///
/// # Panics
///
/// Panics, with `explain` applied to the ids still running, if one still runs
/// after [`crate::HANG_GUARD`]; panics as [`process_is_running`] does.
pub async fn wait_processes_gone(
    pids: &[u32],
    explain: impl Fn(&[u32]) -> String + Send + 'static,
) {
    let pids = pids.to_vec();
    match tokio::task::spawn_blocking(move || wait_processes_gone_blocking(&pids, explain)).await {
        Ok(()) => {}
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("the wait on the processes was cancelled: {error}"),
    }
}

/// [`wait_processes_gone`] for a caller outside an async runtime.
///
/// # Panics
///
/// Panics as [`wait_processes_gone`] does.
pub fn wait_processes_gone_blocking(pids: &[u32], explain: impl Fn(&[u32]) -> String) {
    let deadline = std::time::Instant::now() + crate::HANG_GUARD;
    loop {
        let running: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|&pid| process_is_running(pid))
            .collect();
        if running.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "processes still running after {:?}: {}",
            crate::HANG_GUARD,
            explain(&running)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A fixed command (not a real vendor CLI) whose direct child stays alive for
/// roughly `ping_count - 1` seconds and then exits on its own. ConPTY spawns
/// its `conhost.exe --headless` host for any command regardless of what it is,
/// so this is enough to reproduce a host-process leak without depending on an
/// installed agent CLI.
///
/// The delay uses `ping -n` rather than `timeout /t`, which fails outright
/// when stdin is redirected (as it is under a PTY).
#[cfg(windows)]
#[must_use]
pub fn sleepy_command(ping_count: &str) -> Vec<String> {
    vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "ping".to_string(),
        "-n".to_string(),
        ping_count.to_string(),
        "127.0.0.1".to_string(),
        ">nul".to_string(),
    ]
}

/// Unix counterpart of [`sleepy_command`]: a direct child that sleeps and
/// then exits on its own, spawning no grandchild. `ping_count` keeps the same
/// unit as the Windows fixture (a count of `ping -n`-style probes); it is
/// mapped to `max(ping_count - 1, 1)` seconds here so callers pass the same
/// numbers on both platforms.
#[cfg(not(windows))]
#[must_use]
pub fn sleepy_command(ping_count: &str) -> Vec<String> {
    let count: u64 = ping_count
        .parse()
        .expect("ping_count must be a valid non-negative integer");
    let secs = count.saturating_sub(1).max(1);
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("sleep {secs}"),
    ]
}

/// A fixed command whose direct child exits immediately with `code` (a
/// decimal string): the shared fixture behind every window a test needs to
/// already be `Exited` by the time it looks.
#[cfg(windows)]
#[must_use]
pub fn exit_with_code_command(code: &str) -> Vec<String> {
    vec![
        "cmd.exe".to_string(),
        "/c".to_string(),
        "exit".to_string(),
        code.to_string(),
    ]
}

/// Unix counterpart of [`exit_with_code_command`].
#[cfg(not(windows))]
#[must_use]
pub fn exit_with_code_command(code: &str) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("exit {code}"),
    ]
}

/// The name libtest runs the test `name` under, where `module_path` is the
/// `module_path!()` of the module declaring it: that path without the test
/// crate's own name in front, then `name`. A test that starts its own binary
/// again to run one of its tests passes this with `--exact`.
#[must_use]
pub fn libtest_name(module_path: &str, name: &str) -> String {
    match module_path.split_once("::") {
        Some((_, path)) => format!("{path}::{name}"),
        None => name.to_owned(),
    }
}

/// Copies the executable `from` to `to` for a test to run, without this
/// process ever holding a descriptor open for writing on `to`. On Unix the
/// copy is made by a `cp` child: a descriptor open for writing here is
/// inherited by any child another test thread forks meanwhile, and keeps `to`
/// busy (`ETXTBSY`) for an exec until that child execs.
///
/// # Panics
///
/// Panics if the copy fails.
// The `cp` child is waited for before this returns, so it leaves nothing to
// tear down; it is the test process's child, not one of the daemon's.
#[allow(clippy::disallowed_methods)]
pub fn install_exe(from: &Path, to: &Path) {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("cp")
            .arg("--")
            .arg(from)
            .arg(to)
            .status()
            .unwrap_or_else(|error| panic!("run cp for {}: {error}", from.display()));
        assert!(
            status.success(),
            "cp {} {} failed: {status}",
            from.display(),
            to.display()
        );
    }
    #[cfg(not(unix))]
    std::fs::copy(from, to)
        .unwrap_or_else(|error| panic!("copy {} to {}: {error}", from.display(), to.display()));
}

/// The canonicalized path of the test binary that is currently running.
///
/// The result is canonicalized because `current_exe()` alone is not: on
/// Windows it lacks the extended-length prefix that `canonicalize` adds, so
/// comparing it against a canonicalized path would fail there.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if that
/// path cannot be canonicalized.
#[must_use]
pub fn own_canonical_exe() -> PathBuf {
    std::env::current_exe()
        .expect("current_exe should resolve for a running test binary")
        .canonicalize()
        .expect("canonicalize current_exe")
}

/// The TOML source of a profile with `display_name` whose `[command]`
/// default is `command`, each element written as a TOML literal string.
///
/// One construction behind every profile-source helper built on it, so a
/// window opened from a profile always runs the very argv its direct-command
/// counterpart builds.
pub fn profile_source(display_name: &str, command: &[String]) -> String {
    let elements: Vec<String> = command
        .iter()
        .map(|element| format!("'{element}'"))
        .collect();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n",
        elements.join(", ")
    )
}

// --- composer-draw mode ------------------------------------------------------
//
// A fixture draws a composer only when the four trailing arguments below are
// present, so its plain argv stays as it is everywhere else.

/// The marker a fixture in composer-draw mode paints at the start of its
/// composer line.
///
/// Published here because a detection definition's `[composer] marker` must
/// hold the same value, as a regex: a caller writes this constant into the
/// definition rather than restating the fixture's own string, so the two can
/// never drift. It therefore carries no regex metacharacter, and no quote,
/// backslash or newline that a TOML string would have to escape.
pub const COMPOSER_MARKER: &str = "YAGURA-COMPOSER";

/// What a fixture in composer-draw mode paints between the marker and the
/// body, and what a definition's `[composer] separator` must hold.
///
/// Absent from the painted line while the composer is empty, which is what
/// tells an empty composer apart from an unobservable one.
pub const COMPOSER_SEPARATOR: &str = ">>";

/// What a fixture in composer-draw mode paints at the start of every wrapped
/// line of a body, and what a definition's `[composer] continuation_prefix`
/// must hold.
pub const COMPOSER_CONTINUATION_PREFIX: &str = "::";

/// The four argv elements that put a fixture into composer-draw mode, to be
/// appended after the fixture's own arguments: [`COMPOSER_MARKER`],
/// [`COMPOSER_SEPARATOR`], [`COMPOSER_CONTINUATION_PREFIX`], then
/// `reflect_delay_ms` (`0` reflects a write as soon as it arrives).
///
/// A fixture draws a composer only when all four are present, in this order;
/// with any of them missing it silently falls back to plain echo.
#[must_use]
pub fn composer_arguments(reflect_delay_ms: u64) -> [String; 4] {
    [
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        COMPOSER_CONTINUATION_PREFIX.to_string(),
        reflect_delay_ms.to_string(),
    ]
}

/// Ends every window a record in `holders_dir` names, as a test's last step,
/// as [`end_recorded_holder`] does; returns the holder pids it ended. A holder
/// that may outlive its daemon is left running by the daemon's end, and
/// nothing else ends it. `instance` is the instance argument the holders were
/// started with (`None` for the default instance).
///
/// Waits at most [`crate::HANG_GUARD`] for each process to end and never
/// panics, so it may run while a failing test unwinds.
#[must_use = "the ended holders tell a test what its windows left running"]
pub fn end_recorded_holders(holders_dir: &Path, instance: Option<&std::ffi::OsStr>) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(holders_dir) else {
        return Vec::new();
    };
    let mut ended = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(window_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Ok(record) = yagura_protocol::holder::read_holder_record(holders_dir, window_id)
            && let Some(holder) = end_recorded_holder(holders_dir, instance, &record)
        {
            ended.push(holder);
        }
    }
    ended
}

/// Ends the window `record` names, each process only while the record's pid
/// and start still name a running process, and leaves none of them to clean
/// up; returns the holder's pid when it was still running. With a watcher, the
/// holder and the watcher end first ([`end_holder_and_watcher`]), then the
/// child's process group (Unix) and the child; without one, the child first,
/// then the holder. On Unix the killed holder's socket and lock are removed,
/// since neither the holder nor its watcher removed them.
pub fn end_recorded_holder(
    holders_dir: &Path,
    instance: Option<&std::ffi::OsStr>,
    record: &yagura_protocol::holder::HolderRecord,
) -> Option<u32> {
    let child = record
        .child_pid
        .and_then(|pid| recorded_process(pid, record.child_start.as_deref()));
    let watched = recorded_watcher(record).is_some();
    if !watched {
        end_child(child.as_ref());
    }
    let holder = end_holder_and_watcher(record);
    if watched {
        end_child(child.as_ref());
    }
    remove_killed_holder_endpoint(holders_dir, instance, &record.window_id);
    holder
}

/// Ends the holder `record` names and its watcher so that neither ends the
/// window's tree or removes the holder's files: the holder is stopped, the
/// watcher killed and seen to end, then the holder killed and seen to end.
/// Without a running watcher, the holder alone is killed. Returns the
/// holder's pid when it was still running.
///
/// Waits at most [`crate::HANG_GUARD`] for each process to end and never
/// panics.
pub fn end_holder_and_watcher(record: &yagura_protocol::holder::HolderRecord) -> Option<u32> {
    let holder = recorded_process(record.holder_pid, record.holder_start.as_deref());
    if let Some(watcher) = recorded_watcher(record) {
        #[cfg(unix)]
        if let Some((pid, _)) = &holder
            && let Ok(pid) = libc::pid_t::try_from(*pid)
        {
            // SAFETY: plain values; the pid was just opened by its start.
            unsafe { libc::kill(pid, libc::SIGSTOP) };
        }
        end_process(&watcher.1);
    }
    let (pid, handle) = holder?;
    end_process(&handle);
    Some(pid)
}

fn recorded_watcher(
    record: &yagura_protocol::holder::HolderRecord,
) -> Option<(u32, yagura_os::ProcessHandle)> {
    recorded_process(record.watcher_pid?, record.watcher_start.as_deref())
}

fn end_child(child: Option<&(u32, yagura_os::ProcessHandle)>) {
    let Some((pid, handle)) = child else {
        return;
    };
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(*pid) {
        // SAFETY: plain values; the child leads its own group, and a group
        // of another id is refused with ESRCH.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
    end_process(handle);
}

/// Removes the socket and lock (Unix) of `window_id`'s holder in `holders_dir`
/// once its killer has seen it end, which a killed holder cannot do itself:
/// under the endpoint lock, named as the holder names its endpoint, so a live
/// listener's are left. Nothing to remove on Windows.
pub fn remove_killed_holder_endpoint(
    holders_dir: &Path,
    instance: Option<&std::ffi::OsStr>,
    window_id: &str,
) {
    #[cfg(unix)]
    remove_killed_holder_socket(holders_dir, instance, window_id);
    #[cfg(not(unix))]
    let _ = (holders_dir, instance, window_id);
}

#[cfg(unix)]
fn remove_killed_holder_socket(
    holders_dir: &Path,
    instance: Option<&std::ffi::OsStr>,
    window_id: &str,
) {
    use yagura_os::PeerAuth as _;
    let Ok(namespace) = yagura_os::resolve_instance_from(instance) else {
        return;
    };
    let Ok(user_id) = yagura_os::PlatformIpc::default().current_user_id() else {
        return;
    };
    let Ok(endpoint) = yagura_os::holder_endpoint(&user_id, &namespace, holders_dir, window_id)
    else {
        return;
    };
    let _ = yagura_os::platform_unix::remove_abandoned_endpoint(Path::new(&endpoint));
}

fn recorded_process(pid: u32, start: Option<&str>) -> Option<(u32, yagura_os::ProcessHandle)> {
    let start: yagura_os::ProcessStart = start?.parse().ok()?;
    let handle = yagura_os::ProcessHandle::open(pid, &start).ok()??;
    (handle.liveness() == yagura_os::Liveness::Alive).then_some((pid, handle))
}

fn end_process(handle: &yagura_os::ProcessHandle) {
    // Watched before the kill: on macOS a process reads as dead while it still
    // holds its descriptors, the endpoint lock removed next among them.
    let exit = handle.watch_exit();
    let _ = handle.kill();
    if let Ok(exit) = exit {
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(exit.wait());
        });
        let _ = ended.recv_timeout(crate::HANG_GUARD);
        return;
    }
    let deadline = std::time::Instant::now() + crate::HANG_GUARD;
    while handle.liveness() == yagura_os::Liveness::Alive && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}
