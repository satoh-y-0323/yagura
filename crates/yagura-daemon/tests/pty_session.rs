#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contract for the daemon's PTY ownership layer (`PtySession`).
//!
//! The child under test is the `echo_child` test binary, which writes stdin back to
//! stdout unmodified; `yagura-test-support` resolves its path and spawns it.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
use yagura_test_support::paths::strip_extended_length_prefix;
use yagura_test_support::process::{echo_child_command, echo_child_path};
use yagura_test_support::scan::contains_subsequence;

use yagura_daemon::{DaemonConfig, PtySession, WindowsShellSearch, resolve_shell};

async fn spawn_echo_session(config: &DaemonConfig) -> PtySession {
    let child = echo_child_path();
    let child_str = child.to_str().expect("echo_child path must be valid UTF-8");
    PtySession::spawn_with_command(&[child_str], config)
        .await
        .expect("spawn_with_command should start echo_child")
}

/// Pass criterion: the written bytes appear at least once as a subsequence of the read
/// stream. ConPTY local echo, VT sequences and CRLF translation are tolerated.
async fn recv_until_contains(session: &PtySession, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let mut arrivals = session.output_arrivals();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        arrivals.borrow_and_update();
        // The raw bytes are kept before they are fed, so a chunk is in here by
        // the time its arrival is counted.
        let collected = session.captured_output();
        if contains_subsequence(&collected, needle) {
            return collected;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, arrivals.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => panic!("the window's output ended while waiting for output: {err}"),
            Err(_) => panic!(
                "timed out waiting for output to contain {needle:?}; collected so far: {collected:?}"
            ),
        }
    }
}

// resolve_shell is a pure function of its arguments; `shell_env` is a Unix-only input,
// so these Windows expectations always pass None for it.

#[cfg(windows)]
#[test]
fn resolve_shell_none_returns_existing_powershell_family_binary() {
    let resolved = resolve_shell(None, None, &WindowsShellSearch::from_env())
        .expect("resolve_shell(None, None) should find pwsh or powershell.exe");
    assert!(
        resolved.exists(),
        "resolved shell path must exist: {resolved:?}"
    );
    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        name == "pwsh.exe" || name == "powershell.exe",
        "unexpected resolved shell: {resolved:?}"
    );
}

#[cfg(windows)]
#[test]
fn resolve_shell_with_existing_configured_path_returns_it() {
    // `current_exe()` is already canonical here, so the test feeds an all-uppercase
    // variant that still exists on a case-insensitive filesystem: it passes only once
    // resolve_shell canonicalizes (restoring the on-disk casing) and strips the
    // extended-length prefix.
    let raw_exe = std::env::current_exe().expect("current_exe");
    let denormalized = PathBuf::from(raw_exe.to_string_lossy().to_uppercase());
    assert!(
        denormalized.exists(),
        "the all-uppercase variant must still resolve to the same file on a case-insensitive \
         filesystem: {denormalized:?}"
    );
    assert_ne!(
        denormalized, raw_exe,
        "test setup must actually be non-canonical (case-denormalized)"
    );

    let resolved = resolve_shell(Some(&denormalized), None, &WindowsShellSearch::from_env())
        .expect("resolve_shell should accept an existing path");

    let expected = strip_extended_length_prefix(
        &denormalized
            .canonicalize()
            .expect("denormalized path must canonicalize"),
    );
    assert_eq!(
        resolved, expected,
        "resolve_shell must return the canonicalized path (true on-disk casing) with the \
         \\\\?\\ prefix stripped"
    );
}

#[cfg(windows)]
#[test]
fn resolve_shell_with_missing_configured_path_falls_back() {
    let missing = Path::new(r"Z:\definitely\not\a\real\path\yagura-missing-shell.exe");
    let resolved = resolve_shell(Some(missing), None, &WindowsShellSearch::from_env()).expect(
        "resolve_shell should fall back to pwsh/powershell.exe when configured path is missing",
    );
    assert!(resolved.exists());
    assert_ne!(resolved, missing);
}

#[cfg(windows)]
#[test]
fn resolve_shell_is_pure_and_does_not_mutate_process_environment() {
    let before = std::env::var("YAGURA_SHELL").ok();
    let first = resolve_shell(None, None, &WindowsShellSearch::from_env())
        .expect("first resolve_shell(None, None) call");
    let second = resolve_shell(None, None, &WindowsShellSearch::from_env())
        .expect("second resolve_shell(None, None) call");
    let after = std::env::var("YAGURA_SHELL").ok();
    assert_eq!(
        first, second,
        "resolve_shell must be referentially transparent"
    );
    assert_eq!(
        before, after,
        "resolve_shell must not mutate process environment"
    );
}

