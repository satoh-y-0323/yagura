#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The compiled relay between a CLI and the daemon, driven over its stdio as a
//! CLI drives it, against a stand-in daemon that speaks the daemon's side of
//! the relay contract: what the relay carries, whom it names, how it answers
//! the requests a lost connection or an absent daemon leaves open, how it acts
//! on each kind of refusal, and that a window's relay catches up with a daemon
//! that becomes reachable without any request from the CLI.
//!
//! Every case binds the endpoint of an instance reserved for it before the
//! relay starts, so the relay can only ever reach the stand-in.

use std::collections::BTreeSet;
use std::ffi::OsStr;

use serde_json::{Value, json};
use yagura_protocol::relay::{
    DAEMON_RESTARTED, DAEMON_UNAVAILABLE, INVALID_REQUEST, RELAY_REFUSED, RESOURCE_EXHAUSTED,
    RelayFrame, RelayRefusedKind, RelayResume,
};
use yagura_protocol::{MAX_FRAME_BODY_LEN, STATUS_TOKEN_ENV, WINDOW_ID_ENV};
use yagura_test_support::paths::TempDir;

use crate::common;

use common::{
    FakeDaemon, Hello, RelayProcess, answer_line, cancelled_line, initialize_line,
    initialized_line, ping_line, rpc_error_word, spawn_relay, tool_call_error, tool_call_line,
    tools_list_line,
};

const WINDOW: &str = "w-relay";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn window_relay(instance: &str) -> RelayProcess {
    spawn_relay(
        instance,
        &[(WINDOW_ID_ENV, WINDOW), (STATUS_TOKEN_ENV, TOKEN)],
        &[],
    )
}

fn assert_names_the_window(hello: &Hello) {
    assert_eq!(hello.window_id.as_deref(), Some(WINDOW), "{hello:?}");
    assert_eq!(hello.token.as_deref(), Some(TOKEN), "{hello:?}");
}

fn assert_names_no_window(hello: &Hello, situation: &str) {
    assert_eq!(
        (hello.window_id.as_deref(), hello.token.as_deref()),
        (None, None),
        "{situation} must not be named as a window: {hello:?}"
    );
}

/// Every answer to the requests `ids`, by id, in whatever order they come;
/// any other line on the way fails the case.
async fn answers(relay: &mut RelayProcess, ids: &[u64]) -> Vec<Value> {
    let mut got: Vec<Option<Value>> = vec![None; ids.len()];
    while got.iter().any(Option::is_none) {
        let line = relay.next_line().await;
        let slot = line
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| ids.iter().position(|expected| *expected == id))
            .filter(|_| line.get("method").is_none())
            .unwrap_or_else(|| panic!("expected an answer to one of {ids:?}, got {line}"));
        assert!(got[slot].is_none(), "answered twice: {line}");
        got[slot] = Some(line);
    }
    got.into_iter().map(Option::unwrap).collect()
}

/// The ids of the requests among `lines`.
fn request_ids(lines: &[Value]) -> BTreeSet<u64> {
    lines
        .iter()
        .filter(|line| line.get("method").is_some())
        .filter_map(|line| line.get("id").and_then(Value::as_u64))
        .collect()
}

/// What a `daemon_restarted` answer must point the model to: the read paths
/// that tell whether a call took effect, and the
/// two tools whose second effect is not harmless (`send_keys`: look at the
/// screen instead of sending again; `send_message`: look, then call again if
/// it did not arrive).
fn assert_restart_guidance(text: &str) {
    for tool in [
        "send_keys",
        "send_message",
        "read_output",
        "list_windows",
        "get_window_status",
        "read_chat",
        "list_plans",
        "get_plan",
        "list_tickets",
        "get_ticket",
    ] {
        assert!(
            text.contains(tool),
            "the daemon_restarted answer must name {tool}; it reads: {text}"
        );
    }
}

