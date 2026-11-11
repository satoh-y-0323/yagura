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

use serde_json::{Value, json};

use common::relay::{
    RelayClient, cancelled_line, discover_line, hello, initialize_line, initialized_line,
    roots_list_changed_line, stateless_meta, tool_call_line, tool_json,
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
use yagura_test_support::relay_stdio::tool_call_error;

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
/// next is answered `resource_exhausted` while all of those still wait. A
/// line led by a UTF-8 byte order mark, which rmcp reads as the request after
/// it, counts the same. A request whose id is an integer written as a float,
/// which rmcp answers under the integer, leaves the count once answered.
#[tokio::test]
async fn a_request_past_the_four_running_and_sixteen_waiting_is_answered_resource_exhausted() {
    let h = Harness::start("relay-request-limit", config()).await;
    // No detection: the window stays `Unknown`, which completes no wait.
    let window_id = h.open(process::echo_child_command(), None).await;

    for (what, lead) in [("plain lines", ""), ("lines led by a BOM", "\u{FEFF}")] {
        let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
        initialize(&mut client).await;

        let held = REQUESTS_AT_ONCE + REQUESTS_WAITING;
        for id in 1..=held {
            let line = tool_call_line(
                id,
                "wait_for_status",
                json!({"window_id": window_id, "target_status": "idle", "timeout_sec": 600}),
                false,
            );
            client.request_raw(&format!("{lead}{line}")).await;
        }
        let past = tool_call_line(held + 1, "list_windows", json!({}), false);
        let seq = client.request_raw(&format!("{lead}{past}")).await;
        // rmcp answers a ping itself: answered before the request past the
        // limit, that request was taken in rather than refused.
        let sentinel = client
            .request(&json!({"jsonrpc": "2.0", "id": held + 2, "method": "ping"}))
            .await;
        let mut before = Vec::new();
        let answer = loop {
            match client.next_frame().await {
                RelayFrame::Out {
                    line,
                    seq: answered,
                    ..
                } => {
                    let value: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|err| panic!("relay_out must carry JSON ({err}): {line}"));
                    if answered == Some(seq) {
                        break value;
                    }
                    assert_ne!(
                        answered,
                        Some(sentinel),
                        "{what}: the request past the limit must be refused at once, but the \
                         ping sent after it was answered first: {value}"
                    );
                    before.push(value);
                }
                other => panic!("expected relay_out while waiting for seq {seq}, got {other:?}"),
            }
        };

        let (word, text) = tool_call_error(&answer);
        assert_eq!(
            word, RESOURCE_EXHAUSTED,
            "{what}: the request past the limit must be refused, got {text}"
        );
        let answered_early: Vec<&Value> = before
            .iter()
            .filter(|line| line.get("id").and_then(Value::as_u64).is_some())
            .collect();
        assert!(
            answered_early.is_empty(),
            "{what}: the {held} held requests must still wait, got {answered_early:?}"
        );
    }

    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    initialize(&mut client).await;
    let past_the_limit = REQUESTS_AT_ONCE + REQUESTS_WAITING + 1;
    let mut unanswered = Vec::new();
    for id in 1..=past_the_limit {
        let mut line = tool_call_line(id, "list_windows", json!({}), false);
        let written = if id % 2 == 0 {
            format!("{id}.0")
        } else {
            format!("{id}e0")
        };
        line["id"] = json!("ID");
        let line = line.to_string().replacen("\"ID\"", &written, 1);
        let seq = client.request_raw(&line).await;
        let (answer, answered) = loop {
            match client.next_frame().await {
                RelayFrame::Out {
                    line,
                    seq: answered,
                    ..
                } => {
                    let value: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|err| panic!("relay_out must carry JSON ({err}): {line}"));
                    if value["id"].as_f64() == written.parse::<f64>().ok() {
                        break (value, answered);
                    }
                }
                other => panic!("expected relay_out while waiting for id {written}, got {other:?}"),
            }
        };
        let refused = if answer["result"]["isError"] == true {
            Some(tool_call_error(&answer).0)
        } else if answer["result"].is_object() {
            None
        } else {
            Some(answer.to_string())
        };
        if answered != Some(seq) || refused.is_some() {
            unanswered.push((written, answered, refused));
        }
    }
    assert!(
        unanswered.is_empty(),
        "each of {past_the_limit} calls sent one after another under an integral float id must \
         be answered as its own sequence number and served; got {unanswered:?}"
    );
}

