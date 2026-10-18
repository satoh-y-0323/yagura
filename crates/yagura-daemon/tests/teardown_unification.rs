#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Window teardown is unified: a child that exits on its own tears down its whole
//! process group, the same way `close_window` does.
//!
//! The session's exit monitor ends the child's process tree as soon as the direct
//! child exits, so grandchildren are torn down immediately instead of surviving until
//! the window is evicted.

mod common;

use std::sync::Arc;
use std::time::Duration;

use yagura_daemon::{DaemonConfig, OpenWindowRequest, WindowRegistry};

/// Opens a window on a fresh in-process registry. No `IpcServer` / named pipe is
/// involved: every contract here is observable through the public registry API alone.
async fn open_window(command: Vec<String>) -> (Arc<WindowRegistry>, String) {
    let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
    let window_id = registry
        .open(OpenWindowRequest {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open should spawn the child")
        .window_id;
    (registry, window_id)
}

/// Waits for the direct child to report its own exit -- the same signal the
/// session's exit monitor publishes.
async fn wait_for_direct_child_exit(session: &yagura_daemon::PtySession, timeout: Duration) {
    let mut rx = session.exit_notified();
    if *rx.borrow() {
        return;
    }
    tokio::time::timeout(timeout, rx.changed())
        .await
        .expect("timed out waiting for the direct child to report its own exit")
        .expect("exit_notified watch channel must not close while the session is alive");
}

#[tokio::test]
async fn child_exit_terminates_the_whole_process_tree() {
    let dir = common::unique_temp_dir("teardown-c1");
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");

    let (registry, window_id) = open_window(common::grandchild_command(tick_path)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    common::wait_until_file_grows(&tick, Duration::from_secs(20)).await;

    wait_for_direct_child_exit(entry.session(), Duration::from_secs(10)).await;

    // The measured property: the grandchild must not survive the direct child's exit.
    common::assert_file_len_stable(&tick, Duration::from_secs(3)).await;
}

#[tokio::test]
async fn terminate_tree_stays_idempotent_after_the_child_exits_on_its_own() {
    let dir = common::unique_temp_dir("teardown-c2");
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");

    let (registry, window_id) = open_window(common::grandchild_command(tick_path)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    wait_for_direct_child_exit(entry.session(), Duration::from_secs(10)).await;

    let first = entry.session().terminate_tree().is_ok();
    let second = entry.session().terminate_tree().is_ok();
    assert_eq!(
        first, second,
        "terminate_tree()'s return value must be stable across repeated calls"
    );
}

#[tokio::test]
async fn terminate_tree_called_twice_is_idempotent() {
    let dir = common::unique_temp_dir("teardown-c3");
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");

    let (registry, window_id) = open_window(common::grandchild_command(tick_path)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    common::wait_until_file_grows(&tick, Duration::from_secs(20)).await;

    let first = entry.session().terminate_tree().is_ok();
    let second = entry.session().terminate_tree().is_ok();
    assert!(
        first,
        "the first terminate_tree() call must report an effect"
    );
    assert_eq!(
        first, second,
        "calling terminate_tree() a second time must not change the outcome"
    );

    common::assert_file_len_stable(&tick, Duration::from_secs(3)).await;
}

#[tokio::test]
async fn surviving_grandchild_fixture_keeps_both_the_child_and_the_grandchild_alive() {
    let dir = common::unique_temp_dir("teardown-c4");
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");

    let (registry, window_id) = open_window(common::surviving_grandchild_command(tick_path)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    common::wait_until_file_grows(&tick, Duration::from_secs(20)).await;

    // The direct child must also still be alive, observed past the point where the
    // path-A fixture's direct child would already have exited.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        entry.session().exit_status().is_none(),
        "the surviving-fixture's direct child must still be running"
    );
}

// ---------------------------------------------------------------------
// A grandchild the child starts as its very first action belongs to the
// window's teardown unit, however quickly it is started: it is gone once the
// child has exited on its own. Each case judges by the grandchild's own
// process id ending, never by a time window; the bounds below only keep a
// broken teardown from hanging the case.
// ---------------------------------------------------------------------

/// Upper bound on waiting for a process id to end or to be published.
const PID_BOUND: Duration = Duration::from_secs(15);

/// Waits until `pid_path` holds a process id and returns it.
fn wait_for_published_pid(pid_path: &std::path::Path) -> u32 {
    let deadline = std::time::Instant::now() + PID_BOUND;
    loop {
        if let Some(pid) = std::fs::read_to_string(pid_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild never published its process id at {}",
            pid_path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether process `pid` has ended, waiting up to `bound` for it to.
#[cfg(windows)]
fn process_ends_within(pid: u32, bound: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: the handle is opened for waiting only, used on this thread, and
    // closed before returning.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => true,
            Err(error) => panic!("OpenProcess({pid}) failed unexpectedly: {error}"),
            Ok(handle) => {
                let millis = u32::try_from(bound.as_millis()).unwrap_or(u32::MAX);
                let signaled = WaitForSingleObject(handle, millis) == WAIT_OBJECT_0;
                let _ = CloseHandle(handle);
                signaled
            }
        }
    }
}

/// Whether process `pid` has ended, waiting up to `bound` for it to.
#[cfg(unix)]
// `kill -0` only probes a pid; it starts no child a window's group could adopt.
#[allow(clippy::disallowed_methods)]
fn process_ends_within(pid: u32, bound: Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill should be runnable")
            .success();
        if !alive {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn grandchild_ends_within(pid: u32, bound: Duration) -> bool {
    tokio::task::spawn_blocking(move || process_ends_within(pid, bound))
        .await
        .expect("the process wait must not panic")
}

#[tokio::test]
async fn a_pty_session_ends_a_first_action_grandchild_once_its_child_has_exited() {
    let dir = common::unique_temp_dir("teardown-p6");
    let pid_file = dir.join("grandchild.pid");
    let argv = yagura_test_support::process::first_grandchild_command(&pid_file, 0);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

    let session =
        yagura_daemon::PtySession::spawn_with_command(&argv_refs, &DaemonConfig::default())
            .await
            .expect("the session should spawn");
    wait_for_direct_child_exit(&session, PID_BOUND).await;

    let grandchild = wait_for_published_pid(&pid_file);
    assert!(
        grandchild_ends_within(grandchild, PID_BOUND).await,
        "the grandchild (pid {grandchild}) outlived its child: the session holds no teardown unit \
         that reaches it"
    );
}

#[tokio::test]
async fn a_window_ends_a_first_action_grandchild_once_its_child_has_exited() {
    let dir = common::unique_temp_dir("teardown-r1");
    let pid_file = dir.join("grandchild.pid");

    let (registry, window_id) = open_window(
        yagura_test_support::process::first_grandchild_command(&pid_file, 0),
    )
    .await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");
    wait_for_direct_child_exit(entry.session(), PID_BOUND).await;

    let grandchild = wait_for_published_pid(&pid_file);
    assert!(
        grandchild_ends_within(grandchild, PID_BOUND).await,
        "the grandchild (pid {grandchild}) the window's child started first thing outlived the \
         child"
    );
}

#[tokio::test]
async fn the_first_grandchild_fixture_keeps_the_grandchild_alive_while_the_child_lives() {
    let dir = common::unique_temp_dir("teardown-r1-control");
    let pid_file = dir.join("grandchild.pid");

    let (registry, window_id) = open_window(
        yagura_test_support::process::first_grandchild_command(&pid_file, 600_000),
    )
    .await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    let grandchild = wait_for_published_pid(&pid_file);
    let ended_while_child_lives =
        grandchild_ends_within(grandchild, Duration::from_millis(500)).await;
    let child_still_running = entry.session().exit_status().is_none();
    registry.terminate_all().await;

    assert!(
        !ended_while_child_lives,
        "the fixture's grandchild (pid {grandchild}) must be running while its child is"
    );
    assert!(
        child_still_running,
        "the fixture's child must still be running"
    );
    assert!(
        grandchild_ends_within(grandchild, PID_BOUND).await,
        "closing the window must still end the grandchild (pid {grandchild})"
    );
}
