#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(windows)]

//! A window's pseudoconsole host process is reaped on every end of the window:
//! `close_window`, the child exiting on its own, and `close_window` while the
//! child floods output. The host is a sibling direct child of the daemon, not a
//! descendant of the target command, so it needs its own teardown.
//!
//! A host is told apart by a before/after diff of this process's host
//! children. The tests in this file take one lock, and no other test shares
//! the binary, so no other window opens inside that diff.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use yagura_daemon::{DaemonConfig, OpenWindowRequest, WindowRegistry};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// Opens a long-lived window, waits for its host to appear, closes the window
/// and waits for that host to be gone.
async fn open_and_close_one_window() {
    let before = common::pty_host_child_pids();
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    let host_pids = common::wait_for_new_pty_host_pids(&before, Duration::from_secs(10)).await;

    yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5))
        .await
        .expect("close_window should not error");

    common::wait_processes_gone(&host_pids, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn close_window_reaps_the_pty_host_process() {
    let _serial = SERIAL.lock().await;
    open_and_close_one_window().await;
}

/// Each cycle's own host must be reaped by that cycle's `close_window` before
/// the next cycle starts, so hosts never accumulate.
#[tokio::test]
async fn repeated_open_close_does_not_accumulate_pty_host_processes() {
    let _serial = SERIAL.lock().await;
    for _ in 0..5 {
        open_and_close_one_window().await;
    }
}

/// A window that is never closed: once its child has exited by itself, the
/// host that appeared for it must be gone too.
#[tokio::test]
async fn pty_host_is_reaped_when_the_child_exits_on_its_own() {
    let _serial = SERIAL.lock().await;
    let before = common::pty_host_child_pids();
    let (registry, window_id) = open_window(common::sleepy_command("3")).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    let host_pids = common::wait_for_new_pty_host_pids(&before, Duration::from_secs(10)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while entry.session().exit_status().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the short-lived child never exited"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    common::wait_processes_gone(&host_pids, Duration::from_secs(10)).await;
}

/// The grandchild's PID: the `capture_child` heartbeat's `tick` process, a
/// direct child of the window's command.
fn grandchild_pids_of(child_pid: u32) -> Vec<u32> {
    common::children_of(&common::process_table(), child_pid)
        .into_iter()
        .map(|row| row.pid)
        .collect()
}

/// Closing a window whose child is flooding output returns within its bound
/// and leaves no host, child or grandchild behind.
#[tokio::test]
async fn close_window_during_an_output_flood_reaps_host_child_and_grandchild() {
    let _serial = SERIAL.lock().await;
    let tmp = yagura_test_support::paths::TempDir::new("yagura-teardown-pty-host", "flood");
    let tick_path = tmp.path().join("tick.log");

    let before = common::pty_host_child_pids();
    let command = vec![
        yagura_test_support::process::capture_child_path()
            .to_str()
            .expect("capture_child path must be valid UTF-8")
            .to_string(),
        "heartbeat".to_string(),
        tick_path
            .to_str()
            .expect("tick path must be valid UTF-8")
            .to_string(),
        "flood".to_string(),
    ];
    let (registry, window_id) = open_window(command).await;
    let entry = registry
        .get(&window_id)
        .expect("window must exist in registry");

    let host_pids = common::wait_for_new_pty_host_pids(&before, Duration::from_secs(10)).await;
    common::wait_until_file_grows(Path::new(&tick_path), Duration::from_secs(10)).await;

    let flood_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.session().captured_output().is_empty() {
        assert!(
            tokio::time::Instant::now() < flood_deadline,
            "the flooding child produced no output"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let child_pid = entry.session().child_process_id();
    let grandchildren = grandchild_pids_of(child_pid);
    assert!(
        !grandchildren.is_empty(),
        "the heartbeat child must have a running grandchild before the close"
    );

    tokio::time::timeout(
        Duration::from_secs(15),
        yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5)),
    )
    .await
    .expect("close_window must return within its bound while the child floods output")
    .expect("close_window should not error");

    let mut all = host_pids;
    all.push(child_pid);
    all.extend(grandchildren);
    common::wait_processes_gone(&all, Duration::from_secs(10)).await;
}
