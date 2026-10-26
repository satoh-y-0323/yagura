#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The ticket tools of the MCP bridge against a scripted transport: which
//! `ClientMessage` each tool sends, which required arguments it refuses before
//! any round trip, that it never re-implements the daemon's ticket validation,
//! and how the daemon's replies and error codes reach the caller.
//!
//! Arguments are built from JSON, the shape an MCP client actually sends, so
//! these tests fix the external argument names rather than the Rust types the
//! bridge happens to deserialize them into.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use yagura_mcp::ticket_tools::{
    AddTicketResultArgs, CreateTicketArgs, GetTicketArgs, ListTicketsArgs, SetMilestoneArgs,
    UpdateTicketArgs, add_ticket_result, create_ticket, get_ticket, list_tickets, set_milestone,
    update_ticket,
};
use yagura_mcp::tools::ToolError;
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    ClientMessage, DaemonMessage, MilestoneInfo, TicketConfigInfo, TicketFieldInfo, TicketInfo,
    TicketResultInfo, TicketStateInfo, TicketSummary,
};

/// Replays queued responses in order and records every request, so a test can
/// assert both what came back and whether the daemon was contacted.
struct MockTransport {
    responses: Mutex<Vec<Result<DaemonMessage, TransportError>>>,
    sent: Mutex<Vec<ClientMessage>>,
    call_count: AtomicUsize,
}

impl MockTransport {
    /// A transport whose `round_trip` panics, so a missing short-circuit fails
    /// loudly instead of passing silently.
    fn never_called() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn with_response(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            responses: Mutex::new(vec![response]),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_sent(&self) -> ClientMessage {
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .last()
            .expect("round_trip must have been called at least once")
            .clone()
    }
}

impl DaemonTransport for MockTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::Window("w-author".to_string())
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .push(msg);
        let mut responses = self
            .responses
            .lock()
            .expect("responses mutex must not be poisoned");
        if responses.is_empty() {
            panic!("MockTransport::round_trip called with no scripted response queued");
        }
        responses.remove(0)
    }
}

fn daemon_error(code: &str, message: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }
}

fn args<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("the tool arguments must deserialize")
}

fn field_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

// ---------------------------------------------------------------------
// Replies the daemon gives
// ---------------------------------------------------------------------

fn sample_milestone() -> MilestoneInfo {
    MilestoneInfo {
        name: "v1".to_string(),
        position: 3,
        title: "Version 1".to_string(),
        description: Some("The first release.".to_string()),
    }
}

/// A ticket with every optional field populated, so each field's appearance
/// in the reply is observable without depending on how an absent one is shown.
fn sample_ticket() -> TicketInfo {
    TicketInfo {
        number: 12,
        title: "Make the board".to_string(),
        body: "line one\nline two".to_string(),
        state: "on-hold".to_string(),
        milestone: Some("v1".to_string()),
        fields: field_map(&[("resume_trigger", "when the store lands")]),
        results: vec![TicketResultInfo {
            seq: 1,
            at: 1_700_000_000_000,
            text: "decided on a store".to_string(),
        }],
        plans: vec!["plan-7".to_string()],
        created_at: 1_699_000_000_000,
        updated_at: 1_700_000_000_500,
    }
}

fn sample_config() -> TicketConfigInfo {
    TicketConfigInfo {
        guide: "One ticket, one purpose.".to_string(),
        states: vec![TicketStateInfo {
            name: "on-hold".to_string(),
            description: "Paused.".to_string(),
            requires: vec!["resume_trigger".to_string()],
        }],
        fields: vec![TicketFieldInfo {
            name: "resume_trigger".to_string(),
            description: "What brings it back.".to_string(),
        }],
    }
}

fn sample_summary() -> TicketSummary {
    TicketSummary {
        number: 12,
        title: "Make the board".to_string(),
        state: "on-hold".to_string(),
        milestone: Some("v1".to_string()),
        updated_at: 1_700_000_000_500,
    }
}

fn ticket_detail() -> DaemonMessage {
    DaemonMessage::TicketDetail {
        ticket: sample_ticket(),
    }
}

