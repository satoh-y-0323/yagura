#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(windows)]

//! Every Windows window runs under the bundled pseudoconsole: its host is the
//! `OpenConsole.exe` beside the holder's executable, never the OS's `conhost.exe`,
//! and `close_window` ends that host together with the rest of the window's tree.
//!
//! The pseudoconsole host and the window's child are both children of the window's
//! `yagura-holder`. A window that never starts has no child to find its holder by, so
//! that case diffs every bundled host beside the built holder; the tests in this file
//! take one lock so no other window opens inside that diff.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use yagura_daemon::{DaemonConfig, OpenWindowRequest, WindowRegistry};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const BUNDLED_HOST_IMAGE: &str = "OpenConsole.exe";

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

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .unwrap_or_else(|| panic!("{} should have a parent directory", path.display()))
}

/// Every PID in the subtree rooted at `root`, `root` included.
fn subtree(root: u32) -> Vec<u32> {
    let table = common::process_table();
    let mut found = vec![root];
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        found.extend(
            common::children_of(&table, parent)
                .into_iter()
                .map(|row| row.pid)
                .filter(|pid| !found.contains(pid))
                .collect::<Vec<_>>(),
        );
        index += 1;
    }
    found
}

/// Every running bundled host whose image is beside the built holder, whoever its
/// parent was.
fn bundled_hosts_beside_the_holder() -> Vec<u32> {
    let holder_dir =
        common::canonical_lower(parent_dir(&yagura_test_support::process::holder_path()));
    common::process_table()
        .into_iter()
        .filter(|row| row.image.eq_ignore_ascii_case(BUNDLED_HOST_IMAGE))
        .filter(|row| {
            common::image_path(row.pid)
                .is_some_and(|image| common::canonical_lower(parent_dir(&image)) == holder_dir)
        })
        .map(|row| row.pid)
        .collect()
}

#[tokio::test]
async fn a_window_runs_under_the_bundled_open_console() {
    let _serial = SERIAL.lock().await;
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;
    let entry = registry.get(&window_id).expect("window must exist");
    let child = entry.session().child_process_id();

    let holder = common::holder_of(child);
    let hosts = common::window_pty_hosts(child);
    assert!(
        !hosts.is_empty(),
        "the window's holder {} must host the window's pseudoconsole",
        holder.pid
    );
    let images: Vec<&str> = hosts.iter().map(|row| row.image.as_str()).collect();
    assert!(
        images
            .iter()
            .all(|image| image.eq_ignore_ascii_case(BUNDLED_HOST_IMAGE)),
        "the window's pseudoconsole host must be {BUNDLED_HOST_IMAGE}, got {images:?}"
    );

    let holder_image = common::image_path(holder.pid).expect("the holder's image should read");
    for host in &hosts {
        let host_image = common::image_path(host.pid).expect("the host's image should read");
        assert_eq!(
            common::canonical_lower(parent_dir(&host_image)),
            common::canonical_lower(parent_dir(&holder_image)),
            "the host must be the bundled copy beside the holder, got {}",
            host_image.display()
        );
    }

    let _ = yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn close_window_ends_the_open_console_and_the_whole_window_tree() {
    let _serial = SERIAL.lock().await;
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;
    let entry = registry.get(&window_id).expect("window must exist");

    let child = entry.session().child_process_id();
    // The tree below must be the window's own: a wrong root walks processes that
    // cannot be opened and so read as already gone.
    let child_image = common::process_table()
        .into_iter()
        .find(|row| row.pid == child)
        .map(|row| row.image);
    assert!(
        child_image.is_some_and(|image| image.eq_ignore_ascii_case("cmd.exe")),
        "the session must report the pid of the program it started, got {child}"
    );
    let hosts = common::window_pty_hosts(child);
    assert!(
        hosts
            .iter()
            .any(|row| row.image.eq_ignore_ascii_case(BUNDLED_HOST_IMAGE)),
        "the window must run under {BUNDLED_HOST_IMAGE}, got hosts {hosts:?}"
    );

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    let mut window_tree = subtree(child);
    while window_tree.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fixture precondition failed: the child should have a descendant (ping)"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        window_tree = subtree(child);
    }
    for host in &hosts {
        window_tree.extend(subtree(host.pid));
    }

    yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5))
        .await
        .expect("close_window should not error");

    common::wait_processes_gone(&window_tree).await;
}

#[tokio::test]
async fn a_window_whose_program_cannot_start_leaves_no_pseudoconsole_host_behind() {
    let _serial = SERIAL.lock().await;
    let before = bundled_hosts_beside_the_holder();
    let registry = WindowRegistry::new(DaemonConfig::default());
    let missing = common::guaranteed_nonexistent_exe_path();

    let opened = registry
        .open(OpenWindowRequest {
            command: vec![missing.to_string_lossy().into_owned()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await;
    assert!(opened.is_err(), "a missing program must not open a window");

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let left: Vec<u32> = bundled_hosts_beside_the_holder()
            .into_iter()
            .filter(|pid| !before.contains(pid))
            .collect();
        if left.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pseudoconsole host of a window that never started is still running: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
