//! A relay session: the daemon's side of one relay connection
//! ([`yagura_protocol::relay`]).
//!
//! The session identifies its window the way `identify` does, holds one rmcp
//! server, feeds it the CLI's lines from `relay_in` and sends what it writes
//! back as `relay_out`. Every request a tool makes runs on a connection of the
//! session's own into the IPC connection body, so MCP stays a thin layer over
//! the IPC; when the relay connection ends, every one of them ends with it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use rmcp::model::{ClientNotification, ClientRequest, ServerConfig, ServerResult};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData, RoleServer, Service};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
    ReadHalf, WriteHalf,
};
use tokio::sync::{Semaphore, watch};
use yagura_protocol::relay::{
    Line, RESOURCE_EXHAUSTED, RelayFrame, RelayRefusedKind, RelayResume, SUPPORTED_CONTRACTS,
    read_line, tool_call_error_line,
};
use yagura_protocol::{
    ClientMessage, DaemonMessage, FrameError, decode_daemon_message, encode_client_message,
    frame_decode, frame_encode,
};

use super::server::YaguraMcpServer;
use super::{DaemonTransport, RequesterIdentity, TransportError};
use crate::control::RequestOrigin;
use crate::ipc::{IpcServer, VerifiedPeer};

/// How many sessions may be identified as one window at once.
const SESSIONS_PER_WINDOW: usize = 4;

/// How many requests one session runs at once.
const REQUESTS_AT_ONCE: usize = 4;

/// How many `tools/call` requests may wait for one of the
/// [`REQUESTS_AT_ONCE`]; one past them is answered `resource_exhausted`.
const REQUESTS_WAITING: usize = 16;

/// The buffer between the session and its rmcp server, each way.
const RMCP_PIPE_BUFFER: usize = 64 * 1024;

/// Read size of the relay connection.
const READ_CHUNK_LEN: usize = 8192;

/// Whether `body`, the first frame of a connection, opens a relay session.
pub(crate) fn is_relay_hello(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|kind| kind == "relay_hello")
        })
        .unwrap_or(false)
}

/// How many relay sessions are open: per window, and without one.
#[derive(Default)]
pub(crate) struct SessionCounts {
    counts: Mutex<Counts>,
}

#[derive(Default)]
struct Counts {
    windowless: usize,
    windows: HashMap<String, usize>,
}

/// Which limit a session counts against.
#[derive(Clone)]
enum Bucket {
    Windowless,
    Window(String),
}

/// One open session's place in [`SessionCounts`], given back on drop.
struct SessionSlot {
    counts: Arc<SessionCounts>,
    bucket: Bucket,
}

impl SessionCounts {
    /// A place for one more session in `bucket`, or `None` at its limit:
    /// [`SESSIONS_PER_WINDOW`] for a window, `max_windowless` without one.
    fn take(self: &Arc<Self>, bucket: Bucket, max_windowless: usize) -> Option<SessionSlot> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let counted = match &bucket {
            Bucket::Windowless => &mut counts.windowless,
            Bucket::Window(window_id) => counts.windows.entry(window_id.clone()).or_default(),
        };
        let limit = match bucket {
            Bucket::Windowless => max_windowless,
            Bucket::Window(_) => SESSIONS_PER_WINDOW,
        };
        if *counted >= limit {
            return None;
        }
        *counted += 1;
        Some(SessionSlot {
            counts: Arc::clone(self),
            bucket,
        })
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        let mut counts = self
            .counts
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match &self.bucket {
            Bucket::Windowless => counts.windowless = counts.windowless.saturating_sub(1),
            Bucket::Window(window_id) => {
                if let Some(counted) = counts.windows.get_mut(window_id) {
                    *counted = counted.saturating_sub(1);
                    if *counted == 0 {
                        counts.windows.remove(window_id);
                    }
                }
            }
        }
    }
}

/// The relay connection's frames, read off its stream.
struct FramesIn<S> {
    reader: ReadHalf<S>,
    buf: Vec<u8>,
    chunk: Box<[u8; READ_CHUNK_LEN]>,
}

/// A frame that breaks the contract: over the size limit, or a body that is
/// no relay frame.
struct ContractBroken;

