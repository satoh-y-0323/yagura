#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests: the tools that open a window report the name the daemon gave it,
//! and a name the daemon refused as taken reaches the caller as its own error kind.
//!
//! The daemon's answers are written as wire JSON, so these tests name only the wire keys.

use std::sync::Mutex;

use serde_json::{Value, json};
use yagura_mcp::plan_tools::{OpenTaskWindowArgs, open_task_window};
use yagura_mcp::tools::{OpenWindowArgs, SendHandoffArgs, ToolError, open_window, send_handoff};
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{ClientMessage, DaemonMessage};

/// A transport that answers any request with one scripted response.
struct ScriptedTransport {
    response: Mutex<Option<DaemonMessage>>,
}

impl ScriptedTransport {
    fn answering(frame: Value) -> Self {
        let body = serde_json::to_vec(&frame).expect("a frame encodes");
        let response = yagura_protocol::decode_daemon_message(&body)
            .unwrap_or_else(|err| panic!("{frame} must decode: {err:?}"));
        Self {
            response: Mutex::new(Some(response)),
        }
    }
}

impl DaemonTransport for ScriptedTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        Ok(self
            .response
            .lock()
            .expect("scripted response mutex must not be poisoned")
            .take()
            .expect("round_trip must be called at most once"))
    }
}

const TAKEN_MESSAGE: &str = "a window named \u{201c}Twin\u{201d} is already open";

fn name_taken() -> Value {
    json!({
        "type": "error",
        "code": "name_taken",
        "message": TAKEN_MESSAGE,
        "rejected_argument": "name",
    })
}

fn args<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("tool arguments decode")
}

fn open_window_args() -> [Value; 2] {
    [
        json!({ "command": ["pwsh"], "name": "Twin" }),
        json!({ "profile": "twin", "name": "Twin" }),
    ]
}

fn assert_name_taken(err: ToolError) {
    assert_eq!(err.code(), "name_taken", "got {err:?}");
    assert!(
        err.to_string().contains(TAKEN_MESSAGE),
        "the daemon's reason reaches the caller: {err}"
    );
}

fn window_opened(name: Option<&str>) -> Value {
    let mut frame = json!({ "type": "window_opened", "window_id": "w2-abc" });
    if let Some(name) = name {
        frame["name"] = json!(name);
    }
    frame
}

#[tokio::test]
async fn open_window_reports_the_name_the_daemon_gave_the_window() {
    for arguments in open_window_args() {
        let transport = ScriptedTransport::answering(window_opened(Some("Twin 2")));

        let result = open_window(&transport, args::<OpenWindowArgs>(arguments))
            .await
            .expect("the window opened");

        assert_eq!(result["window_id"], json!("w2-abc"));
        assert_eq!(result["name"], json!("Twin 2"), "got {result}");
    }
}

#[tokio::test]
async fn open_window_writes_no_name_when_the_daemon_reports_none() {
    let transport = ScriptedTransport::answering(window_opened(None));

    let result = open_window(
        &transport,
        args::<OpenWindowArgs>(open_window_args()[0].clone()),
    )
    .await
    .expect("the window opened");

    assert!(result.get("name").is_none(), "got {result}");
}

#[tokio::test]
async fn open_window_reports_a_taken_name_as_name_taken() {
    for arguments in open_window_args() {
        let transport = ScriptedTransport::answering(name_taken());

        let err = open_window(&transport, args::<OpenWindowArgs>(arguments))
            .await
            .expect_err("the daemon refused the name");

        assert_name_taken(err);
    }
}

fn task_window_opened(name: Option<&str>) -> Value {
    let mut frame = json!({
        "type": "task_window_opened",
        "plan_id": "p-1",
        "task_id": "t1",
        "window_id": "w2-abc",
        "assignment": { "source": "unassigned", "assignment": null },
    });
    if let Some(name) = name {
        frame["name"] = json!(name);
    }
    frame
}

fn open_task_window_args() -> OpenTaskWindowArgs {
    args(json!({ "plan_id": "p-1", "task_id": "t1" }))
}

#[tokio::test]
async fn open_task_window_reports_the_name_the_daemon_gave_the_window() {
    let transport = ScriptedTransport::answering(task_window_opened(Some("Review 2")));

    let result = open_task_window(&transport, open_task_window_args())
        .await
        .expect("the window opened");

    assert_eq!(result["name"], json!("Review 2"), "got {result}");
}

#[tokio::test]
async fn open_task_window_writes_no_name_when_the_daemon_reports_none() {
    let transport = ScriptedTransport::answering(task_window_opened(None));

    let result = open_task_window(&transport, open_task_window_args())
        .await
        .expect("the window opened");

    assert!(result.get("name").is_none(), "got {result}");
}

#[tokio::test]
async fn open_task_window_reports_a_taken_name_as_name_taken() {
    let transport = ScriptedTransport::answering(name_taken());

    let err = open_task_window(&transport, open_task_window_args())
        .await
        .expect_err("the daemon refused the name");

    assert_name_taken(err);
}

fn handoff_sent(name: Option<&str>) -> Value {
    let mut frame = json!({
        "type": "handoff_sent",
        "window_id": "w2-abc",
        "opened": true,
        "delivery": "taken",
    });
    if let Some(name) = name {
        frame["name"] = json!(name);
    }
    frame
}

fn send_handoff_args() -> SendHandoffArgs {
    args(json!({ "profile": "twin", "reference": "src/lib.rs", "note": "look" }))
}

#[tokio::test]
async fn send_handoff_reports_the_name_of_the_window_it_opened() {
    let transport = ScriptedTransport::answering(handoff_sent(Some("Twin 2")));

    let result = send_handoff(&transport, send_handoff_args())
        .await
        .expect("the handoff was sent");

    assert_eq!(result["name"], json!("Twin 2"), "got {result}");
}

#[tokio::test]
async fn send_handoff_writes_no_name_when_the_daemon_reports_none() {
    let transport = ScriptedTransport::answering(handoff_sent(None));

    let result = send_handoff(&transport, send_handoff_args())
        .await
        .expect("the handoff was sent");

    assert!(result.get("name").is_none(), "got {result}");
}

#[tokio::test]
async fn send_handoff_reports_a_taken_name_as_name_taken() {
    let transport = ScriptedTransport::answering(name_taken());

    let err = send_handoff(&transport, send_handoff_args())
        .await
        .expect_err("the daemon refused the name");

    assert_name_taken(err);
}
