#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The report tools of the MCP bridge against a scripted transport: which
//! `ClientMessage` each tool sends, which arguments it refuses before any
//! round trip, and how the daemon's replies and error codes reach the caller.
//!
//! Arguments are built from JSON, the shape an MCP client actually sends, so
//! these tests fix the external argument names rather than the Rust types
//! the bridge happens to deserialize them into.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use yagura_mcp::report_tools::{SendReportArgs, WaitForReportArgs, send_report, wait_for_report};
use yagura_mcp::tools::ToolError;
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    AwaitingWindow, ClientMessage, DaemonMessage, REPORT_SUMMARY_MAX_BYTES, ReceivedReport,
    ReportOutcome, ReportTaskRef, WindowStatus,
};

/// Replays queued responses in order and records every request, so a test
/// can assert both what came back and whether the daemon was contacted.
struct MockTransport {
    responses: Mutex<Vec<Result<DaemonMessage, TransportError>>>,
    sent: Mutex<Vec<ClientMessage>>,
    call_count: AtomicUsize,
}

impl MockTransport {
    /// A transport whose `round_trip` panics, so a missing short-circuit
    /// fails loudly instead of passing silently.
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
        RequesterIdentity::Window("w-child".to_string())
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

fn send_args(value: Value) -> SendReportArgs {
    serde_json::from_value(value).expect("send_report arguments must deserialize")
}

fn wait_args(value: Value) -> WaitForReportArgs {
    serde_json::from_value(value).expect("wait_for_report arguments must deserialize")
}

fn report_sent_to_parent() -> DaemonMessage {
    DaemonMessage::ReportSent {
        delivered_to: Some("w-parent".to_string()),
        task: Some(ReportTaskRef {
            plan_id: "plan-7".to_string(),
            task_id: "build-parser".to_string(),
        }),
    }
}

fn assert_refused_locally(result: Result<Value, ToolError>, transport: &MockTransport, case: &str) {
    let err = result.expect_err(case);
    assert_eq!(err.code(), "invalid_argument", "{case}: got {err:?}");
    assert_eq!(
        transport.call_count(),
        0,
        "{case}: the argument must be refused before any round trip"
    );
}

/// The two report tools, each called with a well-formed argument set, so a
/// property that must hold for both is checked against both.
#[derive(Debug, Clone, Copy)]
enum ReportTool {
    SendReport,
    WaitForReport,
}

const ALL_REPORT_TOOLS: [ReportTool; 2] = [ReportTool::SendReport, ReportTool::WaitForReport];

async fn call(tool: ReportTool, transport: &MockTransport) -> Result<Value, ToolError> {
    match tool {
        ReportTool::SendReport => {
            send_report(
                transport,
                send_args(json!({ "outcome": "succeeded", "summary": "Parser done." })),
            )
            .await
        }
        ReportTool::WaitForReport => {
            wait_for_report(transport, wait_args(json!({ "timeout_sec": 5 }))).await
        }
    }
}

// --- send_report sends the wire message and relays report_sent -----

#[tokio::test]
async fn send_report_sends_the_outcome_the_summary_and_the_optional_path() {
    let cases = [
        (
            json!({
                "outcome": "succeeded",
                "summary": "Parser done; see the file.",
                "path": "reports/parser.md",
            }),
            ClientMessage::SendReport {
                outcome: ReportOutcome::Succeeded,
                summary: "Parser done; see the file.".to_string(),
                path: Some("reports/parser.md".to_string()),
            },
        ),
        (
            json!({ "outcome": "failed", "summary": "The build does not link." }),
            ClientMessage::SendReport {
                outcome: ReportOutcome::Failed,
                summary: "The build does not link.".to_string(),
                path: None,
            },
        ),
    ];

    for (args, expected) in cases {
        let transport = MockTransport::with_response(Ok(report_sent_to_parent()));

        send_report(&transport, send_args(args.clone()))
            .await
            .unwrap_or_else(|err| panic!("send_report with {args} must succeed: {err:?}"));

        assert_eq!(transport.call_count(), 1, "{args}");
        assert_eq!(transport.last_sent(), expected, "{args}");
    }
}

#[tokio::test]
async fn send_report_reports_the_delivered_window_and_the_recorded_task() {
    let transport = MockTransport::with_response(Ok(report_sent_to_parent()));

    let result = send_report(
        &transport,
        send_args(json!({ "outcome": "succeeded", "summary": "Parser done." })),
    )
    .await
    .expect("send_report must succeed");

    assert_eq!(
        result,
        json!({
            "delivered_to": "w-parent",
            "task": { "plan_id": "plan-7", "task_id": "build-parser" },
        })
    );
}

#[tokio::test]
async fn send_report_keeps_an_absent_recipient_and_task_as_null_keys() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ReportSent {
        delivered_to: None,
        task: None,
    }));

    let result = send_report(
        &transport,
        send_args(json!({ "outcome": "failed", "summary": "Gave up." })),
    )
    .await
    .expect("send_report must succeed");

    assert_eq!(result, json!({ "delivered_to": null, "task": null }));
    let object = result.as_object().expect("the reply must be a JSON object");
    assert!(
        object.contains_key("delivered_to") && object.contains_key("task"),
        "an absent value must stay a null key, not a missing one: {result}"
    );
}

