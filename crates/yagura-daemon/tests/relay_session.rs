#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon's side of the relay contract. The test speaks
//! the relay frames itself (`common::relay`), playing both a window's relay
//! and the CLI behind it, against a real `IpcServer` over a real endpoint.
//!
//! Covers the session (limits, identification, a resumed opening, the tool
//! list, the contract, the ways a stateless CLI enters, how many requests it
//! runs, frames breaking the contract), what the end of a relay connection
//! does to the calls it carried, and notifications a CLI sends before its
//! first request.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use common::relay::{
    RelayClient, cancelled_line, discover_line, error_word, hello, initialize_line,
    initialized_line, roots_list_changed_line, stateless_meta, tool_call_line, tool_json,
};
use common::{
    Harness, SubmitDefinitionInput, VALID_TOKEN, approver_base_config, policy, unique_temp_dir,
    wait_until_composer_is, wait_until_input_log_contains, write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::reading::ComposerReadout;
use yagura_daemon::window::{InputOrigin, WindowEntry};
use yagura_protocol::MAX_FRAME_BODY_LEN;
use yagura_protocol::relay::{
    RESOURCE_EXHAUSTED, RelayFrame, RelayRefusedKind, RelayResume, SUPPORTED_CONTRACTS,
};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::process;

/// Well-formed, but not the token the harness gives its windows.
const WRONG_TOKEN: &str = "fedcba9876543210fedcba9876543210";

/// How many sessions one window may hold.
const SESSIONS_PER_WINDOW: usize = 4;

/// How many requests one session runs at once, and how many more may wait.
const REQUESTS_AT_ONCE: u64 = 4;
const REQUESTS_WAITING: u64 = 16;

/// Typed into a receiver last: read back, it proves the child read
/// everything written before it.
const SENTINEL: &[u8] = b"SENTINEL-AFTER";

fn config() -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&[
            "open_window",
            "close_window",
            "send_message",
            "send_handoff",
        ])),
        ..approver_base_config()
    }
}

fn mcp_connected(h: &Harness, window_id: &str) -> Option<bool> {
    h.registry
        .list()
        .into_iter()
        .find(|info| info.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} is missing from the registry"))
        .mcp_connected
}

/// Reads the window's mark until it is `expected`: the end of a connection
/// reaches the daemon asynchronously.
async fn wait_for_mcp_connected(h: &Harness, window_id: &str, expected: bool) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while mcp_connected(h, window_id) != Some(expected) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "mcp_connected of {window_id} never became Some({expected})"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Opens with `initialize` and `notifications/initialized`, as codex, grok
/// and muse do, and returns the `initialize` answer.
async fn initialize(client: &mut RelayClient) -> Value {
    let seq = client.request(&initialize_line(0)).await;
    let answer = client.answer(seq).await;
    client.carry(&initialized_line().to_string()).await;
    answer
}

/// A `list_windows` call and a `tools/list` are both answered: the session
/// goes on serving.
async fn expect_the_session_serves(client: &mut RelayClient, first_id: u64, stateless: bool) {
    let seq = client
        .request(&tool_call_line(
            first_id,
            "list_windows",
            json!({}),
            stateless,
        ))
        .await;
    let listed = tool_json(&client.answer(seq).await);
    assert!(
        listed.get("windows").is_some(),
        "list_windows must answer its windows, got {listed}"
    );
    let mut list = json!({"jsonrpc": "2.0", "id": first_id + 1, "method": "tools/list"});
    if stateless {
        list["params"] = json!({"_meta": stateless_meta()});
    }
    let seq = client.request(&list).await;
    let tools = client.answer(seq).await;
    assert!(
        tools["result"]["tools"].is_array(),
        "tools/list must answer the tool list, got {tools}"
    );
}

fn tools_list_changed(capabilities_holder: &Value) -> Option<bool> {
    capabilities_holder["result"]["capabilities"]["tools"]["listChanged"].as_bool()
}

fn is_list_changed(line: &Value) -> bool {
    line["method"] == "notifications/tools/list_changed"
}

/// The opening a relay resends: the `initialize` line, its answer as the CLI
/// received it, and `notifications/initialized`.
fn opening(answer: &Value) -> Vec<String> {
    vec![
        initialize_line(0).to_string(),
        answer.to_string(),
        initialized_line().to_string(),
    ]
}

/// A first session that opens with `initialize`; returns its tool summary
/// and the `initialize` answer.
async fn first_session(h: &Harness) -> (String, Value) {
    let (mut client, tools) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    let answer = initialize(&mut client).await;
    (tools, answer)
}

