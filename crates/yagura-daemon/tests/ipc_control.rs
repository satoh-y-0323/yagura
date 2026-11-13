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

use crate::common;

use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::broadcast::error::TryRecvError;

use common::{
    CONTROL_DETECTION_NAME, Harness, READY_MARKER, VALID_TOKEN, approval_config,
    approver_base_config as base_config, decide, echo_child_path, env_probe_path,
    expect_approval_requested, expect_error, identify, let_every_clock_bound_run_out, policy,
    query_status, read_frame, read_response, register_approver, send, unique_temp_dir,
    wait_until_status,
};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::exit_with_code_command;
use yagura_test_support::process::{
    first_grandchild_command, process_is_running, wait_processes_gone,
};

use yagura_daemon::waiter::StatusAt;
use yagura_daemon::{DaemonConfig, OpenWindowRequest};
use yagura_protocol::{
    ApprovalRequesterKind, ClientMessage, DaemonMessage, DeliveryState, HANDOFF_NOTE_MAX_BYTES,
    HANDOFF_REFERENCE_MAX_BYTES, WindowStatus, WithdrawReason,
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
fn wire(delivery: DeliveryState) -> String {
    serde_json::to_value(delivery)
        .expect("a delivery encodes")
        .as_str()
        .expect("a delivery is a string on the wire")
        .to_string()
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
        wait_until_status(client, &window_id, WindowStatus::Exited).await;
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
    match read_response(client).await {
        DaemonMessage::Pong => EventOutcome::Accepted,
        DaemonMessage::Error { code, .. } => {
            let pong = read_response(client).await;
            assert!(
                matches!(pong, DaemonMessage::Pong),
                "the connection must stay usable after a rejected status_event, got {pong:?}"
            );
            EventOutcome::Rejected(code)
        }
        other => panic!("unexpected frame while probing status_event: {other:?}"),
    }
}

/// Every status a window publishes from the moment this is made. A window's
/// clock takes `Running` back to `Unknown`, so its current status cannot tell
/// "never moved" from "moved and went back"; the published transitions can.
struct PublishedStatuses {
    window_id: String,
    changes: tokio::sync::broadcast::Receiver<StatusAt>,
}

impl PublishedStatuses {
    fn watch(h: &Harness, window_id: &str) -> Self {
        let changes = h
            .registry
            .get(window_id)
            .expect("window must exist in registry")
            .subscribe_status();
        Self {
            window_id: window_id.to_string(),
            changes,
        }
    }

    /// Lets every clock bound run out, so whatever the daemon queued for this
    /// window has been judged and published, then asserts that none of the
    /// statuses it published was `status`.
    async fn assert_never_published(mut self, status: WindowStatus, why: &str) {
        let_every_clock_bound_run_out().await;
        let mut published = Vec::new();
        loop {
            match self.changes.try_recv() {
                Ok(at) => published.push(at.status.status()),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(missed)) => panic!(
                    "{missed} status change(s) of {} were missed, so their absence cannot be shown",
                    self.window_id
                ),
            }
        }
        assert!(
            !published.contains(&status),
            "{why}: {} published {published:?}",
            self.window_id
        );
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

    let answer = read_response(&mut client).await;
    match &answer {
        DaemonMessage::WindowInterrupted {
            window_id: acked, ..
        } => assert_eq!(acked, &window_id),
        other => panic!("expected window_interrupted, got {other:?}"),
    }
    assert_eq!(
        outcome_of(&answer),
        Some("interrupted"),
        "a window opened without a profile is interrupted with the configured bytes"
    );

    // The echo child writes back whatever reaches the PTY input, so the
    // configured bytes show as a line of the window's text.
    h.wait_for_scrollback(&window_id, b"INTERRUPT-MARK").await;
}

/// The `outcome` an `interrupt_window` answer carries, read off the wire.
fn outcome_of(answer: &DaemonMessage) -> Option<&'static str> {
    let encoded = serde_json::to_value(answer).expect("an answer encodes");
    [
        "interrupted",
        "stop_requested",
        "stop_requested_unconfirmed",
    ]
    .into_iter()
    .find(|named| encoded["outcome"].as_str() == Some(named))
}