/// A `tools/call` line carrying `pad` bytes of text: large enough that the
/// frame cannot be written whole into the connection's buffer.
fn padded_tool_call(id: u64, pad: usize) -> Value {
    tool_call_line(
        id,
        "send_message",
        json!({"window_id": "w-1", "text": "x".repeat(pad)}),
    )
}

// --- What a relay carries and whom it names ---------------------------------

#[tokio::test]
async fn a_relay_carries_each_cli_line_in_a_relay_in_and_writes_each_relay_out_line_to_stdout() {
    const INSTANCE: &str = "mcprelay-carry";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = spawn_relay(INSTANCE, &[], &[]);

    relay.send(&initialize_line(1)).await;
    let mut conn = daemon.accept().await;
    let hello = conn.hello().await;
    assert_names_no_window(&hello, "a relay without identity variables");
    assert_eq!(
        hello.resume, None,
        "a process's first connection resumes nothing"
    );
    conn.welcome("tools-1").await;
    let (request, first_seq) = conn.request().await;
    assert_eq!(
        request,
        initialize_line(1),
        "the line is carried as the CLI wrote it"
    );
    let answer = conn.answer(&request, first_seq, false).await;
    assert_eq!(relay.answer(&json!(1)).await, answer);

    relay.send(&initialized_line()).await;
    let (line, seq) = conn.relay_in().await;
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        initialized_line()
    );
    assert_eq!(seq, None, "a notification carries no seq");

    relay.send_raw("{this is not json").await;
    let (line, _) = conn.relay_in().await;
    assert_eq!(
        line, "{this is not json",
        "a line that is not JSON is carried as it is, for the daemon to answer"
    );

    relay.send(&tools_list_line(2)).await;
    let (request, seq) = conn.request().await;
    assert_eq!(request, tools_list_line(2));
    assert_ne!(seq, first_seq, "each request carries its own seq");
    let answer = conn.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(2)).await, answer);

    // A UTF-8 byte order mark before a request, which MCP readers skip, leaves
    // it a request.
    relay
        .send_raw(&format!("\u{FEFF}{}", tools_list_line(3)))
        .await;
    let (line, seq) = conn.relay_in().await;
    let seq = seq.unwrap_or_else(|| {
        panic!("a request led by a byte order mark is carried with a seq, got {line:?}")
    });
    let request: Value = serde_json::from_str(line.strip_prefix('\u{FEFF}').unwrap_or(&line))
        .unwrap_or_else(|err| panic!("the request is carried whole ({err}): {line:?}"));
    assert_eq!(request, tools_list_line(3));
    let answer = conn.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(3)).await, answer);
}

#[tokio::test]
async fn a_relay_started_with_both_identity_variables_names_that_window_before_any_line() {
    const INSTANCE: &str = "mcprelay-ident";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let _relay = window_relay(INSTANCE);

    let hello = daemon.accept().await.hello().await;
    assert_names_the_window(&hello);
}

// A registered entry outside a window can only spell its identity as empty.
#[tokio::test]
async fn a_relay_started_with_empty_identity_variables_names_no_window() {
    const INSTANCE: &str = "mcprelay-empty";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = spawn_relay(
        INSTANCE,
        &[(WINDOW_ID_ENV, ""), (STATUS_TOKEN_ENV, "")],
        &[],
    );
    relay.send(&initialize_line(1)).await;
    let hello = daemon.accept().await.hello().await;
    assert_names_no_window(&hello, "empty identity variables");
}

#[tokio::test]
async fn a_relay_given_a_credentials_file_names_that_window_before_any_line() {
    const INSTANCE: &str = "mcprelay-cred";
    let files = TempDir::new("yagura-mcp-credentials", "files");
    let path = files.path().join("credentials.json");
    std::fs::write(
        &path,
        json!({"window_id": WINDOW, "token": TOKEN}).to_string(),
    )
    .expect("write the credentials file");
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let _relay = spawn_relay(
        INSTANCE,
        &[],
        &[OsStr::new("--credentials"), path.as_os_str()],
    );

    let hello = daemon.accept().await.hello().await;
    assert_names_the_window(&hello);
}

