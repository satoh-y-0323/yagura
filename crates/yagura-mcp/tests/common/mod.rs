#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Fixtures shared by the tests that drive the compiled relay (`yagura-mcp`)
//! as a CLI does, against a stand-in daemon that speaks the daemon's side of
//! the relay contract (`yagura_protocol::relay`).
//!
//! The stand-in binds the endpoint of an instance reserved for one test before
//! the relay starts. Binding claims the name exclusively, so the relay can only
//! ever reach the stand-in, never a resident daemon.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yagura_os::{InstanceNamespace, IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::relay::{RelayFrame, RelayRefusedKind, RelayResume};
use yagura_protocol::{INSTANCE_ENV, frame_decode, frame_encode};
use yagura_test_support::HANG_GUARD;
pub use yagura_test_support::relay_stdio::{RelayProcess, rpc_error_word, tool_call_error};

/// The contract this build's relay speaks.
pub const CONTRACT: u32 = 1;

/// Starts the relay cargo built for this test run against the instance
/// `instance`, with `env` set and `args` on its command line.
pub fn spawn_relay(instance: &str, env: &[(&str, &str)], args: &[&OsStr]) -> RelayProcess {
    let mut all = vec![(INSTANCE_ENV, instance)];
    all.extend_from_slice(env);
    RelayProcess::spawn(Path::new(env!("CARGO_BIN_EXE_yagura-mcp")), &all, args)
}