impl<S: AsyncRead> FramesIn<S> {
    /// The next frame, or `Ok(None)` once the relay closed the connection.
    /// Cancel-safe: what was read stays in the buffer.
    async fn next(&mut self) -> Result<Option<RelayFrame>, ContractBroken> {
        loop {
            match frame_decode(&self.buf) {
                Ok(Some((body, consumed))) => {
                    let frame = serde_json::from_slice(body).map_err(|_| ContractBroken);
                    self.buf.drain(..consumed);
                    return frame.map(Some);
                }
                Ok(None) => {}
                Err(FrameError::TooLarge) => return Err(ContractBroken),
            }
            match self.reader.read(&mut self.chunk[..]).await {
                Ok(0) | Err(_) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&self.chunk[..n]),
            }
        }
    }
}

/// Writes one relay frame; `false` once the relay connection is gone.
async fn send<S: AsyncWrite>(writer: &mut WriteHalf<S>, frame: &RelayFrame) -> bool {
    let Ok(body) = serde_json::to_vec(frame) else {
        return false;
    };
    writer.write_all(&frame_encode(&body)).await.is_ok()
}

fn refused(kind: RelayRefusedKind, reason: &str) -> RelayFrame {
    RelayFrame::Refused {
        kind,
        contracts: SUPPORTED_CONTRACTS.to_vec(),
        reason: reason.to_string(),
    }
}

/// Serves one relay connection whose first frame, in `read_buf`, is a
/// `relay_hello`.
pub(crate) async fn serve<S>(
    server: Arc<IpcServer>,
    verified: VerifiedPeer,
    stream: S,
    read_buf: Vec<u8>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut frames = FramesIn {
        reader,
        buf: read_buf,
        chunk: Box::new([0; READ_CHUNK_LEN]),
    };
    let Ok(Some(RelayFrame::Hello {
        contract,
        window_id,
        token,
        resume,
    })) = frames.next().await
    else {
        return;
    };
    let max_windowless = server.config().max_windows;

    if !SUPPORTED_CONTRACTS.contains(&contract) {
        let Some(_slot) = server
            .relay_sessions()
            .take(Bucket::Windowless, max_windowless)
        else {
            send(
                &mut writer,
                &refused(
                    RelayRefusedKind::Limit,
                    "the daemon holds as many sessions as it may",
                ),
            )
            .await;
            return;
        };
        let reason = format!("the daemon does not speak relay contract {contract}");
        if !send(&mut writer, &refused(RelayRefusedKind::Contract, &reason)).await {
            return;
        }
        // Carries nothing, and still counts toward the sessions, until the
        // relay closes it.
        while let Ok(Some(_)) = frames.next().await {}
        return;
    }

    let identity = match (window_id, token) {
        (Some(window_id), Some(token)) => identify(&server, &verified, window_id, token).await,
        _ => Identity::none(),
    };
    let bucket = match &identity.origin {
        RequestOrigin::Window(window_id) => Bucket::Window(window_id.clone()),
        RequestOrigin::External | RequestOrigin::Approver => Bucket::Windowless,
    };
    let Some(_slot) = server.relay_sessions().take(bucket, max_windowless) else {
        send(
            &mut writer,
            &refused(
                RelayRefusedKind::Limit,
                "the daemon holds as many sessions as it may",
            ),
        )
        .await;
        return;
    };

    let (closed_tx, closed_rx) = watch::channel(false);
    let transport = SessionTransport {
        server,
        verified,
        origin: identity.origin,
        requester: identity.requester,
        permits: Semaphore::new(REQUESTS_AT_ONCE),
        closed: closed_rx,
    };
    let mut rmcp = Rmcp::start(YaguraMcpServer::new(transport));
    run_session(&mut frames, &mut writer, &mut rmcp, contract, resume).await;

    // Every connection a call holds into the connection body ends here, which
    // is how the body learns that their requester left.
    closed_tx.send_replace(true);
    rmcp.task.abort();
    drop(identity.claim);
}

/// The window a session identified as, and the connection that holds the
/// window's claim for as long as the session lasts.
struct Identity {
    origin: RequestOrigin,
    requester: RequesterIdentity,
    claim: Option<DuplexStream>,
}

impl Identity {
    fn none() -> Self {
        Self {
            origin: RequestOrigin::External,
            requester: RequesterIdentity::External,
            claim: None,
        }
    }
}