#[tokio::test]
async fn a_relay_given_a_missing_credentials_file_names_no_window() {
    const INSTANCE: &str = "mcprelay-nocred";
    let files = TempDir::new("yagura-mcp-credentials", "files");
    let path = files.path().join("absent.json");
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = spawn_relay(
        INSTANCE,
        &[],
        &[OsStr::new("--credentials"), path.as_os_str()],
    );
    relay.send(&initialize_line(1)).await;
    let hello = daemon.accept().await.hello().await;
    assert_names_no_window(&hello, "a missing credentials file");
}

// --- the requests a lost connection leaves open --------------------------

#[tokio::test]
async fn a_sent_tool_call_is_answered_daemon_restarted_once_the_connection_ends_without_waiting_for_a_daemon()
 {
    const INSTANCE: &str = "mcprelay-r2call";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;

    relay
        .send(&tool_call_line(
            5,
            "send_keys",
            json!({"window_id": "w-1", "keys": ["enter"]}),
        ))
        .await;
    conn.request().await;
    // The relay's next attempts reach the stand-in, which never welcomes them.
    drop(conn);

    let (word, text) = tool_call_error(&relay.answer(&json!(5)).await);
    assert_eq!(word, DAEMON_RESTARTED, "{text}");
    assert_restart_guidance(&text);
}

#[tokio::test]
async fn a_tool_call_whose_relay_in_was_cut_while_being_written_is_answered_daemon_restarted() {
    const INSTANCE: &str = "mcprelay-r2cut";
    const PAD: usize = 8 * 1024 * 1024;
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;

    relay.send(&padded_tool_call(6, PAD)).await;
    assert!(
        conn.next_declared_length().await > PAD,
        "the relay starts writing the padded relay_in"
    );
    drop(conn);

    let (word, text) = tool_call_error(&relay.answer(&json!(6)).await);
    assert_eq!(word, DAEMON_RESTARTED, "{text}");
}

#[tokio::test]
async fn a_window_relays_sent_initialize_and_tools_list_are_sent_again_once_reconnected() {
    const INSTANCE: &str = "mcprelay-r2again";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut first = daemon.accept().await;
    first.hello().await;
    first.welcome("tools-1").await;
    relay.send(&initialize_line(1)).await;
    first.request().await;
    drop(first);

    let mut second = daemon.accept().await;
    let hello = second.hello().await;
    assert_names_the_window(&hello);
    assert_eq!(
        hello.resume.as_ref().map(|resume| resume.tools.as_str()),
        Some("tools-1"),
        "a later connection resumes with the previous welcome's tools"
    );
    second.welcome("tools-1").await;
    let (request, seq) = second.request().await;
    assert_eq!(request, initialize_line(1), "the initialize is sent again");
    let init_answer = second.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(1)).await, init_answer);
    relay.send(&initialized_line()).await;
    second.relay_in().await;
    relay.send(&tools_list_line(2)).await;
    second.request().await;
    drop(second);

    let mut third = daemon.accept().await;
    let hello = third.hello().await;
    let opening: Vec<Value> = hello
        .resume
        .as_ref()
        .and_then(|resume| resume.opening.clone())
        .unwrap_or_else(|| {
            panic!("the CLI opened with initialize, so the relay resumes it: {hello:?}")
        })
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        opening,
        vec![initialize_line(1), init_answer, initialized_line()],
        "the opening is the CLI's initialize, its answer and its initialized"
    );
    third.welcome("tools-1").await;
    let (request, seq) = third.request().await;
    assert_eq!(request, tools_list_line(2), "the tools/list is sent again");
    let answer = third.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(2)).await, answer);
}