// Unix order: configured -> shell_env -> the literal /bin/sh (NotFound if absent),
// with no canonicalization.

#[cfg(unix)]
mod unix_resolve_shell {
    use super::*;

    #[test]
    fn resolve_shell_falls_back_to_bin_sh_literal_when_nothing_else_is_configured() {
        let resolved = resolve_shell(None, None, &WindowsShellSearch::from_env())
            .expect("resolve_shell(None, None) should fall back to /bin/sh");
        assert_eq!(
            resolved,
            PathBuf::from("/bin/sh"),
            "the final Unix fallback must be the literal /bin/sh"
        );
    }

    #[test]
    fn resolve_shell_prefers_configured_over_shell_env() {
        // std::env::current_exe() always exists on this platform and is not
        // `/bin/sh`, so it is distinguishable from every fallback.
        let configured = std::env::current_exe().expect("current_exe");
        let resolved = resolve_shell(
            Some(&configured),
            Some(Path::new("/bin/sh")),
            &WindowsShellSearch::from_env(),
        )
        .expect("resolve_shell should accept an existing configured path");
        assert_eq!(
            resolved, configured,
            "configured must take priority over shell_env (Unix order: configured -> \
             shell_env -> /bin/sh)"
        );
    }

    #[test]
    fn resolve_shell_falls_back_to_shell_env_when_configured_is_missing() {
        let missing = Path::new("/definitely/not/a/real/path/yagura-missing-shell");
        let shell_env = std::env::current_exe().expect("current_exe");
        let resolved = resolve_shell(
            Some(missing),
            Some(&shell_env),
            &WindowsShellSearch::from_env(),
        )
        .expect("resolve_shell should fall back to shell_env when configured is missing");
        assert_eq!(
            resolved, shell_env,
            "a missing configured path must fall back to an existing shell_env (Unix order)"
        );
    }

    #[test]
    fn resolve_shell_does_not_canonicalize_on_unix() {
        // A `..` round trip makes the path non-canonical: `Path` equality normalizes
        // away a non-leading `.` component but not a `..` one, so a `.`-based path
        // would already compare equal to `configured`.
        let configured = std::env::current_exe().expect("current_exe");
        let parent = configured
            .parent()
            .expect("current_exe must have a parent dir");
        let parent_name = parent
            .file_name()
            .expect("current_exe's parent dir must have a name");
        let file_name = configured
            .file_name()
            .expect("current_exe must have a file name");
        let denormalized = parent.join("..").join(parent_name).join(file_name);
        assert_ne!(
            denormalized, configured,
            "test setup must actually be non-canonical (contains a `..` component)"
        );

        let resolved = resolve_shell(Some(&denormalized), None, &WindowsShellSearch::from_env())
            .expect("resolve_shell should accept an existing configured path");
        assert_eq!(
            resolved, denormalized,
            "Unix resolve_shell must not canonicalize the configured path (the \\\\?\\ strip \
             is Windows-only)"
        );
    }
}