/// Reads past a `relay_welcome` to the refusal, then the connection's end.
async fn expect_refused_then_closed(client: &mut RelayClient, first: RelayFrame) -> RelayFrame {
    let refused = match first {
        RelayFrame::Welcome { .. } => client.next_frame().await,
        other => other,
    };
    assert!(
        matches!(refused, RelayFrame::Refused { .. }),
        "expected relay_refused, got {refused:?}"
    );
    client.expect_closed("after relay_refused").await;
    refused
}

fn refused_kind(frame: &RelayFrame) -> RelayRefusedKind {
    match frame {
        RelayFrame::Refused { kind, .. } => *kind,
        other => panic!("expected relay_refused, got {other:?}"),
    }
}

// =====================================================================
// How many sessions the daemon holds
// =====================================================================

#[tokio::test]
async fn a_windows_fifth_relay_session_is_refused_as_limit_and_closed() {
    let h = Harness::start("relay-window-limit", config()).await;
    let window_id = h.open(process::echo_child_command(), None).await;

    let mut held = Vec::new();
    for _ in 0..SESSIONS_PER_WINDOW {
        let (client, _) =
            RelayClient::welcomed(&h.endpoint, hello(Some((&window_id, VALID_TOKEN)), None)).await;
        held.push(client);
    }
    let (mut fifth, answer) =
        RelayClient::open(&h.endpoint, hello(Some((&window_id, VALID_TOKEN)), None)).await;
    let refused = expect_refused_then_closed(&mut fifth, answer).await;
    assert_eq!(refused_kind(&refused), RelayRefusedKind::Limit);
}

#[tokio::test]
async fn relay_sessions_without_a_window_are_held_up_to_max_windows() {
    let max_windows = 2;
    let h = Harness::start(
        "relay-windowless-limit",
        DaemonConfig {
            max_windows,
            ..config()
        },
    )
    .await;

    let mut held = Vec::new();
    for _ in 0..max_windows {
        held.push(
            RelayClient::welcomed(&h.endpoint, hello(None, None))
                .await
                .0,
        );
    }
    let (mut next, answer) = RelayClient::open(&h.endpoint, hello(None, None)).await;
    let refused = expect_refused_then_closed(&mut next, answer).await;
    assert_eq!(refused_kind(&refused), RelayRefusedKind::Limit);
}

/// The refused connection stays open, carrying nothing: it still counts
/// toward the sessions the daemon holds.
#[tokio::test]
async fn a_contract_the_daemon_does_not_speak_is_refused_naming_its_contracts_and_stays_counted() {
    let max_windows = 2;
    let h = Harness::start(
        "relay-contract",
        DaemonConfig {
            max_windows,
            ..config()
        },
    )
    .await;
    let unspoken = SUPPORTED_CONTRACTS.iter().max().unwrap() + 1;

    let mut held = Vec::new();
    for _ in 0..max_windows {
        let (client, answer) = RelayClient::open(
            &h.endpoint,
            RelayFrame::Hello {
                contract: unspoken,
                window_id: None,
                token: None,
                resume: None,
            },
        )
        .await;
        match answer {
            RelayFrame::Refused {
                kind, contracts, ..
            } => {
                assert_eq!(kind, RelayRefusedKind::Contract);
                assert_eq!(
                    contracts,
                    SUPPORTED_CONTRACTS.to_vec(),
                    "the refusal names the contracts the daemon speaks"
                );
            }
            other => panic!("expected relay_refused, got {other:?}"),
        }
        held.push(client);
    }

    let (mut next, answer) = RelayClient::open(&h.endpoint, hello(None, None)).await;
    let refused = expect_refused_then_closed(&mut next, answer).await;
    assert_eq!(
        refused_kind(&refused),
        RelayRefusedKind::Limit,
        "the connections refused for their contract are still held"
    );
}

// =====================================================================
// Identification
// =====================================================================

#[tokio::test]
async fn an_identified_relay_session_marks_its_window_until_its_connection_closes() {
    let h = Harness::start("relay-identified", config()).await;
    let window_id = h.open(process::echo_child_command(), None).await;
    assert_eq!(mcp_connected(&h, &window_id), Some(false));

    let (client, _) =
        RelayClient::welcomed(&h.endpoint, hello(Some((&window_id, VALID_TOKEN)), None)).await;
    wait_for_mcp_connected(&h, &window_id, true).await;

    drop(client);
    wait_for_mcp_connected(&h, &window_id, false).await;
}

