#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Runs `ps` to see whether the grandchild outlived the window; no window is opening
// meanwhile, so the spawn cannot be pulled into one.
#![allow(clippy::disallowed_methods)]

//! A window whose child never reads its input while a grandchild holds the PTY open
//! still closes: the terminal's replies to the child's queries cannot be delivered, and
//! tearing the window down must not wait for them.
//!
//! The child (`hold_pty_child`) floods DA1 queries and then idles without reading.
//! Because the grandchild keeps the PTY's slave side open, killing the child alone does
//! not make a blocked write to the PTY fail. Windows is not covered: the bundled
//! OpenConsole keeps draining the input pipe, so no write ever blocks there.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use common::{Harness, unique_temp_dir, wait_until_model_shows};
use yagura_daemon::DaemonConfig;

const FLOODED_MARKER: &[u8] = b"HOLD-FLOODED";
const GUARD: Duration = Duration::from_secs(120);

fn is_alive(pid: &str) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("run ps");
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

fn read_pid(path: &Path) -> String {
    std::fs::read_to_string(path)
        .expect("the child writes the grandchild's pid before it floods")
        .trim()
        .to_string()
}

#[test]
fn close_window_returns_and_ends_the_tree_when_replies_cannot_be_delivered() {
    common::run_on_own_runtime(GUARD, || async {
        let h = Harness::start("close-stuck-replies", DaemonConfig::default()).await;
        let dir = unique_temp_dir("close-stuck-replies");
        let pid_file = dir.join("grandchild.pid");
        let command = vec![
            env!("CARGO_BIN_EXE_hold_pty_child").to_string(),
            pid_file.to_str().expect("utf-8 path").to_string(),
        ];
        let window_id = h.open(command, None).await;
        wait_until_model_shows(&h, &window_id, FLOODED_MARKER, Duration::from_secs(60)).await;

        let grandchild = read_pid(&pid_file);
        assert!(
            is_alive(&grandchild),
            "fixture precondition: the grandchild must be running before the close"
        );

        let entry = h.registry.get(&window_id).expect("window must exist");
        yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5))
            .await
            .expect("close_window should not error");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while is_alive(&grandchild) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the grandchild {grandchild} survived close_window"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            *entry.session().exit_notified().borrow(),
            "the window's child must be reported exited once close_window returned"
        );
    });
}
