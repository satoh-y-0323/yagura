#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process: the properties here
// are the exit code and the output of the process itself.
#![allow(clippy::disallowed_methods)]

//! Integration test: `yagura-daemon --emit-status` is what a CLI's hook runs, and a hook
//! also fires when the CLI was started by hand outside any yagura window. There it must
//! be invisible: nothing on stdout or stderr and exit code 0 (a hook that prints or exits
//! non-zero is surfaced to the user, and exit code 2 would even block the CLI's action).
//!
//! Every spawn sets `YAGURA_INSTANCE` to an identifier unique to this process, so the
//! endpoint it would connect to never belongs to a running daemon, and points the
//! data, state and config base directories into a temporary directory, so the
//! instance directories it creates never land in the user's real profile.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use yagura_test_support::paths::TempDir;

const WINDOW_ID_VAR: &str = "YAGURA_WINDOW_ID";
const STATUS_TOKEN_VAR: &str = "YAGURA_STATUS_TOKEN";

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

/// Runs `--emit-status running` with exactly the given window variables set; the test
/// process's own values (it may itself run inside a yagura window) are never inherited.
fn emit_status(window_id: Option<&str>, token: Option<&str>) -> Output {
    let instance = format!("emit{:x}", std::process::id());
    let bases = TempDir::new("yagura-emit", "bases");
    let mut command = Command::new(daemon_bin());
    command
        .args(["--emit-status", "running", "--source", "hook"])
        .env("YAGURA_INSTANCE", instance)
        .env_remove("YAGURA_STORE_DB")
        .env_remove(WINDOW_ID_VAR)
        .env_remove(STATUS_TOKEN_VAR)
        .stdin(Stdio::null());
    if cfg!(windows) {
        command
            .env("LOCALAPPDATA", bases.make_dir("local"))
            .env("APPDATA", bases.make_dir("roaming"));
    } else {
        command
            .env("XDG_DATA_HOME", bases.make_dir("data"))
            .env("XDG_STATE_HOME", bases.make_dir("state"))
            .env("XDG_CONFIG_HOME", bases.make_dir("config"))
            .env("HOME", bases.make_dir("home"));
    }
    if let Some(window_id) = window_id {
        command.env(WINDOW_ID_VAR, window_id);
    }
    if let Some(token) = token {
        command.env(STATUS_TOKEN_VAR, token);
    }
    command
        .output()
        .expect("failed to spawn the yagura-daemon binary for --emit-status")
}

fn assert_silent_success(output: &Output, situation: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{situation}: --emit-status must exit 0, got {:?}; stderr: {stderr}",
        output.status.code()
    );
    assert!(
        stdout.is_empty() && stderr.is_empty(),
        "{situation}: --emit-status must print nothing; stdout: {stdout:?} stderr: {stderr:?}"
    );
}

#[test]
fn emit_status_without_either_window_variable_does_nothing_and_succeeds() {
    let output = emit_status(None, None);
    assert_silent_success(&output, "no window variables");
}

#[test]
fn emit_status_with_only_the_window_id_does_nothing_and_succeeds() {
    let output = emit_status(Some("w-1"), None);
    assert_silent_success(&output, "only the window id");
}

#[test]
fn emit_status_with_only_the_token_does_nothing_and_succeeds() {
    let output = emit_status(None, Some("00112233445566778899aabbccddeeff"));
    assert_silent_success(&output, "only the status token");
}

// An empty value is how a CLI clears a variable for its tools' children, so it is no claim.
#[test]
fn emit_status_with_both_window_variables_empty_does_nothing_and_succeeds() {
    let output = emit_status(Some(""), Some(""));
    assert_silent_success(&output, "both window variables empty");
}

#[test]
fn emit_status_with_one_window_variable_empty_does_nothing_and_succeeds() {
    let token = "00112233445566778899aabbccddeeff";
    for (window_id, token, situation) in [
        ("", token, "an empty window id beside a token"),
        ("w-1", "", "an empty token beside a window id"),
    ] {
        let output = emit_status(Some(window_id), Some(token));
        assert_silent_success(&output, situation);
    }
}

#[test]
fn emit_status_inside_a_window_fails_when_the_daemon_is_unreachable() {
    let output = emit_status(Some("w-1"), Some("00112233445566778899aabbccddeeff"));
    let code = output.status.code();
    assert!(
        code.is_some_and(|code| code != 0 && code != 2),
        "with both window variables present and no daemon listening the hook must fail \
         with a code other than 0 (a report into nothing is not success) and other than 2 \
         (a CLI reads 2 as \"block the action\"); got {code:?}"
    );
}
