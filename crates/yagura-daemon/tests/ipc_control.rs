#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: IPC wiring of the four control messages
//! (`interrupt_window` / `close_window` / `wait_for_status` / `read_output`)
//! plus `status_event` authentication and rate limiting. Every test drives a
//! real `WindowRegistry` + `IpcServer` pair over a real IPC endpoint.
//!
//! ## Fixture conventions
//!
//! - Non-`Unknown` windows use [`common::control_definition`] (rule `CTRL-READY` ->
//!   `idle`), are stimulated with the marker and then *observed* to have reached
//!   `idle` before the assertion under test starts. `rules = []` is unusable
//!   here: it stays `Unknown` forever.
//! - `Unknown` windows are opened with no detection definition.
//! - Approval policies and the status token are injected straight into
//!   `DaemonConfig`; the `--playbook` / env entry points are covered elsewhere.
//! - Interrupt delivery is observed by replacing `DaemonConfig.interrupt_bytes`
//!   with a readable marker: under ConPTY the default `0x03` becomes a
//!   `CTRL_C_EVENT` and is never echoed, so the scrollback cannot show it.
//! - [`probe_status_event`]'s `window_id` is `Option<&str>`: `None` omits the
//!   field on the wire (refused as `invalid_argument`), while `Some("")` sends
//!   an explicit empty string, which names no window (`window_not_found`).

mod common;

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::{
    CONTROL_DETECTION_NAME, Harness, READY_MARKER, SubmitDefinitionInput, VALID_TOKEN,
    approval_config, approver_base_config as base_config, assert_file_len_stable,
    call_list_windows, contains_subsequence, count_marker, decide, echo_child_path, env_probe_path,
    expect_approval_requested, expect_error, hex_echo_path, identify, open_submit_child_window,
    open_submit_child_window_with_clear_scope, open_submit_child_window_with_composer,
    open_submit_child_window_with_composer_and_bottom_line_count,
    open_submit_child_window_with_composer_logging_input, paste_buffered_values, policy,
    query_status, read_frame, read_response, register_approver, report_status, send,
    send_from_two_connections, surviving_grandchild_command, unique_temp_dir,
    wait_for_accepted_handoff_envelope_json, wait_until_file_grows, wait_until_input_log_contains,
    wait_until_status, write_submit_definition,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::exit_with_code_command;
use yagura_test_support::process::submit_child_command;

use yagura_daemon::config::InterruptMethod;
use yagura_daemon::handoff::{EnvelopeFrom, compose_envelope};
use yagura_daemon::window::InputOrigin;
use yagura_daemon::{DaemonConfig, OpenWindowRequest};
use yagura_protocol::{
    ApprovalRequesterKind, ClientMessage, ControlKey, DaemonMessage, HANDOFF_NOTE_MAX_BYTES,
    HANDOFF_REFERENCE_MAX_BYTES, HandoffDelivery, WindowStatus, WithdrawReason,
};

/// Same shape as [`VALID_TOKEN`] but a different value, so that a rejection
/// can only be attributed to the comparison and not to a shape check.
const WRONG_TOKEN: &str = "fedcba9876543210fedcba9876543210";

/// Replaces `DaemonConfig.interrupt_bytes` so that interrupt delivery is
/// observable in the scrollback.
const INTERRUPT_MARKER: &[u8] = b"INTERRUPT-MARK\r\n";

/// Config whose detection directory holds [`common::control_definition`],
/// enabling windows opened with `detection = Some(CONTROL_DETECTION_NAME)` to
/// leave `Unknown`.
fn detection_config(dir: &Path) -> DaemonConfig {
    common::detection_config(dir, base_config())
}

/// A handoff's answer as its wire string.
fn wire(delivery: HandoffDelivery) -> String {
    serde_json::to_value(delivery)
        .expect("a delivery encodes")
        .as_str()
        .expect("a delivery is a string on the wire")
        .to_string()
}

/// `config` under a playbook that auto-approves `send_handoff` and answers a handoff
/// the window has not taken within `secs` as undelivered.
fn with_handoff_delivery_limit(mut config: DaemonConfig, secs: u64) -> DaemonConfig {
    let source = format!(
        "[approval]\nauto_approve = [\"send_handoff\"]\n\n\
         [limits]\nhandoff_delivery_timeout_secs = {secs}\n"
    );
    let policy = yagura_daemon::control::parse_policy(&source)
        .unwrap_or_else(|err| panic!("the playbook must load ({err}):\n{source}"));
    yagura_daemon::control::apply_limits(&mut config, policy.limits());
    config.approval_policy = Some(policy);
    config
}

/// Waits until `window_id`'s inbox holds `count` parcels.
async fn wait_until_inbox_holds(h: &Harness, window_id: &str, count: usize) {
    let entry = h.registry.get(window_id).expect("window must exist");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() != count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "hang guard: the inbox never held {count} parcel(s), it holds {}",
            entry.inbox().pending_count()
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A fixed command whose direct child prints the value of each environment
/// variable named in `vars` (space-separated, in order) and then stays alive well
/// past this file's timeouts, so the line can be read back from the scrollback.
fn echo_env_and_wait_command(vars: &[&str]) -> Vec<String> {
    if cfg!(windows) {
        let refs: Vec<String> = vars.iter().map(|v| format!("%{v}%")).collect();
        vec![
            "cmd.exe".to_string(),
            "/k".to_string(),
            format!("echo {}", refs.join(" ")),
        ]
    } else {
        let refs: Vec<String> = vars.iter().map(|v| format!("${v}")).collect();
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo {}; exec sleep 300", refs.join(" ")),
        ]
    }
}

impl Harness {
    /// Opens a window whose child exits immediately and observes the
    /// confirmed `Exited` status before returning.
    async fn open_exited_window(&self, client: &mut Connection) -> String {
        let window_id = self.open(exit_with_code_command("5"), None).await;
        wait_until_status(
            client,
            &window_id,
            WindowStatus::Exited,
            Duration::from_secs(10),
        )
        .await;
        window_id
    }
}

/// Outcome of a `status_event` probe. An accepted event produces no response at
/// all, so acceptance is observed by a trailing `Ping` whose `Pong` comes back
/// first.
#[derive(Debug, PartialEq, Eq)]
enum EventOutcome {
    Accepted,
    Rejected(String),
}

async fn probe_status_event(
    client: &mut Connection,
    window_id: Option<&str>,
    token: &str,
    status: WindowStatus,
) -> EventOutcome {
    send(
        client,
        &ClientMessage::StatusEvent {
            source: "hook-test".to_string(),
            status,
            window_id: window_id.map(str::to_string),
            token: token.to_string(),
        },
    )
    .await;
    send(client, &ClientMessage::Ping).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::Pong => EventOutcome::Accepted,
        DaemonMessage::Error { code, .. } => {
            let pong = read_response(client, Duration::from_secs(5)).await;
            assert!(
                matches!(pong, DaemonMessage::Pong),
                "the connection must stay usable after a rejected status_event, got {pong:?}"
            );
            EventOutcome::Rejected(code)
        }
        other => panic!("unexpected frame while probing status_event: {other:?}"),
    }
}

#[tokio::test]
async fn interrupt_window_acknowledges_and_delivers_configured_bytes_when_approved_and_idle() {
    let dir = unique_temp_dir("interrupt-ok");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_bytes: INTERRUPT_MARKER.to_vec(),
        ..detection_config(&dir)
    };
    let h = Harness::start("interrupt-ok", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowInterrupted { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_interrupted, got {other:?}"),
    }

    // The echo child writes back whatever reaches the PTY input, so the
    // configured bytes are observable in the scrollback.
    h.wait_for_scrollback(&window_id, INTERRUPT_MARKER, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn interrupt_window_with_an_unimplemented_method_is_refused_without_falling_back() {
    // With `interrupt_method = signal` selected, an approved interrupt on an idle
    // window must come back as an error, and the bytes of the other method must
    // not be delivered behind the caller's back (no silent fallback).
    let dir = unique_temp_dir("interrupt-signal");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_method: InterruptMethod::Signal,
        interrupt_bytes: INTERRUPT_MARKER.to_vec(),
        ..detection_config(&dir)
    };
    let h = Harness::start("interrupt-signal", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "unsupported_interrupt_method");

    // The echo child writes back whatever reaches its PTY, so the marker appearing at all
    // would mean the refusal was accompanied by a byte-write fallback.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !contains_subsequence(&h.scrollback(&window_id), INTERRUPT_MARKER),
        "a refused interrupt method must not fall back to writing the interrupt bytes"
    );
}

#[tokio::test]
async fn interrupt_window_returns_approval_required_without_a_policy() {
    let dir = unique_temp_dir("interrupt-denied");
    let config = detection_config(&dir);
    let h = Harness::start("interrupt-denied", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "approval_required");
}

#[tokio::test]
async fn interrupt_window_returns_status_unknown_even_when_the_tool_is_approved() {
    // The Unknown gate outranks the playbook (fail-safe).
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        ..base_config()
    };
    let h = Harness::start("interrupt-unknown", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut client, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window without a detection definition stays Unknown"
    );

    send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn interrupt_window_on_an_unknown_status_window_reports_status_unknown_even_to_an_unapproved_caller()
 {
    // The Unknown gate is reported to every caller, approved or not: masking it
    // behind approval_required would let an unapproved caller be told nothing
    // is wrong when the real reason the operation cannot run is that the
    // window's state could not be detected at all.
    let h = Harness::start("interrupt-unknown-undisclosed", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut client, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window without a detection definition stays Unknown"
    );

    send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn interrupt_window_returns_window_not_found_for_an_unknown_window_id() {
    // Window resolution is step (1) of the decision order, ahead of the
    // approval check -- an approving policy must not change the answer.
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        ..base_config()
    };
    let h = Harness::start("interrupt-missing", config).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: "does-not-exist".to_string(),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "window_not_found");
}