#[tokio::test]
async fn a_relay_hello_with_a_wrong_token_marks_nothing_and_its_tool_answers_carry_the_warning() {
    let h = Harness::start("relay-wrong-token", config()).await;
    let window_id = h.open(process::echo_child_command(), None).await;
    let other = h.open(process::echo_child_command(), None).await;

    let (mut client, _) =
        RelayClient::welcomed(&h.endpoint, hello(Some((&window_id, WRONG_TOKEN)), None)).await;
    initialize(&mut client).await;
    let seq = client
        .request(&tool_call_line(
            1,
            "close_window",
            json!({"window_id": other, "force": true}),
            false,
        ))
        .await;
    let closed = tool_json(&client.answer(seq).await);

    assert_eq!(
        closed["requester"], "external",
        "a refused identification acts as no window, got {closed}"
    );
    assert!(
        closed["identity_warning"]
            .as_str()
            .is_some_and(|warning| !warning.is_empty()),
        "the answer must say the identification was refused, got {closed}"
    );
    assert_eq!(
        mcp_connected(&h, &window_id),
        Some(false),
        "a refused identification marks no window"
    );
}

// =====================================================================
// A resumed opening and the tool list
// =====================================================================

/// The opening is fed to the session and its answer is not sent again; an
/// unchanged tool summary sends no `list_changed`.
#[tokio::test]
async fn a_resumed_opening_is_not_answered_again_and_an_unchanged_tool_list_is_not_announced() {
    let h = Harness::start("relay-resume-same", config()).await;
    let (tools, answer) = first_session(&h).await;

    let (mut client, _) = RelayClient::welcomed(
        &h.endpoint,
        hello(
            None,
            Some(RelayResume {
                opening: Some(opening(&answer)),
                tools,
            }),
        ),
    )
    .await;
    let seq = client
        .request(&tool_call_line(1, "list_windows", json!({}), false))
        .await;
    let (listed, before) = client.answer_with_preceding(seq).await;
    tool_json(&listed);
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let seq = client.request(&list).await;
    let (_, between) = client.answer_with_preceding(seq).await;

    assert!(
        before.is_empty() && between.is_empty(),
        "nothing but the answers may reach the CLI (no initialize answer, no \
         list_changed), got {before:?} then {between:?}"
    );
}

#[tokio::test]
async fn a_resumed_session_whose_tool_summary_differs_is_told_the_tool_list_changed() {
    let h = Harness::start("relay-resume-changed", config()).await;
    let (tools, answer) = first_session(&h).await;

    let (mut client, _) = RelayClient::welcomed(
        &h.endpoint,
        hello(
            None,
            Some(RelayResume {
                opening: Some(opening(&answer)),
                tools: format!("{tools}-from-another-build"),
            }),
        ),
    )
    .await;
    let changed = client
        .line_where("notifications/tools/list_changed", is_list_changed)
        .await;
    assert!(
        changed.get("id").is_none(),
        "list_changed is a notification, got {changed}"
    );
}

#[tokio::test]
async fn a_resumed_opening_whose_protocol_version_or_capabilities_differ_is_refused_as_session_mismatch()
 {
    let h = Harness::start("relay-resume-mismatch", config()).await;
    let (tools, answer) = first_session(&h).await;

    let mut other_version = answer.clone();
    let version = answer["result"]["protocolVersion"]
        .as_str()
        .expect("the initialize answer names its protocol version");
    other_version["result"]["protocolVersion"] = json!(if version == "2024-11-05" {
        "2025-03-26"
    } else {
        "2024-11-05"
    });
    let mut other_capabilities = answer.clone();
    other_capabilities["result"]["capabilities"] = json!({});

    for (what, recorded) in [
        ("protocolVersion", other_version),
        ("capabilities", other_capabilities),
    ] {
        let (mut client, first) = RelayClient::open(
            &h.endpoint,
            hello(
                None,
                Some(RelayResume {
                    opening: Some(opening(&recorded)),
                    tools: tools.clone(),
                }),
            ),
        )
        .await;
        let refused = expect_refused_then_closed(&mut client, first).await;
        assert_eq!(
            refused_kind(&refused),
            RelayRefusedKind::SessionMismatch,
            "a recorded {what} that differs must be refused as session_mismatch"
        );
    }
}

// =====================================================================
// How a stateless CLI enters
// =====================================================================