fn ticket_list() -> DaemonMessage {
    DaemonMessage::TicketList {
        workspace_id: Some(4),
        config: Some(sample_config()),
        config_error: None,
        milestones: vec![sample_milestone()],
        tickets: vec![sample_summary()],
        truncated: true,
    }
}

fn milestone_set() -> DaemonMessage {
    DaemonMessage::MilestoneSet {
        milestone: sample_milestone(),
    }
}

/// Every field of `expected` reaches the caller's JSON under its own name.
fn assert_fields_reach_the_caller<T: serde::Serialize>(
    reply: &Value,
    expected: &T,
    names: &[&str],
) {
    let expected = serde_json::to_value(expected).expect("serialize the expected value");
    for name in names {
        assert_eq!(
            reply[*name], expected[*name],
            "{name} must reach the caller unchanged: {reply}"
        );
    }
}

// ---------------------------------------------------------------------
// The six tools, each called with a well-formed argument set
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum TicketTool {
    ListTickets,
    GetTicket,
    CreateTicket,
    UpdateTicket,
    AddTicketResult,
    SetMilestone,
}

const ALL_TICKET_TOOLS: [TicketTool; 6] = [
    TicketTool::ListTickets,
    TicketTool::GetTicket,
    TicketTool::CreateTicket,
    TicketTool::UpdateTicket,
    TicketTool::AddTicketResult,
    TicketTool::SetMilestone,
];

async fn call(tool: TicketTool, transport: &MockTransport) -> Result<Value, ToolError> {
    match tool {
        TicketTool::ListTickets => list_tickets(transport, args(json!({}))).await,
        TicketTool::GetTicket => get_ticket(transport, args(json!({ "ticket": 12 }))).await,
        TicketTool::CreateTicket => {
            create_ticket(transport, args(json!({ "title": "t", "body": "b" }))).await
        }
        TicketTool::UpdateTicket => {
            update_ticket(transport, args(json!({ "ticket": 12, "state": "done" }))).await
        }
        TicketTool::AddTicketResult => {
            add_ticket_result(transport, args(json!({ "ticket": 12, "text": "merged" }))).await
        }
        TicketTool::SetMilestone => {
            set_milestone(
                transport,
                args(json!({ "name": "v1", "title": "Version 1", "position": 3 })),
            )
            .await
        }
    }
}

/// A reply of a kind no ticket tool asks for, so it is a contract violation
/// for every one of them.
fn reply_no_ticket_tool_expects() -> DaemonMessage {
    DaemonMessage::WindowClosed {
        window_id: "w-1".to_string(),
    }
}

// ---------------------------------------------------------------------
// Which ClientMessage each tool sends
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_tickets_sends_the_state_and_milestone_it_was_given_and_names_no_workspace() {
    let transport = MockTransport::with_response(Ok(ticket_list()));

    list_tickets(
        &transport,
        args::<ListTicketsArgs>(json!({ "state": "todo", "milestone": "v1" })),
    )
    .await
    .expect("list_tickets must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::ListTickets {
            workspace_id: None,
            state: Some("todo".to_string()),
            milestone: Some("v1".to_string()),
            limit: None,
        }
    );
}

#[tokio::test]
async fn list_tickets_without_arguments_asks_for_everything() {
    let transport = MockTransport::with_response(Ok(ticket_list()));

    list_tickets(&transport, args::<ListTicketsArgs>(json!({})))
        .await
        .expect("list_tickets must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ListTickets {
            workspace_id: None,
            state: None,
            milestone: None,
            limit: None,
        }
    );
}

#[tokio::test]
async fn get_ticket_sends_the_ticket_number_and_names_no_workspace() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    get_ticket(&transport, args::<GetTicketArgs>(json!({ "ticket": 12 })))
        .await
        .expect("get_ticket must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::GetTicket {
            workspace_id: None,
            ticket: 12,
        }
    );
}