/// Identifies as `window_id` through `identify` on a connection of the
/// session's own, so the claim passes the same checks and counts its failures
/// the same way; the connection is kept, and holds the claim, when accepted.
async fn identify(
    server: &Arc<IpcServer>,
    verified: &VerifiedPeer,
    window_id: String,
    token: String,
) -> Identity {
    let mut connection = server.open_internal_connection(verified, RequestOrigin::External);
    let reply = exchange(
        &mut connection,
        &ClientMessage::Identify { window_id, token },
    )
    .await;
    match reply {
        Ok(DaemonMessage::Identified { window_id }) => Identity {
            origin: RequestOrigin::Window(window_id.clone()),
            requester: RequesterIdentity::Window(window_id),
            claim: Some(connection),
        },
        Ok(DaemonMessage::Error { code, message, .. }) => Identity {
            requester: RequesterIdentity::Refused(format!(
                "the daemon refused this window identity ({code}): {message}"
            )),
            ..Identity::none()
        },
        Ok(other) => Identity {
            requester: RequesterIdentity::Refused(format!(
                "the window identity could not be settled: unexpected reply {}",
                super::tools::message_kind(&other)
            )),
            ..Identity::none()
        },
        Err(TransportError::Unavailable(reason) | TransportError::Protocol(reason)) => Identity {
            requester: RequesterIdentity::Refused(format!(
                "the window identity could not be settled: {reason}"
            )),
            ..Identity::none()
        },
    }
}

/// Writes `msg` on `connection` and reads its one answer.
async fn exchange(
    connection: &mut DuplexStream,
    msg: &ClientMessage,
) -> Result<DaemonMessage, TransportError> {
    connection
        .write_all(&frame_encode(&encode_client_message(msg)))
        .await
        .map_err(|err| TransportError::Unavailable(err.to_string()))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_LEN];
    loop {
        match frame_decode(&buf) {
            Ok(Some((body, _))) => {
                return decode_daemon_message(body)
                    .map_err(|err| TransportError::Protocol(err.to_string()));
            }
            Ok(None) => {}
            Err(err) => return Err(TransportError::Protocol(err.to_string())),
        }
        match connection.read(&mut chunk).await {
            Ok(0) => {
                return Err(TransportError::Unavailable(
                    "the connection ended before the daemon answered".to_string(),
                ));
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err) => return Err(TransportError::Unavailable(err.to_string())),
        }
    }
}

/// The transport of one session's tools: each round trip runs on a new
/// connection of the session's own, acting as the window the session
/// identified as, at most [`REQUESTS_AT_ONCE`] at a time.
pub(crate) struct SessionTransport {
    server: Arc<IpcServer>,
    verified: VerifiedPeer,
    origin: RequestOrigin,
    requester: RequesterIdentity,
    permits: Semaphore,
    /// Turns `true` when the relay connection ends.
    closed: watch::Receiver<bool>,
}

fn session_ended() -> TransportError {
    TransportError::Unavailable("the relay session ended".to_string())
}

impl DaemonTransport for SessionTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        self.requester.clone()
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        let mut closed = self.closed.clone();
        let ended = async move {
            let _ = closed.wait_for(|closed| *closed).await;
        };
        tokio::pin!(ended);
        let _permit = tokio::select! {
            biased;
            () = &mut ended => return Err(session_ended()),
            permit = self.permits.acquire() => permit.map_err(|_| session_ended())?,
        };
        let mut connection = self
            .server
            .open_internal_connection(&self.verified, self.origin.clone());
        tokio::select! {
            biased;
            () = &mut ended => Err(session_ended()),
            answer = exchange(&mut connection, &msg) => answer,
        }
    }
}

/// The session's rmcp server, running on its own task, and the session's end
/// of the pipe it reads and writes lines on.
struct Rmcp {
    task: tokio::task::JoinHandle<()>,
    lines_in: WriteHalf<DuplexStream>,
    lines_out: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    /// Turns `true` once rmcp has chosen how the session runs.
    lifecycle: watch::Receiver<bool>,
    /// The summary of the tool list this server serves.
    tools: String,
}

impl Rmcp {
    fn start(server: YaguraMcpServer<SessionTransport>) -> Self {
        let (lifecycle_tx, lifecycle) = watch::channel(false);
        let tools = tool_summary();
        let service = LifecycleWatch {
            inner: server,
            chosen: lifecycle_tx,
        };
        let (rmcp_end, session_end) = tokio::io::duplex(RMCP_PIPE_BUFFER);
        let task = tokio::spawn(async move {
            match rmcp::serve_server(service, rmcp_end).await {
                Ok(running) => {
                    let _ = running.waiting().await;
                }
                Err(err) => tracing::warn!(error = %err, "a relay session's MCP server stopped"),
            }
        });
        let (lines_out, lines_in) = tokio::io::split(session_end);
        Self {
            task,
            lines_in,
            lines_out: BufReader::new(lines_out).lines(),
            lifecycle,
            tools,
        }
    }

