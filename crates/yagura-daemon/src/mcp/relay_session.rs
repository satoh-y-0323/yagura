//! A relay session: the daemon's side of one relay connection
//! ([`yagura_protocol::relay`]).
//!
//! The session identifies its window the way `identify` does, holds one rmcp
//! server, feeds it the CLI's lines from `relay_in` and sends what it writes
//! back as `relay_out`. Every request a tool makes runs on a connection of the
//! session's own into the IPC connection body, so MCP stays a thin layer over
//! the IPC; when the relay connection ends, every one of them ends with it.
//!
//! The reports a `wait_for_report` takes stay the session's ([`Receipts`])
//! until the relay acknowledges the answer that carried them: `relay_done`
//! commits them, and `relay_dropped`, a cancel before the answer, or the end
//! of the session puts them back in the inbox.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use accounted::Accounted;
use rmcp::model::{
    CallToolResult, ClientJsonRpcMessage, ClientNotification, ClientRequest, ContentBlock,
    JsonRpcMessage, RequestId, ServerConfig, ServerResult,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData, RoleServer, Service};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
    ReadHalf, WriteHalf,
};
use tokio::sync::{Semaphore, watch};
use yagura_protocol::relay::{
    HELD_LINES_MAX, INVALID_REQUEST, RESOURCE_EXHAUSTED, RelayFrame, RelayRefusedKind, RelayResume,
    SUPPORTED_CONTRACTS, is_one_line, parse_line, rpc_error_line, tool_call_error_line,
};
use yagura_protocol::{
    ClientMessage, DaemonMessage, FrameError, decode_daemon_message, encode_client_message,
    frame_decode, frame_encode,
};

use super::server::YaguraMcpServer;
use super::{DaemonTransport, RequesterIdentity, TransportError};
use crate::control::RequestOrigin;
use crate::ipc::report::{ReportReceipt, TakenReports};
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

/// A frame that breaks the contract: over the size limit, a body that is no
/// relay frame, or a CLI line in it that is not one line.
struct ContractBroken;

/// Whether every CLI line `frame` carries is one line. Checked as a frame
/// arrives, so no line reaches rmcp, which splits on `\n`, as more than the
/// one the session accounted for.
fn lines_are_one_line(frame: &RelayFrame) -> bool {
    match frame {
        RelayFrame::In { line, .. } => is_one_line(line),
        RelayFrame::Hello { resume, .. } => resume
            .as_ref()
            .and_then(|resume| resume.opening.as_ref())
            .is_none_or(|lines| lines.iter().all(|line| is_one_line(line))),
        RelayFrame::Welcome { .. }
        | RelayFrame::Refused { .. }
        | RelayFrame::Out { .. }
        | RelayFrame::Done { .. }
        | RelayFrame::Dropped { .. } => true,
    }
}