#[tokio::test]
async fn create_ticket_sends_every_argument_verbatim() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    create_ticket(
        &transport,
        args::<CreateTicketArgs>(json!({
            "title": "Make the board",
            "body": "line one\nline two",
            "milestone": "v1",
            "state": "on-hold",
            "fields": { "resume_trigger": "when the store lands" },
        })),
    )
    .await
    .expect("create_ticket must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::CreateTicket {
            title: "Make the board".to_string(),
            body: "line one\nline two".to_string(),
            milestone: Some("v1".to_string()),
            state: Some("on-hold".to_string()),
            fields: Some(field_map(&[("resume_trigger", "when the store lands")])),
        }
    );
}

#[tokio::test]
async fn create_ticket_without_the_optional_arguments_sends_none_of_them() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    create_ticket(
        &transport,
        args::<CreateTicketArgs>(json!({ "title": "t", "body": "b" })),
    )
    .await
    .expect("create_ticket must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::CreateTicket {
            title: "t".to_string(),
            body: "b".to_string(),
            milestone: None,
            state: None,
            fields: None,
        }
    );
}

#[tokio::test]
async fn update_ticket_sends_only_what_it_was_given() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    update_ticket(
        &transport,
        args::<UpdateTicketArgs>(json!({ "ticket": 12, "state": "done" })),
    )
    .await
    .expect("update_ticket must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::UpdateTicket {
            ticket: 12,
            title: None,
            body: None,
            milestone: None,
            state: Some("done".to_string()),
            fields: None,
        }
    );
}

#[tokio::test]
async fn update_ticket_forwards_an_empty_milestone_and_an_empty_field_value_as_removals() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    update_ticket(
        &transport,
        args::<UpdateTicketArgs>(json!({
            "ticket": 12,
            "milestone": "",
            "fields": { "resume_trigger": "" },
        })),
    )
    .await
    .expect("an empty milestone or field value takes it away; it is not a missing argument");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::UpdateTicket {
            ticket: 12,
            title: None,
            body: None,
            milestone: Some(String::new()),
            state: None,
            fields: Some(field_map(&[("resume_trigger", "")])),
        }
    );
}

#[tokio::test]
async fn add_ticket_result_sends_the_ticket_and_the_text() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    add_ticket_result(
        &transport,
        args::<AddTicketResultArgs>(json!({ "ticket": 12, "text": "merged as abc123" })),
    )
    .await
    .expect("add_ticket_result must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::AddTicketResult {
            ticket: 12,
            text: "merged as abc123".to_string(),
        }
    );
}

#[tokio::test]
async fn set_milestone_sends_every_argument_verbatim() {
    let transport = MockTransport::with_response(Ok(milestone_set()));

    set_milestone(
        &transport,
        args::<SetMilestoneArgs>(json!({
            "name": "v1",
            "title": "Version 1",
            "description": "The first release.",
            "position": 3,
        })),
    )
    .await
    .expect("set_milestone must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::SetMilestone {
            name: "v1".to_string(),
            title: "Version 1".to_string(),
            description: Some("The first release.".to_string()),
            position: 3,
        }
    );
}

#[tokio::test]
async fn set_milestone_without_a_description_sends_none() {
    let transport = MockTransport::with_response(Ok(milestone_set()));

    set_milestone(
        &transport,
        args::<SetMilestoneArgs>(json!({ "name": "v1", "title": "Version 1", "position": -2 })),
    )
    .await
    .expect("set_milestone must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::SetMilestone {
            name: "v1".to_string(),
            title: "Version 1".to_string(),
            description: None,
            position: -2,
        }
    );
}

// ---------------------------------------------------------------------
// Line endings: the daemon refuses a bare CR, so a caller that wrote its
// request on Windows must not be refused for that alone.
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_ticket_normalizes_crlf_and_lone_cr_in_the_body_and_field_values_to_lf() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    create_ticket(
        &transport,
        args::<CreateTicketArgs>(json!({
            "title": "t",
            "body": "a\r\nb\rc\nd",
            "fields": { "reason": "x\r\ny\rz" },
        })),
    )
    .await
    .expect("create_ticket must succeed");

    match transport.last_sent() {
        ClientMessage::CreateTicket { body, fields, .. } => {
            assert_eq!(body, "a\nb\nc\nd", "the body must reach the daemon with LF");
            assert_eq!(
                fields,
                Some(field_map(&[("reason", "x\ny\nz")])),
                "field values must reach the daemon with LF"
            );
        }
        other => panic!("expected create_ticket, got {other:?}"),
    }
}