#[tokio::test]
async fn a_relay_outside_a_window_answers_every_sent_request_daemon_restarted_once_the_connection_ends()
 {
    const INSTANCE: &str = "mcprelay-r2nowin";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = spawn_relay(INSTANCE, &[], &[]);
    relay.send(&initialize_line(1)).await;
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;
    let (request, seq) = conn.request().await;
    conn.answer(&request, seq, false).await;
    relay.answer(&json!(1)).await;

    relay.send(&tools_list_line(2)).await;
    relay
        .send(&tool_call_line(3, "list_windows", json!({})))
        .await;
    conn.request().await;
    conn.request().await;
    drop(conn);

    let got = answers(&mut relay, &[2, 3]).await;
    assert_eq!(rpc_error_word(&got[0]), DAEMON_RESTARTED, "{}", got[0]);
    let (word, text) = tool_call_error(&got[1]);
    assert_eq!(word, DAEMON_RESTARTED, "{text}");
}

// --- requests while no daemon is reached ------------------------------------

#[tokio::test]
async fn a_window_relay_holds_requests_while_no_daemon_is_reached_and_once_back_answers_held_tool_calls_unavailable()
 {
    const INSTANCE: &str = "mcprelay-r3hold";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    daemon.close_attempts(true);
    let mut relay = window_relay(INSTANCE);
    assert!(
        matches!(
            daemon.closed_attempt().await,
            Some(RelayFrame::Hello { .. })
        ),
        "the relay's first attempt names it"
    );

    relay.send(&initialize_line(1)).await;
    relay
        .send(&tool_call_line(
            2,
            "send_message",
            json!({"window_id": "w-1", "text": "hi"}),
        ))
        .await;
    relay.send(&tools_list_line(3)).await;
    relay
        .send(&tool_call_line(4, "list_windows", json!({})))
        .await;
    relay.send(&cancelled_line(4)).await;
    relay.send(&ping_line(5)).await;
    let (pong, before) = relay.answer_with_preceding(&json!(5)).await;
    assert_eq!(
        pong["result"],
        json!({}),
        "the relay answers a ping itself: {pong}"
    );
    assert!(
        before.is_empty(),
        "held requests are not answered yet: {before:?}"
    );

    daemon.close_attempts(false);
    let mut conn = daemon.accept().await;
    assert_names_the_window(&conn.hello().await);
    conn.welcome("tools-1").await;
    let mut carried = conn.answer_until(&[json!(1), json!(3)]).await;
    let got = answers(&mut relay, &[1, 2, 3]).await;
    assert_eq!(
        got[0],
        answer_line(&initialize_line(1)),
        "the held initialize is sent"
    );
    let (word, text) = tool_call_error(&got[1]);
    assert_eq!(
        word, DAEMON_UNAVAILABLE,
        "a held tools/call is answered, not sent: {text}"
    );
    assert_eq!(
        got[2],
        answer_line(&tools_list_line(3)),
        "the held tools/list is sent"
    );

    relay
        .send(&tool_call_line(6, "list_windows", json!({})))
        .await;
    carried.extend(conn.answer_until(&[json!(6)]).await);
    let (answer, before) = relay.answer_with_preceding(&json!(6)).await;
    assert_eq!(
        answer,
        answer_line(&tool_call_line(6, "list_windows", json!({})))
    );
    assert!(
        before.is_empty(),
        "the cancelled held request is never answered: {before:?}"
    );
    let ids = request_ids(&carried);
    assert!(
        !ids.contains(&2) && !ids.contains(&4),
        "a held tools/call, cancelled or not, is never sent: {carried:?}"
    );
    assert!(
        !carried
            .iter()
            .any(|line| line["method"] == "notifications/cancelled"),
        "the cancel of a held request is not sent: {carried:?}"
    );
}