#[tokio::test]
async fn spawn_with_command_echoes_ascii_input() {
    let config = DaemonConfig::default();
    let session = spawn_echo_session(&config).await;

    let payload = b"hello-yagura\r\n";
    session
        .write_input(payload)
        .await
        .expect("write_input should succeed while child is running");

    let _ = recv_until_contains(&session, payload, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn spawn_with_command_echoes_japanese_utf8_as_subsequence() {
    let config = DaemonConfig::default();
    let session = spawn_echo_session(&config).await;

    let payload = "やぐら日本語テスト\r\n".as_bytes();
    session
        .write_input(payload)
        .await
        .expect("write_input should succeed while child is running");

    let _ = recv_until_contains(&session, payload, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn initial_pty_size_is_80x24() {
    let config = DaemonConfig::default();
    let session = spawn_echo_session(&config).await;
    assert_eq!(session.size().await.unwrap(), (80, 24));
}

#[tokio::test]
async fn resize_updates_reported_size_and_does_not_error() {
    let config = DaemonConfig::default();
    let session = spawn_echo_session(&config).await;

    session
        .resize(120, 40)
        .await
        .expect("resize within valid range must succeed");
    assert_eq!(session.size().await.unwrap(), (120, 40));
}

#[tokio::test]
async fn exit_is_detected_with_correct_exit_code_and_state_is_retained() {
    let config = DaemonConfig::default();
    // A shell with a known exit code keeps exit detection deterministic, unlike echo_child.
    #[cfg(windows)]
    let command = ["cmd.exe", "/c", "exit 7"];
    #[cfg(unix)]
    let command = ["/bin/sh", "-c", "exit 7"];
    let session = PtySession::spawn_with_command(&command, &config)
        .await
        .expect("spawn_with_command should start the shell");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(code) = session.exit_status() {
            assert_eq!(code, 7);
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for child process exit to be detected");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The Exited state is retained: a repeated query returns the same result.
    assert_eq!(session.exit_status(), Some(7));
}

/// Polls `exit_status()` until it becomes `Some` or `timeout` elapses.
///
/// Deliberate exception to the rule of using a paused clock for time-dependent tests:
/// this must observe a real OS child process dying, which a virtual clock cannot cause.
async fn wait_for_exit_status(session: &PtySession, timeout: Duration) -> i32 {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(code) = session.exit_status() {
            return code;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for terminate() to deliver a kill to the child");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn terminate_kills_a_long_running_child_within_ten_seconds() {
    let config = DaemonConfig::default();
    let argv = echo_child_command();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let session = PtySession::spawn_with_command(&argv, &config)
        .await
        .expect("spawn_with_command should start echo_child");

    println!("child_process_id={:?}", session.child_process_id());

    // Give the monitor thread time to run before terminate() is called.
    tokio::time::sleep(Duration::from_millis(500)).await;
    session.terminate();

    let code = wait_for_exit_status(&session, Duration::from_secs(10)).await;
    println!("exit_code={code}");
    // terminate() must kill the child itself rather than rely on the writer-close side
    // effect; exit code 1 is TerminateProcess on Windows / a mapped signal exit on Unix.
    assert_eq!(
        code, 1,
        "terminate() must kill the child itself (exit code 1), not merely rely on the writer-close side effect; observed exit_code={code}"
    );
}

#[tokio::test]
async fn terminate_called_twice_then_again_after_exit_is_observed_does_not_panic_or_hang() {
    let config = DaemonConfig::default();
    let argv = echo_child_command();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let session = PtySession::spawn_with_command(&argv, &config)
        .await
        .expect("spawn_with_command should start echo_child");

    println!("child_process_id={:?}", session.child_process_id());

    tokio::time::sleep(Duration::from_millis(500)).await;
    session.terminate();
    session.terminate();

    let _ = wait_for_exit_status(&session, Duration::from_secs(10)).await;

    // Exit has now been observed; a further terminate() call must be a
    // harmless no-op, not a panic or a hang.
    session.terminate();
}

#[tokio::test]
async fn dropping_a_session_ends_its_running_child() {
    let config = DaemonConfig::default();
    let session = spawn_echo_session(&config).await;
    let mut exited = session.exit_notified();
    assert!(
        !*exited.borrow(),
        "fixture precondition: echo_child must still be running"
    );

    drop(session);

    let ended = tokio::time::timeout(Duration::from_secs(10), exited.wait_for(|&gone| gone))
        .await
        .is_ok_and(|seen| seen.is_ok());
    assert!(ended, "dropping the session must end its child");
}

#[cfg(unix)]
#[tokio::test]
async fn terminate_called_once_delivers_escalated_sigkill_to_sighup_ignoring_child() {
    let config = DaemonConfig::default();
    // The marker follows the trap, so the first signal cannot land before the trap is set.
    let argv = [
        "/bin/sh",
        "-c",
        "trap \"\" HUP; printf HUP-IGNORED; exec sleep 3600",
    ];
    let session = PtySession::spawn_with_command(&argv, &config)
        .await
        .expect("spawn_with_command should start the SIGHUP-ignoring child");
    let _ = recv_until_contains(&session, b"HUP-IGNORED", Duration::from_secs(10)).await;

    session.terminate();

    let _ = wait_for_exit_status(&session, Duration::from_secs(10)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn terminate_called_twice_still_delivers_escalated_sigkill_to_sighup_ignoring_child() {
    let config = DaemonConfig::default();
    // exec replaces the shell in place (same pid, no grandchild) and the HUP trap ignores
    // terminate()'s initial signal, so only the escalated force-kill can end this process.
    let argv = ["/bin/sh", "-c", "trap \"\" HUP; exec sleep 3600"];
    let session = PtySession::spawn_with_command(&argv, &config)
        .await
        .expect("spawn_with_command should start the SIGHUP-ignoring child");

    println!("child_process_id={:?}", session.child_process_id());

    tokio::time::sleep(Duration::from_millis(500)).await;
    session.terminate();
    session.terminate();

    let _ = wait_for_exit_status(&session, Duration::from_secs(10)).await;
}