#[tokio::test]
async fn a_session_without_an_opening_enters_statelessly_through_server_discover() {
    let h = Harness::start("relay-stateless", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;

    let seq = client.request(&discover_line(1)).await;
    let discovered = client.answer(seq).await;
    assert_eq!(
        tools_list_changed(&discovered),
        Some(true),
        "server/discover must declare tools.listChanged, got {discovered}"
    );
    expect_the_session_serves(&mut client, 2, true).await;
}

// =====================================================================
// Requests at once
// =====================================================================

/// A session runs four requests at once and holds sixteen more waiting; the
/// next is answered `resource_exhausted` while all of those still wait.
#[tokio::test]
async fn a_request_past_the_four_running_and_sixteen_waiting_is_answered_resource_exhausted() {
    let h = Harness::start("relay-request-limit", config()).await;
    // No detection: the window stays `Unknown`, which completes no wait.
    let window_id = h.open(process::echo_child_command(), None).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    initialize(&mut client).await;

    let held = REQUESTS_AT_ONCE + REQUESTS_WAITING;
    for id in 1..=held {
        client
            .request(&tool_call_line(
                id,
                "wait_for_status",
                json!({"window_id": window_id, "target_status": "idle", "timeout_sec": 600}),
                false,
            ))
            .await;
    }
    let seq = client
        .request(&tool_call_line(held + 1, "list_windows", json!({}), false))
        .await;
    let (answer, before) = client.answer_with_preceding(seq).await;

    assert_eq!(
        error_word(&answer).as_deref(),
        Some(RESOURCE_EXHAUSTED),
        "the request past the limit must be refused, got {answer}"
    );
    let answered_early: Vec<&Value> = before
        .iter()
        .filter(|line| line.get("id").and_then(Value::as_u64).is_some())
        .collect();
    assert!(
        answered_early.is_empty(),
        "the {held} held requests must still wait, got {answered_early:?}"
    );
}

// =====================================================================
// Frames that break the contract
// =====================================================================

#[tokio::test]
async fn only_a_frame_breaking_the_contract_closes_the_relay_connection() {
    let h = Harness::start("relay-bad-frames", config()).await;

    let oversized = u32::try_from(MAX_FRAME_BODY_LEN + 1)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    let untyped = yagura_protocol::frame_encode(br#"{"line":"{}"}"#);
    let unreadable = yagura_protocol::frame_encode(b"not a relay frame");
    for (what, bytes) in [
        ("a frame over the size limit", oversized),
        ("a frame without a type", untyped),
        ("a body that is no relay frame", unreadable),
    ] {
        let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
        initialize(&mut client).await;
        client.send_bytes(&bytes).await;
        client.expect_closed(what).await;
    }
}

/// A line the CLI wrote that is not JSON is carried as it is; the session
/// goes on.
#[tokio::test]
async fn a_relay_in_line_that_is_not_json_does_not_end_the_session() {
    let h = Harness::start("relay-bad-line", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    initialize(&mut client).await;

    client.carry("{not json").await;
    expect_the_session_serves(&mut client, 1, false).await;
}

// =====================================================================
// The end of a relay connection ends the calls it carried
// =====================================================================

struct Stage {
    h: Harness,
    requester: String,
    receiver: String,
    input_log: PathBuf,
    resume: PathBuf,
}

impl Stage {
    /// A requester window, and a receiver that stops reading once a body
    /// arrives and reads on when `resume` exists.
    async fn start(tag: &str) -> Self {
        let dir = unique_temp_dir(tag);
        let h = Harness::start(
            tag,
            DaemonConfig {
                detection_dir: Some(dir.clone()),
                ..config()
            },
        )
        .await;
        let requester = h.open(process::echo_child_command(), None).await;

        let input_log = dir.join("receiver-input.log");
        let resume = dir.join("resume");
        write_submit_definition(
            &dir,
            "receiver",
            "receiver-marker",
            SubmitDefinitionInput {
                bracketed_paste: true,
                ready_hold_ms: 0,
                submit_delay_ms: 120,
                clear: "\\u0015",
                delivery_confirm_ms: 2000,
                resend_max: 0,
            },
        );
        let mut command =
            process::submit_child_command_with_composer("receiver-marker", 0, 0, 0, 0);
        command.push(String::new()); // fold_placeholder: none
        command.push(format!("stall-until|{}|", path_str(&resume)));
        command.push(String::new()); // overlay: none
        command.push(String::new()); // overlay_heading
        command.push(path_str(&input_log).to_string());
        let receiver = h.open_delivering(command, Some("receiver")).await;
        let mut observer = h.connect().await;
        common::wait_until_status(
            &mut observer,
            &receiver,
            yagura_protocol::WindowStatus::WaitingInput,
        )
        .await;
        Self {
            h,
            requester,
            receiver,
            input_log,
            resume,
        }
    }

    fn receiver_entry(&self) -> Arc<WindowEntry> {
        self.h
            .registry
            .get(&self.receiver)
            .expect("the receiver must exist")
    }

    /// The requester's relay session, opened the way a window's CLI opens it.
    async fn requester_session(&self) -> RelayClient {
        let (mut client, _) = RelayClient::welcomed(
            &self.h.endpoint,
            hello(Some((&self.requester, VALID_TOKEN)), None),
        )
        .await;
        initialize(&mut client).await;
        client
    }

    /// Lets the receiver read on, and asserts that what it was sent is taken
    /// back and no submit key is ever written.
    async fn expect_taken_back_without_a_submit_key(&self) {
        std::fs::write(&self.resume, b"").expect("let the receiver read on");
        wait_until_input_log_contains(&self.input_log, b"\x15").await;
        wait_until_composer_is(&self.h, &self.receiver, &ComposerReadout::Empty).await;
        self.receiver_entry()
            .write_pty_input(SENTINEL, InputOrigin::Interactive)
            .await
            .expect("the sentinel must not fail");
        let read = wait_until_input_log_contains(&self.input_log, SENTINEL).await;
        assert!(
            !read.contains(&b'\r'),
            "no submit key may follow a call whose relay connection closed: {read:?}"
        );
    }
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("the fixture path must be valid UTF-8")
}

#[tokio::test]
async fn closing_the_relay_connection_during_a_send_message_presses_no_submit_key_and_takes_the_body_back()
 {
    let stage = Stage::start("relay-end-send").await;
    let mut client = stage.requester_session().await;

    client
        .request(&tool_call_line(
            1,
            "send_message",
            json!({"window_id": stage.receiver, "text": "BODY\n"}),
            false,
        ))
        .await;
    stage
        .h
        .wait_for_scrollback(&stage.receiver, b"STALL-BEGAN")
        .await;
    drop(client);

    stage.expect_taken_back_without_a_submit_key().await;
}

#[tokio::test]
async fn closing_the_relay_connection_during_a_handoff_withdraws_its_envelope() {
    let stage = Stage::start("relay-end-handoff").await;
    let mut client = stage.requester_session().await;

    client
        .request(&tool_call_line(
            1,
            "send_handoff",
            json!({"window_id": stage.receiver, "reference": "src/lib.rs", "note": "please look"}),
            false,
        ))
        .await;
    stage
        .h
        .wait_for_scrollback(&stage.receiver, b"STALL-BEGAN")
        .await;
    drop(client);

    stage.expect_taken_back_without_a_submit_key().await;
    assert_eq!(
        stage.receiver_entry().inbox().pending_count(),
        0,
        "the envelope of a handoff whose requester left is no longer waiting"
    );
}

// =====================================================================
// Notifications before the first request
// =====================================================================

/// agy's opening: `server/discover`, then `notifications/roots/list_changed`
/// before any other request.
#[tokio::test]
async fn a_roots_list_changed_between_server_discover_and_the_first_request_keeps_the_session() {
    let h = Harness::start("relay-first-roots", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;

    client.request(&discover_line(1)).await;
    client.carry(&roots_list_changed_line().to_string()).await;
    expect_the_session_serves(&mut client, 2, true).await;
}

#[tokio::test]
async fn a_cancelled_before_the_first_request_keeps_the_session() {
    let h = Harness::start("relay-first-cancelled", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;

    client.carry(&cancelled_line(99).to_string()).await;
    expect_the_session_serves(&mut client, 1, true).await;
}

/// A relay connecting again for a CLI that opened without `initialize`
/// resumes with no opening; the CLI's next line may be a notification.
#[tokio::test]
async fn a_resumed_session_without_an_opening_whose_first_line_is_a_notification_keeps_the_session()
{
    let h = Harness::start("relay-first-resumed", config()).await;
    let (first, tools) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    drop(first);

    for notification in [roots_list_changed_line(), cancelled_line(7)] {
        let (mut client, _) = RelayClient::welcomed(
            &h.endpoint,
            hello(
                None,
                Some(RelayResume {
                    opening: None,
                    tools: tools.clone(),
                }),
            ),
        )
        .await;
        client.carry(&notification.to_string()).await;
        expect_the_session_serves(&mut client, 1, true).await;
    }
}

/// The form codex, grok and muse use, beside the stateless ones above.
#[tokio::test]
async fn a_session_opened_with_initialize_answers_tool_calls_and_declares_tools_list_changed() {
    let h = Harness::start("relay-first-initialize", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;

    let answer = initialize(&mut client).await;
    assert_eq!(
        tools_list_changed(&answer),
        Some(true),
        "initialize must declare tools.listChanged, got {answer}"
    );
    expect_the_session_serves(&mut client, 1, false).await;
}