#[tokio::test]
async fn a_window_relay_answers_the_request_past_the_sixteen_it_holds_resource_exhausted() {
    const INSTANCE: &str = "mcprelay-r3full";
    const HELD: u64 = 16;
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    daemon.close_attempts(true);
    let mut relay = window_relay(INSTANCE);
    daemon.closed_attempt().await;

    for id in 1..=HELD + 1 {
        relay
            .send(&tool_call_line(id, "list_windows", json!({})))
            .await;
    }
    let (answer, before) = relay.answer_with_preceding(&json!(HELD + 1)).await;
    let (word, text) = tool_call_error(&answer);
    assert_eq!(word, RESOURCE_EXHAUSTED, "{text}");
    assert!(
        before.is_empty(),
        "the sixteen held are not answered: {before:?}"
    );
}

#[tokio::test]
async fn a_relay_outside_a_window_answers_at_once_daemon_unavailable_while_no_daemon_is_reached() {
    const INSTANCE: &str = "mcprelay-r3nowin";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    daemon.close_attempts(true);
    let mut relay = spawn_relay(INSTANCE, &[], &[]);

    relay.send(&initialize_line(1)).await;
    assert_eq!(
        rpc_error_word(&relay.answer(&json!(1)).await),
        DAEMON_UNAVAILABLE
    );
    relay
        .send(&tool_call_line(2, "list_windows", json!({})))
        .await;
    let (word, text) = tool_call_error(&relay.answer(&json!(2)).await);
    assert_eq!(word, DAEMON_UNAVAILABLE, "{text}");
    match daemon.closed_attempt().await {
        Some(RelayFrame::Hello {
            window_id: None,
            token: None,
            ..
        }) => {}
        other => panic!("the relay tried the endpoint, naming no window: {other:?}"),
    }
}

#[tokio::test]
async fn a_relay_outside_a_window_opened_with_initialize_resumes_its_opening_on_its_next_connection()
 {
    const INSTANCE: &str = "mcprelay-r3open";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = spawn_relay(INSTANCE, &[], &[]);
    relay.send(&initialize_line(1)).await;
    let mut first = daemon.accept().await;
    first.hello().await;
    first.welcome("tools-1").await;
    let (request, seq) = first.request().await;
    let init_answer = first.answer(&request, seq, false).await;
    relay.answer(&json!(1)).await;
    relay.send(&initialized_line()).await;
    first.relay_in().await;
    relay
        .send(&tool_call_line(2, "list_windows", json!({})))
        .await;
    let (request, seq) = first.request().await;
    first.answer(&request, seq, false).await;
    relay.answer(&json!(2)).await;
    // The daemon is replaced while a call is in flight, which the relay learns
    // by answering it.
    relay
        .send(&tool_call_line(3, "list_windows", json!({})))
        .await;
    first.request().await;
    drop(first);
    relay.answer(&json!(3)).await;

    relay
        .send(&tool_call_line(4, "list_windows", json!({})))
        .await;
    let mut second = daemon.accept().await;
    let hello = second.hello().await;
    assert_names_no_window(&hello, "a relay without identity variables");
    let resume = hello
        .resume
        .clone()
        .unwrap_or_else(|| panic!("a later connection resumes: {hello:?}"));
    let opening: Vec<Value> = resume
        .opening
        .clone()
        .unwrap_or_else(|| panic!("the CLI opened with initialize: {resume:?}"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        (opening, resume.tools.as_str()),
        (
            vec![initialize_line(1), init_answer, initialized_line()],
            "tools-1"
        ),
        "the second relay_hello resumes the CLI's opening and the previous tools"
    );
    second.welcome("tools-1").await;
    let (request, seq) = second.request().await;
    assert_eq!(request, tool_call_line(4, "list_windows", json!({})));
    let answer = second.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(4)).await, answer);
}

// --- each kind of refusal -----------------------------------------------