#[tokio::test]
async fn update_ticket_normalizes_crlf_and_lone_cr_in_the_body_and_field_values_to_lf() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    update_ticket(
        &transport,
        args::<UpdateTicketArgs>(json!({
            "ticket": 12,
            "body": "a\r\nb\rc",
            "fields": { "reason": "x\r\ny" },
        })),
    )
    .await
    .expect("update_ticket must succeed");

    match transport.last_sent() {
        ClientMessage::UpdateTicket { body, fields, .. } => {
            assert_eq!(body.as_deref(), Some("a\nb\nc"));
            assert_eq!(fields, Some(field_map(&[("reason", "x\ny")])));
        }
        other => panic!("expected update_ticket, got {other:?}"),
    }
}

#[tokio::test]
async fn add_ticket_result_normalizes_crlf_and_lone_cr_in_the_text_to_lf() {
    let transport = MockTransport::with_response(Ok(ticket_detail()));

    add_ticket_result(
        &transport,
        args::<AddTicketResultArgs>(json!({ "ticket": 12, "text": "a\r\nb\rc\nd" })),
    )
    .await
    .expect("add_ticket_result must succeed");

    match transport.last_sent() {
        ClientMessage::AddTicketResult { text, .. } => assert_eq!(text, "a\nb\nc\nd"),
        other => panic!("expected add_ticket_result, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// A missing or empty required argument is refused before any round trip
// ---------------------------------------------------------------------

fn assert_refused_locally(result: Result<Value, ToolError>, transport: &MockTransport, case: &str) {
    let err = result.expect_err(case);
    assert_eq!(err.code(), "invalid_argument", "{case}: got {err:?}");
    assert_eq!(
        transport.call_count(),
        0,
        "{case}: a missing or empty required argument must be refused before any round trip"
    );
}

#[tokio::test]
async fn get_ticket_refuses_a_missing_ticket_before_any_round_trip() {
    let transport = MockTransport::never_called();
    let result = get_ticket(&transport, args::<GetTicketArgs>(json!({}))).await;
    assert_refused_locally(result, &transport, "missing ticket");
}

#[tokio::test]
async fn create_ticket_refuses_a_missing_or_empty_title_or_body_before_any_round_trip() {
    for (case, arguments) in [
        ("missing title", json!({ "body": "b" })),
        ("empty title", json!({ "title": "", "body": "b" })),
        ("missing body", json!({ "title": "t" })),
        ("empty body", json!({ "title": "t", "body": "" })),
    ] {
        let transport = MockTransport::never_called();
        let result = create_ticket(&transport, args::<CreateTicketArgs>(arguments)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn update_ticket_refuses_a_missing_ticket_before_any_round_trip() {
    let transport = MockTransport::never_called();
    let result = update_ticket(
        &transport,
        args::<UpdateTicketArgs>(json!({ "state": "done" })),
    )
    .await;
    assert_refused_locally(result, &transport, "missing ticket");
}

#[tokio::test]
async fn add_ticket_result_refuses_a_missing_ticket_or_a_missing_or_empty_text_before_any_round_trip()
 {
    for (case, arguments) in [
        ("missing ticket", json!({ "text": "t" })),
        ("missing text", json!({ "ticket": 12 })),
        ("empty text", json!({ "ticket": 12, "text": "" })),
    ] {
        let transport = MockTransport::never_called();
        let result = add_ticket_result(&transport, args::<AddTicketResultArgs>(arguments)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn set_milestone_refuses_a_missing_or_empty_name_or_title_or_a_missing_position_before_any_round_trip()
 {
    for (case, arguments) in [
        ("missing name", json!({ "title": "T", "position": 1 })),
        (
            "empty name",
            json!({ "name": "", "title": "T", "position": 1 }),
        ),
        ("missing title", json!({ "name": "v1", "position": 1 })),
        (
            "empty title",
            json!({ "name": "v1", "title": "", "position": 1 }),
        ),
        ("missing position", json!({ "name": "v1", "title": "T" })),
    ] {
        let transport = MockTransport::never_called();
        let result = set_milestone(&transport, args::<SetMilestoneArgs>(arguments)).await;
        assert_refused_locally(result, &transport, case);
    }
}

/// The bridge checks only that a required argument is there; whether a state,
/// a field name or a value is acceptable is the daemon's call, and its refusal
/// reaches the caller as it gave it.
#[tokio::test]
async fn update_ticket_forwards_what_the_daemon_judges_and_relays_its_invalid_argument() {
    let transport = MockTransport::with_response(Ok(daemon_error(
        "invalid_argument",
        "state on-hold requires the field resume_trigger",
    )));

    let err = update_ticket(
        &transport,
        args::<UpdateTicketArgs>(json!({ "ticket": 12, "state": "no-such-state" })),
    )
    .await
    .expect_err("the daemon refused");

    assert_eq!(transport.call_count(), 1, "the daemon must be asked");
    assert_eq!(err.code(), "invalid_argument");
    assert!(
        err.to_string().contains("resume_trigger"),
        "the daemon's message must reach the caller, got {err}"
    );
}

// ---------------------------------------------------------------------
// How the daemon's replies reach the caller
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_tickets_reports_the_config_the_milestones_the_summaries_and_truncation() {
    let transport = MockTransport::with_response(Ok(ticket_list()));

    let reply = list_tickets(&transport, args(json!({})))
        .await
        .expect("list_tickets must succeed");

    assert_eq!(reply["config"]["guide"], json!("One ticket, one purpose."));
    assert_eq!(
        reply["config"]["states"],
        json!([{ "name": "on-hold", "description": "Paused.", "requires": ["resume_trigger"] }])
    );
    assert_eq!(
        reply["config"]["fields"],
        json!([{ "name": "resume_trigger", "description": "What brings it back." }])
    );
    assert_eq!(
        reply["milestones"],
        json!([{
            "name": "v1",
            "position": 3,
            "title": "Version 1",
            "description": "The first release.",
        }])
    );
    assert_eq!(
        reply["tickets"],
        json!([{
            "number": 12,
            "title": "Make the board",
            "state": "on-hold",
            "milestone": "v1",
            "updated_at": 1_700_000_000_500u64,
        }])
    );
    assert_eq!(reply["truncated"], json!(true));
    assert!(
        reply["tickets"][0].get("body").is_none(),
        "a summary carries no body: {reply}"
    );
}

#[tokio::test]
async fn list_tickets_reports_why_the_config_could_not_be_read_in_place_of_it() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TicketList {
        workspace_id: None,
        config: None,
        config_error: Some("tickets.toml could not be parsed".to_string()),
        milestones: Vec::new(),
        tickets: vec![sample_summary()],
        truncated: false,
    }));

    let reply = list_tickets(&transport, args(json!({})))
        .await
        .expect("a missing config is not a failed read: the summaries are still there");

    assert_eq!(
        reply["config_error"],
        json!("tickets.toml could not be parsed")
    );
    assert_eq!(reply["tickets"][0]["number"], json!(12));
    assert_eq!(reply["truncated"], json!(false));
}

#[tokio::test]
async fn every_ticket_detail_tool_reports_every_field_of_the_ticket() {
    let names = [
        "number",
        "title",
        "body",
        "state",
        "milestone",
        "fields",
        "results",
        "plans",
        "created_at",
        "updated_at",
    ];
    let expected = sample_ticket();

    for tool in [
        TicketTool::GetTicket,
        TicketTool::CreateTicket,
        TicketTool::UpdateTicket,
        TicketTool::AddTicketResult,
    ] {
        let transport = MockTransport::with_response(Ok(ticket_detail()));
        let reply = call(tool, &transport)
            .await
            .unwrap_or_else(|err| panic!("{tool:?} must succeed: {err}"));
        assert_fields_reach_the_caller(&reply, &expected, &names);
        assert_eq!(
            reply["results"][0]["text"],
            json!("decided on a store"),
            "{tool:?}: results must carry their text: {reply}"
        );
        assert_eq!(
            reply["plans"],
            json!(["plan-7"]),
            "{tool:?}: the ticket's plans must be listed: {reply}"
        );
    }
}

#[tokio::test]
async fn set_milestone_reports_every_field_of_the_milestone() {
    let transport = MockTransport::with_response(Ok(milestone_set()));

    let reply = call(TicketTool::SetMilestone, &transport)
        .await
        .expect("set_milestone must succeed");

    assert_fields_reach_the_caller(
        &reply,
        &sample_milestone(),
        &["name", "position", "title", "description"],
    );
}

#[tokio::test]
async fn every_ticket_tool_treats_a_reply_of_another_kind_as_an_internal_error() {
    for tool in ALL_TICKET_TOOLS {
        let transport = MockTransport::with_response(Ok(reply_no_ticket_tool_expects()));

        let err = call(tool, &transport)
            .await
            .expect_err("a reply of a kind the tool did not ask for is not a success");

        assert_eq!(err.code(), "internal", "{tool:?}: got {err:?}");
    }
}

// ---------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------

#[tokio::test]
async fn every_ticket_tool_reports_the_three_ticket_codes_under_their_own_names() {
    for tool in ALL_TICKET_TOOLS {
        for (code, message) in [
            ("ticket_not_found", "no such ticket in this workspace"),
            ("tickets_not_configured", "no tickets.toml is set up"),
            ("tickets_config_invalid", "tickets.toml is not valid"),
        ] {
            let transport = MockTransport::with_response(Ok(daemon_error(code, message)));

            let err = call(tool, &transport).await.expect_err("must fail");

            assert_eq!(err.code(), code, "{tool:?}: got {err:?}");
            assert!(
                err.to_string().contains(message),
                "{tool:?}: the daemon's message must reach the caller, got {err}"
            );
        }
    }
}

/// The daemon codes a ticket tool can meet that existed before the ticket
/// tools did, each of which must keep reaching the caller under its own name.
const PRE_EXISTING_DAEMON_CODES: &[&str] = &[
    "invalid_argument",
    "unsupported",
    "resource_exhausted",
    "auth_failed",
];

#[tokio::test]
async fn ticket_tools_keep_every_pre_existing_daemon_code_under_its_own_name() {
    for tool in ALL_TICKET_TOOLS {
        for code in PRE_EXISTING_DAEMON_CODES {
            let transport =
                MockTransport::with_response(Ok(daemon_error(code, "refused by the daemon")));

            let err = call(tool, &transport).await.expect_err("must fail");

            assert_eq!(
                err.code(),
                *code,
                "{tool:?} with daemon code {code}: got {err:?}"
            );
        }
    }
}

#[tokio::test]
async fn ticket_tools_treat_an_unrecognized_daemon_code_as_internal() {
    for tool in ALL_TICKET_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "no_such_code",
            "a code the bridge has never heard of",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "internal", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_ticket_tool_reports_an_older_daemons_unknown_type_refusal_as_unsupported() {
    for tool in ALL_TICKET_TOOLS {
        let transport = MockTransport::with_response(Ok(daemon_error(
            "unknown_type",
            "the daemon does not recognize this message type",
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "unsupported", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_ticket_tool_reports_a_transport_level_revision_refusal_as_unsupported() {
    for tool in ALL_TICKET_TOOLS {
        let transport = MockTransport::with_response(Err(TransportError::Unsupported(
            "the running daemon is too old for this request".to_string(),
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "unsupported", "{tool:?}: got {err:?}");
    }
}

#[tokio::test]
async fn every_ticket_tool_reports_an_unreachable_daemon_as_daemon_unavailable() {
    for tool in ALL_TICKET_TOOLS {
        let transport = MockTransport::with_response(Err(TransportError::Unavailable(
            "pipe not connected".to_string(),
        )));

        let err = call(tool, &transport).await.expect_err("must fail");

        assert_eq!(err.code(), "daemon_unavailable", "{tool:?}: got {err:?}");
    }
}