/// Starts the relay with no instance variable at all.
pub fn spawn_relay_without_instance(env: &[(&str, &str)]) -> RelayProcess {
    RelayProcess::spawn(Path::new(env!("CARGO_BIN_EXE_yagura-mcp")), env, &[])
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// A stand-in daemon at one instance's endpoint. Each connection the relay
/// makes is one attempt: handed to the test by [`FakeDaemon::accept`], or,
/// while [`FakeDaemon::close_attempts`] is on, read up to its first frame and
/// closed at once. The endpoint stays bound throughout.
pub struct FakeDaemon {
    attempts: mpsc::UnboundedReceiver<FakeConn>,
    closed: mpsc::UnboundedReceiver<Option<RelayFrame>>,
    closing: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeDaemon {
    /// Binds the endpoint of the named instance `instance` and starts
    /// accepting on it.
    pub async fn bind(instance: &str) -> Self {
        let namespace = yagura_os::resolve_instance_from(Some(OsStr::new(instance)))
            .unwrap_or_else(|err| panic!("{instance:?} must be a valid instance id: {err}"));
        Self::bind_namespace(&namespace).await
    }

    /// Binds the endpoint `namespace` resolves to and starts accepting on it.
    ///
    /// # Panics
    ///
    /// Panics if the endpoint is already claimed: proceeding would let the
    /// relay reach whatever owns it.
    pub async fn bind_namespace(namespace: &InstanceNamespace) -> Self {
        let ipc = PlatformIpc::default();
        let user_id = ipc
            .current_user_id()
            .expect("current_user_id must resolve in a normal test-runner session");
        let endpoint = yagura_os::default_endpoint_owning(&user_id, namespace);
        let security = ipc
            .endpoint_security()
            .expect("endpoint_security must succeed in a normal test-runner session");
        let mut listener = ipc.bind(&endpoint, security).await.unwrap_or_else(|err| {
            panic!(
                "cannot claim endpoint {endpoint:?}: {err}. A daemon running under this \
                 instance must be stopped first; this test does not proceed against an \
                 endpoint it does not own"
            )
        });
        let (attempt_tx, attempts) = mpsc::unbounded_channel();
        let (closed_tx, closed) = mpsc::unbounded_channel();
        let closing = Arc::new(AtomicBool::new(false));
        let closing_flag = Arc::clone(&closing);
        let task = tokio::spawn(async move {
            while let Ok(conn) = ipc.accept(&mut listener).await {
                let conn = FakeConn::new(Box::new(conn));
                if closing_flag.load(Ordering::SeqCst) {
                    let closed_tx = closed_tx.clone();
                    tokio::spawn(async move {
                        let mut conn = conn;
                        let first = conn.first_frame_quietly().await;
                        drop(conn);
                        let _ = closed_tx.send(first);
                    });
                } else if attempt_tx.send(conn).is_err() {
                    return;
                }
            }
        });
        Self {
            attempts,
            closed,
            closing,
            task,
        }
    }

    /// The relay's next attempt, handed to the test.
    pub async fn accept(&mut self) -> FakeConn {
        tokio::time::timeout(HANG_GUARD, self.attempts.recv())
            .await
            .unwrap_or_else(|_| panic!("the relay made no further attempt to reach the daemon"))
            .expect("the stand-in daemon stopped accepting")
    }

    /// From now on, every new attempt is closed at once after its first frame
    /// (`true`), or handed to the test (`false`).
    pub fn close_attempts(&self, on: bool) {
        self.closing.store(on, Ordering::SeqCst);
    }

    /// The first frame of the next attempt closed by
    /// [`FakeDaemon::close_attempts`], when it was a relay frame.
    pub async fn closed_attempt(&mut self) -> Option<RelayFrame> {
        tokio::time::timeout(HANG_GUARD, self.closed.recv())
            .await
            .unwrap_or_else(|_| panic!("the relay made no attempt while attempts were closed"))
            .expect("the stand-in daemon stopped accepting")
    }
}

/// The identity and resumption a relay named in its `relay_hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub window_id: Option<String>,
    pub token: Option<String>,
    pub resume: Option<RelayResume>,
}

/// One connection of the relay to the stand-in daemon.
pub struct FakeConn {
    stream: Box<dyn Stream>,
    buffer: Vec<u8>,
}

impl FakeConn {
    fn new(stream: Box<dyn Stream>) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    /// Reads more bytes into the buffer; `false` once the relay closed.
    async fn fill(&mut self) -> bool {
        let mut chunk = vec![0u8; 64 * 1024];
        match tokio::time::timeout(HANG_GUARD, self.stream.read(&mut chunk)).await {
            Ok(Ok(0) | Err(_)) => false,
            Ok(Ok(read)) => {
                self.buffer.extend_from_slice(&chunk[..read]);
                true
            }
            Err(_) => panic!("timed out waiting for the relay's next frame"),
        }
    }

    async fn next_body(&mut self) -> Option<Vec<u8>> {
        loop {
            match frame_decode(&self.buffer) {
                Ok(Some((body, consumed))) => {
                    let body = body.to_vec();
                    self.buffer.drain(..consumed);
                    return Some(body);
                }
                Ok(None) => {}
                Err(err) => panic!("the relay sent a frame the IPC framing refuses: {err:?}"),
            }
            if !self.fill().await {
                return None;
            }
        }
    }

    async fn first_frame_quietly(&mut self) -> Option<RelayFrame> {
        let body = self.next_body().await?;
        serde_json::from_slice(&body).ok()
    }

    /// The next frame, or `None` once the relay closed the connection.
    /// Panics on a body that is not a relay frame, naming it.
    pub async fn next_frame_or_closed(&mut self) -> Option<RelayFrame> {
        let body = self.next_body().await?;
        Some(serde_json::from_slice(&body).unwrap_or_else(|err| {
            panic!(
                "the relay must speak relay frames only ({err}); it sent {}",
                String::from_utf8_lossy(&body)
            )
        }))
    }

    pub async fn next_frame(&mut self) -> RelayFrame {
        self.next_frame_or_closed()
            .await
            .expect("the relay closed its connection")
    }

    /// The length the next frame declares, read from its prefix alone: the
    /// rest of the frame is left unread.
    pub async fn next_declared_length(&mut self) -> usize {
        while self.buffer.len() < 4 {
            assert!(self.fill().await, "the relay closed before its next frame");
        }
        let prefix: [u8; 4] = self.buffer[..4].try_into().unwrap();
        u32::from_le_bytes(prefix) as usize
    }

    /// The relay's `relay_hello`, which must be its first frame.
    pub async fn hello(&mut self) -> Hello {
        match self.next_frame().await {
            RelayFrame::Hello {
                contract,
                window_id,
                token,
                resume,
            } => {
                assert_eq!(contract, CONTRACT, "the relay names the contract it speaks");
                Hello {
                    window_id,
                    token,
                    resume,
                }
            }
            other => panic!("the relay's first frame must be relay_hello, got {other:?}"),
        }
    }

    pub async fn send(&mut self, frame: &RelayFrame) {
        let body = serde_json::to_vec(frame).expect("a relay frame encodes");
        // A relay that already closed is the case's business, not this write's.
        let _ = self.stream.write_all(&frame_encode(&body)).await;
    }

    pub async fn welcome(&mut self, tools: &str) {
        self.send(&RelayFrame::Welcome {
            contract: CONTRACT,
            tools: tools.to_string(),
        })
        .await;
    }

    pub async fn refuse(&mut self, kind: RelayRefusedKind, contracts: Vec<u32>) {
        self.send(&RelayFrame::Refused {
            kind,
            contracts,
            reason: "refused by the stand-in daemon".to_string(),
        })
        .await;
    }

    /// The next frame, which must be `relay_in`: its line as sent and its
    /// sequence number.
    pub async fn relay_in(&mut self) -> (String, Option<u64>) {
        match self.next_frame().await {
            RelayFrame::In { line, seq } => (line, seq),
            other => panic!("expected relay_in, got {other:?}"),
        }
    }

    /// The next `relay_in`, which must carry a request: its line as JSON and
    /// its sequence number.
    pub async fn request(&mut self) -> (Value, u64) {
        let (line, seq) = self.relay_in().await;
        let value = parse(&line);
        let seq = seq.unwrap_or_else(|| panic!("a request must carry a seq, got {line}"));
        (value, seq)
    }

    /// Answers the request `request` carried under `seq` with a result naming
    /// its method, and returns the answer line.
    pub async fn answer(&mut self, request: &Value, seq: u64, receipt: bool) -> Value {
        let line = answer_line(request);
        self.send(&RelayFrame::Out {
            line: line.to_string(),
            seq: Some(seq),
            receipt: receipt.then_some(true),
        })
        .await;
        line
    }

    /// Answers every request carried until requests with each of the `ids`
    /// have been carried (and answered), and returns every line carried on the
    /// way, in order.
    pub async fn answer_until(&mut self, ids: &[Value]) -> Vec<Value> {
        let mut carried: Vec<Value> = Vec::new();
        let seen = |carried: &[Value], id: &Value| {
            carried
                .iter()
                .any(|line| line.get("method").is_some() && line.get("id") == Some(id))
        };
        while !ids.iter().all(|id| seen(&carried, id)) {
            let (line, seq) = self.relay_in().await;
            let value = parse(&line);
            if let Some(seq) = seq {
                self.answer(&value, seq, false).await;
            }
            carried.push(value);
        }
        carried
    }
}

fn parse(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|err| panic!("expected a JSON line ({err}): {line}"))
}

/// The stand-in daemon's answer to `request`: a result naming its method.
pub fn answer_line(request: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": {"answered_by": "stand-in", "method": request["method"]},
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
            "clientInfo": {"name": "yagura-mcp-test", "version": "0"},
        },
    })
}

pub fn initialized_line() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

pub fn tools_list_line(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}})
}

pub fn ping_line(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping"})
}

pub fn tool_call_line(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

pub fn cancelled_line(request_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": request_id, "reason": "the client gave up on this call"},
    })
}
