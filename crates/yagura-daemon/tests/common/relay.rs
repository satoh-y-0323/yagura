#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A client that speaks the relay contract (`yagura_protocol::relay`) to a
//! daemon's IPC endpoint the way a window's relay does, and carries MCP
//! JSON-RPC lines over it. The test plays the CLI and the relay at once.

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use yagura_protocol::relay::{RelayFrame, RelayResume};
use yagura_protocol::{Declaration, frame_encode};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::Connection;

/// The contract this build's relay speaks.
pub const CONTRACT: u32 = 1;

/// The `_meta` a stateless (2026-07-28) client puts on every request.
pub fn stateless_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}},
        "io.modelcontextprotocol/clientInfo": {"name": "relay-session-test", "version": "0"},
    })
}

/// `server/discover` as a stateless CLI opens with it.
pub fn discover_line(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "server/discover",
        "params": {"_meta": stateless_meta()},
    })
}

/// `initialize` as a CLI that opens with it sends it.
pub fn initialize_line(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "relay-session-test", "version": "0"},
        },
    })
}

pub fn initialized_line() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

/// The notification a client may send right after `server/discover`, before its first
/// other request.
pub fn roots_list_changed_line() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/roots/list_changed", "params": {}})
}

pub fn cancelled_line(request_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": request_id},
    })
}

/// A `tools/call`; `stateless` adds the request `_meta`.
pub fn tool_call_line(id: u64, name: &str, arguments: Value, stateless: bool) -> Value {
    let mut params = json!({"name": name, "arguments": arguments});
    if stateless {
        params["_meta"] = stateless_meta();
    }
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
}

/// A `relay_hello` for a window (`Some((window_id, token))`) or for no window,
/// declaring no profile, as a relay of an older registration sends it.
pub fn hello(identity: Option<(&str, &str)>, resume: Option<RelayResume>) -> RelayFrame {
    RelayFrame::Hello {
        contract: CONTRACT,
        window_id: identity.map(|(window_id, _)| window_id.to_string()),
        token: identity.map(|(_, token)| token.to_string()),
        resume,
        declaration: None,
    }
}

/// A first `relay_hello` for the window `window_id`, declaring the profile `profile`
/// as a relay started with that profile's id does.
pub fn hello_declaring(window_id: &str, token: &str, profile: &str) -> RelayFrame {
    RelayFrame::Hello {
        contract: CONTRACT,
        window_id: Some(window_id.to_string()),
        token: Some(token.to_string()),
        resume: None,
        declaration: Some(Declaration {
            profile: profile.to_string(),
        }),
    }
}

/// One relay connection to a daemon.
pub struct RelayClient {
    conn: Connection,
    next_seq: u64,
}

impl RelayClient {
    pub async fn connect(endpoint: &str) -> Self {
        Self {
            conn: yagura_test_support::ipc::connect_with_retry(endpoint).await,
            next_seq: 1,
        }
    }

    /// Connects, sends `hello` and returns the daemon's first frame.
    pub async fn open(endpoint: &str, hello: RelayFrame) -> (Self, RelayFrame) {
        let mut client = Self::connect(endpoint).await;
        client.send(&hello).await;
        let answer = client.next_frame().await;
        (client, answer)
    }

    /// [`Self::open`], expecting a `relay_welcome`; returns its `tools`.
    pub async fn welcomed(endpoint: &str, hello: RelayFrame) -> (Self, String) {
        let (client, answer) = Self::open(endpoint, hello).await;
        match answer {
            RelayFrame::Welcome { contract, tools } => {
                assert_eq!(
                    contract, CONTRACT,
                    "the session speaks the relay's contract"
                );
                (client, tools)
            }
            other => panic!("expected relay_welcome, got {other:?}"),
        }
    }

    pub async fn send(&mut self, frame: &RelayFrame) {
        let body = serde_json::to_vec(frame).expect("a relay frame encodes");
        self.send_body(&body).await;
    }

    /// Writes `body` framed as the IPC framing does, whatever it holds.
    pub async fn send_body(&mut self, body: &[u8]) {
        self.send_bytes(&frame_encode(body)).await;
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        self.conn
            .write_all(bytes)
            .await
            .expect("write to the daemon's endpoint");
    }

