#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts this test binary again as the child the tests observe.
#![allow(clippy::disallowed_methods)]

//! A process is named by its id and its start: a process that exited is dead
//! before anyone reaps it, and an id with another start is not that process,
//! so an id taken over by another process is never mistaken for it.

use std::process::{Child, Command, Stdio};

const IDLE_CHILD: &str = "idle_child";
const IDLE_CHILD_ENV: &str = "YAGURA_OS_TEST_IDLE_CHILD";

/// The child of the tests below: lives until its stdin ends.
#[test]
#[ignore = "the child of the process identity tests"]
fn idle_child() {
    if std::env::var_os(IDLE_CHILD_ENV).is_none() {
        return;
    }
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
}

fn spawn_idle_child() -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            yagura_test_support::process::libtest_name(module_path!(), IDLE_CHILD).as_str(),
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(IDLE_CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the idle child")
}

/// Ends the child if a test fails before it does.
struct Reaped(Option<Child>);

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Blocks until `child` has exited, without reaping it.
#[cfg(unix)]
fn wait_for_exit_unreaped(child: &Child) {
    let pid = libc::id_t::try_from(child.id()).unwrap();
    loop {
        // SAFETY: `info` is a zeroed siginfo_t that waitid fills; `pid` is
        // this test's unreaped child, and WNOWAIT leaves it unreaped.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result =
            unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if result == 0 {
            return;
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EINTR),
            "waitid(WEXITED | WNOWAIT) failed: {error}"
        );
    }
}

/// Blocks until `child` has exited. Waiting on its handle reaps nothing: the
/// process object lives until `child` closes the handle.
#[cfg(windows)]
fn wait_for_exit_unreaped(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};

    // SAFETY: the handle is `child`'s own, open for as long as `child` is.
    let waited = unsafe { WaitForSingleObject(HANDLE(child.as_raw_handle() as isize), INFINITE) };
    assert_eq!(waited, WAIT_OBJECT_0, "the wait on the child failed");
}

/// Alive while it runs, under its own start only; dead once it has exited,
/// before its reap as after it.
#[test]
fn a_child_is_alive_only_under_its_own_start_and_dead_once_exited_before_its_reap() {
    use yagura_os::{Liveness, ProcessHandle, ProcessStart, process_liveness, process_start};

    let mut child = Reaped(Some(spawn_idle_child()));
    let pid = child.0.as_ref().unwrap().id();
    let start = process_start(pid)
        .unwrap()
        .expect("the running child has a start");
    assert_eq!(process_liveness(pid, &start), Liveness::Alive);
    let handle = ProcessHandle::open(pid, &start)
        .unwrap()
        .expect("the running child opens by its id and start");
    assert_eq!(handle.liveness(), Liveness::Alive);

    // Another launch's start, different from the child's by construction:
    // two processes can read the same start within one clock tick.
    let other_start: ProcessStart = format!("{start}~")
        .parse()
        .expect("a start reads back from its string");
    assert_ne!(
        start.to_string(),
        other_start.to_string(),
        "two running processes read the same start"
    );
    assert_eq!(
        process_liveness(pid, &other_start),
        Liveness::Dead,
        "a running id was taken for the process of another start"
    );
    assert!(
        ProcessHandle::open(pid, &other_start).unwrap().is_none(),
        "a running id opened as the process of another start"
    );

    child.0.as_mut().unwrap().kill().unwrap();
    wait_for_exit_unreaped(child.0.as_ref().unwrap());
    assert_eq!(
        process_liveness(pid, &start),
        Liveness::Dead,
        "an exited child not yet reaped is not dead"
    );
    assert_eq!(
        handle.liveness(),
        Liveness::Dead,
        "the handle opened while the child ran does not see it dead before its reap"
    );
    assert!(
        ProcessHandle::open(pid, &start)
            .unwrap()
            .is_none_or(|handle: ProcessHandle| handle.liveness() == Liveness::Dead),
        "an exited child not yet reaped opens as a live process"
    );

    child.0.take().unwrap().wait().unwrap();
    assert_eq!(process_liveness(pid, &start), Liveness::Dead);
    assert!(
        ProcessHandle::open(pid, &start)
            .unwrap()
            .is_none_or(|handle: ProcessHandle| handle.liveness() == Liveness::Dead),
        "a reaped child's id opens as a live process"
    );
}