// --- Arguments refused before any round trip ------------------------

#[tokio::test]
async fn send_report_refuses_an_outcome_outside_the_two_values_before_any_round_trip() {
    for (case, args) in [
        ("missing outcome", json!({ "summary": "Parser done." })),
        (
            "empty outcome",
            json!({ "outcome": "", "summary": "Parser done." }),
        ),
        (
            "unknown outcome",
            json!({ "outcome": "partial", "summary": "Parser done." }),
        ),
        (
            "outcome in another case",
            json!({ "outcome": "Succeeded", "summary": "Parser done." }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = send_report(&transport, send_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

#[tokio::test]
async fn send_report_refuses_a_missing_or_empty_summary_before_any_round_trip() {
    for (case, args) in [
        ("missing summary", json!({ "outcome": "succeeded" })),
        (
            "empty summary",
            json!({ "outcome": "succeeded", "summary": "" }),
        ),
    ] {
        let transport = MockTransport::never_called();
        let result = send_report(&transport, send_args(args)).await;
        assert_refused_locally(result, &transport, case);
    }
}

/// The daemon owns the size limits, so the bridge forwards an oversized
/// summary and relays the daemon's own refusal rather than duplicating it.
#[tokio::test]
async fn send_report_forwards_an_oversized_summary_and_relays_the_daemons_refusal() {
    let oversized = "x".repeat(REPORT_SUMMARY_MAX_BYTES + 1);
    let transport = MockTransport::with_response(Ok(daemon_error(
        "invalid_argument",
        "summary exceeds the limit",
    )));

    let err = send_report(
        &transport,
        send_args(json!({ "outcome": "succeeded", "summary": oversized })),
    )
    .await
    .expect_err("the daemon's refusal must reach the caller");

    assert_eq!(err.code(), "invalid_argument", "got {err:?}");
    assert_eq!(
        transport.call_count(),
        1,
        "the size limit belongs to the daemon, so the request must reach it"
    );
}

// --- Daemon codes keep their names ---------------------------------

const REPORT_DAEMON_CODES: &[&str] = &[
    "auth_failed",
    "invalid_state",
    "resource_exhausted",
    "invalid_argument",
];

#[tokio::test]
async fn report_tools_keep_the_daemons_refusal_codes_under_their_own_names() {
    for tool in ALL_REPORT_TOOLS {
        for code in REPORT_DAEMON_CODES {
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
async fn report_tools_map_an_older_daemon_an_unreachable_one_and_a_stray_reply() {
    for tool in ALL_REPORT_TOOLS {
        let cases: [(Result<DaemonMessage, TransportError>, &str); 4] = [
            (
                Ok(daemon_error(
                    "unknown_type",
                    "the daemon does not recognize this message type",
                )),
                "unsupported",
            ),
            (
                Err(TransportError::Unsupported(
                    "the running daemon is too old for this request".to_string(),
                )),
                "unsupported",
            ),
            (
                Err(TransportError::Unavailable(
                    "pipe not connected".to_string(),
                )),
                "daemon_unavailable",
            ),
            (
                Ok(DaemonMessage::WindowClosed {
                    window_id: "w-1".to_string(),
                }),
                "internal",
            ),
        ];
        for (response, expected) in cases {
            let transport = MockTransport::with_response(response);

            let err = call(tool, &transport).await.expect_err("must fail");

            assert_eq!(err.code(), expected, "{tool:?}: got {err:?}");
        }
    }
}

// --- wait_for_report sends timeout_sec and relays every report ------

#[tokio::test]
async fn wait_for_report_sends_the_timeout_including_zero() {
    for timeout_sec in [0_u64, 30] {
        let transport = MockTransport::with_response(Ok(DaemonMessage::Reports {
            reports: Vec::new(),
            awaiting: None,
        }));

        wait_for_report(&transport, wait_args(json!({ "timeout_sec": timeout_sec })))
            .await
            .unwrap_or_else(|err| panic!("timeout_sec {timeout_sec} must succeed: {err:?}"));

        assert_eq!(transport.call_count(), 1, "timeout_sec {timeout_sec}");
        assert_eq!(
            transport.last_sent(),
            ClientMessage::WaitForReport { timeout_sec },
            "timeout_sec {timeout_sec}"
        );
    }
}

#[tokio::test]
async fn wait_for_report_reports_every_report_with_the_wire_keys_and_nulls() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Reports {
        reports: vec![
            ReceivedReport {
                from_window_id: "w-child".to_string(),
                from_name: "parser".to_string(),
                outcome: ReportOutcome::Succeeded,
                summary: "Parser done.".to_string(),
                path: Some("reports/parser.md".to_string()),
                task: Some(ReportTaskRef {
                    plan_id: "plan-7".to_string(),
                    task_id: "build-parser".to_string(),
                }),
                at_ms: 1_700_000_000_123,
            },
            ReceivedReport {
                from_window_id: "w-helper".to_string(),
                from_name: "helper".to_string(),
                outcome: ReportOutcome::Failed,
                summary: "Could not reproduce.".to_string(),
                path: None,
                task: None,
                at_ms: 1_700_000_000_456,
            },
        ],
        awaiting: None,
    }));

    let result = wait_for_report(&transport, wait_args(json!({ "timeout_sec": 30 })))
        .await
        .expect("wait_for_report must succeed");

    assert_eq!(
        result,
        json!({
            "reports": [
                {
                    "from_window_id": "w-child",
                    "from_name": "parser",
                    "outcome": "succeeded",
                    "summary": "Parser done.",
                    "path": "reports/parser.md",
                    "task": { "plan_id": "plan-7", "task_id": "build-parser" },
                    "at_ms": 1_700_000_000_123_u64,
                },
                {
                    "from_window_id": "w-helper",
                    "from_name": "helper",
                    "outcome": "failed",
                    "summary": "Could not reproduce.",
                    "path": null,
                    "task": null,
                    "at_ms": 1_700_000_000_456_u64,
                },
            ],
        })
    );
    let second = result["reports"][1]
        .as_object()
        .expect("each report must be a JSON object");
    assert!(
        second.contains_key("path") && second.contains_key("task"),
        "an absent path or task must stay a null key, as on the wire: {result}"
    );
}

#[tokio::test]
async fn wait_for_report_reports_an_empty_list_when_nothing_arrived() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Reports {
        reports: Vec::new(),
        awaiting: None,
    }));

    let result = wait_for_report(&transport, wait_args(json!({ "timeout_sec": 0 })))
        .await
        .expect("wait_for_report must succeed");

    assert_eq!(result, json!({ "reports": [] }));
}

// --- wait_for_report surfaces the daemon's awaiting list -------------

#[tokio::test]
async fn wait_for_report_includes_awaiting_when_the_daemon_reports_it() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Reports {
        reports: Vec::new(),
        awaiting: Some(vec![AwaitingWindow {
            window_id: "w-child".to_string(),
            name: "parser".to_string(),
            status: WindowStatus::Running,
            mcp_connected: Some(true),
        }]),
    }));

    let result = wait_for_report(&transport, wait_args(json!({ "timeout_sec": 0 })))
        .await
        .expect("wait_for_report must succeed");

    assert_eq!(
        result,
        json!({
            "reports": [],
            "awaiting": [
                {
                    "window_id": "w-child",
                    "name": "parser",
                    "status": "running",
                    "mcp_connected": true,
                },
            ],
        }),
        "wait_for_report must surface the daemon's awaiting list, got: {result}"
    );
}

// --- timeout_sec is required -----------------------------------------

#[tokio::test]
async fn wait_for_report_refuses_a_missing_timeout_before_any_round_trip() {
    let transport = MockTransport::never_called();
    let result = wait_for_report(&transport, wait_args(json!({}))).await;
    assert_refused_locally(result, &transport, "missing timeout_sec");
}
