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

use rmcp::model::RequestId;
use rmcp::transport::async_rw::DEFAULT_MAX_LINE_LENGTH;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hook_events_door::open_launched;
use common::relay::{
    RelayClient, cancelled_line, discover_line, hello, hello_declaring, initialize_line,
    initialized_line, roots_list_changed_line, stateless_meta, tool_call_line, tool_json,
};
use common::{Harness, VALID_TOKEN, approver_base_config, policy};
use yagura_daemon::DaemonConfig;
use yagura_daemon::window::HooksRoute;
use yagura_protocol::MAX_FRAME_BODY_LEN;
use yagura_protocol::relay::{
    INVALID_REQUEST, RESOURCE_EXHAUSTED, RelayFrame, RelayRefusedKind, RelayResume,
    SUPPORTED_CONTRACTS, request_id,
};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::process;
use yagura_test_support::relay_stdio::{rpc_error_word, tool_call_error};

/// Well-formed, but not the token the harness gives its windows.
const WRONG_TOKEN: &str = "fedcba9876543210fedcba9876543210";

/// How many sessions one window may hold.
const SESSIONS_PER_WINDOW: usize = 4;

/// How many requests one session runs at once, and how many more may wait.
const REQUESTS_AT_ONCE: u64 = 4;
const REQUESTS_WAITING: u64 = 16;

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
                declaration: None,
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