    /// Feeds one line to rmcp; `false` once it stopped reading.
    async fn feed(&mut self, line: &str) -> bool {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        self.lines_in.write_all(&bytes).await.is_ok()
    }
}

/// The summary [`RelayFrame::Welcome::tools`] carries: a digest of the tool
/// list, which differs whenever a tool, its description or its schema does.
fn tool_summary() -> String {
    let tools = YaguraMcpServer::<SessionTransport>::tool_router().list_all();
    let listed = serde_json::to_vec(&tools).unwrap_or_default();
    Sha256::digest(&listed)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The rmcp service, watched for the moment rmcp chooses how the session runs.
///
/// rmcp chooses it on `initialize` or on the first request of a stateless
/// session other than `server/discover` (and `ping`, which it answers itself),
/// and only then hands that request to the server. Before that it ends the
/// session on any notification, so the session holds notifications until it
/// sees rmcp hand over such a request.
struct LifecycleWatch<S> {
    inner: S,
    chosen: watch::Sender<bool>,
}

impl<S: Service<RoleServer>> Service<RoleServer> for LifecycleWatch<S> {
    async fn handle_request(
        &self,
        request: ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<ServerResult, ErrorData> {
        if !matches!(request, ClientRequest::DiscoverRequest(_)) {
            self.chosen.send_replace(true);
        }
        self.inner.handle_request(request, context).await
    }

    async fn handle_notification(
        &self,
        notification: ClientNotification,
        context: NotificationContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.inner.handle_notification(notification, context).await
    }

    fn get_info(&self) -> ServerConfig {
        self.inner.get_info()
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
}

/// A JSON-RPC id as a key: its JSON text, so `1` and `"1"` stay apart.
fn id_key(id: &Value) -> String {
    id.to_string()
}

/// The lines a resumed opening holds: the CLI's `initialize`, the answer it
/// was given, and `notifications/initialized` when it sent one.
struct Opening {
    request: String,
    answer: Value,
    initialized: Option<String>,
}

impl Opening {
    fn read(lines: &[String]) -> Option<Self> {
        let mut request = None;
        let mut answer = None;
        let mut initialized = None;
        for line in lines {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match value.get("method").and_then(Value::as_str) {
                Some("initialize") => request = Some(line.clone()),
                Some("notifications/initialized") => initialized = Some(line.clone()),
                Some(_) => {}
                None => answer = Some(value),
            }
        }
        Some(Self {
            request: request?,
            answer: answer?,
            initialized,
        })
    }
}

/// Whether two `initialize` answers open the same session: the same
/// protocol version and capabilities.
fn same_session(recorded: &Value, now: &Value) -> bool {
    let part = |answer: &Value, key: &str| {
        answer
            .get("result")
            .and_then(|result| result.get(key))
            .cloned()
    };
    let version = part(recorded, "protocolVersion");
    let capabilities = part(recorded, "capabilities");
    version.is_some()
        && capabilities.is_some()
        && version == part(now, "protocolVersion")
        && capabilities == part(now, "capabilities")
}

/// Feeds the opening a resumed session carries, without sending its answer
/// again. `false` when the session does not start.
async fn resume_opening<S: AsyncWrite>(
    writer: &mut WriteHalf<S>,
    rmcp: &mut Rmcp,
    lines: &[String],
) -> bool {
    let mismatch = refused(
        RelayRefusedKind::SessionMismatch,
        "the daemon does not open the recorded session the same way",
    );
    let Some(opening) = Opening::read(lines) else {
        send(writer, &mismatch).await;
        return false;
    };
    if !rmcp.feed(&opening.request).await {
        return false;
    }
    let Ok(Some(line)) = rmcp.lines_out.next_line().await else {
        return false;
    };
    let answered = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null);
    if !same_session(&opening.answer, &answered) {
        send(writer, &mismatch).await;
        return false;
    }
    match opening.initialized {
        Some(initialized) => rmcp.feed(&initialized).await,
        None => true,
    }
}

/// Runs a session from its `relay_hello` until the relay connection ends.
async fn run_session<S: AsyncRead + AsyncWrite>(
    frames: &mut FramesIn<S>,
    writer: &mut WriteHalf<S>,
    rmcp: &mut Rmcp,
    contract: u32,
    resume: Option<RelayResume>,
) {
    if let Some(lines) = resume.as_ref().and_then(|resume| resume.opening.as_ref())
        && !resume_opening(writer, rmcp, lines).await
    {
        return;
    }
    let welcome = RelayFrame::Welcome {
        contract,
        tools: rmcp.tools.clone(),
    };
    if !send(writer, &welcome).await {
        return;
    }
    if let Some(resume) = &resume
        && resume.tools != rmcp.tools
    {
        let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        let out = RelayFrame::Out {
            line: changed.to_string(),
            seq: None,
            receipt: None,
        };
        if !send(writer, &out).await {
            return;
        }
    }

    let mut carried = Carried::default();
    let mut chosen = *rmcp.lifecycle.borrow();
    let mut held: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            frame = frames.next() => {
                let Ok(Some(frame)) = frame else {
                    return;
                };
                // `relay_done` and `relay_dropped` acknowledge answers that ask
                // for a receipt, and this session asks for none.
                let RelayFrame::In { line, seq } = frame else {
                    continue;
                };
                match carried.carry(&line, seq, chosen) {
                    Carry::Feed => {
                        if !rmcp.feed(&line).await {
                            return;
                        }
                    }
                    Carry::Hold => held.push(line),
                    Carry::Answer(out) => {
                        if !send(writer, &out).await {
                            return;
                        }
                    }
                }
            }
            line = rmcp.lines_out.next_line() => {
                let Ok(Some(line)) = line else {
                    return;
                };
                let seq = carried.answered(&line);
                let out = RelayFrame::Out { line, seq, receipt: None };
                if !send(writer, &out).await {
                    return;
                }
            }
            changed = rmcp.lifecycle.changed(), if !chosen => {
                if changed.is_err() {
                    return;
                }
                if *rmcp.lifecycle.borrow() {
                    chosen = true;
                    for line in std::mem::take(&mut held) {
                        if !rmcp.feed(&line).await {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// The requests a session carried and has not seen answered.
#[derive(Default)]
struct Carried {
    /// The relay's sequence number of each request, by JSON-RPC id.
    seqs: HashMap<String, u64>,
    /// The `tools/call` requests in flight, by JSON-RPC id.
    calls: HashSet<String>,
}

/// What the session does with a line from the CLI.
enum Carry {
    Feed,
    /// A notification rmcp cannot take before it chose how the session runs.
    Hold,
    /// The session answers it itself.
    Answer(RelayFrame),
}

impl Carried {
    fn carry(&mut self, line: &str, seq: Option<u64>, chosen: bool) -> Carry {
        match read_line(line) {
            Line::Request { id, method } => {
                let key = id_key(&id);
                if method == "tools/call" {
                    if self.calls.len() >= REQUESTS_AT_ONCE + REQUESTS_WAITING {
                        return Carry::Answer(RelayFrame::Out {
                            line: tool_call_error_line(
                                &id,
                                RESOURCE_EXHAUSTED,
                                "the session already runs and holds as many tool calls as it may; \
                                 call again once one of them has answered",
                            ),
                            seq,
                            receipt: None,
                        });
                    }
                    self.calls.insert(key.clone());
                }
                if let Some(seq) = seq {
                    self.seqs.insert(key, seq);
                }
                Carry::Feed
            }
            Line::Notification { method, cancels } => {
                // rmcp answers no request the CLI cancelled.
                if method == "notifications/cancelled"
                    && let Some(cancelled) = cancels
                {
                    let key = id_key(&cancelled);
                    self.calls.remove(&key);
                    self.seqs.remove(&key);
                }
                if chosen { Carry::Feed } else { Carry::Hold }
            }
            Line::Answer { .. } | Line::Other => Carry::Feed,
        }
    }

    /// The sequence number of the request `line` answers, if it answers one.
    fn answered(&mut self, line: &str) -> Option<u64> {
        let Line::Answer { id } = read_line(line) else {
            return None;
        };
        let key = id_key(&id);
        self.calls.remove(&key);
        self.seqs.remove(&key)
    }
}
