#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The MCP tools the daemon serves: the tool functions against a scripted
//! transport or a real daemon's endpoint, the rmcp server on an in-memory
//! pipe, and relay sessions spoken the way a CLI's relay speaks them.

mod cancellation;
mod e2e;
mod list_windows_surface;
mod plan_review_tools;
mod plan_tools;
mod rejected_argument_bridge;
mod report_tools;
mod ticket_e2e;
mod ticket_tools;
mod tool_listing;
mod tools;
mod tools_chat_boundary_probe;
mod window_name_tools;

use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use yagura_daemon::mcp::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_os::{IpcTransport, PlatformIpc};
use yagura_protocol::relay::RelayFrame;
use yagura_protocol::{
    ClientMessage, DaemonMessage, decode_daemon_message, encode_client_message, frame_encode,
};

use crate::common::relay::{RelayClient, hello};

/// A window's claim on a connection: its ID and its per-boot status token.
#[derive(Debug, Clone)]
pub struct WindowIdentity {
    pub window_id: String,
    pub token: String,
}

/// A [`DaemonTransport`] over a daemon's IPC endpoint, standing in for a
/// client of the endpoint: each round trip opens a connection of its own,
/// identifies on it when it carries an identity, and sends its request.
/// Dropping a round trip mid-flight drops its connection.
pub struct EndpointTransport {
    endpoint: String,
    identity: Option<WindowIdentity>,
    requester: Mutex<RequesterIdentity>,
}

impl EndpointTransport {
    pub fn new(endpoint: impl Into<String>, identity: Option<WindowIdentity>) -> Self {
        Self {
            endpoint: endpoint.into(),
            identity,
            requester: Mutex::new(RequesterIdentity::External),
        }
    }
}

type ClientConnection = <PlatformIpc as IpcTransport>::ClientConnection;

async fn exchange(
    connection: &mut ClientConnection,
    msg: &ClientMessage,
) -> Result<DaemonMessage, TransportError> {
    connection
        .write_all(&frame_encode(&encode_client_message(msg)))
        .await
        .map_err(|err| TransportError::Unavailable(err.to_string()))?;
    let mut len = [0u8; 4];
    connection
        .read_exact(&mut len)
        .await
        .map_err(|err| TransportError::Unavailable(err.to_string()))?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    connection
        .read_exact(&mut body)
        .await
        .map_err(|err| TransportError::Unavailable(err.to_string()))?;
    decode_daemon_message(&body).map_err(|err| TransportError::Protocol(err.to_string()))
}

impl DaemonTransport for EndpointTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        self.requester
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        let mut connection = PlatformIpc::default()
            .connect(&self.endpoint)
            .await
            .map_err(|err| TransportError::Unavailable(err.to_string()))?;
        if let Some(identity) = &self.identity {
            let settled = match exchange(
                &mut connection,
                &ClientMessage::Identify {
                    window_id: identity.window_id.clone(),
                    token: identity.token.clone(),
                },
            )
            .await?
            {
                DaemonMessage::Identified { window_id } => RequesterIdentity::Window(window_id),
                DaemonMessage::Error { code, message, .. } => RequesterIdentity::Refused(format!(
                    "the daemon refused this window identity ({code}): {message}"
                )),
                other => {
                    return Err(TransportError::Protocol(format!(
                        "unexpected reply to identify: {other:?}"
                    )));
                }
            };
            *self
                .requester
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = settled;
        }
        exchange(&mut connection, &msg).await
    }
}

/// A transport for a server whose requests never reach a daemon.
pub struct NoDaemon;

impl DaemonTransport for NoDaemon {
    fn requester_identity(&self) -> RequesterIdentity {
        RequesterIdentity::External
    }

    async fn round_trip(&self, _msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        Err(TransportError::Unavailable("no daemon".to_string()))
    }
}

/// An MCP client on a relay session of a daemon, the way a CLI reaches the
/// daemon through its window's relay.
pub struct McpSession {
    client: RelayClient,
    next_id: i64,
}

