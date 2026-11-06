#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Window teardown is unified: a child that exits on its own tears down its whole
//! process group, the same way `close_window` does.
//!
//! The session's exit monitor ends the child's process tree as soon as the direct
//! child exits, so grandchildren are torn down immediately instead of surviving until
//! the window is evicted.

use crate::common;

use std::path::Path;
use std::sync::Arc;

use yagura_daemon::{DaemonConfig, OpenWindowRequest, WindowRegistry};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::process::{
    first_grandchild_command, process_is_running, wait_processes_gone,
};

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
async fn wait_for_direct_child_exit(session: &yagura_daemon::PtySession) {
    let mut rx = session.exit_notified();
    if *rx.borrow() {
        return;
    }
    tokio::time::timeout(HANG_GUARD, rx.changed())
        .await
        .expect("timed out waiting for the direct child to report its own exit")
        .expect("exit_notified watch channel must not close while the session is alive");
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

    wait_for_direct_child_exit(entry.session()).await;

    // Neither call may panic or hang on a tree that is already gone.
    entry.session().terminate_tree();
    entry.session().terminate_tree();
}

#[tokio::test]
async fn terminate_tree_called_twice_is_idempotent() {
    let dir = common::unique_temp_dir("teardown-c3");
    let pid_file = dir.join("grandchild.pid");

    // The child lives on, so only `terminate_tree` can end the tree.
    let (registry, window_id) = open_window(first_grandchild_command(&pid_file, 600_000)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");
    let grandchild = running_published_pid(&pid_file);

    entry.session().terminate_tree();
    entry.session().terminate_tree();

    wait_processes_gone(&[grandchild], move |_| {
        format!("the grandchild (pid {grandchild}) outlived terminate_tree() called twice")
    })
    .await;
}

// ---------------------------------------------------------------------
// A grandchild the child starts as its very first action belongs to the
// window's teardown unit, however quickly it is started: it is gone once the
// child has exited on its own. Each case first sees the grandchild running,
// then lets the child exit and waits for the grandchild's own process id to
// end; the guard only keeps a broken teardown from hanging the case.
// ---------------------------------------------------------------------

/// Waits until `pid_path` holds a process id, and returns it once it is seen
/// naming a running process: a probe that never reports "running" cannot let
/// a later wait for the end pass.
fn running_published_pid(pid_path: &Path) -> u32 {
    let deadline = std::time::Instant::now() + HANG_GUARD;
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(pid_path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild never published its process id at {}",
            pid_path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        process_is_running(pid),
        "the grandchild (pid {pid}) must be running before the teardown under test"
    );
    pid
}

/// The `grandchild-first` argv whose child exits only once `release` exists.
fn released_first_grandchild_command(pid_file: &Path, release: &Path) -> Vec<String> {
    let mut argv = first_grandchild_command(pid_file, 0);
    argv.push(
        release
            .to_str()
            .expect("release path must be valid UTF-8")
            .to_string(),
    );
    argv
}

fn release_child(release: &Path) {
    std::fs::write(release, b"go").expect("the release file should be writable");
}

#[tokio::test]
async fn a_pty_session_ends_a_first_action_grandchild_once_its_child_has_exited() {
    let dir = common::unique_temp_dir("teardown-p6");
    let pid_file = dir.join("grandchild.pid");
    let release = dir.join("release");
    let argv = released_first_grandchild_command(&pid_file, &release);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

    let session = common::spawn_session(&argv_refs, &DaemonConfig::default())
        .await
        .expect("the session should spawn");
    let grandchild = running_published_pid(&pid_file);
    release_child(&release);
    wait_for_direct_child_exit(&session).await;

    wait_processes_gone(&[grandchild], move |_| {
        format!(
            "the grandchild (pid {grandchild}) outlived its child: the session holds no teardown \
             unit that reaches it"
        )
    })
    .await;
}

#[tokio::test]
async fn a_window_ends_a_first_action_grandchild_once_its_child_has_exited() {
    let dir = common::unique_temp_dir("teardown-r1");
    let pid_file = dir.join("grandchild.pid");
    let release = dir.join("release");

    let (registry, window_id) =
        open_window(released_first_grandchild_command(&pid_file, &release)).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");
    let grandchild = running_published_pid(&pid_file);
    release_child(&release);
    wait_for_direct_child_exit(entry.session()).await;

    wait_processes_gone(&[grandchild], move |_| {
        format!(
            "the grandchild (pid {grandchild}) the window's child started first thing outlived \
             the child"
        )
    })
    .await;
}