#[tokio::test]
async fn after_a_limit_refusal_the_next_cli_request_names_the_relay_once_more() {
    const INSTANCE: &str = "mcprelay-r4limit";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut first = daemon.accept().await;
    first.hello().await;
    first.refuse(RelayRefusedKind::Limit, vec![1]).await;
    drop(first);

    relay
        .send(&tool_call_line(1, "list_windows", json!({})))
        .await;
    let mut second = daemon.accept().await;
    assert_names_the_window(&second.hello().await);
    second.refuse(RelayRefusedKind::Limit, vec![1]).await;
    drop(second);
    let (word, text) = tool_call_error(&relay.answer(&json!(1)).await);
    assert_eq!(word, RELAY_REFUSED, "{text}");

    relay
        .send(&tool_call_line(2, "list_windows", json!({})))
        .await;
    let mut third = daemon.accept().await;
    assert_names_the_window(&third.hello().await);
    third.welcome("tools-1").await;
    let (request, seq) = third.request().await;
    assert_eq!(request, tool_call_line(2, "list_windows", json!({})));
    let answer = third.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(2)).await, answer);
}

#[tokio::test]
async fn after_a_contract_refusal_requests_are_answered_at_once_and_the_relay_names_itself_again_once_that_connection_ends()
 {
    const INSTANCE: &str = "mcprelay-r4contr";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut first = daemon.accept().await;
    first.hello().await;
    first.refuse(RelayRefusedKind::Contract, vec![99]).await;

    relay
        .send(&tool_call_line(1, "list_windows", json!({})))
        .await;
    let (word, text) = tool_call_error(&relay.answer(&json!(1)).await);
    assert_eq!(word, RELAY_REFUSED, "{text}");
    relay.send(&initialize_line(2)).await;
    assert_eq!(
        rpc_error_word(&relay.answer(&json!(2)).await),
        RELAY_REFUSED
    );

    drop(first);
    let mut second = daemon.accept().await;
    assert_names_the_window(&second.hello().await);
}

#[tokio::test]
async fn after_a_session_mismatch_refusal_the_relay_stops_reading_and_ends() {
    const INSTANCE: &str = "mcprelay-r4mism";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut first = daemon.accept().await;
    first.hello().await;
    first.welcome("tools-1").await;
    relay.send(&initialize_line(1)).await;
    let (request, seq) = first.request().await;
    first.answer(&request, seq, false).await;
    relay.answer(&json!(1)).await;
    relay.send(&initialized_line()).await;
    first.relay_in().await;
    drop(first);

    let mut second = daemon.accept().await;
    second.hello().await;
    second
        .refuse(RelayRefusedKind::SessionMismatch, vec![1])
        .await;
    drop(second);
    // The CLI's stdin stays open: the relay ends by itself.
    relay.exited().await;
}

// --- cancelled answers, receipts, sizes ---------------------------------

#[tokio::test]
async fn a_cancelled_requests_receipt_answer_is_not_written_and_is_reported_dropped_and_a_written_one_done()
 {
    const INSTANCE: &str = "mcprelay-r5recpt";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;

    relay
        .send(&tool_call_line(
            1,
            "wait_for_report",
            json!({"timeout_sec": 60}),
        ))
        .await;
    let (cancelled_request, cancelled_seq) = conn.request().await;
    relay.send(&cancelled_line(1)).await;
    let (line, seq) = conn.relay_in().await;
    assert_eq!(
        (serde_json::from_str::<Value>(&line).unwrap(), seq),
        (cancelled_line(1), None),
        "the cancel of a sent request is carried"
    );
    conn.answer(&cancelled_request, cancelled_seq, true).await;
    assert_eq!(
        conn.next_frame().await,
        RelayFrame::Dropped { seq: cancelled_seq },
        "the answer of a cancelled request is reported dropped"
    );

    relay
        .send(&tool_call_line(
            2,
            "wait_for_report",
            json!({"timeout_sec": 60}),
        ))
        .await;
    let (request, seq) = conn.request().await;
    let answer = conn.answer(&request, seq, true).await;
    let (written, before) = relay.answer_with_preceding(&json!(2)).await;
    assert_eq!(written, answer);
    assert!(
        before.is_empty(),
        "the cancelled request's answer is never written: {before:?}"
    );
    assert_eq!(
        conn.next_frame().await,
        RelayFrame::Done { seq },
        "a receipt answer written to the CLI is reported done"
    );
}