/// A window opened from a profile whose hooks the daemon arranged is claimed only by a
/// relay that declares the window's own profile. One that declares another profile, or
/// none (a relay of an older registration), is answered as no window. A window whose hooks
/// the daemon did not arrange is claimed whatever its relay declares.
#[tokio::test]
async fn only_a_relay_declaring_the_windows_own_profile_claims_a_window_the_daemon_arranged_hooks_for()
 {
    const LAUNCHED_FROM: &str = "launched-cli";
    let h = Harness::start("relay-declared-profile", config()).await;
    let cases = [
        (
            "its own profile",
            HooksRoute::RegisteredEntries,
            Some(LAUNCHED_FROM),
            true,
        ),
        (
            "another profile",
            HooksRoute::RegisteredEntries,
            Some("other-cli"),
            false,
        ),
        ("no profile", HooksRoute::RegisteredEntries, None, false),
        (
            "another profile, hooks left out",
            HooksRoute::LeftOut,
            Some("other-cli"),
            false,
        ),
        (
            "no profile, hooks not arranged",
            HooksRoute::Unarranged,
            None,
            true,
        ),
    ];
    for (id, (what, hooks, declared, claimed)) in (1..).zip(cases) {
        let window_id = open_launched(&h, LAUNCHED_FROM, hooks, None).await;
        let spare = h.open(process::echo_child_command(), None).await;
        let first = match declared {
            Some(profile) => hello_declaring(&window_id, VALID_TOKEN, profile),
            None => hello(Some((&window_id, VALID_TOKEN)), None),
        };

        let (mut client, _) = RelayClient::welcomed(&h.endpoint, first).await;

        assert_eq!(
            mcp_connected(&h, &window_id),
            Some(claimed),
            "{what}: whether the relay claims the window"
        );
        if !claimed {
            initialize(&mut client).await;
            let seq = client
                .request(&tool_call_line(
                    id,
                    "close_window",
                    json!({"window_id": spare, "force": true}),
                    false,
                ))
                .await;
            let closed = tool_json(&client.answer(seq).await);
            assert_eq!(
                closed["requester"], "external",
                "{what}: the relay acts as no window, got {closed}"
            );
        }
    }
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

/// The session does not resume an opening it cannot open the same way: one
/// whose recorded protocol version or capabilities differ, or whose
/// `initialize` or `notifications/initialized` the session writes again past
/// the line rmcp reads, which rmcp would drop. It is refused before any
/// `relay_welcome`.
#[tokio::test]
async fn a_resumed_opening_the_session_cannot_open_the_same_way_is_refused_as_session_mismatch() {
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

    let mut long_initialize = initialize_line(0);
    long_initialize["params"]["_meta"] = json!({"numbers": "NUMBERS"});
    let long_initialize = written_again_past_the_line(&long_initialize);
    let read = serde_json::from_str::<Value>(&long_initialize).unwrap();
    assert!(
        matches!(
            rmcp::model::ClientJsonRpcMessage::deserialize(&read),
            Ok(rmcp::model::JsonRpcMessage::Request(sent))
                if matches!(sent.request, rmcp::model::ClientRequest::InitializeRequest(_))
        ),
        "the fixture: rmcp reads the long line as an initialize request"
    );
    let mut long_opening = opening(&answer);
    long_opening[0] = long_initialize;

    let mut long_initialized = initialized_line();
    long_initialized["params"] = json!({"_meta": {"numbers": "NUMBERS"}});
    let long_initialized = written_again_past_the_line(&long_initialized);
    let read = serde_json::from_str::<Value>(&long_initialized).unwrap();
    assert!(
        matches!(
            rmcp::model::ClientJsonRpcMessage::deserialize(&read),
            Ok(rmcp::model::JsonRpcMessage::Notification(sent))
                if matches!(
                    sent.notification,
                    rmcp::model::ClientNotification::InitializedNotification(_)
                )
        ),
        "the fixture: rmcp reads the long line as notifications/initialized"
    );
    let mut long_initialized_opening = opening(&answer);
    long_initialized_opening[2] = long_initialized;

    for (what, lines) in [
        (
            "a recorded protocolVersion that differs",
            opening(&other_version),
        ),
        (
            "recorded capabilities that differ",
            opening(&other_capabilities),
        ),
        (
            "an initialize written again past the line rmcp reads",
            long_opening,
        ),
        (
            "a notifications/initialized written again past the line rmcp reads",
            long_initialized_opening,
        ),
    ] {
        let resume = hello(
            None,
            Some(RelayResume {
                opening: Some(lines),
                tools: tools.clone(),
            }),
        );
        assert_fits_a_frame(&resume);
        let (mut client, first) = RelayClient::open(&h.endpoint, resume).await;
        assert!(
            matches!(first, RelayFrame::Refused { .. }),
            "{what} must be refused before any relay_welcome, got {first:?}"
        );
        let refused = expect_refused_then_closed(&mut client, first).await;
        assert_eq!(
            refused_kind(&refused),
            RelayRefusedKind::SessionMismatch,
            "{what} must be refused as session_mismatch"
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
/// it, counts the same. A cancel whose id is null, which rmcp reads as no
/// message and so cancels nothing, frees nothing either. Calls under one id,
/// which rmcp runs each as its own, count one by one. A request whose id is
/// an integer written as a float, which rmcp answers under the integer, leaves
/// the count once answered.
#[tokio::test]
async fn a_request_past_the_four_running_and_sixteen_waiting_is_answered_resource_exhausted() {
    let h = Harness::start("relay-request-limit", config()).await;
    // No detection: the window stays `Unknown`, which completes no wait.
    let window_id = h.open(process::echo_child_command(), None).await;

    for (what, lead, cancel_under_a_null_id, one_id) in [
        ("plain lines", "", false, false),
        ("lines led by a BOM", "\u{FEFF}", false, false),
        ("calls cancelled under a null id", "", true, false),
        ("calls under one id", "", false, true),
    ] {
        let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
        initialize(&mut client).await;

        let held = REQUESTS_AT_ONCE + REQUESTS_WAITING;
        let id_of = |id: u64| if one_id { 1 } else { id };
        for id in 1..=held {
            let line = tool_call_line(
                id_of(id),
                "wait_for_status",
                json!({"window_id": window_id, "target_status": "idle", "timeout_sec": 600}),
                false,
            );
            client.request_raw(&format!("{lead}{line}")).await;
        }
        if cancel_under_a_null_id {
            for id in 1..=held {
                let mut cancel = cancelled_line(id);
                cancel["id"] = Value::Null;
                client.carry(&cancel.to_string()).await;
            }
        }
        let past = tool_call_line(id_of(held + 1), "list_windows", json!({}), false);
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
/// `jsonrpc` among them, and one whose `error` is malformed. A request rmcp
/// cannot read takes no place among the session's tool calls.
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

    // Requests rmcp reads no request out of take no place among the tool
    // calls a session runs and holds, however many come.
    let past_the_limit = REQUESTS_AT_ONCE + REQUESTS_WAITING + 1;
    let mut answered_otherwise = Vec::new();
    for n in 0..past_the_limit {
        let id = 100 + n;
        let mut unreadable = tool_call_line(id, "list_windows", json!({}), false);
        let what = match n % 3 {
            0 => {
                unreadable.as_object_mut().unwrap().remove("jsonrpc");
                "a tools/call without jsonrpc"
            }
            1 => {
                unreadable["jsonrpc"] = json!("1.0");
                "a tools/call of another jsonrpc version"
            }
            _ => {
                unreadable["result"] = json!({});
                "a tools/call that also holds a result"
            }
        };
        client.request_raw(&unreadable.to_string()).await;
        // rmcp answers an unreadable line before it reads the next one.
        let ping = client
            .request(&json!({"jsonrpc": "2.0", "id": 200 + n, "method": "ping"}))
            .await;
        let (_, before) = client.answer_with_preceding(ping).await;
        let codes: Vec<Option<i64>> = before
            .iter()
            .map(|answer| answer["error"]["code"].as_i64())
            .collect();
        if codes != [Some(INVALID_REQUEST_CODE)] || before.iter().any(|a| a.get("id").is_some()) {
            answered_otherwise.push((what, before));
        }
    }
    let seq = client
        .request(&tool_call_line(300, "list_windows", json!({}), false))
        .await;
    let listed = client.answer(seq).await;
    assert_ne!(
        listed["result"]["isError"], true,
        "a tools/call after {past_the_limit} requests rmcp reads no request out of must be \
         served, not refused as if they ran, got {listed}"
    );
    tool_json(&listed);
    assert!(
        answered_otherwise.is_empty(),
        "each request rmcp reads no request out of must be answered as rmcp answers it (an \
         Invalid Request error without an id), got {answered_otherwise:?}"
    );
    expect_the_session_serves(&mut client, 6, false).await;
}

/// `template` with its `"NUMBERS"` string replaced by numbers in exponent
/// form: a line within the line rmcp reads, that the session writes again
/// (the numbers written out in full) past it.
fn written_again_past_the_line(template: &Value) -> String {
    let numbers = vec!["1e15"; 1_000_000].join(",");
    let line = template
        .to_string()
        .replacen("\"NUMBERS\"", &format!("[{numbers}]"), 1);
    let rewritten = serde_json::from_str::<Value>(&line).unwrap().to_string();
    assert!(
        line.len() < DEFAULT_MAX_LINE_LENGTH && rewritten.len() >= DEFAULT_MAX_LINE_LENGTH,
        "the fixture: a line of {} bytes, written again in {} bytes, against rmcp's line limit \
         of {DEFAULT_MAX_LINE_LENGTH}",
        line.len(),
        rewritten.len()
    );
    line
}

fn assert_fits_a_frame(frame: &RelayFrame) {
    let framed = serde_json::to_vec(frame).unwrap().len();
    assert!(
        framed <= MAX_FRAME_BODY_LEN,
        "the fixture: a frame of {framed} bytes against a frame limit of {MAX_FRAME_BODY_LEN}"
    );
}

/// A request the relay carries in one frame, and rmcp would read as the CLI
/// wrote it, but that the session writes again past the line rmcp reads, is
/// answered under the CLI's id as the relay answers a request it cannot
/// carry: a tools/call with an `isError` result, any other request with a
/// JSON-RPC error. The session goes on serving.
#[tokio::test]
async fn a_tool_call_written_again_past_the_line_rmcp_reads_is_answered_as_an_error() {
    const CLI_ID: u64 = 7;
    let h = Harness::start("relay-rewritten-long", config()).await;
    let (mut client, _) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    initialize(&mut client).await;
    let carried = |line: &str| {
        assert_fits_a_frame(&RelayFrame::In {
            line: line.to_string(),
            seq: Some(1),
        });
    };

    let line = written_again_past_the_line(&tool_call_line(
        CLI_ID,
        "list_windows",
        json!({"numbers": "NUMBERS"}),
        false,
    ));
    carried(&line);
    let seq = client.request_raw(&line).await;
    let answer = client.answer(seq).await;
    assert_eq!(
        answer["id"],
        json!(CLI_ID),
        "the answer goes back under the CLI's id, got {answer}"
    );
    let (word, text) = tool_call_error(&answer);
    assert_eq!(
        word, INVALID_REQUEST,
        "a tools/call rmcp cannot read as written again is refused as an invalid request, got \
         {text}"
    );

    let list = |id: u64, numbers: Value| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {"_meta": {"numbers": numbers}},
        })
    };
    let seq = client.request(&list(CLI_ID + 1, json!([1e15]))).await;
    let listed = client.answer(seq).await;
    assert!(
        listed["result"]["tools"].is_array(),
        "the fixture: rmcp answers the short tools/list as a request, got {listed}"
    );
    let line = written_again_past_the_line(&list(CLI_ID + 2, json!("NUMBERS")));
    carried(&line);
    let seq = client.request_raw(&line).await;
    let answer = client.answer(seq).await;
    assert_eq!(
        answer["id"],
        json!(CLI_ID + 2),
        "the answer goes back under the CLI's id, got {answer}"
    );
    assert_eq!(
        rpc_error_word(&answer),
        INVALID_REQUEST,
        "a request other than tools/call rmcp cannot read as written again is refused as an \
         invalid request, got {answer}"
    );
    expect_the_session_serves(&mut client, CLI_ID + 3, false).await;
}

/// The relay reads a JSON-RPC id (`request_id`) as the session's rmcp reads
/// one (`RequestId`): for each value, the same id written back, or none.
#[test]
fn the_relay_reads_each_request_id_as_rmcp_does() {
    let ids = [
        r#""call-1""#,
        "0",
        "-1",
        "-9223372036854775808",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "1.0",
        "-2.0",
        "1e0",
        "9007199254740991",
        "9007199254740991.0",
        "9007199254740992",
        "9007199254740992.0",
        "9007199254740993",
        "9007199254740993.0",
        "1.5",
        "true",
        "null",
        "[1]",
    ];
    let differ: Vec<(&str, Option<Value>, Option<Value>)> = ids
        .into_iter()
        .filter_map(|text| {
            let id: Value = serde_json::from_str(text).unwrap();
            let relay = request_id(&id);
            let rmcp = RequestId::deserialize(&id)
                .ok()
                .map(RequestId::into_json_value);
            (relay != rmcp).then_some((text, relay, rmcp))
        })
        .collect();
    assert!(
        differ.is_empty(),
        "the relay must read each id as rmcp does, (id, relay, rmcp): {differ:?}"
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

/// A relay connecting again for a CLI that opened without `initialize`
/// resumes with no opening; the CLI's next line may be a notification.
#[tokio::test]
async fn a_resumed_session_without_an_opening_whose_first_line_is_a_notification_keeps_the_session()
{
    let h = Harness::start("relay-first-resumed", config()).await;
    let (first, tools) = RelayClient::welcomed(&h.endpoint, hello(None, None)).await;
    drop(first);

    let (mut client, _) = RelayClient::welcomed(
        &h.endpoint,
        hello(
            None,
            Some(RelayResume {
                opening: None,
                tools,
            }),
        ),
    )
    .await;
    client.carry(&roots_list_changed_line().to_string()).await;
    expect_the_session_serves(&mut client, 1, true).await;
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

/// A cancel naming an id the session carries no request under cancels
/// nothing: the call rmcp runs under the session's number equal to that id
/// still answers, with the report it took.
#[tokio::test]
async fn a_cancel_of_an_id_no_request_was_sent_under_leaves_the_running_call_answered() {
    const MARK: &str = "CANCEL-UNKNOWN-6W3";
    let faults = yagura_daemon::fault_point::FaultPoints::arm(
        yagura_daemon::fault_point::FaultPoint::ReportTakenUnanswered,
    );
    let h = Harness::start(
        "relay-cancel-unknown",
        DaemonConfig {
            faults: faults.clone(),
            ..config()
        },
    )
    .await;
    let (parent, child) = report_to_parent(&h, MARK).await;

    let mut session = window_session(&h.endpoint, &parent).await;
    // `initialize` took the session's number 1, so this call runs under 2.
    let seq = session
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
    session.carry(&cancelled_line(2).to_string()).await;
    after_frames(&mut session, 3).await;
    faults.release();

    let answer = session.answer(seq).await;
    let reports = tool_json(&answer)["reports"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        report_keys(&reports)
            .iter()
            .map(|(from, summary, _)| (from.as_str(), summary.as_str()))
            .collect::<Vec<_>>(),
        [(child.as_str(), MARK)],
        "a cancel of an id the CLI sent no request under must leave its call answered, got {answer}"
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

// =====================================================================
// The process a bridge runs as (lineage)
// =====================================================================

/// A window whose profile declares its CLI's process image is claimed through a relay
/// only by a bridge its own CLI started: the daemon judges the process that connected,
/// as the relay's connection saw it, at the identify the relay makes on its behalf. A
/// bridge a CLI of the same image started inside the window acts as no window. A claim
/// from a process outside the window's CLI cannot be judged, so it is refused, but not as
/// a credential failure: the connection is not closed for it. A window whose profile
/// declares no image is claimed by its token alone, from any process.
#[tokio::test]
async fn a_bridge_claims_a_window_declaring_its_image_only_from_the_windows_own_cli() {
    use crate::hook_events_door::{By, LineageImage, lineage_daemon, open_from_profile};
    const DECLARED: &str = "lineage-bridge";
    const UNDECLARED: &str = "lineage-bridge-plain";
    let (h, instance, profiles_dir) = lineage_daemon().await;
    let image = LineageImage::install("bridge");
    let mut cli = image.write_profile(&profiles_dir, DECLARED, &instance, true, "");
    let mut plain = image.write_profile(&profiles_dir, UNDECLARED, &instance, false, "");
    let window_id = open_from_profile(&h, DECLARED).await;
    let plain_id = open_from_profile(&h, UNDECLARED).await;

    let nested = cli.bridge(By::Nested, DECLARED).await;
    assert!(
        nested.contains("\"result\""),
        "the nested CLI's bridge is served: {nested}"
    );
    assert_eq!(
        mcp_connected(&h, &window_id),
        Some(false),
        "the nested CLI's bridge does not claim the window"
    );
    cli.end_bridges().await;

    let mut outside = h.connect().await;
    for attempt in 1..=4 {
        common::send(
            &mut outside,
            &yagura_protocol::ClientMessage::Identify {
                window_id: window_id.clone(),
                token: VALID_TOKEN.to_string(),
                declaration: Some(yagura_protocol::Declaration {
                    profile: DECLARED.to_string(),
                }),
            },
        )
        .await;
        match common::read_response(&mut outside).await {
            yagura_protocol::DaemonMessage::Error { code, .. } => assert_ne!(
                code, "auth_failed",
                "attempt {attempt}: a claim that cannot be judged is no credential failure"
            ),
            other => panic!(
                "attempt {attempt}: a claim from outside the window's CLI must be refused, \
                 got {other:?}"
            ),
        }
    }
    assert_eq!(mcp_connected(&h, &window_id), Some(false));
    common::identify(&mut outside, &plain_id).await;

    let own = cli.bridge(By::Cli, DECLARED).await;
    assert!(
        own.contains("\"result\""),
        "the window's bridge is served: {own}"
    );
    assert_eq!(
        mcp_connected(&h, &window_id),
        Some(true),
        "the window's own bridge claims the window"
    );
    cli.end_bridges().await;

    cli.exit().await;
    plain.exit().await;
}