impl McpSession {
    /// Opens a session to `endpoint`; with `identity` (a window ID and its
    /// token) it claims that window, as a window's own CLI does.
    pub async fn open(endpoint: &str, identity: Option<(&str, &str)>) -> Self {
        let (client, _) = RelayClient::welcomed(endpoint, hello(identity, None)).await;
        Self { client, next_id: 1 }
    }

    /// The MCP handshake: `initialize`, then `notifications/initialized`.
    pub async fn initialize(&mut self) {
        let id = self
            .start(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "yagura-mcp-test", "version": "0" }
                }),
            )
            .await;
        let response = self.response_to(id).await;
        assert!(
            response.get("result").is_some(),
            "initialize must succeed, got {response}"
        );
        self.client
            .carry(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string())
            .await;
    }

    /// Calls the tool `name` and returns the whole JSON-RPC response.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.start_tool_call(name, arguments).await;
        self.response_to(id).await
    }

    /// Ends the session, as a relay that exits does.
    pub async fn kill(self) {
        drop(self.client);
    }

    /// Sends a `tools/call` for `name` without waiting for its answer, and
    /// returns the request id to read the answer or cancel the call by.
    pub async fn start_tool_call(&mut self, name: &str, arguments: Value) -> i64 {
        self.start(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Cancels the request `id` the way an MCP client gives up on a call.
    pub async fn cancel(&mut self, id: i64) {
        self.client
            .carry(
                &json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": id, "reason": "the client gave up on this call" },
                })
                .to_string(),
            )
            .await;
    }

    /// An MCP `ping`, answered by the server itself without reaching the
    /// daemon. Its answer is read as the next line.
    pub async fn ping(&mut self) {
        let id = self.start("ping", json!({})).await;
        self.response_to(id).await;
    }

    /// Reads the next line and requires it to be the answer to `id`: no other
    /// call may have answered in between.
    pub async fn response_to(&mut self, id: i64) -> Value {
        let response = self.next_line().await;
        assert_eq!(
            response.get("id").and_then(Value::as_i64),
            Some(id),
            "expected the answer to request {id} next, got {response}"
        );
        response
    }

    /// Reads one answer per id in `ids`, in whatever order they arrive, and
    /// returns them in the order of `ids`.
    pub async fn responses_to(&mut self, ids: &[i64]) -> Vec<Value> {
        let mut answers: Vec<Option<Value>> = vec![None; ids.len()];
        for _ in ids {
            let response = self.next_line().await;
            let id = response.get("id").and_then(Value::as_i64);
            let slot = ids
                .iter()
                .position(|expected| Some(*expected) == id)
                .unwrap_or_else(|| panic!("expected an answer to one of {ids:?}, got {response}"));
            assert!(
                answers[slot].is_none(),
                "request {id:?} was answered twice: {response}"
            );
            answers[slot] = Some(response);
        }
        answers.into_iter().map(Option::unwrap).collect()
    }

    async fn start(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.client
            .request(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        id
    }

    async fn next_line(&mut self) -> Value {
        match self.client.next_frame().await {
            RelayFrame::Out { line, seq, receipt } => {
                if let (Some(seq), Some(true)) = (seq, receipt) {
                    self.client.send(&RelayFrame::Done { seq }).await;
                }
                serde_json::from_str(&line)
                    .unwrap_or_else(|err| panic!("expected a JSON-RPC line, got {line:?}: {err}"))
            }
            other => panic!("expected relay_out, got {other:?}"),
        }
    }
}

/// A `tools/call` response's `isError` flag and its single text block: the
/// shape every tool answers in, success or failure.
pub fn tool_reply(response: &Value) -> (bool, String) {
    let result = response
        .get("result")
        .unwrap_or_else(|| panic!("a tool call must return a JSON-RPC result, got {response}"));
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("a tool result must carry content, got {response}"));
    let [block] = content.as_slice() else {
        panic!("a tool result must carry exactly one content block, got {response}");
    };
    let text = block
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the content block must be text, got {response}"))
        .to_string();
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (is_error, text)
}