// =====================================================================
// Frames that break the contract
// =====================================================================

/// A `relay_in` frame as the IPC framing carries it.
fn relay_in_bytes(line: String, seq: Option<u64>) -> Vec<u8> {
    let body = serde_json::to_vec(&RelayFrame::In { line, seq }).unwrap();
    yagura_protocol::frame_encode(&body)
}

/// A `relay_in` line, and each line of a resumed opening, is one line: a
/// newline or a carriage return in it breaks the contract, whatever the
/// receiver would make of it.
#[tokio::test]
async fn only_a_frame_breaking_the_contract_closes_the_relay_connection() {
    let h = Harness::start("relay-bad-frames", config()).await;
    let tools_list =
        |id: u64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"}).to_string();

    let oversized = u32::try_from(MAX_FRAME_BODY_LEN + 1)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    let untyped = yagura_protocol::frame_encode(br#"{"line":"{}"}"#);
    let unreadable = yagura_protocol::frame_encode(b"not a relay frame");
    let two_requests = relay_in_bytes(format!("{}\n{}", tools_list(1), tools_list(2)), None);
    let carriage_return = relay_in_bytes(tools_list(1).replacen(',', ",\r", 1), Some(1));
    for (what, bytes) in [
        ("a frame over the size limit", oversized),
        ("a frame without a type", untyped),
        ("a body that is no relay frame", unreadable),
        ("a relay_in line holding two requests", two_requests),
        ("a relay_in line holding a carriage return", carriage_return),
    ] {
        let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
        initialize(&mut client).await;
        client.send_bytes(&bytes).await;
        client.expect_closed(what).await;
    }

    // The opening a session resumes is the first frame's: no session starts.
    let (tools, answer) = first_session(&h).await;
    let mut lines = opening(&answer);
    lines[2] = format!("{}\n{}", initialized_line(), tools_list(9));
    let mut client = RelayClient::connect(&h.endpoint).await;
    client
        .send(&hello(
            None,
            Some(RelayResume {
                opening: Some(lines),
                tools,
            }),
        ))
        .await;
    client
        .expect_closed("a relay_hello whose resumed opening holds a line with a newline")
        .await;
}

/// A line the CLI wrote that is not one JSON-RPC message does not end the
/// session, and is answered as rmcp answers it: nothing for a line that is
/// not JSON or that answers no request, an `Invalid Request` error without an
/// id for JSON that is no JSON-RPC message, an error-like line without
/// `jsonrpc` among them, and one whose `error` is malformed.
#[tokio::test]
async fn a_relay_in_line_that_is_not_json_does_not_end_the_session() {
    const INVALID_REQUEST_CODE: i64 = -32600;
    let h = Harness::start("relay-bad-line", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    initialize(&mut client).await;

    let null_id_error =
        json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "x"}})
            .to_string();
    let error_without_jsonrpc = json!({"error": {"code": 1, "message": "x"}}).to_string();
    let malformed_error = json!({"jsonrpc": "2.0", "error": "x"}).to_string();
    let cases: [(&str, &str, Option<i64>); 5] = [
        ("a line that is not JSON", "{not json", None),
        (
            "JSON that is no JSON-RPC message",
            "[1]",
            Some(INVALID_REQUEST_CODE),
        ),
        ("an error answer whose id is null", &null_id_error, None),
        (
            "an error-like line without jsonrpc",
            &error_without_jsonrpc,
            Some(INVALID_REQUEST_CODE),
        ),
        (
            "an error line whose error is malformed",
            &malformed_error,
            Some(INVALID_REQUEST_CODE),
        ),
    ];
    for (id, (what, line, expected)) in (1u64..).zip(cases) {
        client.carry(line).await;
        let seq = client
            .request(&tool_call_line(id, "list_windows", json!({}), false))
            .await;
        let (listed, before) = client.answer_with_preceding(seq).await;
        tool_json(&listed);
        let codes: Vec<Option<i64>> = before
            .iter()
            .map(|answer| answer["error"]["code"].as_i64())
            .collect();
        assert_eq!(
            codes,
            expected.map(Some).into_iter().collect::<Vec<_>>(),
            "{what} must be answered as rmcp answers it (an error of the code, or nothing), \
             got {before:?}"
        );
        assert!(
            before.iter().all(|answer| answer.get("id").is_none()),
            "{what}: an answer to a line whose id cannot be read carries no id, got {before:?}"
        );
    }
    expect_the_session_serves(&mut client, 6, false).await;
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