#[tokio::test]
async fn interrupt_window_on_an_exited_window_returns_child_exited_when_approved() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        ..base_config()
    };
    let h = Harness::start("interrupt-exited", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn interrupt_window_on_an_exited_window_returns_child_exited_without_any_approval() {
    // Pre-validation runs ahead of the approval gate: an
    // exited window answers child_exited to every caller, approved or not,
    // because get_window_status already discloses Exited to everyone.
    let h = Harness::start("interrupt-exited-denied", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

async fn close_window_kills_the_process_tree(tag: &str, force: Option<bool>) {
    let dir = unique_temp_dir(tag);
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..base_config()
    };
    let h = Harness::start(tag, config).await;
    let mut client = h.connect().await;
    // The surviving fixture is required: with the immediate-exit one the window's
    // own exit watcher would tear the process tree down before `close_window` is
    // ever sent, and this test would stop measuring the close path.
    let window_id = h.open(surviving_grandchild_command(tick_path), None).await;

    wait_until_file_grows(&tick, Duration::from_secs(20)).await;

    assert!(
        h.registry
            .get(&window_id)
            .expect("window must exist in registry")
            .session()
            .exit_status()
            .is_none(),
        "fixture precondition: the direct child must still be running when close_window is sent, \
         otherwise path A did the teardown and this test proves nothing about close_window"
    );

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }

    assert_file_len_stable(&tick, Duration::from_secs(3)).await;
}

#[tokio::test]
async fn close_window_without_force_terminates_the_whole_process_tree() {
    close_window_kills_the_process_tree("close-graceful", Some(false)).await;
}

#[tokio::test]
async fn close_window_with_force_reaches_the_same_final_state() {
    // Force only reorders terminate / job shutdown; the observable
    // end state (window_closed + zero orphans) is identical.
    close_window_kills_the_process_tree("close-forced", Some(true)).await;
}

#[tokio::test]
async fn close_window_succeeds_on_an_unknown_window_when_approved() {
    // close is deliberately outside the Unknown gate: otherwise a
    // hung / undetectable window could never be closed.
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..base_config()
    };
    let h = Harness::start("close-unknown", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut client, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window without a detection definition stays Unknown"
    );

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn close_window_on_an_exited_window_is_idempotent_when_approved() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..base_config()
    };
    let h = Harness::start("close-exited", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_close_lets_the_child_finish_its_hangup_handling() {
    // The child writes its marker from a SIGHUP trap; ending the tree right after the
    // signal, without waiting for the exit, kills it before the trap runs. The loop
    // waits on a background sleep because a trap only runs between foreground commands.
    let h = Harness::start("close-hangup-flush", base_config()).await;
    let dir = common::unique_temp_dir("close-hangup-flush");
    let flushed = dir.join("flushed");
    let script = format!(
        "trap ': > \"{}\"; exit 0' HUP; printf HUP-TRAPPED; while :; do sleep 1 & wait $!; done",
        flushed.display()
    );
    let window_id = h
        .open(vec!["/bin/sh".into(), "-c".into(), script], None)
        .await;
    common::wait_until_model_shows(&h, &window_id, b"HUP-TRAPPED", Duration::from_secs(10)).await;

    let entry = h.registry.get(&window_id).expect("window must exist");
    yagura_daemon::control::close_window(&entry, false, Duration::from_secs(5))
        .await
        .expect("close_window should not error");

    assert!(
        flushed.exists(),
        "a graceful close must let the child handle its hangup before the tree is ended"
    );
}

#[tokio::test]
async fn close_window_returns_approval_required_without_a_policy() {
    let h = Harness::start("close-denied", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "approval_required");
}

// `CloseWindow` is the probe tool for the three message-content tests below:
// `close_window` is exempt from the Unknown status gate, so an `Unknown`
// target still reaches the approval-message decision undisturbed by that
// gate, without needing a window driven to `idle` first.

#[tokio::test]
async fn approval_required_message_states_no_approver_is_connected_when_a_policy_is_loaded_but_lacks_the_tool()
 {
    // Diagnosability contract: needing a human decision is
    // reported the same way whether a playbook resolved but did not list the
    // tool, or nothing resolved at all -- the real bottleneck is that nobody
    // is registered to decide it.
    let config = DaemonConfig {
        playbook: None,
        approval_policy: Some(policy(&["interrupt_window"])),
        ..base_config()
    };
    let h = Harness::start("approval-message-no-approver-loaded", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
}

#[tokio::test]
async fn approval_required_message_states_no_approver_is_connected_when_nothing_resolves() {
    let config = DaemonConfig {
        playbook: None,
        approval_policy: None,
        ..base_config()
    };
    let h = Harness::start("approval-message-no-approver-unresolved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
}

#[tokio::test]
async fn approval_required_message_states_the_playbook_failed_to_load_when_configured_but_unloaded()
{
    // Diagnosability contract: a playbook path was configured
    // but never turned into a loaded policy, distinct from the two
    // no-approver cases above.
    let config = DaemonConfig {
        playbook: Some(std::path::PathBuf::from("nonexistent-playbook.toml")),
        approval_policy: None,
        ..base_config()
    };
    let h = Harness::start("approval-message-failed-to-load", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("failed to load"),
        "message must state that the playbook failed to load, got {message:?}"
    );
}

// =====================================================================
// identify: claiming a connection's origin as a specific window
// =====================================================================

#[tokio::test]
async fn identify_with_the_correct_window_and_token_unlocks_that_windows_own_operations_without_a_policy()
 {
    let dir = unique_temp_dir("identify-self");
    let h = Harness::start("identify-self", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected identified, got {other:?}"),
    }

    // No playbook is configured at all: only the self-window relief in the
    // gate can let this through.
    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowInterrupted { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_interrupted, got {other:?}"),
    }
}

#[tokio::test]
async fn identify_twice_to_the_same_window_is_idempotent() {
    let h = Harness::start("identify-idempotent", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    for _ in 0..2 {
        send(
            &mut client,
            &ClientMessage::Identify {
                window_id: window_id.clone(),
                token: VALID_TOKEN.to_string(),
            },
        )
        .await;
        match read_response(&mut client, Duration::from_secs(5)).await {
            DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
            other => panic!("expected identified, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn identify_to_a_different_window_after_already_identified_is_invalid_state() {
    let h = Harness::start("identify-conflict", base_config()).await;
    let mut client = h.connect().await;
    let window_a = h.open_unknown_window().await;
    let window_b = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_a.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_a),
        other => panic!("expected identified, got {other:?}"),
    }

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_b,
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_state");
}

#[tokio::test]
async fn identify_with_another_windows_real_token_is_auth_failed() {
    // No status_token_override here: each window mints its own real token, so
    // a token that authenticates window A must not also authenticate window B.
    let dir = unique_temp_dir("identify-cross-window");
    let config = DaemonConfig {
        status_token_override: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("identify-cross-window", config).await;
    let mut client = h.connect().await;

    let window_a = h
        .open(
            echo_env_and_wait_command(&["YAGURA_STATUS_TOKEN"]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    let token_a = wait_for_window_token(&h, &window_a, Duration::from_secs(10)).await;
    let window_b = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_b,
            token: token_a.clone(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");

    // The same token must still authenticate an identify against its own window.
    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_a.clone(),
            token: token_a,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_a),
        other => panic!("expected identified, got {other:?}"),
    }
}

#[tokio::test]
async fn identify_with_a_malformed_token_is_auth_failed() {
    let h = Harness::start("identify-malformed-token", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id,
            token: "not-a-valid-token-shape".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");
}

#[tokio::test]
async fn identify_with_an_unknown_window_id_is_window_not_found_and_does_not_count_toward_the_auth_failed_limit()
 {
    let h = Harness::start("identify-unknown-window", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    // Three window_not_found identifies, then a valid one: if window_not_found
    // silently counted toward the three-strikes limit the same way auth_failed
    // does, the connection would already be closed by the time the valid
    // identify below is sent.
    for _ in 0..3 {
        send(
            &mut client,
            &ClientMessage::Identify {
                window_id: "does-not-exist".to_string(),
                token: VALID_TOKEN.to_string(),
            },
        )
        .await;
        let msg = read_response(&mut client, Duration::from_secs(5)).await;
        expect_error(msg, "window_not_found");
    }

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id,
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { .. } => {}
        other => panic!(
            "expected identified after non-counting window_not_found failures, got {other:?}"
        ),
    }
}

#[tokio::test]
async fn three_auth_failed_identifies_close_the_connection() {
    let h = Harness::start("identify-three-strikes", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    for _ in 0..3 {
        send(
            &mut client,
            &ClientMessage::Identify {
                window_id: window_id.clone(),
                token: WRONG_TOKEN.to_string(),
            },
        )
        .await;
        let msg = read_response(&mut client, Duration::from_secs(5)).await;
        expect_error(msg, "auth_failed");
    }

    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buf))
        .await
        .expect("the connection must not hang after the third auth_failed identify")
        .expect("reading past a closed connection must not error, only report EOF");
    assert_eq!(
        n, 0,
        "the connection must be closed (EOF) after the third auth_failed identify"
    );
}

#[tokio::test]
async fn a_failed_identify_leaves_the_connections_origin_external() {
    let h = Harness::start("identify-failure-leaves-external", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: WRONG_TOKEN.to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");

    // With no playbook and no successful identify, this connection is still
    // External: the same window's own destructive operation must not have
    // been unlocked by the failed attempt.
    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let msg2 = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg2, "approval_required");
}

// =====================================================================
// register_approver peer-executable verification: the peer
// executable path captured at connection time must match `approver_exe`
// (canonicalized both sides) for registration to succeed. A rejection here
// does not count toward `identify`'s 3-strikes disconnect and leaves the
// connection's origin External.
// =====================================================================

#[tokio::test]
async fn register_approver_without_approver_exe_configured_is_auth_failed() {
    let config = DaemonConfig {
        approver_exe: None,
        ..base_config()
    };
    let h = Harness::start("approver-exe-unconfigured", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(&mut client, &ClientMessage::RegisterApprover).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");

    // Registration was refused, so the connection is still External: the
    // same connection's own destructive request must not have been
    // unlocked.
    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let msg2 = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg2, "approval_required");
}

#[tokio::test]
async fn register_approver_with_the_configured_executables_canonical_path_succeeds() {
    // `base_config` already sets `approver_exe` to this test binary's own
    // canonicalized path, matching what the peer-executable probe resolves
    // for a same-process connecting peer.
    let h = Harness::start("approver-exe-matching", base_config()).await;
    let mut client = h.connect().await;

    let pending = register_approver(&mut client).await;
    assert!(
        pending.is_empty(),
        "expected no pending requests on a fresh registration, got {pending:?}"
    );
}

#[tokio::test]
async fn register_approver_with_a_different_executable_configured_is_auth_failed() {
    let config = DaemonConfig {
        approver_exe: Some(env_probe_path()),
        ..base_config()
    };
    let h = Harness::start("approver-exe-mismatched", config).await;
    let mut client = h.connect().await;

    send(&mut client, &ClientMessage::RegisterApprover).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");
}

#[tokio::test]
async fn three_auth_failed_register_approver_attempts_do_not_close_the_connection() {
    let config = DaemonConfig {
        approver_exe: None,
        ..base_config()
    };
    let h = Harness::start("approver-exe-three-strikes", config).await;
    let mut client = h.connect().await;

    for _ in 0..3 {
        send(&mut client, &ClientMessage::RegisterApprover).await;
        let msg = read_response(&mut client, Duration::from_secs(5)).await;
        expect_error(msg, "auth_failed");
    }

    // Unlike `identify`'s 3-strikes rule, `register_approver`'s auth_failed
    // is never counted, so the connection must still be usable.
    send(&mut client, &ClientMessage::Ping).await;
    let pong = read_response(&mut client, Duration::from_secs(5)).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "expected pong after three register_approver auth_failed responses, got {pong:?}"
    );
}

#[tokio::test]
async fn identify_still_succeeds_after_a_register_approver_auth_failure() {
    let config = DaemonConfig {
        approver_exe: None,
        ..base_config()
    };
    let h = Harness::start("approver-exe-then-identify", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(&mut client, &ClientMessage::RegisterApprover).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "auth_failed");

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => {
            panic!("expected identified after a register_approver auth failure, got {other:?}")
        }
    }
}

// =====================================================================
// send_text gating: `send_text` goes through the same
// status/approval gate as `interrupt_window` / `close_window`.
// =====================================================================

#[tokio::test]
async fn send_text_to_an_unknown_window_is_status_unknown_even_for_the_windows_own_connection() {
    let h = Harness::start("send-text-unknown-self", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected identified, got {other:?}"),
    }

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn send_keys_to_an_unknown_window_is_status_unknown_even_for_the_windows_own_connection() {
    let h = Harness::start("send-keys-unknown-self", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected identified, got {other:?}"),
    }

    send(
        &mut client,
        &ClientMessage::SendKeys {
            window_id,
            keys: vec![ControlKey::Escape],
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn send_keys_from_its_own_window_writes_the_exact_bytes_immediately() {
    let dir = unique_temp_dir("send-keys-own-window");
    let h = Harness::start("send-keys-own-window", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let hex_echo = hex_echo_path();
    let window_id = h
        .open(
            vec![
                hex_echo
                    .to_str()
                    .expect("hex_echo path must be valid UTF-8")
                    .to_string(),
            ],
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::Idle,
        Duration::from_secs(10),
    )
    .await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    assert!(matches!(
        read_response(&mut client, Duration::from_secs(5)).await,
        DaemonMessage::Identified { .. }
    ));

    let keys = vec![ControlKey::Up, ControlKey::Down, ControlKey::Enter];
    send(
        &mut client,
        &ClientMessage::SendKeys {
            window_id: window_id.clone(),
            keys,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected keys_sent, got {other:?}"),
    }

    h.wait_for_scrollback(&window_id, b"1b5b411b5b420d", Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn send_keys_auto_approve_is_distinct_from_send_message() {
    let dir = unique_temp_dir("send-keys-auto-approve");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_keys"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-keys-auto-approve", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendKeys {
            window_id: window_id.clone(),
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected auto-approved keys_sent, got {other:?}"),
    }

    let dir = unique_temp_dir("send-keys-send-message-only");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-keys-send-message-only", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendKeys {
            window_id,
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    expect_error(
        read_response(&mut client, Duration::from_secs(5)).await,
        "approval_required",
    );
}

#[tokio::test]
async fn send_text_to_another_window_succeeds_when_the_playbook_lists_send_message() {
    let dir = unique_temp_dir("send-text-approved");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }
}

#[tokio::test]
async fn send_text_from_an_external_connection_without_a_policy_is_approval_required() {
    let dir = unique_temp_dir("send-text-denied");
    let h = Harness::start("send-text-denied", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "hello".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
}

#[tokio::test]
async fn send_text_with_a_playbook_configured_but_failed_to_load_is_approval_required() {
    let dir = unique_temp_dir("send-text-broken-playbook");
    let config = DaemonConfig {
        playbook: Some(std::path::PathBuf::from("nonexistent-playbook.toml")),
        approval_policy: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-broken-playbook", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "hello".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("failed to load"),
        "message must state that the playbook failed to load, got {message:?}"
    );
}

// =====================================================================
// Pre-validation precedes the gate, for every caller.
// =====================================================================

#[tokio::test]
async fn send_text_exceeding_the_size_limit_is_invalid_argument_even_with_an_approving_policy() {
    let dir = unique_temp_dir("send-text-oversized-approved");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-oversized-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    let oversized_text = "a".repeat(1024 * 1024 + 1);
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: oversized_text,
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_text_of_exactly_the_size_limit_is_not_refused_for_its_size() {
    let h = Harness::start("send-text-at-limit", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    // The trailing escape character makes the body check refuse it after the
    // size check, so nothing reaches the child.
    let text = format!("{}\x1b", "a".repeat(1024 * 1024 - 1));
    let expected = yagura_protocol::validate_send_text_body(&text)
        .expect_err("a body ending in an escape character is refused")
        .to_string();
    send(&mut client, &ClientMessage::SendText { window_id, text }).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    assert_eq!(expect_error(msg, "invalid_argument"), expected);
}

#[tokio::test]
async fn send_text_with_an_escape_character_and_no_terminator_is_invalid_argument() {
    // Takes `plan_raw_write`'s branch (no terminator), so this proves the
    // check runs before that branch is even chosen.
    let dir = unique_temp_dir("send-text-escape-raw");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-escape-raw", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "\x1b[2Jerased".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_text_with_an_escape_character_and_a_trailing_terminator_is_invalid_argument() {
    // A single trailing terminator would otherwise take the single-line
    // submit-plan branch (`single_line_body`); the escape character must
    // still be caught before that branch is chosen.
    let dir = unique_temp_dir("send-text-escape-single-line");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-escape-single-line", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "\x1b[2Jerased\n".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_text_with_an_escape_character_in_a_multiline_body_is_invalid_argument() {
    // Two terminators forces `plan_raw_write`'s multi-line branch; the
    // escape character between them must still be caught.
    let dir = unique_temp_dir("send-text-escape-multiline");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-escape-multiline", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "first\r\x1b[2Jsecond\r".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_text_to_an_exited_window_is_child_exited_even_with_an_approving_policy() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..base_config()
    };
    let h = Harness::start("send-text-exited-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: "hello".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn send_keys_to_an_exited_window_is_child_exited_even_with_an_approving_policy() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_keys"])),
        ..base_config()
    };
    let h = Harness::start("send-keys-exited-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendKeys {
            window_id,
            keys: vec![ControlKey::Enter],
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn send_keys_with_an_unknown_key_name_is_invalid_argument_without_reaching_the_approver() {
    let dir = unique_temp_dir("send-keys-unknown-key");
    let config = approval_config(&dir, 30);
    let h = Harness::start("send-keys-unknown-key", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    let body = format!(r#"{{"type":"send_keys","window_id":"{window_id}","keys":["tab"]}}"#);
    let frame = yagura_protocol::frame_encode(body.as_bytes());
    requester
        .write_all(&frame)
        .await
        .expect("write raw send_keys frame");

    expect_error(
        read_response(&mut requester, Duration::from_secs(5)).await,
        "invalid_argument",
    );

    send(&mut approver, &ClientMessage::Ping).await;
    assert!(
        matches!(
            read_response(&mut approver, Duration::from_secs(5)).await,
            DaemonMessage::Pong
        ),
        "an invalid key name must not enqueue an approval request"
    );
}

// =====================================================================
// Baseline for this build stage: the approver-registration and decision
// messages are wired to the connection but their handling is not implemented
// yet. These expectations flip once that handling lands.
// =====================================================================

#[tokio::test]
async fn wait_for_status_reports_reached_when_the_target_status_arrives() {
    let dir = unique_temp_dir("wait-reached");
    let h = Harness::start("wait-reached", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Idle,
            timeout_sec: 10,
        },
    )
    .await;

    // Stimulated after the wait is registered, so the transition (not the
    // initial value) is what completes the wait.
    h.write_input(&window_id, READY_MARKER).await;

    match read_response(&mut client, Duration::from_secs(15)).await {
        DaemonMessage::WaitResult {
            window_id: acked,
            outcome,
            status,
        } => {
            assert_eq!(acked, window_id);
            assert_eq!(outcome, "reached");
            assert_eq!(status, WindowStatus::Idle);
        }
        other => panic!("expected wait_result, got {other:?}"),
    }
}

#[tokio::test]
async fn wait_for_status_reports_timeout_with_the_status_observed_at_reply_time() {
    // A window without a detection definition never leaves Unknown, so the wait
    // can only end in a timeout -- and the reported status is the real one.
    let h = Harness::start("wait-timeout", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Idle,
            timeout_sec: 1,
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(15)).await {
        DaemonMessage::WaitResult {
            window_id: acked,
            outcome,
            status,
        } => {
            assert_eq!(acked, window_id);
            assert_eq!(outcome, "timeout");
            assert_eq!(status, WindowStatus::Unknown);
        }
        other => panic!("expected wait_result, got {other:?}"),
    }
}

#[tokio::test]
async fn wait_for_status_does_not_block_other_messages_on_the_same_connection() {
    // The waiter runs in its own task, so a Ping issued while a wait
    // is outstanding must be answered first.
    let h = Harness::start("wait-nonblocking", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id,
            target_status: WindowStatus::Idle,
            timeout_sec: 3,
        },
    )
    .await;
    send(&mut client, &ClientMessage::Ping).await;

    let first = read_frame(&mut client, Duration::from_secs(2)).await;
    assert!(
        matches!(first, DaemonMessage::Pong),
        "ping must be answered while the wait is still pending, got {first:?}"
    );

    let second = read_response(&mut client, Duration::from_secs(15)).await;
    assert!(
        matches!(second, DaemonMessage::WaitResult { .. }),
        "the wait must still complete afterwards, got {second:?}"
    );
}

#[tokio::test]
async fn wait_for_status_returns_child_exited_when_the_child_dies_while_waiting() {
    let h = Harness::start("wait-child-exit", base_config()).await;
    let mut client = h.connect().await;
    // Lives for roughly two seconds, i.e. exits while the wait is pending.
    let window_id = h.open(common::sleepy_command("3"), None).await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id,
            target_status: WindowStatus::Idle,
            timeout_sec: 30,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(20)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn wait_for_status_rejects_unknown_as_a_target() {
    // Waiting for Unknown would turn the fail-safe status into a completion
    // condition.
    let h = Harness::start("wait-target-unknown", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id,
            target_status: WindowStatus::Unknown,
            timeout_sec: 5,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn wait_for_status_rejects_a_zero_timeout() {
    let h = Harness::start("wait-zero-timeout", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id,
            target_status: WindowStatus::Idle,
            timeout_sec: 0,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn wait_for_status_rejects_a_timeout_above_the_configured_maximum() {
    let config = DaemonConfig {
        wait_timeout_max_secs: 5,
        ..base_config()
    };
    let h = Harness::start("wait-max-timeout", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id,
            target_status: WindowStatus::Idle,
            timeout_sec: 6,
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn wait_for_status_accepts_a_timeout_exactly_at_the_configured_maximum() {
    let dir = unique_temp_dir("wait-at-max-timeout");
    let config = DaemonConfig {
        wait_timeout_max_secs: 5,
        ..detection_config(&dir)
    };
    let h = Harness::start("wait-at-max-timeout", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Idle,
            timeout_sec: 5,
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(15)).await {
        DaemonMessage::WaitResult { outcome, .. } => assert_eq!(outcome, "reached"),
        other => panic!("expected wait_result, got {other:?}"),
    }
}

#[tokio::test]
async fn wait_for_status_rejects_more_concurrent_waiters_than_the_connection_allows() {
    let config = DaemonConfig {
        max_waiters_per_conn: 2,
        ..base_config()
    };
    let h = Harness::start("wait-limit", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    // The first two stay pending for their full timeout, so the only frame
    // that can come back promptly is the rejection of the third.
    for _ in 0..3 {
        send(
            &mut client,
            &ClientMessage::WaitForStatus {
                window_id: window_id.clone(),
                target_status: WindowStatus::Idle,
                timeout_sec: 20,
            },
        )
        .await;
    }

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "resource_exhausted");
}

async fn open_window_with_marker_lines(h: &Harness) -> String {
    let window_id = h.open_unknown_window().await;
    for i in 1..=3 {
        h.write_input(&window_id, format!("RO-LINE-{i}\r\n").as_bytes())
            .await;
    }
    h.wait_for_scrollback(&window_id, b"RO-LINE-3", Duration::from_secs(10))
        .await;
    window_id
}

#[tokio::test]
async fn read_output_returns_ansi_free_text_for_the_trailing_lines() {
    let h = Harness::start("read-tail", base_config()).await;
    let mut client = h.connect().await;
    let window_id = open_window_with_marker_lines(&h).await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id: window_id.clone(),
            lines: Some(20),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOutput {
            window_id: acked,
            text,
            truncated: _,
        } => {
            assert_eq!(acked, window_id);
            assert!(
                text.contains("RO-LINE-3"),
                "the trailing lines must be present, got {text:?}"
            );
            assert!(
                !text.contains('\u{1b}'),
                "escape sequences must be stripped, got {text:?}"
            );
        }
        other => panic!("expected window_output, got {other:?}"),
    }
}

#[tokio::test]
async fn read_output_reads_further_back_when_lines_is_omitted() {
    let h = Harness::start("read-default", base_config()).await;
    let mut client = h.connect().await;
    let window_id = open_window_with_marker_lines(&h).await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id,
            lines: None,
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOutput { text, .. } => assert!(
            text.contains("RO-LINE-1"),
            "the default line count must reach back past the last line, got {text:?}"
        ),
        other => panic!("expected window_output, got {other:?}"),
    }
}

#[tokio::test]
async fn read_output_honours_a_small_line_count() {
    let h = Harness::start("read-small", base_config()).await;
    let mut client = h.connect().await;
    let window_id = open_window_with_marker_lines(&h).await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id,
            lines: Some(1),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOutput { text, .. } => assert!(
            !text.contains("RO-LINE-1"),
            "a one-line read must not reach the first marker, got {text:?}"
        ),
        other => panic!("expected window_output, got {other:?}"),
    }
}

#[tokio::test]
async fn read_output_rejects_zero_lines() {
    let h = Harness::start("read-zero", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id,
            lines: Some(0),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn read_output_rejects_a_line_count_above_the_configured_maximum() {
    let config = DaemonConfig {
        read_output_max_lines: 10,
        ..base_config()
    };
    let h = Harness::start("read-max", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id,
            lines: Some(11),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn read_output_accepts_a_line_count_exactly_at_the_configured_maximum() {
    let config = DaemonConfig {
        read_output_max_lines: 10,
        ..base_config()
    };
    let h = Harness::start("read-at-max", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id,
            lines: Some(10),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    assert!(
        matches!(msg, DaemonMessage::WindowOutput { .. }),
        "expected window_output, got {msg:?}"
    );
}

#[tokio::test]
async fn read_output_returns_window_not_found_for_an_unknown_window_id() {
    let h = Harness::start("read-missing", base_config()).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::ReadOutput {
            window_id: "does-not-exist".to_string(),
            lines: Some(10),
        },
    )
    .await;

    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "window_not_found");
}

#[tokio::test]
async fn status_event_with_a_valid_token_is_accepted_and_updates_the_window() {
    let dir = unique_temp_dir("event-auth-ok");
    let h = Harness::start("event-auth-ok", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    let outcome = probe_status_event(
        &mut client,
        Some(&window_id),
        VALID_TOKEN,
        WindowStatus::Running,
    )
    .await;
    assert_eq!(outcome, EventOutcome::Accepted);

    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn status_event_with_a_wrong_token_is_rejected_and_leaves_the_status_untouched() {
    let dir = unique_temp_dir("event-auth-wrong");
    let h = Harness::start("event-auth-wrong", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    let outcome = probe_status_event(
        &mut client,
        Some(&window_id),
        WRONG_TOKEN,
        WindowStatus::Running,
    )
    .await;
    assert_eq!(outcome, EventOutcome::Rejected("auth_failed".to_string()));

    // Give a (hypothetical) unauthenticated write time to land before
    // asserting that nothing moved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &window_id).await,
        WindowStatus::Unknown,
        "a rejected event must not reach the detector"
    );
}

#[tokio::test]
async fn status_event_with_an_empty_token_is_rejected() {
    let dir = unique_temp_dir("event-auth-empty");
    let h = Harness::start("event-auth-empty", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    let outcome =
        probe_status_event(&mut client, Some(&window_id), "", WindowStatus::Running).await;
    assert_eq!(outcome, EventOutcome::Rejected("auth_failed".to_string()));
}

#[tokio::test]
async fn status_event_beyond_the_burst_allowance_is_rate_limited() {
    let dir = unique_temp_dir("event-rate");
    let config = DaemonConfig {
        status_event_burst: 2,
        status_event_per_sec: 1,
        ..detection_config(&dir)
    };
    let h = Harness::start("event-rate", config).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    for i in 0..2 {
        assert_eq!(
            probe_status_event(
                &mut client,
                Some(&window_id),
                VALID_TOKEN,
                WindowStatus::Running
            )
            .await,
            EventOutcome::Accepted,
            "burst allowance must cover event {i}"
        );
    }
    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&window_id),
            VALID_TOKEN,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Rejected("rate_limited".to_string()),
    );
}

#[tokio::test]
async fn failed_authentication_does_not_consume_the_rate_budget() {
    // Authentication runs ahead of the bucket, so an unauthenticated
    // flood cannot starve legitimate hooks.
    let dir = unique_temp_dir("event-auth-budget");
    let config = DaemonConfig {
        status_event_burst: 2,
        status_event_per_sec: 1,
        ..detection_config(&dir)
    };
    let h = Harness::start("event-auth-budget", config).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    for _ in 0..2 {
        assert_eq!(
            probe_status_event(
                &mut client,
                Some(&window_id),
                WRONG_TOKEN,
                WindowStatus::Running
            )
            .await,
            EventOutcome::Rejected("auth_failed".to_string()),
        );
    }

    for i in 0..2 {
        assert_eq!(
            probe_status_event(
                &mut client,
                Some(&window_id),
                VALID_TOKEN,
                WindowStatus::Running
            )
            .await,
            EventOutcome::Accepted,
            "the full burst must still be available for event {i}"
        );
    }
    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&window_id),
            VALID_TOKEN,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Rejected("rate_limited".to_string()),
    );
}

#[tokio::test]
async fn the_rate_bucket_is_shared_across_connections() {
    // A per-connection bucket would be bypassable by reconnecting, so the
    // bucket is daemon-wide.
    let dir = unique_temp_dir("event-global");
    let config = DaemonConfig {
        status_event_burst: 2,
        status_event_per_sec: 1,
        ..detection_config(&dir)
    };
    let h = Harness::start("event-global", config).await;
    let mut first = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    for _ in 0..2 {
        assert_eq!(
            probe_status_event(
                &mut first,
                Some(&window_id),
                VALID_TOKEN,
                WindowStatus::Running
            )
            .await,
            EventOutcome::Accepted,
        );
    }
    assert_eq!(
        probe_status_event(
            &mut first,
            Some(&window_id),
            VALID_TOKEN,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Rejected("rate_limited".to_string()),
    );

    let mut second = h.connect().await;
    assert_eq!(
        probe_status_event(
            &mut second,
            Some(&window_id),
            VALID_TOKEN,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Rejected("rate_limited".to_string()),
        "a fresh connection must not reset the daemon-wide bucket"
    );
}

/// Extracts the first run of exactly 32 lowercase hex characters from `text`,
/// which is how a window's real (generated) token is read back out of the
/// child's own output.
fn find_hex32(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let is_hex = |c: char| matches!(c, '0'..='9' | 'a'..='f');
    let mut start = 0usize;
    while start + 32 <= chars.len() {
        let run_len = chars[start..].iter().take_while(|c| is_hex(**c)).count();
        if run_len == 32 && (start == 0 || !is_hex(chars[start - 1])) {
            return Some(chars[start..start + 32].iter().collect());
        }
        start += 1;
    }
    None
}

#[tokio::test]
async fn one_windows_token_cannot_authenticate_an_event_about_another_window() {
    // With no override configured every window gets its own token, so a child
    // that can read its own `YAGURA_STATUS_TOKEN` still cannot speak for a
    // different window.
    let dir = unique_temp_dir("event-window-scope");
    let config = DaemonConfig {
        status_token_override: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("event-window-scope", config).await;
    let mut client = h.connect().await;

    // Window A prints its own token and stays alive afterwards. The argv is
    // fixed literals only (nothing is concatenated into it), and the token
    // travels through the environment, never the command line.
    let window_a = h
        .open(
            echo_env_and_wait_command(&["YAGURA_STATUS_TOKEN"]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    let token_a = wait_for_window_token(&h, &window_a, Duration::from_secs(10)).await;

    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_b = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&window_b),
            &token_a,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Rejected("auth_failed".to_string()),
        "window A's token must not authenticate an event aimed at window B"
    );
    // Give a (hypothetical) unauthenticated write time to land before
    // asserting that nothing moved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &window_b).await,
        WindowStatus::Unknown,
        "a rejected cross-window event must not reach window B's detector"
    );

    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&window_a),
            &token_a,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Accepted,
        "the same token must still authenticate an event about window A itself"
    );
    wait_until_status(
        &mut client,
        &window_a,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;
}

/// Polls `window_id`'s scrollback until the token its child printed appears.
async fn wait_for_window_token(h: &Harness, window_id: &str, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let text = String::from_utf8_lossy(&h.scrollback(window_id)).to_string();
        if let Some(token) = find_hex32(&text) {
            return token;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for window {window_id} to print its token; got {text:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// The two non-override tests below mint a token per window: a bystander window,
// opened first and never fed a marker so it stays `Unknown`, plus a window that
// prints both `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID`. Wait for the window_id
// in the scrollback before reading the token, or the token read can see a snapshot
// from before the window_id half of the echoed line landed.

#[tokio::test]
async fn a_windows_own_token_and_window_id_together_authenticate_its_status_event() {
    // What a spawned child actually receives through the environment is exactly
    // what `--emit-status` needs to authenticate as, and attribute to, that window.
    let dir = unique_temp_dir("hk-window-id-success-1");
    let config = DaemonConfig {
        status_token_override: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("hk-window-id-success-1", config).await;
    let mut client = h.connect().await;

    // Opened first, never fed a marker, so it stays Unknown for the duration of
    // this test (fixture convention shared with the cross-window test above).
    let bystander = h.open_unknown_window().await;

    // Fixed-literal argv only, nothing concatenated into it: the shell's built-in
    // echo is the only way to observe that an environment variable arrived.
    let window_b = h
        .open(
            echo_env_and_wait_command(&["YAGURA_STATUS_TOKEN", "YAGURA_WINDOW_ID"]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    h.wait_for_scrollback(&window_b, window_b.as_bytes(), Duration::from_secs(10))
        .await;
    let token_b = wait_for_window_token(&h, &window_b, Duration::from_secs(10)).await;

    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&window_b),
            &token_b,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Accepted,
        "the token and window_id a spawned child actually received must together \
         authenticate its own status_event"
    );
    wait_until_status(
        &mut client,
        &window_b,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &bystander).await,
        WindowStatus::Unknown,
        "the event must attribute to window_b alone, not to the window opened before it"
    );
}

#[tokio::test]
async fn a_status_event_without_a_window_id_is_rejected_even_with_a_windows_real_token() {
    // A real token names no window by itself, so an event that omits window_id
    // has no target to authenticate against.
    let dir = unique_temp_dir("hk-window-id-fail-closed");
    let config = DaemonConfig {
        status_token_override: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("hk-window-id-fail-closed", config).await;
    let mut client = h.connect().await;

    let window_b = h
        .open(
            echo_env_and_wait_command(&["YAGURA_STATUS_TOKEN", "YAGURA_WINDOW_ID"]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    h.wait_for_scrollback(&window_b, window_b.as_bytes(), Duration::from_secs(10))
        .await;
    let token_b = wait_for_window_token(&h, &window_b, Duration::from_secs(10)).await;
    let before = query_status(&mut client, &window_b).await;

    assert_eq!(
        probe_status_event(&mut client, None, &token_b, WindowStatus::Running).await,
        EventOutcome::Rejected("invalid_argument".to_string()),
        "an event that omits window_id must be refused, even with a real window's token"
    );
    // Give a (hypothetical) unauthenticated write time to land before asserting that
    // nothing moved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &window_b).await,
        before,
        "a rejected omitted-window_id event must not reach the token owner's detector"
    );
}

#[tokio::test]
async fn status_event_with_an_explicit_window_id_attributes_to_that_window_alone() {
    let dir = unique_temp_dir("hk-explicit-attrib");
    let h = Harness::start("hk-explicit-attrib", detection_config(&dir)).await;
    let mut client = h.connect().await;

    let bystander = h.open_unknown_window().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let target_window = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    assert_eq!(
        probe_status_event(
            &mut client,
            Some(&target_window),
            VALID_TOKEN,
            WindowStatus::Running
        )
        .await,
        EventOutcome::Accepted,
    );
    wait_until_status(
        &mut client,
        &target_window,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &bystander).await,
        WindowStatus::Unknown,
        "an explicit window_id must not also touch the window opened before it"
    );
}

#[tokio::test]
async fn a_status_event_without_a_window_id_is_rejected_and_touches_no_window() {
    // More than one window is open, so neither the first-opened nor the most
    // recently opened window may stand in for the missing target.
    let dir = unique_temp_dir("hk-omitted-multi");
    let h = Harness::start("hk-omitted-multi", detection_config(&dir)).await;
    let mut client = h.connect().await;

    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let first_window = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let later_window = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    assert_eq!(
        probe_status_event(&mut client, None, VALID_TOKEN, WindowStatus::Running).await,
        EventOutcome::Rejected("invalid_argument".to_string()),
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    for window in [&first_window, &later_window] {
        assert_eq!(
            query_status(&mut client, window).await,
            WindowStatus::Unknown,
            "an omitted-window_id event must not touch any window"
        );
    }
}

#[tokio::test]
async fn status_event_with_an_empty_window_id_returns_window_not_found() {
    // An empty window_id names a window, just not one that exists: it is not
    // read as an omitted one.
    let dir = unique_temp_dir("hk-empty-window-id");
    let h = Harness::start("hk-empty-window-id", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window = h.open_unknown_window().await;

    let outcome =
        probe_status_event(&mut client, Some(""), VALID_TOKEN, WindowStatus::Running).await;
    assert_eq!(
        outcome,
        EventOutcome::Rejected("window_not_found".to_string())
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        query_status(&mut client, &window).await,
        WindowStatus::Unknown,
        "an empty window_id must not reach any window"
    );
}

#[tokio::test]
async fn status_event_with_a_window_id_from_a_previous_boot_nonce_returns_window_not_found() {
    // A client still holding an id from a previous daemon boot must get
    // window_not_found, not silently match a same-numbered window from this boot.
    let dir = unique_temp_dir("hk-stale-nonce");
    let h = Harness::start("hk-stale-nonce", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let real_window_id = h.open_unknown_window().await;

    let (prefix, real_nonce) = real_window_id
        .rsplit_once('-')
        .expect("window_id must be formatted as `w{n}-{boot_nonce}` (window.rs)");
    let stale_nonce: String = real_nonce
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    let stale_window_id = format!("{prefix}-{stale_nonce}");
    assert_ne!(
        stale_window_id, real_window_id,
        "the constructed stale id must actually differ from the real one"
    );

    let outcome = probe_status_event(
        &mut client,
        Some(&stale_window_id),
        VALID_TOKEN,
        WindowStatus::Running,
    )
    .await;
    assert_eq!(
        outcome,
        EventOutcome::Rejected("window_not_found".to_string())
    );
}

#[tokio::test]
async fn a_failed_open_does_not_appear_in_list_windows() {
    // Calls the registry directly, bypassing the Harness::open helper: the
    // SpawnFailed mapping itself is asserted elsewhere and not repeated here.
    let dir = unique_temp_dir("hk-spawn-failed");
    let h = Harness::start("hk-spawn-failed", detection_config(&dir)).await;

    let missing_exe = r"C:\yagura-definitely-not-a-real-executable-xyz.exe".to_string();
    let failed = h
        .registry
        .open(OpenWindowRequest {
            command: vec![missing_exe],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        })
        .await;
    assert!(
        failed.is_err(),
        "opening a nonexistent executable must fail"
    );

    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    h.open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    assert_eq!(
        h.registry.list().len(),
        1,
        "a failed open must not leave an entry behind in list_windows"
    );
}

#[tokio::test]
async fn dropping_a_connection_with_a_pending_waiter_leaves_the_daemon_healthy() {
    let dir = unique_temp_dir("wait-disconnect");
    let h = Harness::start("wait-disconnect", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_id = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Idle,
            timeout_sec: 20,
        },
    )
    .await;
    drop(client);

    // The transition the aborted waiter was watching for happens *after* the
    // connection is gone: its task must be aborted, not left writing into a
    // dead connection.
    h.write_input(&window_id, READY_MARKER).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let mut follow_up = h.connect().await;
    send(&mut follow_up, &ClientMessage::Ping).await;
    let pong = read_response(&mut follow_up, Duration::from_secs(5)).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "the daemon must still serve new connections, got {pong:?}"
    );
    wait_until_status(
        &mut follow_up,
        &window_id,
        WindowStatus::Idle,
        Duration::from_secs(10),
    )
    .await;
}

// =====================================================================
// approval broker wiring: register_approver / approval_decision, and how a
// pending request gates close_window / interrupt_window / send_text once a
// human decision is required.
// =====================================================================

// `approval_config` / `register_approver` / `expect_approval_requested` /
// `decide` live in `common`; this file's own `detection_config(dir)` wrapper
// keeps `common::approval_config`'s `..detection_config(dir)` equivalent.

#[tokio::test]
async fn close_window_from_an_external_connection_is_pending_and_decision_true_closes_it() {
    let dir = unique_temp_dir("pending-close-approve");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-close-approve", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    let expected_name = h
        .registry
        .list()
        .into_iter()
        .find(|w| w.window_id == window_id)
        .map(|w| w.name)
        .expect("the opened window must be listed");

    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;

    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "close_window");
    assert_eq!(request.requester.kind, ApprovalRequesterKind::External);
    assert_eq!(request.requester.window_id, None);
    assert_eq!(request.requester.name, None);
    let target = request
        .target
        .as_ref()
        .expect("close_window must carry a target");
    assert_eq!(target.window_id, window_id);
    assert_eq!(target.name, expected_name);
    assert_eq!(target.status, WindowStatus::Idle);

    decide(&mut approver, &request.request_id, true).await;

    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&window_id, Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn close_window_pending_decision_false_returns_approval_denied() {
    let dir = unique_temp_dir("pending-close-deny");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-close-deny", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    decide(&mut approver, &request.request_id, false).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_pending_request_that_is_never_decided_times_out_and_notifies_the_approver() {
    let dir = unique_temp_dir("pending-timeout");
    let config = approval_config(&dir, 1);
    let h = Harness::start("pending-timeout", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_timeout");

    match read_response(&mut approver, Duration::from_secs(5)).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request.request_id);
            assert_eq!(reason, WithdrawReason::Timeout);
        }
        other => panic!("expected approval_withdrawn(timeout), got {other:?}"),
    }
}

#[tokio::test]
async fn close_window_with_no_approver_connected_is_refused_without_creating_a_pending_request() {
    let dir = unique_temp_dir("no-approver-no-pending");
    let config = approval_config(&dir, 30);
    let h = Harness::start("no-approver-no-pending", config).await;
    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;

    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );

    // A later-registering approver must see no pending request: nothing was
    // queued while no approver was reachable.
    let mut approver = h.connect().await;
    let pending = register_approver(&mut approver).await;
    assert!(
        pending.is_empty(),
        "expected no pending requests, got {pending:?}"
    );
}

#[tokio::test]
async fn close_window_with_a_broken_playbook_is_refused_even_with_an_approver_registered() {
    let dir = unique_temp_dir("broken-playbook-with-approver");
    let config = DaemonConfig {
        playbook: Some(std::path::PathBuf::from("nonexistent-playbook.toml")),
        ..approval_config(&dir, 30)
    };
    let h = Harness::start("broken-playbook-with-approver", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("failed to load"),
        "message must state that the playbook failed to load, got {message:?}"
    );

    // The approver must not have been asked: a broken playbook is a
    // configuration failure, not something a human decides.
    send(&mut approver, &ClientMessage::Ping).await;
    let pong = read_response(&mut approver, Duration::from_secs(5)).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "expected pong (no approval_requested was queued), got {pong:?}"
    );
}

#[tokio::test]
async fn pending_request_is_withdrawn_once_the_requester_disconnects() {
    let dir = unique_temp_dir("requester-disconnect-withdraws");
    let config = approval_config(&dir, 30);
    let h = Harness::start("requester-disconnect-withdraws", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    drop(requester);

    // The ceiling only guards against a hang; it stays under the 30 s approval
    // timeout so an expiry cannot stand in for the withdrawal.
    match read_response(&mut approver, Duration::from_secs(10)).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request.request_id);
            assert_eq!(reason, WithdrawReason::RequesterDisconnected);
        }
        other => panic!("expected approval_withdrawn(requester_disconnected), got {other:?}"),
    }

    send(
        &mut approver,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id.clone(),
            approved: true,
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "approval_not_pending");
}

#[tokio::test]
async fn pending_request_survives_the_approvers_disconnect_and_reappears_on_reregistration() {
    let dir = unique_temp_dir("approver-disconnect-survive");
    let config = approval_config(&dir, 30);
    let h = Harness::start("approver-disconnect-survive", config).await;
    let mut approver1 = h.connect().await;
    register_approver(&mut approver1).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver1).await;

    drop(approver1);

    let mut approver2 = h.connect().await;
    let pending = register_approver(&mut approver2).await;
    assert!(
        pending.iter().any(|r| r.request_id == request.request_id),
        "a re-registering approver must still see the pending request, got {pending:?}"
    );

    decide(&mut approver2, &request.request_id, true).await;

    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

#[tokio::test]
async fn registering_the_same_connection_twice_is_idempotent_and_returns_the_same_pending_set() {
    let dir = unique_temp_dir("register-idempotent");
    let config = approval_config(&dir, 30);
    let h = Harness::start("register-idempotent", config).await;
    let mut approver = h.connect().await;
    let empty = register_approver(&mut approver).await;
    assert!(empty.is_empty());

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let pending_first = register_approver(&mut approver).await;
    let pending_second = register_approver(&mut approver).await;
    assert_eq!(
        pending_first
            .iter()
            .map(|r| &r.request_id)
            .collect::<Vec<_>>(),
        pending_second
            .iter()
            .map(|r| &r.request_id)
            .collect::<Vec<_>>(),
        "re-registering the same connection must return the same pending set"
    );
    assert!(
        pending_first
            .iter()
            .any(|r| r.request_id == request.request_id)
    );
}

// The permit `Permit<T>` (crates/yagura-daemon/src/ipc/permit.rs) proves a
// gate ran at least once before an executor's write, by construction: no
// path outside that module can build one. It proves nothing about an upper
// bound -- a handler that called `gate_destructive` twice would still
// typecheck, and if both calls landed on `GateOutcome::Pending`, it would
// queue two approval tickets for what a caller only asked for once, showing
// the approver two prompts for a single request. The four tests below pin
// that upper bound in the one place it is actually observable: the pending
// set an approver connection is handed back, which counts tickets rather
// than call sites.

#[tokio::test]
async fn a_single_send_text_request_creates_exactly_one_pending_approval() {
    let dir = unique_temp_dir("pending-count-send-text");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-count-send-text", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let pending = register_approver(&mut approver).await;
    assert_eq!(
        pending.len(),
        1,
        "one send_text request must create exactly one pending approval, not a second one \
         from a gate that ran more than once; got {pending:?}"
    );
    assert_eq!(pending[0].request_id, request.request_id);

    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_single_interrupt_window_request_creates_exactly_one_pending_approval() {
    let dir = unique_temp_dir("pending-count-interrupt");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-count-interrupt", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let pending = register_approver(&mut approver).await;
    assert_eq!(
        pending.len(),
        1,
        "one interrupt_window request must create exactly one pending approval, not a second \
         one from a gate that ran more than once; got {pending:?}"
    );
    assert_eq!(pending[0].request_id, request.request_id);

    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_single_close_window_request_creates_exactly_one_pending_approval() {
    let dir = unique_temp_dir("pending-count-close");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-count-close", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let pending = register_approver(&mut approver).await;
    assert_eq!(
        pending.len(),
        1,
        "one close_window request must create exactly one pending approval, not a second one \
         from a gate that ran more than once; got {pending:?}"
    );
    assert_eq!(pending[0].request_id, request.request_id);

    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_single_send_handoff_request_creates_exactly_one_pending_approval() {
    let dir = unique_temp_dir("pending-count-handoff");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-count-handoff", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let pending = register_approver(&mut approver).await;
    assert_eq!(
        pending.len(),
        1,
        "one send_handoff request must create exactly one pending approval, not a second one \
         from a gate that ran more than once; got {pending:?}"
    );
    assert_eq!(pending[0].request_id, request.request_id);

    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn registering_as_approver_from_a_window_identified_connection_is_invalid_state() {
    let dir = unique_temp_dir("register-after-identify");
    let config = approval_config(&dir, 30);
    let h = Harness::start("register-after-identify", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id,
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { .. } => {}
        other => panic!("expected identified, got {other:?}"),
    }

    send(&mut client, &ClientMessage::RegisterApprover).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_state");
}

#[tokio::test]
async fn identify_from_an_approver_registered_connection_is_invalid_state() {
    let dir = unique_temp_dir("identify-after-register");
    let config = approval_config(&dir, 30);
    let h = Harness::start("identify-after-register", config).await;
    let mut client = h.connect().await;
    register_approver(&mut client).await;

    let window_id = h.open_idle_window(&mut client).await;
    send(
        &mut client,
        &ClientMessage::Identify {
            window_id,
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_state");
}

#[tokio::test]
async fn approval_decision_from_an_unregistered_connection_is_approval_not_pending() {
    let dir = unique_temp_dir("decision-unregistered");
    let config = approval_config(&dir, 30);
    let h = Harness::start("decision-unregistered", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let mut stranger = h.connect().await;
    send(
        &mut stranger,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id,
            approved: true,
        },
    )
    .await;
    let msg = read_response(&mut stranger, Duration::from_secs(5)).await;
    expect_error(msg, "approval_not_pending");
}

#[tokio::test]
async fn approval_decision_for_an_already_settled_request_is_approval_not_pending_the_second_time()
{
    let dir = unique_temp_dir("decision-already-settled");
    let config = approval_config(&dir, 30);
    let h = Harness::start("decision-already-settled", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    decide(&mut approver, &request.request_id, true).await;
    let _ = read_response(&mut requester, Duration::from_secs(5)).await;

    send(
        &mut approver,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id,
            approved: true,
        },
    )
    .await;
    let msg = read_response(&mut approver, Duration::from_secs(5)).await;
    expect_error(msg, "approval_not_pending");
}

#[tokio::test]
async fn deciding_from_one_approver_notifies_the_other_of_the_resolution() {
    let dir = unique_temp_dir("two-approvers-resolve");
    let config = approval_config(&dir, 30);
    let h = Harness::start("two-approvers-resolve", config).await;
    let mut approver1 = h.connect().await;
    register_approver(&mut approver1).await;
    let mut approver2 = h.connect().await;
    register_approver(&mut approver2).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    let request1 = expect_approval_requested(&mut approver1).await;
    let request2 = expect_approval_requested(&mut approver2).await;
    assert_eq!(request1.request_id, request2.request_id);

    decide(&mut approver1, &request1.request_id, true).await;

    match read_response(&mut approver2, Duration::from_secs(5)).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request1.request_id);
            assert_eq!(reason, WithdrawReason::Resolved);
        }
        other => panic!("expected approval_withdrawn(resolved), got {other:?}"),
    }

    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

#[tokio::test]
async fn exceeding_the_per_connection_pending_cap_is_resource_exhausted() {
    let dir = unique_temp_dir("pending-cap-per-conn");
    let config = DaemonConfig {
        max_pending_approvals_per_conn: 1,
        ..approval_config(&dir, 30)
    };
    let h = Harness::start("pending-cap-per-conn", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_a = h.open_idle_window(&mut requester).await;
    let window_b = h.open_idle_window(&mut requester).await;

    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_a,
            force: None,
        },
    )
    .await;
    let _first = expect_approval_requested(&mut approver).await;

    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_b,
            force: None,
        },
    )
    .await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "resource_exhausted");
}

#[tokio::test]
async fn exceeding_the_daemon_wide_pending_cap_is_resource_exhausted_even_from_a_different_connection()
 {
    let dir = unique_temp_dir("pending-cap-daemon-wide");
    let config = DaemonConfig {
        max_pending_approvals: 1,
        max_pending_approvals_per_conn: 10,
        ..approval_config(&dir, 30)
    };
    let h = Harness::start("pending-cap-daemon-wide", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester1 = h.connect().await;
    let window_a = h.open_idle_window(&mut requester1).await;
    send(
        &mut requester1,
        &ClientMessage::CloseWindow {
            window_id: window_a,
            force: None,
        },
    )
    .await;
    let _first = expect_approval_requested(&mut approver).await;

    let mut requester2 = h.connect().await;
    let window_b = h.open_idle_window(&mut requester2).await;
    send(
        &mut requester2,
        &ClientMessage::CloseWindow {
            window_id: window_b,
            force: None,
        },
    )
    .await;
    let msg = read_response(&mut requester2, Duration::from_secs(5)).await;
    expect_error(msg, "resource_exhausted");
}

#[tokio::test]
async fn ping_still_works_on_a_connection_with_a_pending_approval_request() {
    let dir = unique_temp_dir("ping-while-pending");
    let config = approval_config(&dir, 30);
    let h = Harness::start("ping-while-pending", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id,
            force: None,
        },
    )
    .await;
    let _request = expect_approval_requested(&mut approver).await;

    send(&mut requester, &ClientMessage::Ping).await;
    let pong = read_response(&mut requester, Duration::from_secs(5)).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "expected pong on a connection with a request still pending, got {pong:?}"
    );
}

#[tokio::test]
async fn pending_approval_requests_do_not_consume_the_connections_wait_for_status_budget() {
    let dir = unique_temp_dir("waiters-independent-of-approvals");
    let config = DaemonConfig {
        max_waiters_per_conn: 4,
        ..approval_config(&dir, 30)
    };
    let h = Harness::start("waiters-independent-of-approvals", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut client = h.connect().await;
    let mut windows = Vec::new();
    for _ in 0..4 {
        // close_window is exempt from the Unknown gate, so an unstimulated
        // window works as a pending-approval target here.
        let window_id = h.open_unknown_window().await;
        send(
            &mut client,
            &ClientMessage::CloseWindow {
                window_id: window_id.clone(),
                force: None,
            },
        )
        .await;
        let _request = expect_approval_requested(&mut approver).await;
        windows.push(window_id);
    }

    // Five wait_for_status calls on the same connection, whose targets never
    // arrive: if the four pending approvals above left the waiter budget
    // untouched, exactly one (the fifth) is rejected immediately and the
    // other four produce no frame within the short window below.
    for window_id in &windows {
        send(
            &mut client,
            &ClientMessage::WaitForStatus {
                window_id: window_id.clone(),
                target_status: WindowStatus::Idle,
                timeout_sec: 20,
            },
        )
        .await;
    }
    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: windows[0].clone(),
            target_status: WindowStatus::Idle,
            timeout_sec: 20,
        },
    )
    .await;

    // The rejection is an event, so the first read waits for it up to the
    // ceiling; only the reads after it watch a short window for a second one.
    expect_error(
        read_response(&mut client, Duration::from_secs(10)).await,
        "resource_exhausted",
    );
    let mut immediate_errors = 1;
    for _ in 0..4 {
        match tokio::time::timeout(
            Duration::from_millis(500),
            read_response(&mut client, Duration::from_secs(20)),
        )
        .await
        {
            Ok(msg) => {
                expect_error(msg, "resource_exhausted");
                immediate_errors += 1;
            }
            Err(_) => break,
        }
    }
    assert_eq!(
        immediate_errors, 1,
        "exactly one of the five wait_for_status calls must be rejected immediately \
         (max_waiters_per_conn = 4); the rest must be accepted because the four pending \
         approvals above must not consume this connection's waiter budget"
    );
}

#[tokio::test]
async fn an_approvers_own_close_window_and_interrupt_window_bypass_the_gate_without_a_playbook() {
    let dir = unique_temp_dir("approver-self-bypass");
    let config = approval_config(&dir, 30);
    let h = Harness::start("approver-self-bypass", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let window_a = h.open_idle_window(&mut approver).await;
    send(
        &mut approver,
        &ClientMessage::InterruptWindow {
            window_id: window_a.clone(),
        },
    )
    .await;
    match read_response(&mut approver, Duration::from_secs(5)).await {
        DaemonMessage::WindowInterrupted { window_id } => assert_eq!(window_id, window_a),
        other => panic!("expected window_interrupted, got {other:?}"),
    }

    let window_b = h.open_idle_window(&mut approver).await;
    send(
        &mut approver,
        &ClientMessage::CloseWindow {
            window_id: window_b.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut approver, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { window_id } => assert_eq!(window_id, window_b),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

#[tokio::test]
async fn send_text_from_an_identified_window_to_another_window_is_pending_with_full_text_and_window_identity()
 {
    let dir = unique_temp_dir("window-identity-pending");
    let config = approval_config(&dir, 30);
    let h = Harness::start("window-identity-pending", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut source = h.connect().await;
    let window_a = h.open_idle_window(&mut source).await;
    let expected_name_a = h
        .registry
        .list()
        .into_iter()
        .find(|w| w.window_id == window_a)
        .map(|w| w.name)
        .expect("window A must be listed");
    send(
        &mut source,
        &ClientMessage::Identify {
            window_id: window_a.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(&mut source, Duration::from_secs(5)).await {
        DaemonMessage::Identified { .. } => {}
        other => panic!("expected identified, got {other:?}"),
    }

    let window_b = h.open_idle_window(&mut source).await;
    let body = "hello from window A, please approve".to_string();
    send(
        &mut source,
        &ClientMessage::SendText {
            window_id: window_b.clone(),
            text: body.clone(),
        },
    )
    .await;

    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_message");
    assert_eq!(request.text.as_deref(), Some(body.as_str()));
    assert_eq!(request.requester.kind, ApprovalRequesterKind::Window);
    assert_eq!(
        request.requester.window_id.as_deref(),
        Some(window_a.as_str())
    );
    assert_eq!(request.requester.name, Some(expected_name_a));
    assert_eq!(
        request
            .target
            .as_ref()
            .map(|target| target.window_id.as_str()),
        Some(window_b.as_str())
    );

    decide(&mut approver, &request.request_id, true).await;
    match read_response(&mut source, Duration::from_secs(5)).await {
        DaemonMessage::TextSent { window_id } => assert_eq!(window_id, window_b),
        other => panic!("expected text_sent, got {other:?}"),
    }
}

#[tokio::test]
async fn send_keys_from_an_identified_window_to_another_window_is_pending_with_key_names_and_window_identity()
 {
    let dir = unique_temp_dir("window-identity-keys-pending");
    let config = approval_config(&dir, 30);
    let h = Harness::start("window-identity-keys-pending", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut source = h.connect().await;
    let window_a = h.open_idle_window(&mut source).await;
    let expected_name_a = h
        .registry
        .list()
        .into_iter()
        .find(|w| w.window_id == window_a)
        .map(|w| w.name)
        .expect("window A must be listed");
    send(
        &mut source,
        &ClientMessage::Identify {
            window_id: window_a.clone(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    assert!(matches!(
        read_response(&mut source, Duration::from_secs(5)).await,
        DaemonMessage::Identified { .. }
    ));

    let window_b = h.open_idle_window(&mut source).await;
    send(
        &mut source,
        &ClientMessage::SendKeys {
            window_id: window_b.clone(),
            keys: vec![ControlKey::CtrlU, ControlKey::Escape],
        },
    )
    .await;

    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_keys");
    assert_eq!(request.text.as_deref(), Some("ctrl_u, escape"));
    assert_eq!(request.requester.kind, ApprovalRequesterKind::Window);
    assert_eq!(
        request.requester.window_id.as_deref(),
        Some(window_a.as_str())
    );
    assert_eq!(request.requester.name, Some(expected_name_a));
    assert_eq!(
        request
            .target
            .as_ref()
            .map(|target| target.window_id.as_str()),
        Some(window_b.as_str())
    );

    decide(&mut approver, &request.request_id, true).await;
    match read_response(&mut source, Duration::from_secs(5)).await {
        DaemonMessage::KeysSent { window_id } => assert_eq!(window_id, window_b),
        other => panic!("expected keys_sent, got {other:?}"),
    }
}

#[tokio::test]
async fn send_text_requiring_approval_over_the_body_limit_is_rejected_without_reaching_the_approver()
 {
    let dir = unique_temp_dir("approval-text-limit");
    let config = approval_config(&dir, 30);
    let h = Harness::start("approval-text-limit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    // Over the approval-body cap (2048 bytes) but well under the 1 MiB
    // pre-validation limit, so this is refused by the approval-body check,
    // not the pre-validation size check.
    let oversized = "a".repeat(2049);
    send(
        &mut requester,
        &ClientMessage::SendText {
            window_id,
            text: oversized,
        },
    )
    .await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");

    // The approver's connection must stay idle: no approval_requested was
    // queued for a request rejected before it reached the ledger.
    send(&mut approver, &ClientMessage::Ping).await;
    let pong = read_response(&mut approver, Duration::from_secs(5)).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "expected pong (no approval_requested was queued), got {pong:?}"
    );
}

#[tokio::test]
async fn send_text_requiring_approval_at_exactly_the_body_limit_reaches_the_approver() {
    let dir = unique_temp_dir("approval-text-at-limit");
    let config = approval_config(&dir, 30);
    let h = Harness::start("approval-text-at-limit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    let at_limit = "a".repeat(yagura_protocol::APPROVAL_TEXT_MAX_BYTES);
    send(
        &mut requester,
        &ClientMessage::SendText {
            window_id,
            text: at_limit.clone(),
        },
    )
    .await;

    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(
        request.text.as_deref(),
        Some(at_limit.as_str()),
        "a body of exactly the cap must reach the approver whole"
    );
    decide(&mut approver, &request.request_id, false).await;
    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
}

#[tokio::test]
async fn a_pending_close_window_still_succeeds_after_the_target_has_already_exited() {
    let dir = unique_temp_dir("pending-close-after-exit");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-close-after-exit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open(exit_with_code_command("0"), None).await;
    wait_until_status(
        &mut requester,
        &window_id,
        WindowStatus::Exited,
        Duration::from_secs(10),
    )
    .await;

    send(
        &mut requester,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, true).await;

    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!(
            "expected window_closed even though the child had already exited (idempotent), \
             got {other:?}"
        ),
    }
}

#[tokio::test]
async fn a_pending_interrupt_window_returns_child_exited_if_the_target_exits_before_the_decision() {
    let dir = unique_temp_dir("pending-interrupt-exit");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-interrupt-exit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;

    send(
        &mut requester,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    h.registry
        .get(&window_id)
        .expect("window must exist in registry")
        .session()
        .terminate();
    wait_until_status(
        &mut requester,
        &window_id,
        WindowStatus::Exited,
        Duration::from_secs(10),
    )
    .await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn a_pending_interrupt_window_is_refused_status_unknown_if_the_target_falls_back_to_unknown_before_the_decision()
 {
    let dir = unique_temp_dir("pending-unknown-recheck");
    let config = approval_config(&dir, 30);
    let h = Harness::start("pending-unknown-recheck", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut probe = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut probe, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window without a detection definition starts Unknown"
    );

    // Force a short-lived non-Unknown status by opening a turn with a
    // report, and submit the interrupt while it is still in effect.
    send(
        &mut probe,
        &ClientMessage::StatusEvent {
            source: "approval-recheck-probe".to_string(),
            status: WindowStatus::Running,
            window_id: Some(window_id.clone()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(5),
    )
    .await;

    let mut requester = h.connect().await;
    send(
        &mut requester,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    // Non-vacuous by construction: the pending request's own target snapshot
    // proves the submit happened while the window was Running, not Unknown.
    assert_eq!(
        request.target.as_ref().map(|target| target.status),
        Some(WindowStatus::Running),
        "the request must have been submitted while the window was Running, not Unknown, or this \
         test would prove nothing about the re-check at decision time"
    );

    // Let the open turn go quiet, so the window falls back to Unknown before
    // the decision is made.
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Unknown,
        Duration::from_secs(15),
    )
    .await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

// =====================================================================
// send_handoff: window_id-addressed delivery.
//
// Tests through `send_handoff_to_a_window_id_is_resource_exhausted_after_...`
// (below the two-window approval group) drive the submit-plan / delivery-
// confirmation contract directly, using the `submit_child` fixture rather
// than `open_idle_window`'s `echo_child`.
// =====================================================================

#[tokio::test]
async fn send_handoff_to_ones_own_window_needs_no_playbook_and_delivers_the_envelope_once() {
    let dir = unique_temp_dir("handoff-self-window");
    let h = Harness::start("handoff-self-window", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window_with_composer(&mut client).await;
    identify(&mut client, &window_id).await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    // No playbook is configured at all: only the self-window relief in the
    // gate (which also covers send_handoff) can let
    // this through.
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            delivery,
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened, "a window_id target must never report opened: true");
            // common::control_definition sets delivery_confirm_ms = 0: no reaction is
            // ever looked for.
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json =
        wait_for_accepted_handoff_envelope_json(&h, &window_id, Duration::from_secs(10)).await;
    assert_eq!(
        json["from"]["window_id"], window_id,
        "the envelope's from.window_id must be the requester's own id: {json}"
    );
    assert_eq!(
        json["reply"]["window_id"], window_id,
        "the envelope's reply.window_id must be the requester's own id: {json}"
    );
    assert_eq!(json["reference"], "src/foo.rs");
    assert_eq!(json["note"], "please look at the error handling");
}

#[tokio::test]
async fn send_handoff_from_an_external_connection_to_another_window_without_a_policy_is_approval_required()
 {
    let dir = unique_temp_dir("handoff-denied");
    let h = Harness::start("handoff-denied", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("no approver is connected"),
        "message must state that no approver is connected, got {message:?}"
    );
}

#[tokio::test]
async fn send_handoff_between_two_windows_succeeds_when_the_playbook_lists_send_handoff() {
    let dir = unique_temp_dir("handoff-approved");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window_with_composer(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            delivery,
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened);
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json =
        wait_for_accepted_handoff_envelope_json(&h, &window_id, Duration::from_secs(10)).await;
    assert_eq!(
        json["from"], "external",
        "an unidentified connection's envelope must carry \"from\":\"external\": {json}"
    );
    assert!(
        json.get("reply").is_none(),
        "an external requester carries no reply address: {json}"
    );
}

#[tokio::test]
async fn send_handoff_to_an_unknown_window_is_status_unknown_even_for_the_windows_own_connection() {
    let h = Harness::start("handoff-unknown-self", base_config()).await;
    let mut client = h.connect().await;
    let window_id = h.open_unknown_window().await;
    identify(&mut client, &window_id).await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn send_handoff_to_an_exited_window_is_child_exited_even_with_an_approving_policy() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..base_config()
    };
    let h = Harness::start("handoff-exited-approved", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_exited_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "child_exited");
}

#[tokio::test]
async fn send_handoff_with_a_missing_window_id_and_a_multiline_note_is_invalid_argument() {
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..base_config()
    };
    let h = Harness::start("handoff-missing-window-multiline-note", config).await;
    let mut client = h.connect().await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some("does-not-exist-anywhere".to_string()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "line one\nline two".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_handoff_with_a_reference_over_the_size_limit_is_invalid_argument_even_with_an_approving_policy()
 {
    let dir = unique_temp_dir("handoff-oversized-reference");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-oversized-reference", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    let oversized_reference = "a".repeat(HANDOFF_REFERENCE_MAX_BYTES + 1);
    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: oversized_reference,
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_handoff_with_a_note_over_the_size_limit_is_invalid_argument_even_with_an_approving_policy()
 {
    let dir = unique_temp_dir("handoff-oversized-note");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-oversized-note", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    let oversized_note = "a".repeat(HANDOFF_NOTE_MAX_BYTES + 1);
    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: oversized_note,
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");
}

#[tokio::test]
async fn send_handoff_denied_by_the_approver_leaves_the_targets_pty_unchanged() {
    let dir = unique_temp_dir("handoff-denied-by-approver");
    let config = approval_config(&dir, 30);
    let h = Harness::start("handoff-denied-by-approver", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = h.open_idle_window(&mut requester).await;
    let baseline_scrollback = h.scrollback(&window_id);

    send(
        &mut requester,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_handoff");

    decide(&mut approver, &request.request_id, false).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "approval_denied");
    assert_eq!(
        h.scrollback(&window_id),
        baseline_scrollback,
        "a denied handoff must never reach the target's PTY"
    );
}

#[tokio::test]
async fn a_pending_send_handoff_is_refused_status_unknown_if_the_target_falls_back_to_unknown_before_the_decision()
 {
    let dir = unique_temp_dir("handoff-pending-unknown-recheck");
    let config = approval_config(&dir, 30);
    let h = Harness::start("handoff-pending-unknown-recheck", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut probe = h.connect().await;
    let window_id = h.open_unknown_window().await;
    assert_eq!(
        query_status(&mut probe, &window_id).await,
        WindowStatus::Unknown,
        "fixture precondition: a window without a detection definition starts Unknown"
    );

    // Force a short-lived non-Unknown status by opening a turn with a
    // report, and submit the handoff while it is still in effect.
    send(
        &mut probe,
        &ClientMessage::StatusEvent {
            source: "handoff-recheck-probe".to_string(),
            status: WindowStatus::Running,
            window_id: Some(window_id.clone()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(5),
    )
    .await;

    let mut requester = h.connect().await;
    send(
        &mut requester,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    // Non-vacuous by construction: the pending request's own target snapshot
    // proves the submit happened while the window was Running, not Unknown.
    assert_eq!(
        request.target.as_ref().map(|target| target.status),
        Some(WindowStatus::Running),
        "the request must have been submitted while the window was Running, not Unknown, or this \
         test would prove nothing about the re-check at decision time"
    );

    // Let the open turn go quiet, so the window falls back to Unknown before
    // the decision is made.
    wait_until_status(
        &mut probe,
        &window_id,
        WindowStatus::Unknown,
        Duration::from_secs(15),
    )
    .await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester, Duration::from_secs(5)).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn send_handoff_between_two_windows_approved_by_a_registered_approver_delivers_the_envelope()
{
    let dir = unique_temp_dir("handoff-approver-delivers");
    let config = approval_config(&dir, 30);
    let h = Harness::start("handoff-approver-delivers", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut source = h.connect().await;
    let window_a = h.open_idle_window(&mut source).await;
    identify(&mut source, &window_a).await;
    let window_b = h.open_idle_window_with_composer(&mut source).await;

    send(
        &mut source,
        &ClientMessage::SendHandoff {
            window_id: Some(window_b.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "please look at the error handling".to_string(),
        },
    )
    .await;

    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_handoff");
    let text = request
        .text
        .as_deref()
        .expect("a pending send_handoff request must carry the envelope as its text");
    assert!(
        text.starts_with("[yagura handoff] "),
        "the pending text must be the full handoff envelope, got {text:?}"
    );
    assert!(
        text.len() <= 2048,
        "the pending text must fit the approval-body cap (2048 bytes), got {} bytes",
        text.len()
    );

    decide(&mut approver, &request.request_id, true).await;

    match read_response(&mut source, Duration::from_secs(5)).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            delivery,
        } => {
            assert_eq!(acked, window_b);
            assert!(!opened, "a window_id target must never report opened: true");
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json =
        wait_for_accepted_handoff_envelope_json(&h, &window_b, Duration::from_secs(10)).await;
    assert_eq!(
        json["from"]["window_id"], window_a,
        "the envelope's from.window_id must be the requesting window: {json}"
    );
    assert_eq!(
        json["reply"]["window_id"], window_a,
        "the envelope's reply.window_id must be the requesting window: {json}"
    );
    assert_eq!(json["reference"], "src/foo.rs");
    assert_eq!(json["note"], "please look at the error handling");
}

// =====================================================================
// send_handoff: submit-plan / delivery-confirmation contract, window_id-
// addressed. Every case below targets a `submit_child` window rather than
// `open_idle_window`'s `echo_child`, and reads its own `[input]` table from a
// definition built by `common::write_submit_definition`.
// =====================================================================

/// A note long enough that the composed envelope lands in the 300-byte
/// class, without approaching the 2048-byte envelope cap.
fn medium_handoff_note() -> String {
    "please look at the error handling here and consider the retry policy too: ".to_string()
        + &"x".repeat(180)
}

#[tokio::test]
async fn send_handoff_to_a_window_id_confirms_delivery_when_the_submit_key_lands_after_the_body() {
    // (a): the defect this task exists to fix -- writing body and submit key
    // as one call lets the fixture's burst rule (a) swallow the CR. A
    // 900ms submit_delay clears that rule by a wide margin.
    let dir = unique_temp_dir("handoff-a-defect-a");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-a-defect-a", config).await;
    let mut client = h.connect().await;
    // This body is long enough that composer-draw mode wraps it across several
    // painted lines; a wider bottom_line_count keeps the startup marker line
    // inside the matched region even so.
    let window_id = open_submit_child_window_with_composer_and_bottom_line_count(
        &h,
        &mut client,
        &dir,
        "handoff-a",
        "handoff-a-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "",
            delivery_confirm_ms: 800,
            resend_max: 0,
        },
        0,
        20,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: medium_handoff_note(),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            delivery,
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened);
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "the body must be written exactly once"
    );
    h.wait_for_scrollback_occurrences(
        &window_id,
        "SUBMITTED [yagura handoff] ",
        1,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        count_marker(&h, &window_id, "SUBMITTED [yagura handoff] ") >= 1,
        "the submit key must have been taken as a submit, not folded into the body \
         as a line break"
    );
}

#[tokio::test]
async fn send_handoff_to_a_window_id_with_delivery_confirmation_disabled_is_taken_quickly() {
    // (b): delivery_confirm_ms = 0 disables the wait entirely.
    let dir = unique_temp_dir("handoff-b-confirm-disabled");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-b-confirm-disabled", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-b",
        "handoff-b-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
        0,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(
                wire(delivery),
                "taken",
                "delivery_confirm_ms = 0 skips the confirmation wait: the typed envelope is taken"
            );
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

#[tokio::test]
async fn send_handoff_to_a_window_id_that_never_reacts_is_taken_and_the_window_survives() {
    // (c): the paste guard takes every submit key as a line break -- the
    // fixture never reacts, and resend_max = 0 means no retry is attempted
    // either.
    let dir = unique_temp_dir("handoff-c-never-reacts");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-c-never-reacts", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-c",
        "handoff-c-marker",
        0,
        u64::MAX,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "",
            delivery_confirm_ms: 800,
            resend_max: 0,
        },
        0,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            delivery,
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened);
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent (a submitted envelope is taken), got {other:?}"),
    }

    let windows = call_list_windows(&mut client).await;
    assert!(
        windows.iter().any(|w| w.window_id == window_id),
        "a delivery with no reaction must never close the target window: {windows:?}"
    );
}

#[tokio::test]
async fn send_handoff_definition_requiring_resend_without_a_clear_sequence_loads_and_detects() {
    // (c'): resend_max > 0 with an empty clear sequence is a valid combination,
    // because the resend action only writes the submit key again, so the
    // definition loads and the window reaches its own detection rules instead
    // of staying Unknown.
    let dir = unique_temp_dir("handoff-c-prime-resend-no-clear");
    let config = detection_config(&dir);
    let h = Harness::start("handoff-c-prime-resend-no-clear", config).await;
    let mut client = h.connect().await;

    write_submit_definition(
        &dir,
        "handoff-c-prime",
        "handoff-c-prime-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "",
            delivery_confirm_ms: 800,
            resend_max: 1,
        },
    );
    let window_id = h
        .open(
            submit_child_command("handoff-c-prime-marker", 0, 0, 0),
            Some("handoff-c-prime"),
        )
        .await;

    // A rejected load would leave the window Unknown for good; a loaded one
    // detects the fixture's marker line.
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn send_handoff_to_a_window_id_delivers_through_the_same_submit_sequence_after_approval() {
    // (d): the pending -> approve path must still go through the submit
    // plan / confirmation loop, not some separate write.
    let dir = unique_temp_dir("handoff-d-approved-submit");
    let config = approval_config(&dir, 30);
    let h = Harness::start("handoff-d-approved-submit", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut requester = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut requester,
        &dir,
        "handoff-d",
        "handoff-d-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "",
            delivery_confirm_ms: 800,
            resend_max: 0,
        },
        0,
    )
    .await;

    send(
        &mut requester,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, true).await;

    match read_response(&mut requester, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent {
            delivery, opened, ..
        } => {
            assert!(!opened);
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "an approved handoff must still go through the submit-plan write, not a separate one"
    );
}

#[tokio::test]
async fn send_handoff_to_a_window_id_is_resource_exhausted_after_the_per_connection_handoff_cap() {
    // (e): the handoff slot budget now bounds "outstanding, unanswered
    // handoffs", not just profile-opened windows, so a window_id target
    // must be counted too.
    let dir = unique_temp_dir("handoff-e-per-conn-cap");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        max_open_handoffs_per_conn: 1,
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-e-per-conn-cap", config).await;
    let mut client = h.connect().await;
    // The paste guard takes every submit key, so this handoff's slot stays
    // held (unanswered) for the full delivery_confirm_ms window.
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-e",
        "handoff-e-marker",
        0,
        u64::MAX,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "\\u0015",
            delivery_confirm_ms: 5000,
            resend_max: 0,
        },
        0,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "r1".to_string(),
            note: "t1".to_string(),
        },
    )
    .await;
    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "r2".to_string(),
            note: "t2".to_string(),
        },
    )
    .await;

    let second = read_response(&mut client, Duration::from_secs(5)).await;
    let message = expect_error(second, "resource_exhausted");
    assert!(
        message.contains("per-connection"),
        "message must name the per-connection limit: {message:?}"
    );

    match read_response(&mut client, Duration::from_secs(30)).await {
        DaemonMessage::HandoffSent { .. } => {}
        other => panic!("expected the first handoff's own reply, got {other:?}"),
    }
}

#[tokio::test]
async fn send_handoff_to_a_window_id_reacting_to_the_body_alone_is_written_once() {
    // react_on_body = 1 makes the fixture behave like a
    // receiver whose observable state moves as soon as it accepts the body,
    // before any submit key arrives. A confirmation that does not fix its
    // starting point at the submit key's own write would count this as
    // delivered. The submit key is swallowed, so the envelope stays in the
    // composer: nothing but the body's own echo ever moved the status.
    let dir = unique_temp_dir("handoff-a2-body-echo");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-a2-body-echo", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-a2",
        "handoff-a2-marker",
        0,
        u64::MAX,
        1,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "",
            delivery_confirm_ms: 1000,
            resend_max: 0,
        },
        0,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(
                wire(delivery),
                "taken",
                "a submitted envelope is taken however its submission was judged"
            );
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED "),
        0,
        "the submit key must be swallowed by the paste guard, never taken as a submit"
    );
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "no resend must occur"
    );
}

#[tokio::test]
async fn send_handoff_to_a_window_id_does_not_resend_once_the_window_leaves_the_ready_statuses() {
    // (k): once the window is driven to Unknown
    // mid-confirmation, the daemon must not resend into it, even though this
    // definition's resend_max (1) and non-empty clear would otherwise allow
    // one retry.
    let dir = unique_temp_dir("handoff-k-leaves-ready");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-k-leaves-ready", config).await;
    let mut client = h.connect().await;
    let input_log = dir.join("handoff-k-input.log");
    let window_id = open_submit_child_window_with_composer_logging_input(
        &h,
        &mut client,
        &dir,
        "handoff-k",
        "handoff-k-marker",
        0,
        1,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "\\u0015",
            delivery_confirm_ms: 5000,
            resend_max: 1,
        },
        0,
        &input_log,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;

    // The default erase (DEL) right before the default submit key (CR) is
    // read only once the probe after the body is erased and the submit key
    // pressed: from here on the daemon is waiting for a reaction the paste
    // guard never lets the fixture show.
    wait_until_input_log_contains(&input_log, b"\x7f\r", Duration::from_secs(10)).await;
    // `interrupt_window` sits behind an approval gate this connection's
    // policy does not auto-approve, and 0x03 sent through it would not
    // reach the fixture in time anyway; writing directly bypasses that gate
    // the same way `Harness::write_input` always does.
    h.write_input(&window_id, &[0x03]).await;

    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "a window driven out of the ready statuses mid-confirmation must never be resent to"
    );
}

#[tokio::test]
async fn send_handoff_to_a_window_id_confirms_delivery_after_waiting_for_readiness_to_settle() {
    // (a5'): the window's first `waiting_input` is only its startup phase's;
    // the readiness latch is still unlatched (`Building`). The test reports
    // the window working and then waiting again, the way a CLI that flaps on
    // start does: the gate must write nothing meanwhile, and submit only once
    // the second, permanent `waiting_input` has held for ready_hold_ms.
    let dir = unique_temp_dir("handoff-a5-prime-latch-before-ready");
    let config = DaemonConfig {
        handoff_ready_timeout_secs: 10,
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-a5-prime-latch-before-ready", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-a5-prime",
        "handoff-a5-prime-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 3000,
            submit_delay_ms: 300,
            clear: "",
            delivery_confirm_ms: 5000,
            resend_max: 0,
        },
        0,
    )
    .await;
    let receiver = h.registry.get(&window_id).expect("window must exist");
    let seq_before = receiver.write_seq();
    let mut reporter = h.connect().await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    report_status(&mut reporter, &window_id, WindowStatus::Running).await;
    wait_until_status(
        &mut reporter,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        receiver.write_seq(),
        seq_before,
        "nothing may be written while the window has not yet held a ready status"
    );
    report_status(&mut reporter, &window_id, WindowStatus::WaitingInput).await;

    match read_response(&mut client, Duration::from_secs(15)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    h.wait_for_scrollback_occurrences(&window_id, "SUBMITTED ", 1, Duration::from_secs(10))
        .await;
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED "), 1);
    assert_eq!(count_marker(&h, &window_id, "PASTE "), 1);
}

#[tokio::test]
async fn send_handoff_to_a_working_window_waits_in_its_inbox_and_is_typed_once_it_waits_again() {
    // (a5''): the window is working when the request arrives. Nothing is typed into a
    // working window: the envelope waits in its inbox, and is typed once the window
    // waits for input again.
    let dir = unique_temp_dir("handoff-a5-double-prime-inbox");
    let config = DaemonConfig {
        handoff_ready_timeout_secs: 10,
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-a5-double-prime-inbox", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "handoff-a5-double-prime",
        "handoff-a5-double-prime-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "",
            delivery_confirm_ms: 5000,
            resend_max: 0,
        },
        0,
    )
    .await;
    let receiver = h.registry.get(&window_id).expect("window must exist");
    let mut reporter = h.connect().await;
    report_status(&mut reporter, &window_id, WindowStatus::Running).await;
    wait_until_status(
        &mut reporter,
        &window_id,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;
    let seq_before = receiver.write_seq();

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look".to_string(),
        },
    )
    .await;
    wait_until_inbox_holds(&h, &window_id, 1).await;
    assert_eq!(
        receiver.write_seq(),
        seq_before,
        "nothing is typed into a working window"
    );

    report_status(&mut reporter, &window_id, WindowStatus::WaitingInput).await;
    match read_response(&mut client, Duration::from_secs(15)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(wire(delivery), "taken");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(receiver.inbox().pending_count(), 0);
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "the envelope is typed exactly once, when the window waits again"
    );
}

// =====================================================================
// send_message: the single-line submit path (case B) applies to `SendText`
// too. Every case below targets a `submit_child` window built by
// `open_submit_child_window` unless noted otherwise; the observation points
// are the same three the send_handoff suite above uses (`PASTE <n>`,
// `SUBMITTED <body>`, plain scrollback content).
// =====================================================================

/// `send_message`'s single-line submit-plan definition: the same shape (a)
/// above uses (`bracketed_paste = true` / `submit_delay_ms = 900` /
/// `delivery_confirm_ms = 0` / `clear` empty / `resend_max = 0`), so a `\r`
/// landing 900ms after the body clears the fixture's burst rule (a) by a wide
/// margin. `delivery_confirm_ms =
/// 0` is deliberate: `send_message` never reads this value, so its
/// setting here is inert, not a claim under test.
fn send_message_submit_definition() -> SubmitDefinitionInput {
    SubmitDefinitionInput {
        bracketed_paste: true,
        ready_hold_ms: 0,
        submit_delay_ms: 900,
        clear: "",
        delivery_confirm_ms: 0,
        resend_max: 0,
    }
}

#[tokio::test]
async fn send_message_to_a_window_id_submits_a_single_line_regardless_of_which_bare_terminator_ends_it()
 {
    // The judgment that decides whether a `SendText` body takes
    // the submit-plan path is the raw bytes the daemon received, not a
    // bridge-normalized form -- so all three terminators a caller might send
    // over raw IPC (bare CR, bare LF, CRLF) must reach the same outcome.
    // Current behavior writes body and terminator as one call
    // (`plan_raw_write`), so the fixture's same-chunk burst rule (a) folds
    // the terminator into the body instead of submitting it.
    for (case, terminator) in [("cr", "\r"), ("lf", "\n"), ("crlf", "\r\n")] {
        let dir = unique_temp_dir(&format!("send-message-single-line-{case}"));
        let config = DaemonConfig {
            approval_policy: Some(policy(&["send_message"])),
            ..detection_config(&dir)
        };
        let h = Harness::start(&format!("send-message-single-line-{case}"), config).await;
        let mut client = h.connect().await;
        let window_id = open_submit_child_window_with_composer(
            &h,
            &mut client,
            &dir,
            &format!("single-line-{case}"),
            &format!("single-line-{case}-marker"),
            0,
            0,
            0,
            send_message_submit_definition(),
            0,
        )
        .await;

        let body = format!("send-message-body-{case}");
        send(
            &mut client,
            &ClientMessage::SendText {
                window_id: window_id.clone(),
                text: format!("{body}{terminator}"),
            },
        )
        .await;
        match read_response(&mut client, Duration::from_secs(10)).await {
            DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
            other => panic!("expected text_sent for terminator {case}, got {other:?}"),
        }

        h.wait_for_scrollback_occurrences(
            &window_id,
            &format!("SUBMITTED {body}"),
            1,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            count_marker(&h, &window_id, "PASTE "),
            1,
            "the body must be written exactly once for terminator {case}"
        );
    }
}

#[tokio::test]
async fn send_message_with_a_trailing_newline_and_no_forbidden_character_still_submits() {
    // Regression for the control-character check: the trailing
    // terminator that marks "submit this line" must not itself be treated
    // as a forbidden character.
    let dir = unique_temp_dir("send-message-trailing-newline-ok");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-message-trailing-newline-ok", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "trailing-newline-ok",
        "trailing-newline-ok-marker",
        0,
        0,
        0,
        send_message_submit_definition(),
        0,
    )
    .await;

    let body = "send-message-trailing-newline-body";
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: format!("{body}\n"),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }

    h.wait_for_scrollback_occurrences(
        &window_id,
        &format!("SUBMITTED {body}"),
        1,
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn send_message_with_multiple_lines_is_invalid_argument_and_writes_nothing() {
    // A body with more than one terminator is rejected rather than written
    // through unchanged: `validate_send_text_body` rejects any CR/LF
    // that is not the single trailing terminator, before the approval gate
    // and before `plan_raw_write` ever runs -- so no byte reaches the PTY.
    let dir = unique_temp_dir("send-message-multi-line");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-message-multi-line", config).await;
    let mut client = h.connect().await;
    let window_id = h.open_idle_window(&mut client).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "send-message-multi-line-first\rsend-message-multi-line-second\r".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(10)).await;
    expect_error(msg, "invalid_argument");

    assert_eq!(
        count_marker(&h, &window_id, "send-message-multi-line-first"),
        0,
        "a rejected multi-line body must never reach the PTY"
    );
    assert_eq!(
        count_marker(&h, &window_id, "send-message-multi-line-second"),
        0,
        "a rejected multi-line body must never reach the PTY"
    );
}

#[tokio::test]
async fn send_message_without_a_usable_terminator_still_writes_the_raw_bytes_unchanged() {
    // (i): a body with no terminator, a terminator with an empty body, and
    // an empty body are all excluded by the case-B condition ("the body
    // with its terminator removed is non-empty") and must keep going
    // through `plan_raw_write`. Each case gets its own `submit_child`
    // window so a false submit from one case cannot be masked by another
    // case's fresh buffer. If the case-B condition were checked against the
    // wrong thing (e.g. treating a bare terminator as a submittable empty
    // line, or auto-appending `profile.submit` to a body that already had
    // none), a `SUBMITTED` or `PASTE` would appear here where the raw-write
    // contract promises none.
    let dir = unique_temp_dir("send-message-no-terminator");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-message-no-terminator", config).await;
    let mut client = h.connect().await;

    for (case, text) in [
        ("missing", "send-message-no-terminator-body".to_string()),
        ("terminator-only", "\r".to_string()),
        ("empty", String::new()),
    ] {
        let window_id = open_submit_child_window(
            &h,
            &mut client,
            &dir,
            &format!("no-terminator-{case}"),
            &format!("no-terminator-{case}-marker"),
            0,
            0,
            0,
            send_message_submit_definition(),
        )
        .await;
        let receiver = h.registry.get(&window_id).expect("window must exist");
        let seq_before = receiver.write_seq();

        send(
            &mut client,
            &ClientMessage::SendText {
                window_id: window_id.clone(),
                text,
            },
        )
        .await;
        match read_response(&mut client, Duration::from_secs(10)).await {
            DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
            other => panic!("expected text_sent for case {case}, got {other:?}"),
        }

        // The reply follows every write the call makes; a submit plan makes
        // two (the body, then the submit key), a raw write at most one.
        assert!(
            receiver.write_seq() - seq_before <= 1,
            "case {case} must be written raw, in at most one write, got {}",
            receiver.write_seq() - seq_before
        );
        assert_eq!(
            count_marker(&h, &window_id, "PASTE "),
            0,
            "case {case} must never take the bracketed-paste submit-plan path"
        );
        assert_eq!(
            count_marker(&h, &window_id, "SUBMITTED"),
            0,
            "case {case} must never produce a submit"
        );
    }
}

#[tokio::test]
async fn send_message_does_not_wait_for_a_reaction_or_resend() {
    // `send_message` never reads `delivery_confirm_ms` /
    // `resend_max`, so it must never call `wait_for_reaction` the way
    // `send_handoff` does. The fixture's paste guard takes the first submit
    // key as a line break: the window never reaches `running` and never
    // reacts, even though the definition can
    // resend (non-empty `clear`, `resend_max = 1`). If `send_message` waited
    // on a reaction anyway, it would sit out the loader's longest
    // `delivery_confirm_ms`, far past the reply timeout below.
    let dir = unique_temp_dir("send-message-no-confirm");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-message-no-confirm", config).await;
    let mut client = h.connect().await;
    let input_log = dir.join("child-input.log");
    write_submit_definition(
        &dir,
        "no-confirm",
        "no-confirm-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "\\u0015",
            delivery_confirm_ms: 30_000,
            resend_max: 1,
        },
    );
    let mut command = yagura_test_support::process::submit_child_command_with_composer(
        "no-confirm-marker",
        0,
        1,
        0,
        0,
    );
    command.push(String::new()); // fold_placeholder: none
    command.push(String::new()); // clear_scope: ordinary
    command.push(String::new()); // overlay: none
    command.push(String::new()); // overlay_heading
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    let window_id = h.open(command, Some("no-confirm")).await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "send-message-no-confirm-body\r".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(20)).await {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("send_message must never wait on delivery_confirm_ms, got {other:?}"),
    }
    // The confirming probe and its erase, the body, the probe behind it and
    // its erase, then the submit key, each once: no second body and no
    // second submit key although the submit key was swallowed and never
    // reacted to.
    let expected: &[u8] = b"x\x7f\x1b[200~send-message-no-confirm-body\x1b[201~x\x7f\r";
    // The reply can arrive before the child has logged the submit key it read.
    let read = wait_until_input_log_contains(&input_log, expected, Duration::from_secs(20)).await;
    assert_eq!(
        read, expected,
        "send_message must not resend just because the submit key was swallowed"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED "), 0);
}

// =====================================================================
// send_text vs send_handoff: per-window input lock. A `send_handoff` holds `input_lock` for its whole
// submit sequence (body write through submit key); a `send_text` to the
// *same* window from a separate connection, sent once the handoff's body has
// reached the child, must never land its own raw write between that body and
// the handoff's submit key.
// =====================================================================

#[tokio::test]
async fn send_text_never_lands_inside_a_handoffs_write_sequence_into_the_same_window() {
    let dir = unique_temp_dir("handoff-lock-vs-send-text");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff", "send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("handoff-lock-vs-send-text", config).await;
    let mut opener = h.connect().await;
    // This body is long enough that composer-draw mode wraps it across several
    // painted lines; a wider bottom_line_count keeps the startup marker line
    // inside the matched region even so.
    let window_id = open_submit_child_window_with_composer_and_bottom_line_count(
        &h,
        &mut opener,
        &dir,
        "handoff-lock-text",
        "handoff-lock-text-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 900,
            clear: "",
            delivery_confirm_ms: 800,
            resend_max: 0,
        },
        0,
        20,
    )
    .await;

    let mut client_a = h.connect().await;
    let mut client_b = h.connect().await;

    // No terminator: `single_line_body` returns `None` for this text, so it
    // takes `plan_raw_write` -- a single, delay-free, paste-free,
    // submit-key-free write. It is this uncushioned write that a broken
    // lock would let land inside the handoff's own write sequence.
    send(
        &mut client_a,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/a.rs".to_string(),
            note: "handoff A body".to_string(),
        },
    )
    .await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    send(
        &mut client_b,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "interleave-probe".to_string(),
        },
    )
    .await;

    match read_response(&mut client_a, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(
                wire(delivery),
                "taken",
                "handoff a is typed while the window is still ready (nothing else has written \
                 to it yet), so its envelope is taken"
            );
        }
        other => panic!("expected handoff_sent for a, got {other:?}"),
    }
    match read_response(&mut client_b, Duration::from_secs(10)).await {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent for the send_text, got {other:?}"),
    }

    h.wait_for_scrollback_occurrences(&window_id, "SUBMITTED ", 1, Duration::from_secs(5))
        .await;
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED "),
        1,
        "only the handoff's own submit key can ever produce a SUBMITTED line here: the \
         send_text carries no terminator and the handoff never resends (resend_max = 0)"
    );
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        1,
        "the send_text must not have taken the bracketed-paste submit-plan path"
    );

    let text = String::from_utf8_lossy(&h.scrollback(&window_id)).into_owned();
    // In composer-draw mode the send_text's raw write legitimately appears on
    // screen as an unsubmitted draft (the fixture paints whatever is sitting
    // in its input buffer, regardless of which connection wrote it): that is
    // not the property under test. What must never happen is this write
    // landing *inside* the handoff's own accepted line -- folded into the
    // body the handoff's submit key confirmed.
    let submitted_lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("SUBMITTED "))
        .collect();
    assert_eq!(submitted_lines.len(), 1, "got: {submitted_lines:?}");
    assert!(
        submitted_lines[0].contains("src/a.rs"),
        "the one SUBMITTED line must be the handoff's, got: {submitted_lines:?}"
    );
    assert!(
        !submitted_lines[0].contains("interleave-probe"),
        "the send_text's raw write must not have been folded into the handoff's submitted \
         body, got: {submitted_lines:?}"
    );
}

// =====================================================================
// send_handoff: the per-window delivery lock serializes a resending delivery's whole retry sequence against a
// later handoff to the same window: the later handoff's judgment and its
// bytes must not land until the earlier one's retry sequence -- resends and
// all -- is finished. A fixture that throws every pasted body away but
// shows the probe typed behind it makes every attempt end as not reflected,
// so each delivery writes its body once per attempt and leaves the composer
// empty behind it.
// =====================================================================

#[tokio::test]
async fn send_handoff_delivery_lock_serializes_a_resending_delivery_against_a_later_handoff_to_the_same_window()
 {
    let dir = unique_temp_dir("handoff-delivery-lock");
    // Every push fails, so neither envelope is ever taken: each answer is the limit's.
    let config = with_handoff_delivery_limit(detection_config(&dir), 8);
    let h = Harness::start("handoff-delivery-lock", config).await;
    let mut opener = h.connect().await;
    // `discard-paste:`: every pasted body is thrown away and the probe behind
    // it shows alone, so each attempt is given up on with nothing left behind,
    // never submitted.
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut opener,
        &dir,
        "handoff-dlock",
        "handoff-dlock-marker",
        &SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "\\u0015",
            delivery_confirm_ms: 600,
            resend_max: 1,
        }
        .input_toml(),
        0,
        "discard-paste:",
    )
    .await;

    let mut client_a = h.connect().await;
    let mut client_b = h.connect().await;

    send_from_two_connections(
        &mut client_a,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/a.rs".to_string(),
            note: "a body deliberately longer than handoff b's, so a write that landed \
                   on top of it could never report as few buffered bytes as b's own does"
                .to_string(),
        },
        &mut client_b,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/b.rs".to_string(),
            note: "short".to_string(),
        },
        // Handoff a's first body has reached the fixture, so b arrives while
        // a's retry sequence still holds the window.
        h.wait_for_scrollback(&window_id, b"PASTE 1 ", Duration::from_secs(10)),
    )
    .await;

    for mut client in [client_a, client_b] {
        match read_response(&mut client, Duration::from_secs(20)).await {
            DaemonMessage::HandoffSent { delivery, .. } => {
                assert_eq!(
                    wire(delivery),
                    "undelivered_timeout",
                    "every attempt writes a body the receiver throws away, so the envelope is \
                     never taken: a failed push puts it back in the inbox until the limit"
                );
            }
            other => panic!("expected handoff_sent, got {other:?}"),
        }
    }
    assert_eq!(
        h.registry
            .get(&window_id)
            .expect("window must exist")
            .inbox()
            .pending_count(),
        0,
        "an envelope answered as undelivered is no longer waiting"
    );

    // What each body landed on: the delivery lock is broken exactly when one
    // handoff's bytes (a probe it has typed and not yet erased) are still in
    // the receiver's buffer as the other's body is written, and the byte count
    // that write reports is the only place that shows. The receiver throws a
    // body away, so a clean landing reports none.
    let buffered = paste_buffered_values(&h, &window_id);
    assert!(
        buffered.len() >= 4,
        "two deliveries, each writing its body on both of its attempts, got {buffered:?}"
    );
    assert!(
        buffered.iter().all(|&bytes| bytes == 0),
        "every body, resends included, must land on a composer the previous attempt left \
         empty, got {buffered:?}"
    );
}

/// A decline that leaves nothing of this delivery's own behind
/// (`envelope_remains: false`) must carry no residue into the next attempt.
/// The write-count check that vouches for a resend does not itself look at the
/// screen, and a decline writes nothing, so the count does not move between
/// attempts: a residue carried forward regardless of `envelope_remains`
/// would agree with that check and press the submit key over someone else's
/// body. The foreign body here is also sized to the envelope exactly, so a
/// ledger comparing byte lengths would be caught the same way.
#[tokio::test]
async fn send_handoff_keeps_declining_when_the_composer_holds_a_foreign_body_the_whole_time() {
    let dir = unique_temp_dir("handoff-residue-carryover");
    let config = with_handoff_delivery_limit(detection_config(&dir), 3);
    let h = Harness::start("handoff-residue-carryover", config).await;
    let mut opener = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut opener,
        &dir,
        "residue-carryover",
        "residue-carryover-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 60,
            clear: "",
            delivery_confirm_ms: 100,
            resend_max: 1,
        },
        0,
    )
    .await;

    let reference = "src/foo.rs";
    let note = "look at the retry budget for this delivery";
    let envelope = compose_envelope(&EnvelopeFrom::External, reference, note);

    // A body from someone else, chosen to have the exact byte length this
    // delivery's own envelope has: a coincidence a wrongly-carried residue
    // could otherwise pass off as its own.
    let foreign = "F".repeat(envelope.text.len());
    h.registry
        .get(&window_id)
        .expect("window must exist in registry")
        .write_pty_input(foreign.as_bytes(), InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    let mut client = h.connect().await;
    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: reference.to_string(),
            note: note.to_string(),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(
                wire(delivery),
                "undelivered_timeout",
                "a foreign body sitting in the composer the whole time must decline every \
                 attempt, so the envelope is never taken before the limit"
            );
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "the submit key must never be pressed over someone else's body"
    );
}
