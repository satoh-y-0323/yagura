#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(windows)]

//! Every Windows window runs under the bundled pseudoconsole: its host is the
//! `OpenConsole.exe` beside the daemon's executable, never the OS's `conhost.exe`,
//! and `close_window` ends that host together with the rest of the window's tree.
//!
//! The pseudoconsole host and the window's child are both direct children of the
//! daemon process. The host is told apart from other tests' hosts by a before/after
//! diff, and the tests in this file take one lock so no other window opens inside
//! that diff.

mod common;

use std::path::PathBuf;
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

/// Waits until a pseudoconsole host not in `before` is a direct child of this
/// process, and returns every such new host row.
async fn wait_for_new_hosts(before: &[u32]) -> Vec<common::ProcessRow> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let new_hosts = common::new_pids(before, &common::pty_host_child_pids());
        if !new_hosts.is_empty() {
            return common::process_table()
                .into_iter()
                .filter(|row| new_hosts.contains(&row.pid))
                .collect();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no pseudoconsole host appeared as a direct child after opening a window");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn image_path(pid: u32) -> PathBuf {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    let mut buffer = vec![0u16; 32 * 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: the handle is closed right after the one call that reads it, and
    // `buffer` outlives that call with `len` set to its capacity in UTF-16 units.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .unwrap_or_else(|e| panic!("opening host process {pid} should succeed: {e}"));
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        result.unwrap_or_else(|e| panic!("reading host process {pid} image should succeed: {e}"));
    }
    PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize]))
}

fn canonical(path: &std::path::Path) -> String {
    let resolved = std::fs::canonicalize(path)
        .unwrap_or_else(|e| panic!("{} should canonicalize: {e}", path.display()));
    yagura_test_support::paths::strip_extended_length_prefix(&resolved)
        .to_string_lossy()
        .to_lowercase()
}

/// Every PID in the subtree rooted at `root`, `root` included.
fn subtree(root: u32) -> Vec<u32> {
    let table = common::process_table();
    let mut found = vec![root];
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        found.extend(
            table
                .iter()
                .filter(|row| row.parent_pid == parent && !found.contains(&row.pid))
                .map(|row| row.pid)
                .collect::<Vec<_>>(),
        );
        index += 1;
    }
    found
}

#[tokio::test]
async fn a_window_runs_under_the_bundled_open_console() {
    let _serial = SERIAL.lock().await;
    let before = common::pty_host_child_pids();
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;

    let hosts = wait_for_new_hosts(&before).await;
    let images: Vec<&str> = hosts.iter().map(|row| row.image.as_str()).collect();
    assert!(
        images
            .iter()
            .all(|image| image.eq_ignore_ascii_case(BUNDLED_HOST_IMAGE)),
        "the window's pseudoconsole host must be {BUNDLED_HOST_IMAGE}, got {images:?}"
    );

    let exe_dir = std::env::current_exe()
        .expect("current exe should resolve")
        .parent()
        .expect("current exe should have a parent directory")
        .to_path_buf();
    for host in &hosts {
        let host_path = image_path(host.pid);
        let host_dir = host_path
            .parent()
            .expect("host image should have a parent directory");
        assert_eq!(
            canonical(host_dir),
            canonical(&exe_dir),
            "the host must be the bundled copy beside the executable, got {}",
            host_path.display()
        );
    }

    let entry = registry.get(&window_id).expect("window must exist");
    let _ = yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn close_window_ends_the_open_console_and_the_whole_window_tree() {
    let _serial = SERIAL.lock().await;
    let before = common::pty_host_child_pids();
    let (registry, window_id) = open_window(common::sleepy_command("30")).await;
    let entry = registry.get(&window_id).expect("window must exist");

    let hosts = wait_for_new_hosts(&before).await;
    let open_console: Vec<u32> = hosts
        .iter()
        .filter(|row| row.image.eq_ignore_ascii_case(BUNDLED_HOST_IMAGE))
        .map(|row| row.pid)
        .collect();
    assert!(
        !open_console.is_empty(),
        "the window must run under {BUNDLED_HOST_IMAGE}, got hosts {hosts:?}"
    );

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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

    common::wait_processes_gone(&window_tree, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn a_window_whose_program_cannot_start_leaves_no_pseudoconsole_host_behind() {
    let _serial = SERIAL.lock().await;
    let before = common::pty_host_child_pids();
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = common::new_pids(&before, &common::pty_host_child_pids());
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
