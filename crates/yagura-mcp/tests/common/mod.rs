#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Each test binary that includes this module uses only part of it.
#![allow(dead_code)]
// These fixtures exist to start the compiled bridge as a real child process,
// which is what an MCP client does; the PTY spawn path does not apply.
#![allow(clippy::disallowed_methods)]

//! Fixtures shared by the tests that drive the compiled `yagura-mcp` binary
//! over MCP stdio, the way a real MCP client does.
//!
//! The workspace does not enable `rmcp`'s client features, so the JSON-RPC
//! lines are written by hand.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How long a fixture waits for one line from the bridge before failing.
pub const LINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Variables removed from every spawned bridge's environment first, so no
/// ambient value from the test runner's shell (e.g. a developer's
/// `YAGURA_INSTANCE=dev`) can change which daemon it dials or who it claims
/// to be.
const CLEARED_ENV: [&str; 4] = [
    "YAGURA_INSTANCE",
    "YAGURA_STORE_DB",
    "YAGURA_WINDOW_ID",
    "YAGURA_STATUS_TOKEN",
];

/// The compiled `yagura-mcp` binary running as a child, with its stdio
/// piped. Killed on drop, so a failing assertion never leaves it running.
pub struct McpChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpChild {
    /// Starts the binary cargo built for this test run with [`CLEARED_ENV`]
    /// removed and then `env` applied.
    pub fn spawn(env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yagura-mcp"));
        for name in CLEARED_ENV {
            command.env_remove(name);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .expect("failed to spawn the yagura-mcp binary under test");
        let stdin = child.stdin.take().expect("the child's stdin must be piped");
        let stdout = child
            .stdout
            .take()
            .expect("the child's stdout must be piped");
        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        }
    }

    /// The MCP handshake: `initialize`, then `notifications/initialized`.
    pub async fn initialize(&mut self) {
        let response = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "yagura-mcp-test", "version": "0" }
                }),
            )
            .await;
        assert!(
            response.get("result").is_some(),
            "initialize must succeed, got {response}"
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
    }

    /// The names `tools/list` returns.
    pub async fn tool_names(&mut self) -> Vec<String> {
        let response = self.request("tools/list", json!({})).await;
        response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("tools/list must return result.tools, got {response}"))
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .expect("every listed tool must carry a name")
                    .to_string()
            })
            .collect()
    }

    /// Calls the tool `name` and returns the whole JSON-RPC response.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// Sends a `tools/call` for `name` without waiting for its answer, and
    /// returns the request id to read the answer or cancel the call by.
    pub async fn start_tool_call(&mut self, name: &str, arguments: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }))
        .await;
        id
    }

    /// Cancels the request `id` the way an MCP client gives up on a call:
    /// `notifications/cancelled`, which gets no answer of its own.
    pub async fn cancel(&mut self, id: i64) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": id, "reason": "the client gave up on this call" },
        }))
        .await;
    }

    /// An MCP `ping`, answered by the server itself without reaching the
    /// daemon. Its answer is read as the next line.
    pub async fn ping(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }))
            .await;
        self.response_to(id).await;
    }

    /// Reads the next line and requires it to be the answer to `id`: no other
    /// call may have answered in between.
    pub async fn response_to(&mut self, id: i64) -> Value {
        let response = self.read_line().await;
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
            let response = self.read_line().await;
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

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let response = self.read_line().await;
        assert_eq!(
            response.get("id").and_then(Value::as_i64),
            Some(id),
            "expected the response to {method} next, got {response}"
        );
        response
    }

    async fn send(&mut self, value: &Value) {
        let mut line = serde_json::to_string(value).expect("serialize a JSON-RPC message");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write to the yagura-mcp child's stdin");
        self.stdin
            .flush()
            .await
            .expect("flush the yagura-mcp child's stdin");
    }

    async fn read_line(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(LINE_TIMEOUT, self.stdout.read_line(&mut line))
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting {LINE_TIMEOUT:?} for a line on yagura-mcp's stdout")
            })
            .expect("read a line from the yagura-mcp child's stdout");
        serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("expected a JSON-RPC line, got {line:?}: {err}"))
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