// =====================================================================
// A report wait_for_report took is gone only once the relay received it
// =====================================================================

/// Opens a parent window and a child it opened, and has the child report
/// `summary` to the parent; returns the parent's and the child's ids.
async fn report_to_parent(h: &Harness, summary: &str) -> (String, String) {
    let parent = h.open(process::echo_child_command(), None).await;
    let mut as_parent = h.connect().await;
    common::identify(&mut as_parent, &parent).await;
    let child = common::call_open_window(&mut as_parent, process::echo_child_command()).await;
    let mut as_child = h.connect().await;
    common::identify(&mut as_child, &child).await;
    common::send(
        &mut as_child,
        &yagura_protocol::ClientMessage::SendReport {
            outcome: yagura_protocol::ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    match common::read_response(&mut as_child).await {
        yagura_protocol::DaemonMessage::ReportSent { delivered_to, .. } => {
            assert_eq!(delivered_to.as_deref(), Some(parent.as_str()));
        }
        other => panic!("expected report_sent, got {other:?}"),
    }
    (parent, child)
}

/// A window's relay session opened with `initialize`.
pub(crate) async fn window_session(endpoint: &str, window_id: &str) -> RelayClient {
    let (mut client, _) =
        RelayClient::welcomed(endpoint, hello(Some((window_id, VALID_TOKEN)), None)).await;
    initialize(&mut client).await;
    client
}

/// What one `wait_for_report` answer carried.
pub(crate) struct Took {
    pub(crate) seq: u64,
    pub(crate) reports: Vec<Value>,
    pub(crate) receipt: Option<bool>,
}

/// How long a `wait_for_report` expecting a report to come back may park: it
/// returns as the report reaches the inbox, and this only bounds a report
/// that never does, below the reader's own [`HANG_GUARD`].
pub(crate) const COME_BACK_SECS: u64 = HANG_GUARD.as_secs() - 10;

/// Calls `wait_for_report` with `timeout_sec` and reads up to its answer,
/// acknowledging nothing: the caller decides what the relay says back.
pub(crate) async fn take_reports(client: &mut RelayClient, id: u64, timeout_sec: u64) -> Took {
    let seq = client
        .request(&tool_call_line(
            id,
            "wait_for_report",
            json!({"timeout_sec": timeout_sec}),
            false,
        ))
        .await;
    loop {
        match client.next_frame().await {
            RelayFrame::Out {
                line,
                seq: Some(answered),
                receipt,
            } if answered == seq => {
                let answer: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|err| panic!("relay_out must carry JSON ({err}): {line}"));
                let reports = tool_json(&answer)["reports"]
                    .as_array()
                    .unwrap_or_else(|| panic!("wait_for_report answers its reports, got {answer}"))
                    .clone();
                return Took {
                    seq,
                    reports,
                    receipt,
                };
            }
            RelayFrame::Out { .. } => {}
            other => panic!("expected relay_out while waiting for seq {seq}, got {other:?}"),
        }
    }
}

/// The `(from_window_id, summary, at_ms)` of each report.
pub(crate) fn report_keys(reports: &[Value]) -> Vec<(String, String, u64)> {
    reports
        .iter()
        .map(|report| {
            (
                report["from_window_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                report["summary"].as_str().unwrap_or_default().to_string(),
                report["at_ms"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

/// Returns once the daemon has read every frame sent on `client` before this
/// request (a session reads its frames in order), or has closed the
/// connection over one of them.
async fn after_frames(client: &mut RelayClient, id: u64) {
    let seq = client
        .request(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"}))
        .await;
    while let Some(frame) = client.next_frame_or_closed().await {
        if matches!(frame, RelayFrame::Out { seq: Some(answered), .. } if answered == seq) {
            return;
        }
    }
}

#[tokio::test]
async fn a_report_whose_answer_was_not_received_comes_again_and_one_received_does_not() {
    const MARK: &str = "RECEIPT-UNACKED-5Q1";
    let h = Harness::start("relay-receipt-unacked", config()).await;
    let (parent, child) = report_to_parent(&h, MARK).await;

    let mut first = window_session(&h.endpoint, &parent).await;
    let took = take_reports(&mut first, 1, 0).await;
    let keys = report_keys(&took.reports);
    assert_eq!(
        keys.iter()
            .map(|(from, summary, _)| (from.as_str(), summary.as_str()))
            .collect::<Vec<_>>(),
        [(child.as_str(), MARK)],
        "the first wait_for_report takes the report"
    );
    drop(first);

    let mut second = window_session(&h.endpoint, &parent).await;
    let again = take_reports(&mut second, 1, COME_BACK_SECS).await;
    assert_eq!(
        report_keys(&again.reports),
        keys,
        "a report whose answer was never received must come again, the same one"
    );
    assert_eq!(
        again.receipt,
        Some(true),
        "an answer carrying reports asks the relay for a receipt"
    );
    second.send(&RelayFrame::Done { seq: again.seq }).await;
    let after = take_reports(&mut second, 2, 0).await;
    assert!(
        after.reports.is_empty(),
        "a received report is not handed out again: {:?}",
        after.reports
    );
    drop(second);

    let mut third = window_session(&h.endpoint, &parent).await;
    let later = take_reports(&mut third, 1, 0).await;
    assert!(
        later.reports.is_empty(),
        "a received report is gone for the next session too: {:?}",
        later.reports
    );
}

#[tokio::test]
async fn a_report_whose_answer_the_relay_dropped_comes_again_in_the_same_session() {
    const MARK: &str = "RECEIPT-DROPPED-8T2";
    let h = Harness::start("relay-receipt-dropped", config()).await;
    let (parent, _) = report_to_parent(&h, MARK).await;

    let mut session = window_session(&h.endpoint, &parent).await;
    let took = take_reports(&mut session, 1, 0).await;
    let keys = report_keys(&took.reports);
    assert_eq!(keys.len(), 1, "the first wait_for_report takes the report");
    session.send(&RelayFrame::Dropped { seq: took.seq }).await;

    let again = take_reports(&mut session, 2, COME_BACK_SECS).await;
    assert_eq!(
        report_keys(&again.reports),
        keys,
        "a report whose answer the relay dropped must come again, the same one"
    );
}

/// `relay_done` and `relay_dropped` name a sequence number of their own
/// session: another window's session, or another session of the same window,
/// using the same number neither releases nor removes the report.
#[tokio::test]
async fn a_receipt_from_another_session_neither_releases_nor_removes_a_carried_report() {
    const MARK: &str = "RECEIPT-FOREIGN-2M7";
    let h = Harness::start("relay-receipt-foreign", config()).await;
    let (parent, _) = report_to_parent(&h, MARK).await;
    let other_window = h.open(process::echo_child_command(), None).await;

    let mut carrier = window_session(&h.endpoint, &parent).await;
    let took = take_reports(&mut carrier, 1, 0).await;
    let keys = report_keys(&took.reports);
    assert_eq!(keys.len(), 1, "the first wait_for_report takes the report");

    for (what, window_id) in [
        ("another window's session", other_window.as_str()),
        ("another session of the same window", parent.as_str()),
    ] {
        let mut intruder = window_session(&h.endpoint, window_id).await;
        intruder.send(&RelayFrame::Dropped { seq: took.seq }).await;
        intruder.send(&RelayFrame::Done { seq: took.seq }).await;
        after_frames(&mut intruder, 90).await;

        let mut reader = window_session(&h.endpoint, &parent).await;
        let seen = take_reports(&mut reader, 1, 0).await;
        assert!(
            seen.reports.is_empty(),
            "a receipt from {what} must not hand out a report another session carries: {:?}",
            seen.reports
        );
    }

    drop(carrier);
    let mut next = window_session(&h.endpoint, &parent).await;
    let again = take_reports(&mut next, 1, COME_BACK_SECS).await;
    assert_eq!(
        report_keys(&again.reports),
        keys,
        "a receipt from another session must not remove the report the carrier never received"
    );
}

/// A take held between the inbox and its request's answer: the CLI cancels
/// the call there, so the request is gone before the take reaches it.
#[tokio::test]
async fn a_report_taken_and_cancelled_before_its_answer_comes_back_once() {
    const MARK: &str = "RECEIPT-CANCELLED-4R9";
    let faults = yagura_daemon::fault_point::FaultPoints::arm(
        yagura_daemon::fault_point::FaultPoint::ReportTakenUnanswered,
    );
    let h = Harness::start(
        "relay-receipt-cancelled",
        DaemonConfig {
            faults: faults.clone(),
            ..config()
        },
    )
    .await;
    let (parent, child) = report_to_parent(&h, MARK).await;

    let mut session = window_session(&h.endpoint, &parent).await;
    session
        .request(&tool_call_line(
            1,
            "wait_for_report",
            json!({"timeout_sec": 0}),
            false,
        ))
        .await;
    tokio::time::timeout(HANG_GUARD, faults.reached())
        .await
        .expect("the take must reach the point before its answer");
    session.carry(&cancelled_line(1).to_string()).await;
    after_frames(&mut session, 2).await;
    faults.release();

    let again = take_reports(&mut session, 3, COME_BACK_SECS).await;
    let keys = report_keys(&again.reports);
    assert_eq!(
        keys.iter()
            .map(|(from, summary, _)| (from.as_str(), summary.as_str()))
            .collect::<Vec<_>>(),
        [(child.as_str(), MARK)],
        "a report taken for a call cancelled before its answer must come back"
    );
    session.send(&RelayFrame::Done { seq: again.seq }).await;
    let after = take_reports(&mut session, 4, 0).await;
    assert!(
        after.reports.is_empty(),
        "the report that came back is handed out once: {:?}",
        after.reports
    );
}

// =====================================================================
// A tool call's arguments and result stay out of the daemon's log
// =====================================================================

/// What a window's screen shows, read over `client`.
async fn screen_of(client: &mut yagura_test_support::ipc::Connection, window_id: &str) -> String {
    common::send(
        client,
        &yagura_protocol::ClientMessage::ReadOutput {
            window_id: window_id.to_string(),
            lines: Some(500),
        },
    )
    .await;
    match common::read_response(client).await {
        yagura_protocol::DaemonMessage::WindowOutput { text, .. } => text,
        other => panic!("expected window_output, got {other:?}"),
    }
}

#[tokio::test]
async fn a_tool_calls_arguments_and_result_stay_out_of_a_debug_daemon_log() {
    // Carried by a report: its summary reaches the parent only as a tool result, so
    // no window's screen ever shows it.
    const NONCE: &str = "NONCE-RMCP-LOG-7Hk2Qx9";
    let sandbox = common::real_daemon::Sandbox::new("relay-rmcp-log");
    let mut daemon = sandbox
        .start_from(&[
            ("YAGURA_LOG", "debug"),
            ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
        ])
        .await;
    let endpoint = sandbox.endpoint().to_string();

    let parent = common::call_open_window(&mut daemon.client, process::echo_child_command()).await;
    let mut as_parent = yagura_test_support::ipc::connect_with_retry(&endpoint).await;
    common::identify(&mut as_parent, &parent).await;
    let child = common::call_open_window(&mut as_parent, process::echo_child_command()).await;

    let mut child_session = window_session(&endpoint, &child).await;
    let sent = child_session
        .request(&tool_call_line(
            1,
            "send_report",
            json!({"outcome": "succeeded", "summary": NONCE}),
            false,
        ))
        .await;
    let answer = child_session.answer(sent).await;
    assert_eq!(
        tool_json(&answer)["delivered_to"].as_str(),
        Some(parent.as_str()),
        "the report must reach the parent"
    );

    let mut parent_session = window_session(&endpoint, &parent).await;
    let took = take_reports(&mut parent_session, 1, 0).await;
    assert_eq!(
        report_keys(&took.reports)
            .iter()
            .map(|(_, summary, _)| summary.as_str())
            .collect::<Vec<_>>(),
        [NONCE],
        "the parent's wait_for_report must carry the report"
    );

    for window in [&parent, &child] {
        let screen = screen_of(&mut daemon.client, window).await;
        assert!(
            !screen.contains(NONCE),
            "the nonce must not be drawn on window {window}'s screen: {screen:?}"
        );
    }

    // No debug line is certain at startup, and a window that draws nothing
    // never runs the detector: typing a mark makes the parent's screen change
    // on every OS, and the detector's debug line naming it shows the level held.
    const MARK: &str = "DEBUG-LEVEL-MARK-3Fv8";
    let mut typist = yagura_test_support::ipc::connect_with_retry(&endpoint).await;
    common::attach(&mut typist, &parent).await;
    common::write_interactive_input(&mut typist, MARK).await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while !sandbox
        .daemon_log()
        .lines()
        .any(|line| line.contains("DEBUG") && line.contains(MARK))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the detector must log the typed mark at debug level: {:?}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    drop((child_session, parent_session, as_parent, typist));
    let mut shutdown = sandbox.spawn_with_args(&["--shutdown"]);
    let ended = daemon.wait_for_fault_end().await;
    let _ = tokio::task::spawn_blocking(move || shutdown.wait()).await;
    assert!(
        ended.success(),
        "the daemon must end cleanly, got {ended:?}"
    );

    let log = sandbox.daemon_log();
    assert!(
        log.contains("DEBUG"),
        "the daemon must have logged at debug level: {log:?}"
    );
    let hits: Vec<&str> = log.lines().filter(|line| line.contains(NONCE)).collect();
    assert!(
        hits.is_empty(),
        "a tool call's arguments and result must not reach the daemon log, found: {hits:#?}"
    );
}

/// rmcp ends a session whose first line answers no request it made, naming the
/// line it received; the daemon's warning of it names only the kind.
#[tokio::test]
async fn a_session_whose_first_line_is_an_answer_is_logged_without_the_line() {
    const NONCE: &str = "NONCE-INIT-ERROR-4Rt8Wm";
    const STOPPED: &str = "a relay session's MCP server stopped";
    let sandbox = common::real_daemon::Sandbox::new("relay-init-error-log");
    let _daemon = sandbox.start().await;

    let (mut client, _) = RelayClient::welcomed(sandbox.endpoint(), hello(None, None)).await;
    client
        .carry(&json!({"jsonrpc": "2.0", "id": 1, "result": {"n": NONCE}}).to_string())
        .await;
    client
        .expect_closed("a session whose MCP server stopped")
        .await;

    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let log = loop {
        let log = sandbox.daemon_log();
        if log.lines().any(|line| line.contains(STOPPED)) {
            break log;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon must warn that the session's MCP server stopped: {log:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let hits: Vec<&str> = log.lines().filter(|line| line.contains(NONCE)).collect();
    assert!(
        hits.is_empty(),
        "the line rmcp received must not reach the daemon log, found: {hits:#?}"
    );
    assert!(
        log.lines()
            .any(|line| line.contains(STOPPED) && line.contains("expected_initialize_request")),
        "the warning names why the session's MCP server stopped: {log:?}"
    );
}