impl<S: AsyncRead> FramesIn<S> {
    /// The next frame, or `Ok(None)` once the relay closed the connection.
    /// Cancel-safe: what was read stays in the buffer.
    async fn next(&mut self) -> Result<Option<RelayFrame>, ContractBroken> {
        loop {
            match frame_decode(&self.buf) {
                Ok(Some((body, consumed))) => {
                    let frame = serde_json::from_slice(body)
                        .ok()
                        .filter(lines_are_one_line)
                        .ok_or(ContractBroken);
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
    let receipts = Arc::new(Receipts::default());
    let transport = SessionTransport {
        server,
        verified,
        origin: identity.origin,
        requester: identity.requester,
        permits: Semaphore::new(REQUESTS_AT_ONCE),
        closed: closed_rx,
        receipts: Arc::clone(&receipts),
    };
    let mut rmcp = Rmcp::start(YaguraMcpServer::new(transport), Arc::clone(&receipts));
    run_session(
        &mut frames,
        &mut writer,
        &mut rmcp,
        &receipts,
        contract,
        resume,
    )
    .await;

    // Every connection a call holds into the connection body ends here, which
    // is how the body learns that their requester left.
    closed_tx.send_replace(true);
    rmcp.task.abort();
    receipts.close();
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
    let mut connection = server.open_internal_connection(verified, RequestOrigin::External, None);
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
    receipts: Arc<Receipts>,
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
        // A take of reports stays this call's until its answer is out.
        let call = matches!(msg, ClientMessage::WaitForReport { .. })
            .then(|| CALL.try_with(Clone::clone).ok())
            .flatten();
        let (receipt, mut taken) = match call {
            Some(_) => {
                let (receipt, taken) = ReportReceipt::channel();
                (Some(receipt), Some(taken))
            }
            None => (None, None),
        };
        let mut connection =
            self.server
                .open_internal_connection(&self.verified, self.origin.clone(), receipt);
        let answer = tokio::select! {
            biased;
            () = &mut ended => Err(session_ended()),
            answer = exchange(&mut connection, &msg) => answer,
        }?;
        // Handed before the answer was written, so it is here by now.
        if let (Some(call), Some(Ok(taken))) = (call, taken.as_mut().map(|taken| taken.try_recv()))
        {
            self.receipts.hold(call, taken);
        }
        Ok(answer)
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
    fn start(server: YaguraMcpServer<SessionTransport>, receipts: Arc<Receipts>) -> Self {
        let (lifecycle_tx, lifecycle) = watch::channel(false);
        let tools = tool_summary();
        let service = LifecycleWatch {
            inner: server,
            chosen: lifecycle_tx,
            calls: Semaphore::new(REQUESTS_AT_ONCE + REQUESTS_WAITING),
            receipts,
        };
        let (rmcp_end, session_end) = tokio::io::duplex(RMCP_PIPE_BUFFER);
        let task = tokio::spawn(async move {
            match rmcp::serve_server(service, rmcp_end).await {
                Ok(running) => {
                    let _ = running.waiting().await;
                }
                Err(err) => tracing::warn!(
                    kind = initialize_error_kind(&err),
                    "a relay session's MCP server stopped"
                ),
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
    async fn feed(&mut self, line: &Accounted) -> bool {
        let mut bytes = Vec::with_capacity(line.as_bytes().len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        self.lines_in.write_all(&bytes).await.is_ok()
    }

    /// Feeds `lines` in order; `false` once rmcp stopped reading.
    async fn feed_all(&mut self, lines: &[Accounted]) -> bool {
        for line in lines {
            if !self.feed(line).await {
                return false;
            }
        }
        true
    }
}

/// Its own module, so [`Accounted::of`] is the only way to make one.
mod accounted {
    use rmcp::transport::async_rw::DEFAULT_MAX_LINE_LENGTH;
    use serde_json::Value;

    /// A CLI line as the session read it: the JSON value it accounted for,
    /// written again. rmcp reads only these, so it never reads a line the
    /// session read otherwise, each is one line of compact JSON, and each is
    /// within the length rmcp reads.
    pub(super) struct Accounted(String);

    /// A value whose line, written again, passes the length rmcp reads: rmcp
    /// would drop it, newline included, unread and unanswered.
    pub(super) struct PastTheLine;

    impl Accounted {
        pub(super) fn of(value: &Value) -> Result<Self, PastTheLine> {
            let line = value.to_string();
            if line.len() < DEFAULT_MAX_LINE_LENGTH {
                Ok(Self(line))
            } else {
                Err(PastTheLine)
            }
        }

        /// The line for a value no one answers, `None` once it is past the
        /// line.
        pub(super) fn unanswered(value: &Value) -> Option<Self> {
            let line = Self::of(value).ok();
            if line.is_none() {
                tracing::debug!(
                    kind = "oversized",
                    "a relay session dropped a line past the length rmcp reads"
                );
            }
            line
        }

        pub(super) fn as_bytes(&self) -> &[u8] {
            self.0.as_bytes()
        }
    }
}

/// Names the failure without its payload: the variants' `Display` carries the
/// received frame, result or error data verbatim.
fn initialize_error_kind(err: &rmcp::service::ServerInitializeError) -> &'static str {
    use rmcp::service::ServerInitializeError as E;
    match err {
        E::ExpectedInitializeRequest(_) => "expected_initialize_request",
        E::ConnectionClosed(_) => "connection_closed",
        E::UnexpectedInitializeResponse(_) => "unexpected_initialize_response",
        E::InitializeFailed(_) => "initialize_failed",
        E::TransportError { .. } => "transport_error",
        E::Cancelled => "cancelled",
        _ => "other",
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

/// The rmcp service, watched for the moment rmcp chooses how the session runs,
/// and for the end of each tool call.
///
/// rmcp chooses it on `initialize` or on the first request of a stateless
/// session other than `server/discover` (and `ping`, which it answers itself),
/// and only then hands that request to the server. Before that it ends the
/// session on any notification, so the session holds notifications until it
/// sees rmcp hand over such a request.
///
/// The tool calls a session runs and holds are counted here, on the requests
/// rmcp runs, so a line rmcp reads no request out of or drops takes no place.
struct LifecycleWatch<S> {
    inner: S,
    chosen: watch::Sender<bool>,
    /// A place for each tool call from the moment rmcp hands it over until its
    /// handler returns: answered, cancelled or failed.
    calls: Semaphore,
    receipts: Arc<Receipts>,
}

/// The answer to a tool call past the [`REQUESTS_AT_ONCE`] running and the
/// [`REQUESTS_WAITING`] waiting, worded as a relay words its own errors
/// (`tool_call_error_line`).
fn refused_call() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "{RESOURCE_EXHAUSTED}: the session already runs and holds as many tool calls as it \
         may; call again once one of them has answered"
    ))])
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
        if !matches!(request, ClientRequest::CallToolRequest(_)) {
            return self.inner.handle_request(request, context).await;
        }
        let call = call_key(&context.id);
        let Ok(_place) = self.calls.try_acquire() else {
            // Clears the mark of a cancel the session read before the refusal.
            self.receipts.returned(&call, false);
            return Ok(ServerResult::CallToolResult(refused_call()));
        };
        let cancelled = context.ct.clone();
        let result = CALL
            .scope(call.clone(), self.inner.handle_request(request, context))
            .await;
        // rmcp cancels the token only on a cancel before the answer is out,
        // and then drops the answer.
        let answered = !cancelled.is_cancelled()
            && matches!(&result, Ok(ServerResult::CallToolResult(r)) if r.is_error != Some(true));
        self.receipts.returned(&call, answered);
        result
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

/// A request id rmcp runs under, as a key: its JSON text, so `1` and `"1"`
/// stay apart.
fn call_key(id: &RequestId) -> String {
    id.clone().into_json_value().to_string()
}

/// The lines a resumed opening holds: the CLI's `initialize`, the answer it
/// was given, and `notifications/initialized` when it sent one.
struct Opening {
    request: Accounted,
    answer: Value,
    initialized: Option<Accounted>,
}

impl Opening {
    /// Picks the lines as the session's rmcp reads them. The answer is the
    /// server's line, so it is not read as a client message: it is the line
    /// with a `result`, the only answer a relay records.
    fn read(lines: &[String]) -> Option<Self> {
        let mut request = None;
        let mut answer = None;
        let mut initialized = None;
        for line in lines {
            let Some(value) = parse_line(line) else {
                continue;
            };
            if value.get("result").is_some() {
                answer = Some(value);
                continue;
            }
            match ClientJsonRpcMessage::deserialize(&value) {
                Ok(JsonRpcMessage::Request(sent))
                    if matches!(sent.request, ClientRequest::InitializeRequest(_)) =>
                {
                    request = Some(Accounted::of(&value));
                }
                Ok(JsonRpcMessage::Notification(sent))
                    if matches!(
                        sent.notification,
                        ClientNotification::InitializedNotification(_)
                    ) =>
                {
                    initialized = Some(Accounted::of(&value));
                }
                Ok(_) | Err(_) => {}
            }
        }
        // An opening with a line rmcp would not read cannot be replayed whole.
        Some(Self {
            request: request?.ok()?,
            answer: answer?,
            initialized: initialized.transpose().ok()?,
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
    receipts: &Arc<Receipts>,
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

    let mut carried = Carried::new(Arc::clone(receipts));
    let mut chosen = *rmcp.lifecycle.borrow();
    let mut held = HeldNotifications::default();
    loop {
        tokio::select! {
            frame = frames.next() => {
                let Ok(Some(frame)) = frame else {
                    return;
                };
                let (line, seq) = match frame {
                    RelayFrame::In { line, seq } => (line, seq),
                    RelayFrame::Done { seq } => {
                        receipts.done(seq);
                        continue;
                    }
                    RelayFrame::Dropped { seq } => {
                        receipts.dropped(seq);
                        continue;
                    }
                    _ => continue,
                };
                match carried.carry(&line, seq, chosen) {
                    Carry::Feed(lines) => {
                        if !rmcp.feed_all(&lines).await {
                            return;
                        }
                    }
                    Carry::Hold(lines) => lines.into_iter().for_each(|line| held.hold(line)),
                    Carry::Drop => {}
                    Carry::Answer(line) => {
                        let out = RelayFrame::Out { line, seq, receipt: None };
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
                let Some(Answered { line, seq, taken }) = carried.answered(line) else {
                    continue;
                };
                let (receipt, unreceipted) = match (seq, taken) {
                    // Carried from the moment the answer is handed on, so the
                    // relay's receipt always finds it.
                    (Some(seq), Some(taken)) => {
                        receipts.carry(seq, taken);
                        (Some(true), None)
                    }
                    (_, taken) => (None, taken),
                };
                let out = RelayFrame::Out { line, seq, receipt };
                if !send(writer, &out).await {
                    return;
                }
                // An answer to a request without a sequence number cannot be
                // acknowledged: it counts as received once written.
                if let Some(taken) = unreceipted {
                    taken.commit();
                }
            }
            changed = rmcp.lifecycle.changed(), if !chosen => {
                if changed.is_err() {
                    return;
                }
                if *rmcp.lifecycle.borrow() {
                    chosen = true;
                    if !rmcp.feed_all(&held.take()).await {
                        return;
                    }
                }
            }
        }
    }
}

/// The notifications a session holds until rmcp chose how it runs, at most
/// [`HELD_LINES_MAX`] of them. One past them is dropped, as a relay drops one
/// it cannot hold: a notification has no answer to give instead.
///
/// Each came in one frame, so they hold about `HELD_LINES_MAX` ×
/// [`yagura_protocol::MAX_FRAME_BODY_LEN`] bytes at most.
#[derive(Default)]
struct HeldNotifications {
    lines: Vec<Accounted>,
}

impl HeldNotifications {
    fn hold(&mut self, line: Accounted) {
        if self.lines.len() < HELD_LINES_MAX {
            self.lines.push(line);
        }
    }

    fn take(&mut self) -> Vec<Accounted> {
        std::mem::take(&mut self.lines)
    }
}

/// The requests rmcp read from the session and has not answered.
///
/// rmcp runs each request under the id it carries and answers it under that
/// id, so requests the CLI sent under one id would share one answer, one
/// cancel and one receipt there. Each request therefore reaches rmcp under an
/// id of the session's own, in the order the session read it, and its answer
/// goes back under the CLI's id. The server accepts no subscription, the one
/// other place rmcp names a request by its id.
struct Carried {
    /// The session's id for the next request rmcp reads.
    next: i64,
    /// The requests rmcp has not answered, by the session's id.
    requests: HashMap<i64, Request>,
    receipts: Arc<Receipts>,
}

/// One request rmcp runs under the session's id.
struct Request {
    /// Its id as the CLI sent it, read as rmcp reads one.
    id: RequestId,
    /// The relay's sequence number of it.
    seq: Option<u64>,
    /// Whether it is a tool call, which [`Receipts`] keeps track of.
    call: bool,
}

/// What the session does with a line from the CLI.
enum Carry {
    Feed(Vec<Accounted>),
    /// Notifications rmcp cannot take before it chose how the session runs.
    Hold(Vec<Accounted>),
    /// A line rmcp would not answer either.
    Drop,
    /// A request rmcp cannot read, answered by the session: the line it hands
    /// on in rmcp's place.
    Answer(String),
}

/// The answer to a request whose line, as the session writes it again for
/// rmcp, passes the length rmcp reads, worded as a relay words its own errors.
fn too_long_answer(id: &Value, call: bool) -> String {
    let sentence = "the request is longer than the daemon's MCP server reads";
    if call {
        tool_call_error_line(id, INVALID_REQUEST, sentence)
    } else {
        rpc_error_line(id, INVALID_REQUEST, sentence)
    }
}

/// An answer rmcp wrote, as the relay gets it.
struct Answered {
    line: String,
    /// The relay's sequence number of the request it answers.
    seq: Option<u64>,
    /// The reports it carries.
    taken: Option<TakenReports>,
}

impl Carried {
    fn new(receipts: Arc<Receipts>) -> Self {
        Self {
            next: 1,
            requests: HashMap::new(),
            receipts,
        }
    }

    fn carry(&mut self, line: &str, seq: Option<u64>, chosen: bool) -> Carry {
        let Some(mut value) = parse_line(line) else {
            tracing::debug!(
                kind = "not_json",
                "a relay session dropped a line that is no JSON-RPC message"
            );
            return Carry::Drop;
        };
        let (lines, notification) = match ClientJsonRpcMessage::deserialize(&value) {
            Ok(JsonRpcMessage::Request(request)) => {
                let ours = self.next;
                self.next += 1;
                value["id"] = Value::from(ours);
                let call = matches!(request.request, ClientRequest::CallToolRequest(_));
                let id = request.id;
                // Written again, a line the CLI sent within the limit may pass
                // it, and rmcp would leave the request unanswered.
                let Ok(line) = Accounted::of(&value) else {
                    return Carry::Answer(too_long_answer(&id.into_json_value(), call));
                };
                self.requests.insert(ours, Request { id, seq, call });
                (vec![line], false)
            }
            Ok(JsonRpcMessage::Notification(notification)) => match notification.notification {
                ClientNotification::CancelledNotification(cancelled) => {
                    match cancelled.params.request_id {
                        Some(cancelled) => (self.cancel(&value, &cancelled), true),
                        None => (Accounted::unanswered(&value).into_iter().collect(), true),
                    }
                }
                _ => (Accounted::unanswered(&value).into_iter().collect(), true),
            },
            // Neither is a request rmcp runs: it answers or drops them.
            Ok(JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_)) | Err(_) => {
                (Accounted::unanswered(&value).into_iter().collect(), false)
            }
        };
        match (lines.is_empty(), notification && !chosen) {
            (true, _) => Carry::Drop,
            (false, false) => Carry::Feed(lines),
            (false, true) => Carry::Hold(lines),
        }
    }

    /// The cancel `value` of the CLI's request `cancelled`, once for each
    /// request rmcp runs under that id; none when it runs none.
    fn cancel(&mut self, value: &Value, cancelled: &RequestId) -> Vec<Accounted> {
        let ours: Vec<i64> = self
            .requests
            .iter()
            .filter(|(_, request)| request.id == *cancelled)
            .map(|(ours, _)| *ours)
            .collect();
        let mut lines = Vec::with_capacity(ours.len());
        for ours in ours {
            let mut value = value.clone();
            value["params"]["requestId"] = Value::from(ours);
            // Past the line, it cancels nothing: the request keeps running.
            let Some(line) = Accounted::unanswered(&value) else {
                continue;
            };
            if let Some(request) = self.requests.remove(&ours)
                && request.call
            {
                self.receipts.cancel(&call_key(&RequestId::Number(ours)));
            }
            lines.push(line);
        }
        lines
    }

    /// `line` as the relay gets it: an answer goes back under the CLI's id,
    /// with the sequence number of the request it answers and the reports it
    /// carries. `None` for an answer to a request the CLI cancelled.
    fn answered(&mut self, line: String) -> Option<Answered> {
        let unchanged = |line| Answered {
            line,
            seq: None,
            taken: None,
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&line) else {
            return Some(unchanged(line));
        };
        let ours = match value.get("id").filter(|id| !id.is_null()) {
            Some(id) if value.get("method").is_none() => match RequestId::deserialize(id) {
                Ok(RequestId::Number(ours)) => Some(ours),
                Ok(RequestId::String(_)) | Err(_) => None,
            },
            // A request or notification of rmcp's own, or an answer to no id.
            _ => return Some(unchanged(line)),
        };
        let Some((ours, request)) =
            ours.and_then(|ours| self.requests.remove(&ours).map(|request| (ours, request)))
        else {
            tracing::debug!(
                kind = "cancelled",
                "a relay session dropped an answer to a request the CLI cancelled"
            );
            return None;
        };
        value["id"] = request.id.into_json_value();
        let taken = request
            .call
            .then(|| self.receipts.answered(&call_key(&RequestId::Number(ours))))
            .flatten();
        Some(Answered {
            line: value.to_string(),
            seq: request.seq,
            taken,
        })
    }
}

tokio::task_local! {
    /// The key ([`call_key`]) of the session's number the tool call a task
    /// runs is under.
    static CALL: String;
}

/// How many cancels of calls still running a session remembers at once: one
/// for each place a call holds, as a refused call lets go of its mark when it
/// is refused.
const CANCELS_REMEMBERED: usize = REQUESTS_AT_ONCE + REQUESTS_WAITING;

/// The reports a session's `wait_for_report` calls took, from the take until
/// the relay's receipt of the answer that carried them. Memory only: a daemon
/// that dies leaves their rows for the next one to hand out again.
///
/// Every taken value dropped here goes back to its inbox, so each method lets
/// go of the lock before dropping.
#[derive(Default)]
pub(crate) struct Receipts {
    state: Mutex<ReceiptState>,
}

#[derive(Default)]
struct ReceiptState {
    /// Taken by a call whose answer is not handed on yet, by the key of the
    /// session's number it runs under.
    answering: HashMap<String, TakenReports>,
    /// Calls the CLI cancelled while they still ran.
    cancelled: HashSet<String>,
    /// Calls that returned and whose answer is not read yet.
    returned: HashSet<String>,
    /// Carried by an answer that asked for a receipt, by its sequence number.
    carried: HashMap<u64, TakenReports>,
    closed: bool,
}

impl Receipts {
    fn state(&self) -> std::sync::MutexGuard<'_, ReceiptState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `call` took `taken`; it goes back at once when the call was cancelled.
    fn hold(&self, call: String, taken: TakenReports) {
        let released = {
            let mut state = self.state();
            if state.closed || state.cancelled.contains(&call) {
                Some(taken)
            } else {
                state.answering.insert(call, taken)
            }
        };
        drop(released);
    }

    /// `call`'s handler returned; without an answer that carries what it
    /// took, that goes back.
    fn returned(&self, call: &str, answered: bool) {
        let released = {
            let mut state = self.state();
            if state.cancelled.remove(call) {
                state.answering.remove(call)
            } else {
                state.returned.insert(call.to_owned());
                if answered {
                    None
                } else {
                    state.answering.remove(call)
                }
            }
        };
        drop(released);
    }

    /// The CLI cancelled `call`, still in flight: what it took goes back, and
    /// what it takes from now on does too.
    fn cancel(&self, call: &str) {
        let released = {
            let mut state = self.state();
            let still_running = !state.returned.remove(call);
            if still_running && state.cancelled.len() < CANCELS_REMEMBERED {
                state.cancelled.insert(call.to_owned());
            }
            state.answering.remove(call)
        };
        drop(released);
    }

    /// `call`'s answer is being handed on: what it carries, if anything.
    fn answered(&self, call: &str) -> Option<TakenReports> {
        let mut state = self.state();
        state.returned.remove(call);
        state.cancelled.remove(call);
        state.answering.remove(call)
    }

    /// The answer of `seq`, which asks for a receipt, carries `taken`.
    fn carry(&self, seq: u64, taken: TakenReports) {
        let released = {
            let mut state = self.state();
            if state.closed {
                Some(taken)
            } else {
                state.carried.insert(seq, taken)
            }
        };
        drop(released);
    }

    /// `relay_done`: the answer of `seq` was written to the CLI.
    fn done(&self, seq: u64) {
        let received = self.state().carried.remove(&seq);
        if let Some(taken) = received {
            taken.commit();
        }
    }

    /// `relay_dropped`: the answer of `seq` was not written.
    fn dropped(&self, seq: u64) {
        let released = self.state().carried.remove(&seq);
        drop(released);
    }

    /// The session ended: everything taken and not received goes back.
    fn close(&self) {
        let released = {
            let mut state = self.state();
            state.closed = true;
            let answering: Vec<TakenReports> = state.answering.drain().map(|(_, t)| t).collect();
            let carried: Vec<TakenReports> = state.carried.drain().map(|(_, t)| t).collect();
            (answering, carried)
        };
        drop(released);
    }
}