/// Opens `command` the way the profile `profile` opens a window, under the control
/// definition, and returns once the window is `Idle` (`ready`) or right away.
async fn open_as_profile(h: &Harness, profile: &str, command: Vec<String>, ready: bool) -> String {
    let window_id = h
        .registry
        .open_with_extra_env(
            OpenWindowRequest {
                command,
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(CONTROL_DETECTION_NAME.to_string()),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                launch: Some(yagura_daemon::window::LaunchIdentity {
                    profile: profile.to_string(),
                    model: None,
                    effort: None,
                }),
                delivery: Some(common::wake_none_delivery()),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("the window opens")
        .window_id;
    if ready {
        let mut client = h.connect().await;
        wait_until_status(&mut client, &window_id, WindowStatus::Idle).await;
    }
    window_id
}

/// A window opened from a profile of the user's own that declares no interrupt is
/// interrupted the way a window opened without a profile is: with the configured bytes.
#[tokio::test]
async fn interrupt_window_writes_the_configured_bytes_to_a_profile_window_that_declares_no_interrupt()
 {
    let dir = unique_temp_dir("interrupt-undeclared-profile");
    let profiles_dir = unique_temp_dir("interrupt-undeclared-profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create the profiles directory");
    common::write_profile(
        &profiles_dir,
        "plain-cli",
        &yagura_test_support::process::profile_source(
            "Plain CLI",
            &yagura_test_support::process::echo_child_command(),
        ),
    );
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_bytes: INTERRUPT_MARKER.to_vec(),
        profiles_dir: Some(profiles_dir),
        ..detection_config(&dir)
    };
    let h = Harness::start("interrupt-undeclared-profile", config).await;
    let window_id = open_as_profile(
        &h,
        "plain-cli",
        yagura_test_support::process::echo_child_command(),
        false,
    )
    .await;
    h.write_input(&window_id, common::READY_MARKER).await;
    let mut client = h.connect().await;
    wait_until_status(&mut client, &window_id, WindowStatus::Idle).await;

    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let answer = read_response(&mut client).await;
    assert_eq!(
        outcome_of(&answer),
        Some("interrupted"),
        "a profile that declares no interrupt is interrupted with the configured bytes; \
         got {answer:?}"
    );
    h.wait_for_scrollback(&window_id, b"INTERRUPT-MARK").await;
}

/// A window whose profile gives an AI no means to stop its CLI (Codex CLI) refuses an
/// AI's interrupt and writes nothing; the user's own interrupt there writes the user's key,
/// Escape.
#[tokio::test]
async fn a_window_whose_profile_gives_an_ai_no_means_refuses_the_ai_and_takes_the_users_key() {
    let dir = unique_temp_dir("interrupt-no-means");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_bytes: INTERRUPT_MARKER.to_vec(),
        bundled_profiles: true,
        ..detection_config(&dir)
    };
    let h = Harness::start("interrupt-no-means", config).await;
    let window_id = open_as_profile(
        &h,
        "codex-cli",
        vec![
            common::hex_echo_path()
                .to_str()
                .expect("the hex_echo path is UTF-8")
                .to_string(),
        ],
        true,
    )
    .await;

    let mut ai = h.connect().await;
    send(
        &mut ai,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    expect_error(read_response(&mut ai).await, "interrupt_unsupported");

    let mut user = h.connect().await;
    register_approver(&mut user).await;
    send(
        &mut user,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    let answer = read_response(&mut user).await;
    assert_eq!(
        outcome_of(&answer),
        Some("interrupted"),
        "the user's interrupt is never refused; got {answer:?}"
    );
    common::assert_hex_echo_receives(&h, &window_id, b"CTRL-READY", b"\x1b").await;
    let received = common::output_after_marker(&h, &window_id, b"CTRL-READY");
    assert!(
        !common::contains_subsequence(&received, common::hex_of(b"INTERRUPT-MARK").as_bytes()),
        "the refused interrupt wrote nothing, and the user's wrote the user's key, not the \
         configured bytes: {:?}",
        String::from_utf8_lossy(&received)
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

    let msg = read_response(&mut client).await;
    expect_error(msg, "approval_required");
}

#[tokio::test]
async fn interrupt_window_returns_status_unknown_even_when_the_tool_is_approved() {
    // The Unknown gate outranks the playbook (fail-safe).
    let dir = unique_temp_dir("interrupt-unknown");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        bundled_profiles: true,
        ..detection_config(&dir)
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

    let msg = read_response(&mut client).await;
    expect_error(msg, "status_unknown");

    // A profile's means (a hook for Grok Build, none for Codex CLI) is looked at only
    // after the window's status: an Unknown window is refused as Unknown either way.
    for profile in ["grok-build", "codex-cli"] {
        let window_id = open_as_profile(
            &h,
            profile,
            yagura_test_support::process::echo_child_command(),
            false,
        )
        .await;
        assert_eq!(
            query_status(&mut client, &window_id).await,
            WindowStatus::Unknown,
            "fixture precondition: {profile}'s window has shown nothing yet"
        );
        send(&mut client, &ClientMessage::InterruptWindow { window_id }).await;
        expect_error(read_response(&mut client).await, "status_unknown");
    }
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
    expect_error(msg, "child_exited");
}

async fn close_window_kills_the_process_tree(tag: &str, force: Option<bool>) {
    let dir = unique_temp_dir(tag);
    let pid_file = dir.join("grandchild.pid");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..base_config()
    };
    let h = Harness::start(tag, config).await;
    let mut client = h.connect().await;
    // The direct child outlives the test: with one that exits on its own the
    // window's own exit watcher would tear the process tree down before
    // `close_window` is ever sent, and this test would stop measuring the
    // close path.
    let window_id = h
        .open(first_grandchild_command(&pid_file, 600_000), None)
        .await;
    let grandchild = running_published_pid(&pid_file);

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
    match read_response(&mut client).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }

    wait_processes_gone(&[grandchild], move |_| {
        format!("the grandchild (pid {grandchild}) outlived close_window: an orphan survived")
    })
    .await;
}

/// The process id `pid_file` comes to hold, once it is seen naming a running
/// process: a probe that never reports "running" cannot let a later wait for
/// the end pass.
fn running_published_pid(pid_file: &Path) -> u32 {
    let deadline = std::time::Instant::now() + HANG_GUARD;
    let pid = loop {
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild never published its process id at {}",
            pid_file.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        process_is_running(pid),
        "the grandchild (pid {pid}) must be running before the teardown under test"
    );
    pid
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
    match read_response(&mut client).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&window_id).await;
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
    match read_response(&mut client).await {
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
    common::wait_until_model_shows(&h, &window_id, b"HUP-TRAPPED").await;

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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
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
    match read_response(&mut client).await {
        DaemonMessage::WindowInterrupted {
            window_id: acked, ..
        } => assert_eq!(acked, window_id),
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
                declaration: None,
            },
        )
        .await;
        match read_response(&mut client).await {
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
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_a),
        other => panic!("expected identified, got {other:?}"),
    }

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_b,
            token: VALID_TOKEN.to_string(),
            declaration: None,
        },
    )
    .await;
    let msg = read_response(&mut client).await;
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
    let token_a = wait_for_window_token(&h, &window_a).await;
    let window_b = h.open_unknown_window().await;

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_b,
            token: token_a.clone(),
            declaration: None,
        },
    )
    .await;
    let msg = read_response(&mut client).await;
    expect_error(msg, "auth_failed");

    // The same token must still authenticate an identify against its own window.
    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_a.clone(),
            token: token_a,
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
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
            declaration: None,
        },
    )
    .await;
    let msg = read_response(&mut client).await;
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
                declaration: None,
            },
        )
        .await;
        let msg = read_response(&mut client).await;
        expect_error(msg, "window_not_found");
    }

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id,
            token: VALID_TOKEN.to_string(),
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
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
                declaration: None,
            },
        )
        .await;
        let msg = read_response(&mut client).await;
        expect_error(msg, "auth_failed");
    }

    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(HANG_GUARD, client.read(&mut buf))
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
            declaration: None,
        },
    )
    .await;
    let msg = read_response(&mut client).await;
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
    let msg2 = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg2 = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
        let msg = read_response(&mut client).await;
        expect_error(msg, "auth_failed");
    }

    // Unlike `identify`'s 3-strikes rule, `register_approver`'s auth_failed
    // is never counted, so the connection must still be usable.
    send(&mut client, &ClientMessage::Ping).await;
    let pong = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
    expect_error(msg, "auth_failed");

    send(
        &mut client,
        &ClientMessage::Identify {
            window_id: window_id.clone(),
            token: VALID_TOKEN.to_string(),
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
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
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
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
    let msg = read_response(&mut client).await;
    expect_error(msg, "status_unknown");
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
    match read_response(&mut client).await {
        DaemonMessage::TextSent {
            window_id: acked, ..
        } => assert_eq!(acked, window_id),
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let expected = yagura_protocol::validate_message_text(&text)
        .expect_err("a body ending in an escape character is refused")
        .to_string();
    send(&mut client, &ClientMessage::SendText { window_id, text }).await;
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
    expect_error(msg, "child_exited");
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

    match read_response(&mut client).await {
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

    match read_response(&mut client).await {
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

    let first = read_frame(&mut client).await;
    assert!(
        matches!(first, DaemonMessage::Pong),
        "ping must be answered while the wait is still pending, got {first:?}"
    );

    let second = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    match read_response(&mut client).await {
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

    let msg = read_response(&mut client).await;
    expect_error(msg, "resource_exhausted");
}

async fn open_window_with_marker_lines(h: &Harness) -> String {
    let window_id = h.open_unknown_window().await;
    for i in 1..=3 {
        h.write_input(&window_id, format!("RO-LINE-{i}\r\n").as_bytes())
            .await;
    }
    h.wait_for_scrollback(&window_id, b"RO-LINE-3").await;
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

    match read_response(&mut client).await {
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

    match read_response(&mut client).await {
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

    match read_response(&mut client).await {
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut client).await;
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

    wait_until_status(&mut client, &window_id, WindowStatus::Running).await;
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
    let published = PublishedStatuses::watch(&h, &window_id);

    let outcome = probe_status_event(
        &mut client,
        Some(&window_id),
        WRONG_TOKEN,
        WindowStatus::Running,
    )
    .await;
    assert_eq!(outcome, EventOutcome::Rejected("auth_failed".to_string()));

    published
        .assert_never_published(
            WindowStatus::Running,
            "a rejected event must not reach the detector",
        )
        .await;
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

    let published = PublishedStatuses::watch(&h, &window_id);

    let outcome =
        probe_status_event(&mut client, Some(&window_id), "", WindowStatus::Running).await;
    assert_eq!(outcome, EventOutcome::Rejected("auth_failed".to_string()));

    published
        .assert_never_published(
            WindowStatus::Running,
            "an event refused for its token's shape must not reach the detector",
        )
        .await;
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
    let token_a = wait_for_window_token(&h, &window_a).await;

    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let window_b = h
        .open(vec![path.to_string()], Some(CONTROL_DETECTION_NAME))
        .await;
    let published = PublishedStatuses::watch(&h, &window_b);

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
    published
        .assert_never_published(
            WindowStatus::Running,
            "a rejected cross-window event must not reach window B's detector",
        )
        .await;
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
    wait_until_status(&mut client, &window_a, WindowStatus::Running).await;
}

/// Polls `window_id`'s scrollback until the token its child printed appears.
async fn wait_for_window_token(h: &Harness, window_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
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
    h.wait_for_scrollback(&window_b, window_b.as_bytes()).await;
    let token_b = wait_for_window_token(&h, &window_b).await;
    let published = PublishedStatuses::watch(&h, &bystander);

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
    wait_until_status(&mut client, &window_b, WindowStatus::Running).await;

    published
        .assert_never_published(
            WindowStatus::Running,
            "the event must attribute to window_b alone, not to the window opened before it",
        )
        .await;
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
    h.wait_for_scrollback(&window_b, window_b.as_bytes()).await;
    let token_b = wait_for_window_token(&h, &window_b).await;
    let before = query_status(&mut client, &window_b).await;
    let published = PublishedStatuses::watch(&h, &window_b);

    assert_eq!(
        probe_status_event(&mut client, None, &token_b, WindowStatus::Running).await,
        EventOutcome::Rejected("invalid_argument".to_string()),
        "an event that omits window_id must be refused, even with a real window's token"
    );
    published
        .assert_never_published(
            WindowStatus::Running,
            "a rejected omitted-window_id event must not reach the token owner's detector",
        )
        .await;
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
    let published = PublishedStatuses::watch(&h, &bystander);

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
    wait_until_status(&mut client, &target_window, WindowStatus::Running).await;

    published
        .assert_never_published(
            WindowStatus::Running,
            "an explicit window_id must not also touch the window opened before it",
        )
        .await;
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
    let published =
        [&first_window, &later_window].map(|window| PublishedStatuses::watch(&h, window));

    assert_eq!(
        probe_status_event(&mut client, None, VALID_TOKEN, WindowStatus::Running).await,
        EventOutcome::Rejected("invalid_argument".to_string()),
    );

    for published in published {
        published
            .assert_never_published(
                WindowStatus::Running,
                "an omitted-window_id event must not touch any window",
            )
            .await;
    }
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
    let published = PublishedStatuses::watch(&h, &window);

    let outcome =
        probe_status_event(&mut client, Some(""), VALID_TOKEN, WindowStatus::Running).await;
    assert_eq!(
        outcome,
        EventOutcome::Rejected("window_not_found".to_string())
    );

    published
        .assert_never_published(
            WindowStatus::Running,
            "an empty window_id must not reach any window",
        )
        .await;
    assert_eq!(
        query_status(&mut client, &window).await,
        WindowStatus::Unknown,
        "an empty window_id must not reach any window"
    );
}

#[tokio::test]
async fn status_event_with_a_window_id_one_digit_off_a_live_one_returns_window_not_found() {
    // An id this daemon never allocated must get window_not_found even beside a
    // live window it differs from in one digit, never a near match.
    let dir = unique_temp_dir("hk-near-miss-id");
    let h = Harness::start("hk-near-miss-id", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let real_window_id = h.open_unknown_window().await;

    let (head, last) = real_window_id.split_at(real_window_id.len() - 1);
    let stale_window_id = format!("{head}{}", if last == "0" { "1" } else { "0" });
    assert_ne!(
        stale_window_id, real_window_id,
        "the constructed id must actually differ from the real one"
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
            on_name_taken: Default::default(),
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
async fn a_window_opened_without_detection_keeps_the_default_after_another_named_one() {
    // The default name resolves to no definition, so a window that reaches
    // `idle` on the marker can only have picked up the other window's.
    let dir = unique_temp_dir("detection-arg-default");
    let config = DaemonConfig {
        detection_name: "no-such-detection".to_string(),
        ..detection_config(&dir)
    };
    let h = Harness::start("detection-arg-default", config).await;
    let mut client = h.connect().await;
    h.open_idle_window(&mut client).await;

    let path = echo_child_path();
    let path = path.to_str().expect("echo_child path must be valid UTF-8");
    let without_detection = h.open(vec![path.to_string()], None).await;
    assert!(
        h.registry
            .get(&without_detection)
            .expect("window must exist in registry")
            .detection_warning()
            .is_some(),
        "a window opened without `detection` must resolve the daemon's unresolvable default"
    );
    let published = PublishedStatuses::watch(&h, &without_detection);
    h.write_input(&without_detection, READY_MARKER).await;
    h.wait_for_scrollback(
        &without_detection,
        yagura_test_support::detection::CONTROL_READY_MARKER.as_bytes(),
    )
    .await;

    published
        .assert_never_published(
            WindowStatus::Idle,
            "a window opened without `detection` must not judge by the definition another \
             window asked for",
        )
        .await;
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
    // The waiter is registered by the time the `Pong` behind it arrives.
    send(&mut client, &ClientMessage::Ping).await;
    assert!(matches!(
        read_response(&mut client).await,
        DaemonMessage::Pong
    ));
    drop(client);

    // Whether the gone connection's waiter was aborted is not visible to a
    // client; that the daemon still serves and still judges the transition the
    // waiter was watching for is.
    h.write_input(&window_id, READY_MARKER).await;

    let mut follow_up = h.connect().await;
    send(&mut follow_up, &ClientMessage::Ping).await;
    let pong = read_response(&mut follow_up).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "the daemon must still serve new connections, got {pong:?}"
    );
    wait_until_status(&mut follow_up, &window_id, WindowStatus::Idle).await;
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
    // A pending request does not outlive this daemon run, a window does.
    let boot = h.registry.boot_nonce();
    assert!(
        request.request_id.ends_with(boot),
        "an approval request id must carry this daemon run's boot nonce {boot}, got {}",
        request.request_id
    );
    assert!(
        !window_id.contains(boot),
        "a window id must not carry the boot nonce {boot} of the daemon run that opened it, \
         got {window_id}"
    );

    decide(&mut approver, &request.request_id, true).await;

    match read_response(&mut requester).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
    h.wait_until_child_exited(&window_id).await;
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

    let msg = read_response(&mut requester).await;
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

    let msg = read_response(&mut requester).await;
    expect_error(msg, "approval_timeout");

    match read_response(&mut approver).await {
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
    let msg = read_response(&mut requester).await;
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

    let msg = read_response(&mut requester).await;
    let message = expect_error(msg, "approval_required");
    assert!(
        message.contains("failed to load"),
        "message must state that the playbook failed to load, got {message:?}"
    );

    // The approver must not have been asked: a broken playbook is a
    // configuration failure, not something a human decides.
    send(&mut approver, &ClientMessage::Ping).await;
    let pong = read_response(&mut approver).await;
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
    match read_response(&mut approver).await {
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
    let msg = read_response(&mut approver).await;
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

    match read_response(&mut requester).await {
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
    let msg = read_response(&mut requester).await;
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
    let msg = read_response(&mut requester).await;
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
    let msg = read_response(&mut requester).await;
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
    let msg = read_response(&mut requester).await;
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
            declaration: None,
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::Identified { .. } => {}
        other => panic!("expected identified, got {other:?}"),
    }

    send(&mut client, &ClientMessage::RegisterApprover).await;
    let msg = read_response(&mut client).await;
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
            declaration: None,
        },
    )
    .await;
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut stranger).await;
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
    let _ = read_response(&mut requester).await;

    send(
        &mut approver,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id,
            approved: true,
        },
    )
    .await;
    let msg = read_response(&mut approver).await;
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

    match read_response(&mut approver2).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request1.request_id);
            assert_eq!(reason, WithdrawReason::Resolved);
        }
        other => panic!("expected approval_withdrawn(resolved), got {other:?}"),
    }

    match read_response(&mut requester).await {
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
    let msg = read_response(&mut requester).await;
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
    let msg = read_response(&mut requester2).await;
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
    let pong = read_response(&mut requester).await;
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
    // untouched, exactly one (the fifth) is rejected and the other four are
    // registered without an answer.
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

    // A wait_for_status is refused while it is handled, so every refusal is
    // ahead of the `Pong` behind the five.
    send(&mut client, &ClientMessage::Ping).await;
    let mut immediate_errors = 0;
    loop {
        match read_response(&mut client).await {
            DaemonMessage::Pong => break,
            msg => {
                expect_error(msg, "resource_exhausted");
                immediate_errors += 1;
            }
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
    match read_response(&mut approver).await {
        DaemonMessage::WindowInterrupted { window_id, .. } => assert_eq!(window_id, window_a),
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
    match read_response(&mut approver).await {
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
            declaration: None,
        },
    )
    .await;
    match read_response(&mut source).await {
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
    match read_response(&mut source).await {
        DaemonMessage::TextSent { window_id, .. } => assert_eq!(window_id, window_b),
        other => panic!("expected text_sent, got {other:?}"),
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

    let msg = read_response(&mut requester).await;
    expect_error(msg, "invalid_argument");

    // The approver's connection must stay idle: no approval_requested was
    // queued for a request rejected before it reached the ledger.
    send(&mut approver, &ClientMessage::Ping).await;
    let pong = read_response(&mut approver).await;
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
    let msg = read_response(&mut requester).await;
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
    wait_until_status(&mut requester, &window_id, WindowStatus::Exited).await;

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

    match read_response(&mut requester).await {
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
    wait_until_status(&mut requester, &window_id, WindowStatus::Exited).await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester).await;
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
    wait_until_status(&mut probe, &window_id, WindowStatus::Running).await;

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
    wait_until_status(&mut probe, &window_id, WindowStatus::Unknown).await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester).await;
    expect_error(msg, "status_unknown");
}

// =====================================================================
// send_handoff: window_id-addressed delivery. The envelope waits in the
// receiving window's inbox, and the take door hands it over.
// =====================================================================

/// The handoff envelope the take door of `window_id` hands over, parsed as JSON.
async fn taken_handoff_envelope_json(h: &Harness, window_id: &str) -> serde_json::Value {
    const PREFIX: &str = "[yagura handoff] ";
    let taken = crate::send_message_lifecycle::take_and_print(h, window_id).await;
    let json = taken
        .lines()
        .find_map(|line| line.find(PREFIX).map(|at| &line[at + PREFIX.len()..]))
        .unwrap_or_else(|| panic!("the take door must hand over a handoff envelope: {taken:?}"));
    serde_json::from_str(json.trim_end())
        .unwrap_or_else(|err| panic!("the envelope must be JSON ({err}): {json:?}"))
}

#[tokio::test]
async fn send_handoff_to_ones_own_window_needs_no_playbook_and_queues_the_envelope_once() {
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
    match read_response(&mut client).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            state: delivery,
            ..
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened, "a window_id target must never report opened: true");
            assert_eq!(
                wire(delivery),
                "queued",
                "with no wait registered nothing is handed over, so the envelope waits"
            );
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json = taken_handoff_envelope_json(&h, &window_id).await;
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
    let msg = read_response(&mut client).await;
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
    match read_response(&mut client).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            state: delivery,
            ..
        } => {
            assert_eq!(acked, window_id);
            assert!(!opened);
            assert_eq!(wire(delivery), "queued");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json = taken_handoff_envelope_json(&h, &window_id).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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
    let msg = read_response(&mut client).await;
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

    let msg = read_response(&mut requester).await;
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
    wait_until_status(&mut probe, &window_id, WindowStatus::Running).await;

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
    wait_until_status(&mut probe, &window_id, WindowStatus::Unknown).await;

    decide(&mut approver, &request.request_id, true).await;

    let msg = read_response(&mut requester).await;
    expect_error(msg, "status_unknown");
}

#[tokio::test]
async fn send_handoff_between_two_windows_approved_by_a_registered_approver_queues_the_envelope() {
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

    match read_response(&mut source).await {
        DaemonMessage::HandoffSent {
            window_id: acked,
            opened,
            state: delivery,
            ..
        } => {
            assert_eq!(acked, window_b);
            assert!(!opened, "a window_id target must never report opened: true");
            assert_eq!(wire(delivery), "queued");
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let json = taken_handoff_envelope_json(&h, &window_b).await;
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

/// The child of [`a_window_that_ends_on_its_own_takes_what_it_left_waiting_with_it`]:
/// it ends by itself at its first input.
const ENDS_AT_INPUT_CHILD: &str = "a_child_that_ends_at_its_first_input";

#[test]
#[ignore = "the child of the test below, started by common::own_test_child_command"]
fn a_child_that_ends_at_its_first_input() {
    if !common::running_as_own_test(module_path!(), ENDS_AT_INPUT_CHILD) {
        return;
    }
    let mut byte = [0u8; 1];
    let _ = std::io::Read::read(&mut std::io::stdin().lock(), &mut byte);
    std::process::exit(0);
}

/// A sending window whose child exits by itself, with no close asked of it, withdraws
/// what it left waiting in another window's inbox, as a closed one does.
#[tokio::test]
async fn a_window_that_ends_on_its_own_takes_what_it_left_waiting_with_it() {
    use crate::send_message_lifecycle::{entry, open_wait_window, send_queued, sending_config};

    let dir = unique_temp_dir("sender-ends-on-its-own");
    let h = Harness::start("sender-ends-on-its-own", sending_config(&dir)).await;
    let mut external = h.connect().await;
    let target = open_wait_window(&h, &mut external, CONTROL_DETECTION_NAME).await;
    let sender_id = h
        .open(
            common::own_test_child_command(module_path!(), ENDS_AT_INPUT_CHILD),
            None,
        )
        .await;
    let mut sender = h.connect().await;
    identify(&mut sender, &sender_id).await;
    send_queued(&mut sender, &target, "LEFT-BY-A-WINDOW-THAT-ENDS").await;
    assert_eq!(entry(&h, &target).inbox().pending_count(), 1);

    h.write_input(&sender_id, b"q\r").await;
    h.wait_until_child_exited(&sender_id).await;
    crate::send_message_lifecycle::until("the ended sender's envelope to be withdrawn", || {
        entry(&h, &target).inbox().pending_count() == 0
    })
    .await;
}