/// A line the relay cannot carry is too large for a frame, or holds a carriage
/// return: a `relay_in` line is one line, and the relay does not leave the
/// daemon's reader to split it.
#[tokio::test]
async fn a_line_the_relay_cannot_carry_is_answered_invalid_request_alone_and_other_requests_go_on()
{
    const INSTANCE: &str = "mcprelay-r5size";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);
    let mut conn = daemon.accept().await;
    conn.hello().await;
    conn.welcome("tools-1").await;

    relay
        .send(&tool_call_line(1, "list_windows", json!({})))
        .await;
    let (pending, pending_seq) = conn.request().await;

    relay.send(&padded_tool_call(2, MAX_FRAME_BODY_LEN)).await;
    let (word, text) = tool_call_error(&relay.answer(&json!(2)).await);
    assert_eq!(word, INVALID_REQUEST, "{}", &text[..text.len().min(200)]);
    relay
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {"pad": "x".repeat(MAX_FRAME_BODY_LEN)},
        }))
        .await;

    let with_carriage_return = |line: Value| line.to_string().replacen(',', ",\r", 1);
    relay
        .send_raw(&with_carriage_return(tool_call_line(
            4,
            "list_windows",
            json!({}),
        )))
        .await;
    relay
        .send_raw(&with_carriage_return(json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {},
        })))
        .await;

    relay
        .send(&tool_call_line(3, "list_windows", json!({})))
        .await;
    let (request, seq) = conn.request().await;
    assert_eq!(
        request["id"],
        json!(3),
        "none of the lines that cannot be carried is carried; the next request is"
    );
    let third = conn.answer(&request, seq, false).await;
    let first = conn.answer(&pending, pending_seq, false).await;
    let got = answers(&mut relay, &[1, 3, 4]).await;
    assert_eq!(got[..2], [first, third]);
    let (word, text) = tool_call_error(&got[2]);
    assert_eq!(word, INVALID_REQUEST, "{text}");
}

// --- a window's relay catches up by itself -----------------------------
//
// No CLI line is sent until the counted attempt is received: every attempt
// counted here is one the relay made on its own.

#[tokio::test]
async fn a_window_relay_whose_first_two_attempts_are_closed_names_itself_on_a_later_attempt() {
    const INSTANCE: &str = "mcprelay-r17st";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);

    for _ in 0..2 {
        let mut attempt = daemon.accept().await;
        attempt.hello().await;
        drop(attempt);
    }
    let mut conn = daemon.accept().await;
    assert_names_the_window(&conn.hello().await);
    conn.welcome("tools-1").await;

    relay
        .send(&tool_call_line(1, "list_windows", json!({})))
        .await;
    let (request, seq) = conn.request().await;
    let answer = conn.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(1)).await, answer);
}

#[tokio::test]
async fn a_window_relay_whose_connection_and_the_attempt_right_after_close_names_itself_on_a_later_attempt()
 {
    const INSTANCE: &str = "mcprelay-r17cut";
    let mut daemon = FakeDaemon::bind(INSTANCE).await;
    let mut relay = window_relay(INSTANCE);

    let mut first = daemon.accept().await;
    first.hello().await;
    first.welcome("tools-1").await;
    drop(first);
    let mut right_after = daemon.accept().await;
    right_after.hello().await;
    drop(right_after);

    let mut conn = daemon.accept().await;
    let hello = conn.hello().await;
    assert_names_the_window(&hello);
    assert_eq!(
        hello
            .resume
            .map(|RelayResume { tools, .. }| tools)
            .as_deref(),
        Some("tools-1"),
        "the later connection resumes with the previous welcome's tools"
    );
    conn.welcome("tools-1").await;

    relay
        .send(&tool_call_line(1, "list_windows", json!({})))
        .await;
    let (request, seq) = conn.request().await;
    let answer = conn.answer(&request, seq, false).await;
    assert_eq!(relay.answer(&json!(1)).await, answer);
}
