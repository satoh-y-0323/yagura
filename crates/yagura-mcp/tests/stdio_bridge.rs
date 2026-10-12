#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The compiled bridge between an MCP client and the daemon, driven over
//! MCP stdio against a scripted daemon: every MCP tool name reaches the
//! daemon as its own request and the daemon's answer reaches the client, and
//! a bridge started with a window's identity pair claims that window before
//! it serves anything.
//!
//! The scripted daemon binds through `yagura_os::IpcTransport` at a named
//! instance reserved for this file, so the spawned bridge can only ever reach
//! it, never a resident daemon.

use std::ffi::OsStr;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, DaemonMessage, INSTANCE_ENV, PROTOCOL_REVISION, STATUS_TOKEN_ENV, WINDOW_ID_ENV,
    decode_client_message, encode_daemon_message, frame_decode, frame_encode,
};

mod common;

use common::{LINE_TIMEOUT, McpChild, tool_reply};

/// A daemon that answers the transport's own protocol (revision probe and
/// identity claim) and refuses every other request with a message naming the
/// request's wire type. Every request it reads is also handed to the test.
struct ScriptedDaemon {
    received: mpsc::UnboundedReceiver<Value>,
    task: JoinHandle<()>,
}

impl Drop for ScriptedDaemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedDaemon {
    /// Binds the endpoint the bridge dials for `instance_id` and starts
    /// serving it. Binding claims the name exclusively, so a bridge spawned
    /// afterwards cannot reach anything else listening under it.
    async fn bind(instance_id: &str) -> Self {
        let namespace = yagura_os::resolve_instance_from(Some(OsStr::new(instance_id)))
            .unwrap_or_else(|err| panic!("{instance_id:?} must be a valid instance id: {err}"));
        let ipc = PlatformIpc::default();
        let user_id = ipc
            .current_user_id()
            .expect("current_user_id must resolve in a normal test-runner session");
        let endpoint = yagura_os::default_endpoint_owning(&user_id, &namespace);
        let security = ipc
            .endpoint_security()
            .expect("endpoint_security must succeed in a normal test-runner session");
        let mut listener = ipc
            .bind(&endpoint, security)
            .await
            .unwrap_or_else(|err| panic!("cannot claim endpoint {endpoint:?}: {err}"));

        let (sender, received) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Ok(conn) = ipc.accept(&mut listener).await {
                tokio::spawn(serve(conn, sender.clone()));
            }
        });
        Self { received, task }
    }

    /// The next request of wire type `wire_type`, skipping any other.
    async fn next_of_type(&mut self, wire_type: &str) -> Value {
        tokio::time::timeout(LINE_TIMEOUT, async {
            loop {
                let request = self
                    .received
                    .recv()
                    .await
                    .expect("the scripted daemon stopped");
                if request.get("type").and_then(Value::as_str) == Some(wire_type) {
                    return request;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the daemon never received a {wire_type} request"))
    }
}

async fn serve<C: AsyncRead + AsyncWrite + Unpin>(mut conn: C, seen: mpsc::UnboundedSender<Value>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        while let Ok(Some((body, consumed))) = frame_decode(&buffer) {
            let request: Value =
                serde_json::from_slice(body).expect("the bridge must send JSON frames");
            let reply = answer(body, &request);
            buffer.drain(..consumed);
            let _ = seen.send(request);
            if conn
                .write_all(&frame_encode(&encode_daemon_message(&reply)))
                .await
                .is_err()
            {
                return;
            }
        }
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

fn answer(body: &[u8], request: &Value) -> DaemonMessage {
    match decode_client_message(body) {
        Ok(ClientMessage::GetProtocolRevision) => DaemonMessage::ProtocolRevision {
            revision: PROTOCOL_REVISION,
        },
        Ok(ClientMessage::Identify { window_id, .. }) => DaemonMessage::Identified { window_id },
        _ => DaemonMessage::Error {
            code: "invalid_argument".to_string(),
            message: format!("daemon received {}", wire_type(request)),
            rejected_argument: None,
        },
    }
}

fn wire_type(request: &Value) -> &str {
    request
        .get("type")
        .and_then(Value::as_str)
        .expect("every request must carry its wire type")
}

/// One valid call per MCP tool, and the wire type its first request to the
/// daemon carries.
fn tool_cases() -> Vec<(&'static str, Value, &'static str)> {
    let task = json!({ "task_id": "a", "title": "A", "depends_on": [], "writes": [] });
    vec![
        ("list_windows", json!({}), "list_windows"),
        (
            "get_window_status",
            json!({ "window_id": "w-1" }),
            "get_window_status",
        ),
        (
            "send_message",
            json!({ "window_id": "w-1", "text": "hello" }),
            "send_text",
        ),
        (
            "send_keys",
            json!({ "window_id": "w-1", "keys": ["enter"] }),
            "send_keys",
        ),
        (
            "open_window",
            json!({ "command": ["probe"] }),
            "open_window",
        ),
        (
            "interrupt_window",
            json!({ "window_id": "w-1" }),
            "interrupt_window",
        ),
        (
            "close_window",
            json!({ "window_id": "w-1" }),
            "close_window",
        ),
        (
            "wait_for_status",
            json!({ "window_id": "w-1", "target_status": "idle", "timeout_sec": 1 }),
            "wait_for_status",
        ),
        ("read_output", json!({ "window_id": "w-1" }), "read_output"),
        ("list_detections", json!({}), "list_detections"),
        ("list_profiles", json!({}), "list_profiles"),
        (
            "send_handoff",
            json!({ "window_id": "w-1", "reference": "look at this", "note": "please continue" }),
            "send_handoff",
        ),
        ("send_chat", json!({ "body": "hello" }), "send_chat"),
        (
            "broadcast_chat",
            json!({ "body": "hello" }),
            "broadcast_chat",
        ),
        ("read_chat", json!({}), "read_chat"),
        (
            "create_plan",
            json!({ "title": "plan", "tasks": [task.clone()] }),
            "create_plan",
        ),
        (
            "update_plan",
            json!({ "plan_id": "p-1", "tasks": [task] }),
            "update_plan",
        ),
        ("get_plan", json!({ "plan_id": "p-1" }), "get_plan"),
        ("list_plans", json!({}), "list_plans"),
        ("delete_plan", json!({ "plan_id": "p-1" }), "delete_plan"),
        ("list_roles", json!({}), "list_roles"),
        (
            "open_task_window",
            json!({ "plan_id": "p-1", "task_id": "a" }),
            "open_task_window",
        ),
        (
            "send_report",
            json!({ "outcome": "succeeded", "summary": "done" }),
            "send_report",
        ),
        (
            "wait_for_report",
            json!({ "timeout_sec": 0 }),
            "wait_for_report",
        ),
    ]
}

#[tokio::test]
async fn every_tool_reaches_the_daemon_as_its_own_request_and_returns_the_daemons_answer() {
    const INSTANCE: &str = "mcpbridgewire";
    let mut daemon = ScriptedDaemon::bind(INSTANCE).await;
    let mut bridge = McpChild::spawn(&[(INSTANCE_ENV, INSTANCE)]);
    bridge.initialize().await;

    let cases = tool_cases();
    let mut listed = bridge.tool_names().await;
    let mut covered: Vec<String> = cases.iter().map(|(tool, ..)| tool.to_string()).collect();
    listed.sort();
    covered.sort();
    assert_eq!(listed, covered, "every listed tool must have a case here");

    for (tool, arguments, expected_wire_type) in cases {
        let response = bridge.call_tool(tool, arguments).await;
        let (is_error, text) = tool_reply(&response);
        assert!(
            is_error && text.ends_with(&format!("daemon received {expected_wire_type}")),
            "{tool} must reach the daemon as {expected_wire_type} and return its refusal, \
             got {response}"
        );
        daemon.next_of_type(expected_wire_type).await;
    }

    bridge.kill().await;
}

#[tokio::test]
async fn a_bridge_started_with_both_identity_variables_claims_that_window_before_any_call() {
    const INSTANCE: &str = "mcpbridgeident";
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let mut daemon = ScriptedDaemon::bind(INSTANCE).await;
    let mut bridge = McpChild::spawn(&[
        (INSTANCE_ENV, INSTANCE),
        (WINDOW_ID_ENV, "w-env"),
        (STATUS_TOKEN_ENV, TOKEN),
    ]);

    let identify = daemon.next_of_type("identify").await;
    assert_eq!(identify.get("window_id"), Some(&json!("w-env")));
    assert_eq!(identify.get("token"), Some(&json!(TOKEN)));

    bridge.kill().await;
}