    /// Carries a JSON-RPC request with the next relay sequence number and
    /// returns that number.
    pub async fn request(&mut self, line: &Value) -> u64 {
        self.request_raw(&line.to_string()).await
    }

    /// [`Self::request`] for a line carried as written, whatever it holds.
    pub async fn request_raw(&mut self, line: &str) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.send(&RelayFrame::In {
            line: line.to_string(),
            seq: Some(seq),
        })
        .await;
        seq
    }

    /// Carries a JSON-RPC line without a sequence number (a notification, or
    /// a line the relay could not read).
    pub async fn carry(&mut self, line: &str) {
        self.send(&RelayFrame::In {
            line: line.to_string(),
            seq: None,
        })
        .await;
    }

    /// The next frame, or `None` once the daemon closed the connection.
    /// Panics on a body that is not a relay frame, naming it.
    pub async fn next_frame_or_closed(&mut self) -> Option<RelayFrame> {
        let mut len = [0u8; 4];
        match tokio::time::timeout(HANG_GUARD, self.conn.read_exact(&mut len)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return None,
            Err(_) => panic!("timed out waiting for the daemon's next relay frame"),
        }
        let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
        match tokio::time::timeout(HANG_GUARD, self.conn.read_exact(&mut body)).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => panic!("the connection ended inside a frame body: {err}"),
            Err(_) => panic!("timed out reading a relay frame body"),
        }
        Some(serde_json::from_slice(&body).unwrap_or_else(|err| {
            panic!(
                "the daemon must answer a relay connection with relay frames only ({err}); \
                 it sent {}",
                String::from_utf8_lossy(&body)
            )
        }))
    }

    pub async fn next_frame(&mut self) -> RelayFrame {
        self.next_frame_or_closed()
            .await
            .expect("the daemon closed the relay connection")
    }

    /// Reads `relay_out` lines until the answer to `seq`, acknowledging any
    /// answer that asks for a receipt. Returns the answer and every line seen
    /// before it.
    pub async fn answer_with_preceding(&mut self, seq: u64) -> (Value, Vec<Value>) {
        let mut before = Vec::new();
        loop {
            match self.next_frame().await {
                RelayFrame::Out {
                    line,
                    seq: answered,
                    receipt,
                } => {
                    let value: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|err| panic!("relay_out must carry JSON ({err}): {line}"));
                    if let (Some(answered), Some(true)) = (answered, receipt) {
                        self.send(&RelayFrame::Done { seq: answered }).await;
                    }
                    if answered == Some(seq) {
                        return (value, before);
                    }
                    before.push(value);
                }
                other => panic!("expected relay_out while waiting for seq {seq}, got {other:?}"),
            }
        }
    }

    pub async fn answer(&mut self, seq: u64) -> Value {
        self.answer_with_preceding(seq).await.0
    }

    /// Reads `relay_out` lines until one satisfies `accept`, and returns it.
    pub async fn line_where(&mut self, what: &str, accept: impl Fn(&Value) -> bool) -> Value {
        loop {
            match self.next_frame().await {
                RelayFrame::Out { line, .. } => {
                    let value: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|err| panic!("relay_out must carry JSON ({err}): {line}"));
                    if accept(&value) {
                        return value;
                    }
                }
                other => panic!("expected relay_out while waiting for {what}, got {other:?}"),
            }
        }
    }

    /// Waits for the daemon to close the connection; any frame on the way
    /// fails the case.
    pub async fn expect_closed(&mut self, what: &str) {
        if let Some(frame) = self.next_frame_or_closed().await {
            panic!("the daemon must close the connection ({what}), it sent {frame:?}");
        }
    }
}

/// The JSON a tool's successful `tools/call` answer carries in its one text
/// content. Panics on an error answer.
pub fn tool_json(answer: &Value) -> Value {
    let result = answer
        .get("result")
        .unwrap_or_else(|| panic!("expected a tools/call result, got {answer}"));
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "expected a successful tools/call, got {answer}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tool result carries one text content, got {answer}"));
    serde_json::from_str(text).unwrap_or_else(|err| panic!("the text is JSON ({err}): {text}"))
}
