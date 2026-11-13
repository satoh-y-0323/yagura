//! The relay between the CLI that started `yagura-mcp` as its MCP server and
//! the daemon, which serves MCP.
//!
//! Each JSON-RPC line of the CLI's stdin goes to the daemon in a `relay_in`
//! frame, and each `relay_out` line is written to stdout
//! ([`yagura_protocol::relay`]). Of a line the relay reads only its `method`,
//! its `id`, and the `params.requestId` of a `notifications/cancelled`; it
//! never reads an MCP result. It answers a request itself only when no daemon
//! carries it: the connection was lost, no daemon is reached, or the daemon
//! refused the session.
//!
//! A relay that names a window keeps trying to reach the daemon while it is
//! not connected, so the daemon shows the window as connected again after a
//! restart without the CLI calling anything. A relay outside a window reaches
//! the daemon only when the CLI sends a request.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    ReadHalf, WriteHalf,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use yagura_os::{IpcTransport, PlatformIpc};
use yagura_protocol::relay::{
    DAEMON_RESTARTED, DAEMON_UNAVAILABLE, HELD_LINES_MAX, INVALID_REQUEST, Line, RELAY_REFUSED,
    RESOURCE_EXHAUSTED, RelayFrame, RelayRefusedKind, RelayResume, SUPPORTED_CONTRACTS,
    is_one_line, read_line, rpc_error_line, tool_call_error_line,
};
use yagura_protocol::{
    FrameError, MAX_FRAME_BODY_LEN, WindowCredentials, frame_decode, frame_encode,
};

/// How long a window's relay that is not connected waits after one attempt to
/// reach the daemon ends before it starts the next.
///
/// It bounds how long the daemon shows the window as not connected after a
/// restart; no answer depends on it.
pub const RELAY_PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// The most requests a relay holds while it cannot carry them. A request past
/// it is answered `resource_exhausted`.
pub const HELD_REQUESTS_MAX: usize = HELD_LINES_MAX;

/// The contract this relay speaks: the newest one this build knows.
const CONTRACT: u32 = SUPPORTED_CONTRACTS[SUPPORTED_CONTRACTS.len() - 1];

/// The longest CLI line read in full. A longer one is dropped unread: no frame
/// could carry it, and its id cannot be known without reading all of it. Twice
/// the frame body limit, so a line just past what a frame carries is still
/// read and answered `invalid_request`.
const LINE_READ_MAX: usize = 2 * MAX_FRAME_BODY_LEN;

const READ_CHUNK_LEN: usize = 64 * 1024;

/// CLI lines read ahead of the relay.
const LINES_QUEUED: usize = 16;

const TOOLS_CALL: &str = "tools/call";
const INITIALIZE: &str = "initialize";
const INITIALIZED: &str = "notifications/initialized";
const CANCELLED: &str = "notifications/cancelled";
const PING: &str = "ping";

const RESTARTED_TEXT: &str = "the connection to the yagura daemon was lost after this call \
    was sent and before it was answered (the daemon stopped or restarted), so it may or may not \
    have taken effect. Check before calling again: for windows read list_windows and \
    get_window_status; for interrupt_window read get_window_status or wait_for_status; for chat \
    read read_chat with after_seq; for plans and a task window's report read list_plans, then \
    get_plan; for tickets read list_tickets and get_ticket. A call that only \
    waited (wait_for_status, wait_for_report) or was waiting for approval did nothing, so call \
    it again. A send_message or send_handoff may already have reached its receiver, and \
    withdraw_message cannot take it back past the restart: if you send it again, it may arrive twice, under a new id, so say in it that \
    it is a resend. The same holds for a report sent to your opener.";

const BACK_TEXT: &str = "the yagura daemon could not be reached when this call was made, so the \
    relay did not send it and it did nothing. The daemon is reachable again now: call it again.";

const UNREACHED_TEXT: &str = "the yagura daemon could not be reached, so this call was not sent \
    and did nothing. Call it again once yagura is running.";

const HELD_FULL_TEXT: &str = "the relay already holds as many calls as it may while the yagura \
    daemon cannot be reached, so this call was not sent and did nothing. Call it again once the \
    daemon is back.";

const TOO_LARGE_TEXT: &str =
    "the request is too large to carry to the yagura daemon, so it was not sent and did nothing.";

const NOT_ONE_LINE_TEXT: &str = "the request holds a carriage return, which cannot be carried to \
    the yagura daemon, so it was not sent and did nothing.";

type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// Reassembles length-prefixed frames from an arbitrarily chunked byte
/// stream.
///
/// It holds whatever bytes arrived after the last complete frame, so a caller
/// can tell one arriving frame from a second one tagging along. It performs no
/// I/O.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    /// Creates an empty reader.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `bytes` to the internal buffer and returns the bodies of
    /// every frame that is now complete, in arrival order.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::TooLarge`] as soon as a length prefix declares
    /// a body above `MAX_FRAME_BODY_LEN`; the oversized frame is never
    /// buffered to completion (fail-loud rather than silently draining).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut bodies = Vec::new();
        while let Some(body) = self.next_frame()? {
            bodies.push(body);
        }
        Ok(bodies)
    }

    /// Whether any bytes of an incomplete frame are still buffered.
    pub fn has_remaining(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Removes and returns the first complete frame body, if any. The body
    /// is copied out so that the borrow of the buffer ends before the
    /// consumed bytes are drained.
    fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let Some((body, consumed)) = frame_decode(&self.buffer)? else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.buffer.drain(..consumed);
        Ok(Some(body))
    }
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEnd {
    /// The CLI closed the relay's input.
    InputClosed,
    /// The relay's output could no longer be written.
    OutputClosed,
    /// The daemon refused to resume the CLI's session as it was opened, so the
    /// CLI has to start its MCP server again.
    SessionMismatch,
}

/// Relays between the CLI on `input` and `output` and the daemon at
/// `endpoint`, naming `window` when there is one, until one of the
/// [`RelayEnd`] events.
pub async fn run<R, W>(
    endpoint: String,
    window: Option<WindowCredentials>,
    input: R,
    mut output: W,
) -> RelayEnd
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let (lines_tx, mut lines) = mpsc::channel(LINES_QUEUED);
    let _reading = Tasks(vec![tokio::spawn(read_lines(
        BufReader::new(input),
        lines_tx,
    ))]);
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let mut relay = Relay::new(endpoint, window, events_tx);
    if relay.window.is_some() {
        relay.start_attempt();
    }
    loop {
        tokio::select! {
            input = lines.recv() => match input {
                Some(input) => relay.on_input(input),
                None => return RelayEnd::InputClosed,
            },
            Some(event) = events.recv() => relay.on_event(event),
            () = probe_due(relay.probe_at), if relay.probe_at.is_some() => relay.start_attempt(),
        }
        for line in std::mem::take(&mut relay.output) {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            if output.write_all(&bytes).await.is_err() {
                return RelayEnd::OutputClosed;
            }
        }
        if output.flush().await.is_err() {
            return RelayEnd::OutputClosed;
        }
        for frame in std::mem::take(&mut relay.after_output) {
            relay.send(&frame);
        }
        if let Some(end) = relay.ended {
            return end;
        }
    }
}

async fn probe_due(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

enum Input {
    Line(String),
    /// A line longer than [`LINE_READ_MAX`], dropped unread.
    Overlong,
}

async fn read_lines<R: AsyncBufRead + Unpin>(mut input: R, lines: mpsc::Sender<Input>) {
    let mut buffer = Vec::new();
    loop {
        let item = match read_capped_line(&mut input, &mut buffer).await {
            Ok(None) | Err(_) => return,
            Ok(Some(false)) => Input::Overlong,
            Ok(Some(true)) => {
                if buffer.last() == Some(&b'\r') {
                    buffer.pop();
                }
                if buffer.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let bytes = std::mem::take(&mut buffer);
                Input::Line(String::from_utf8(bytes).unwrap_or_else(|invalid| {
                    String::from_utf8_lossy(invalid.as_bytes()).into_owned()
                }))
            }
        };
        if lines.send(item).await.is_err() {
            return;
        }
    }
}

/// Reads one line into `buffer` without its newline. `Some(true)` when it fit
/// in [`LINE_READ_MAX`], `Some(false)` when it did not (and `buffer` is left
/// empty), `None` at the end of the input.
async fn read_capped_line<R: AsyncBufRead + Unpin>(
    input: &mut R,
    buffer: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    buffer.clear();
    let mut fits = true;
    let mut read_any = false;
    loop {
        let available = input.fill_buf().await?;
        if available.is_empty() {
            return Ok(read_any.then_some(fits));
        }
        read_any = true;
        let (chunk, used, ended) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], at + 1, true),
            None => (available, available.len(), false),
        };
        if fits {
            if buffer.len() + chunk.len() > LINE_READ_MAX {
                fits = false;
                buffer.clear();
            } else {
                buffer.extend_from_slice(chunk);
            }
        }
        input.consume(used);
        if ended {
            return Ok(Some(fits));
        }
    }
}

/// `frame` encoded for the wire.
fn encode(frame: &RelayFrame) -> Option<Vec<u8>> {
    serde_json::to_vec(frame)
        .ok()
        .map(|body| frame_encode(&body))
}

/// The `relay_in` frame carrying `line`, or why it cannot carry it: the line
/// is not one line ([`is_one_line`]), or the body would exceed the frame body
/// limit.
fn in_frame(line: String, seq: Option<u64>) -> Result<Arc<Vec<u8>>, Uncarried> {
    if !is_one_line(&line) {
        return Err(Uncarried::NotOneLine);
    }
    let body =
        serde_json::to_vec(&RelayFrame::In { line, seq }).map_err(|_| Uncarried::TooLarge)?;
    if body.len() > MAX_FRAME_BODY_LEN {
        return Err(Uncarried::TooLarge);
    }
    Ok(Arc::new(frame_encode(&body)))
}

/// Why a CLI line cannot go in a `relay_in` frame.
enum Uncarried {
    NotOneLine,
    TooLarge,
}

impl Uncarried {
    fn text(&self) -> &'static str {
        match self {
            Self::NotOneLine => NOT_ONE_LINE_TEXT,
            Self::TooLarge => TOO_LARGE_TEXT,
        }
    }
}

fn refusal_text(kind: RelayRefusedKind, reason: &str) -> String {
    let kind = match kind {
        RelayRefusedKind::Contract => "contract",
        RelayRefusedKind::Limit => "limit",
        RelayRefusedKind::SessionMismatch => "session_mismatch",
    };
    format!(
        "the yagura daemon refused this relay's session ({kind}: {reason}), so this call was not \
         sent and did nothing."
    )
}

/// A request from the CLI, with the frame that carries it.
struct Request {
    id: Value,
    method: String,
    seq: u64,
    frame: Arc<Vec<u8>>,
    /// Held while the daemon was known to be unreachable.
    absent: bool,
}

enum Held {
    Request(Request),
    Notification(Arc<Vec<u8>>),
}

/// The CLI's `initialize`, its answer and its `notifications/initialized`, as
/// written, which a later connection resumes.
#[derive(Default)]
struct Opening {
    request: Option<(Value, String)>,
    answer: Option<String>,
    initialized: Option<String>,
}

impl Opening {
    fn lines(&self) -> Option<Vec<String>> {
        let (_, request) = self.request.as_ref()?;
        let answer = self.answer.as_ref()?;
        let mut lines = vec![request.clone(), answer.clone()];
        lines.extend(self.initialized.clone());
        Some(lines)
    }

    fn answers(&self, id: &Value) -> bool {
        self.answer.is_none()
            && self
                .request
                .as_ref()
                .is_some_and(|(request_id, _)| request_id == id)
    }
}

/// Aborts its tasks when dropped.
struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

enum Link {
    /// Welcomed: carrying lines.
    Up {
        id: u64,
        out: mpsc::UnboundedSender<Arc<Vec<u8>>>,
        _tasks: Tasks,
    },
    /// Refused for its contract, on a connection the daemon keeps open; its
    /// end is how the relay learns the daemon was replaced.
    Refused {
        id: u64,
        text: String,
        _write: WriteHalf<Connection>,
        _tasks: Tasks,
    },
    /// Not connected. `after_limit` while the last daemon reached refused for
    /// its limit: no attempt is made but for a request of the CLI.
    Down { after_limit: bool },
}

enum Attempted {
    Welcomed {
        connection: Connection,
        reader: FrameReader,
        rest: Vec<Vec<u8>>,
        tools: String,
    },
    Refused {
        kind: RelayRefusedKind,
        reason: String,
        connection: Connection,
        reader: FrameReader,
        rest: Vec<Vec<u8>>,
    },
    Unreached,
}

enum Event {
    Attempt(Box<Attempted>),
    Frame { link: u64, body: Vec<u8> },
    Lost { link: u64 },
}

/// One attempt to open a session: connect, send `hello`, and read up to the
/// daemon's answer to it.
async fn attempt(endpoint: &str, hello: Option<Vec<u8>>) -> Attempted {
    let Some(hello) = hello else {
        return Attempted::Unreached;
    };
    let Ok(mut connection) = PlatformIpc::default().connect(endpoint).await else {
        return Attempted::Unreached;
    };
    if connection.write_all(&hello).await.is_err() {
        return Attempted::Unreached;
    }
    let mut reader = FrameReader::new();
    let mut chunk = vec![0u8; READ_CHUNK_LEN];
    loop {
        let read = match connection.read(&mut chunk).await {
            Ok(0) | Err(_) => return Attempted::Unreached,
            Ok(read) => read,
        };
        let Ok(bodies) = reader.push(&chunk[..read]) else {
            return Attempted::Unreached;
        };
        let mut bodies = bodies.into_iter();
        while let Some(body) = bodies.next() {
            match serde_json::from_slice::<RelayFrame>(&body) {
                Ok(RelayFrame::Welcome { tools, .. }) => {
                    return Attempted::Welcomed {
                        connection,
                        reader,
                        rest: bodies.collect(),
                        tools,
                    };
                }
                Ok(RelayFrame::Refused { kind, reason, .. }) => {
                    return Attempted::Refused {
                        kind,
                        reason,
                        connection,
                        reader,
                        rest: bodies.collect(),
                    };
                }
                // A frame of a type this relay does not know is skipped.
                _ => {}
            }
        }
    }
}

fn spawn_reader(
    link: u64,
    mut read: ReadHalf<Connection>,
    mut reader: FrameReader,
    rest: Vec<Vec<u8>>,
    events: mpsc::UnboundedSender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        for body in rest {
            if events.send(Event::Frame { link, body }).is_err() {
                return;
            }
        }
        let mut chunk = vec![0u8; READ_CHUNK_LEN];
        loop {
            let read = match read.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let Ok(bodies) = reader.push(&chunk[..read]) else {
                break;
            };
            for body in bodies {
                if events.send(Event::Frame { link, body }).is_err() {
                    return;
                }
            }
        }
        let _ = events.send(Event::Lost { link });
    })
}

fn spawn_writer(
    link: u64,
    mut write: WriteHalf<Connection>,
    mut frames: mpsc::UnboundedReceiver<Arc<Vec<u8>>>,
    events: mpsc::UnboundedSender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if write.write_all(&frame).await.is_err() {
                let _ = events.send(Event::Lost { link });
                return;
            }
        }
    })
}

struct Relay {
    endpoint: String,
    window: Option<WindowCredentials>,
    events: mpsc::UnboundedSender<Event>,
    link: Link,
    next_link: u64,
    attempting: bool,
    /// The first sequence number given after the running attempt started: an
    /// attempt answers only for the requests held before it.
    attempt_from: u64,
    probe_at: Option<Instant>,
    /// A window's relay's last attempt found no daemon, and none was reached
    /// since.
    absent: bool,
    /// The last `relay_welcome`'s tools; set once a session opened.
    tools: Option<String>,
    opening: Opening,
    next_seq: u64,
    /// Requests carried and not answered, by sequence number.
    sent: BTreeMap<u64, Request>,
    /// Lines waiting for a session, in the order the CLI wrote them.
    held: VecDeque<Held>,
    /// Lines to write to the CLI.
    output: Vec<String>,
    /// Frames to send once `output` is written.
    after_output: Vec<RelayFrame>,
    ended: Option<RelayEnd>,
}

impl Relay {
    fn new(
        endpoint: String,
        window: Option<WindowCredentials>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            endpoint,
            window,
            events,
            link: Link::Down { after_limit: false },
            next_link: 0,
            attempting: false,
            attempt_from: 0,
            probe_at: None,
            absent: false,
            tools: None,
            opening: Opening::default(),
            next_seq: 0,
            sent: BTreeMap::new(),
            held: VecDeque::new(),
            output: Vec::new(),
            after_output: Vec::new(),
            ended: None,
        }
    }

    fn link_id(&self) -> Option<u64> {
        match &self.link {
            Link::Up { id, .. } | Link::Refused { id, .. } => Some(*id),
            Link::Down { .. } => None,
        }
    }

    fn send(&self, frame: &RelayFrame) {
        if let Some(bytes) = encode(frame) {
            self.carry(&Arc::new(bytes));
        }
    }

    fn carry(&self, frame: &Arc<Vec<u8>>) {
        if let Link::Up { out, .. } = &self.link {
            // A writer that is gone reports the loss itself.
            let _ = out.send(Arc::clone(frame));
        }
    }

    fn answer_error(&mut self, id: &Value, method: &str, word: &str, text: &str) {
        let line = if method == TOOLS_CALL {
            tool_call_error_line(id, word, text)
        } else {
            rpc_error_line(id, word, text)
        };
        self.output.push(line);
    }

    fn held_count(&self, requests: bool) -> usize {
        self.held
            .iter()
            .filter(|held| matches!(held, Held::Request(_)) == requests)
            .count()
    }

    fn hello(&self) -> Option<Vec<u8>> {
        let resume = self.tools.clone().map(|tools| RelayResume {
            opening: self.opening.lines(),
            tools,
        });
        encode(&RelayFrame::Hello {
            contract: CONTRACT,
            window_id: self.window.as_ref().map(|window| window.window_id.clone()),
            token: self.window.as_ref().map(|window| window.token.clone()),
            resume,
        })
    }

    fn start_attempt(&mut self) {
        if self.attempting {
            return;
        }
        self.attempting = true;
        self.attempt_from = self.next_seq;
        self.probe_at = None;
        let hello = self.hello();
        let endpoint = self.endpoint.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let outcome = attempt(&endpoint, hello).await;
            let _ = events.send(Event::Attempt(Box::new(outcome)));
        });
    }

    /// While a window's relay is not connected, the next attempt is due one
    /// interval after the last one ended.
    fn schedule_probe(&mut self) {
        let probing = self.window.is_some()
            && !self.attempting
            && matches!(self.link, Link::Down { after_limit: false });
        self.probe_at = probing.then(|| Instant::now() + RELAY_PROBE_INTERVAL);
    }

    fn on_input(&mut self, input: Input) {
        let Input::Line(line) = input else {
            eprintln!("yagura-mcp: dropped an input line longer than {LINE_READ_MAX} bytes");
            return;
        };
        match read_line(&line) {
            Line::Request { id, method } => self.on_request(line, id, method),
            Line::Notification { method, cancels } => self.on_notification(line, &method, cancels),
            Line::Answer { .. } | Line::Other => {
                if let Ok(frame) = in_frame(line, None) {
                    self.carry(&frame);
                }
            }
        }
    }

    fn on_request(&mut self, line: String, id: Value, method: String) {
        let up = matches!(self.link, Link::Up { .. });
        if method == PING && !up {
            // Left unanswered, a CLI starts its MCP server again.
            self.output
                .push(json!({"jsonrpc": "2.0", "id": id, "result": {}}).to_string());
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let opens = method == INITIALIZE && self.opening.answer.is_none();
        let opening_line = opens.then(|| line.clone());
        let frame = match in_frame(line, Some(seq)) {
            Ok(frame) => frame,
            Err(uncarried) => {
                self.answer_error(&id, &method, INVALID_REQUEST, uncarried.text());
                return;
            }
        };
        if let Some(opening_line) = opening_line {
            self.opening.request = Some((id.clone(), opening_line));
        }
        let request = Request {
            id,
            method,
            seq,
            frame,
            // One that comes while an attempt runs is settled by that attempt:
            // its line and the attempt's answer arrive on separate streams.
            absent: self.absent && !self.attempting,
        };
        match &self.link {
            Link::Up { .. } => {
                self.carry(&request.frame);
                self.sent.insert(seq, request);
            }
            Link::Refused { text, .. } => {
                let text = text.clone();
                self.answer_error(&request.id, &request.method, RELAY_REFUSED, &text);
            }
            Link::Down { .. } => {
                if self.held_count(true) >= HELD_REQUESTS_MAX {
                    self.answer_error(
                        &request.id,
                        &request.method,
                        RESOURCE_EXHAUSTED,
                        HELD_FULL_TEXT,
                    );
                    return;
                }
                self.held.push_back(Held::Request(request));
                // A window's relay that knows no daemon is reached leaves the
                // next attempt to its probe, so the requests that come
                // meanwhile count as made while the daemon was absent.
                if self.window.is_none() || matches!(self.link, Link::Down { after_limit: true }) {
                    self.start_attempt();
                }
            }
        }
    }

    fn on_notification(&mut self, line: String, method: &str, cancels: Option<Value>) {
        if method == CANCELLED
            && let Some(cancelled) = &cancels
        {
            let held = self.held.iter().position(
                |held| matches!(held, Held::Request(request) if request.id == *cancelled),
            );
            if let Some(at) = held {
                // A request never sent needs no cancel.
                self.held.remove(at);
                return;
            }
            self.sent.retain(|_, request| request.id != *cancelled);
        }
        let up = matches!(self.link, Link::Up { .. });
        let initialized = (method == INITIALIZED
            && self.opening.answer.is_some()
            && self.opening.initialized.is_none())
        .then(|| line.clone());
        let Ok(frame) = in_frame(line, None) else {
            return;
        };
        if let Some(initialized) = initialized {
            self.opening.initialized = Some(initialized);
            if !up {
                // The next session's resumed opening carries it.
                return;
            }
        }
        match self.link {
            Link::Up { .. } => self.carry(&frame),
            Link::Refused { .. } => {}
            Link::Down { .. } => {
                if self.held_count(false) < HELD_REQUESTS_MAX {
                    self.held.push_back(Held::Notification(frame));
                }
            }
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Attempt(outcome) => self.on_attempt(*outcome),
            Event::Frame { link, body } => {
                if matches!(self.link, Link::Up { id, .. } if id == link)
                    && let Ok(RelayFrame::Out { line, seq, receipt }) =
                        serde_json::from_slice::<RelayFrame>(&body)
                {
                    self.on_out(line, seq, receipt == Some(true));
                }
            }
            Event::Lost { link } => {
                if self.link_id() == Some(link) {
                    self.on_lost();
                }
            }
        }
    }

    fn on_out(&mut self, line: String, seq: Option<u64>, receipt: bool) {
        let Some(seq) = seq else {
            self.output.push(line);
            return;
        };
        let Some(request) = self.sent.remove(&seq) else {
            // The CLI cancelled the request: its answer is not written.
            if receipt {
                self.send(&RelayFrame::Dropped { seq });
            }
            return;
        };
        if request.method == INITIALIZE && self.opening.answers(&request.id) && is_result(&line) {
            self.opening.answer = Some(line.clone());
        }
        self.output.push(line);
        if receipt {
            self.after_output.push(RelayFrame::Done { seq });
        }
    }

    fn on_lost(&mut self) {
        self.link = Link::Down { after_limit: false };
        self.absent = false;
        let window = self.window.is_some();
        let mut again = VecDeque::new();
        for (_, request) in std::mem::take(&mut self.sent) {
            if request.method == TOOLS_CALL || !window {
                self.answer_error(
                    &request.id,
                    &request.method,
                    DAEMON_RESTARTED,
                    RESTARTED_TEXT,
                );
            } else {
                // Without effect, so sent again once a session opens.
                again.push_back(Held::Request(request));
            }
        }
        again.extend(self.held.drain(..));
        self.held = again;
        if window {
            self.start_attempt();
        }
    }

    fn on_attempt(&mut self, outcome: Attempted) {
        self.attempting = false;
        match outcome {
            Attempted::Welcomed {
                connection,
                reader,
                rest,
                tools,
            } => {
                self.absent = false;
                self.tools = Some(tools);
                let id = self.next_link;
                self.next_link += 1;
                let (read, write) = tokio::io::split(connection);
                let (out, frames) = mpsc::unbounded_channel();
                let tasks = Tasks(vec![
                    spawn_reader(id, read, reader, rest, self.events.clone()),
                    spawn_writer(id, write, frames, self.events.clone()),
                ]);
                self.link = Link::Up {
                    id,
                    out,
                    _tasks: tasks,
                };
                self.send_held();
            }
            Attempted::Refused {
                kind: RelayRefusedKind::SessionMismatch,
                ..
            } => {
                eprintln!(
                    "yagura-mcp: the daemon did not resume the session the CLI opened; ending so \
                     that the CLI starts its MCP server again"
                );
                self.ended = Some(RelayEnd::SessionMismatch);
            }
            Attempted::Refused {
                kind: kind @ RelayRefusedKind::Contract,
                reason,
                connection,
                reader,
                rest,
            } => {
                self.absent = false;
                let text = refusal_text(kind, &reason);
                self.answer_held(RELAY_REFUSED, &text, u64::MAX);
                self.held.clear();
                let id = self.next_link;
                self.next_link += 1;
                let (read, write) = tokio::io::split(connection);
                let tasks = Tasks(vec![spawn_reader(
                    id,
                    read,
                    reader,
                    rest,
                    self.events.clone(),
                )]);
                self.link = Link::Refused {
                    id,
                    text,
                    _write: write,
                    _tasks: tasks,
                };
            }
            Attempted::Refused {
                kind: kind @ RelayRefusedKind::Limit,
                reason,
                ..
            } => {
                self.absent = false;
                self.link = Link::Down { after_limit: true };
                // A request that came while this attempt ran is the next
                // request after the refusal, and names the relay once more.
                let before = self.attempt_from;
                if self.answer_held(RELAY_REFUSED, &refusal_text(kind, &reason), before) {
                    self.start_attempt();
                }
            }
            Attempted::Unreached => {
                // An attempt that reached no daemon also ends the limit's
                // refusal: the daemon that gave it is gone.
                self.link = Link::Down { after_limit: false };
                if self.window.is_some() {
                    self.absent = true;
                    for held in &mut self.held {
                        if let Held::Request(request) = held {
                            request.absent = true;
                        }
                    }
                } else if self.answer_held(DAEMON_UNAVAILABLE, UNREACHED_TEXT, self.attempt_from) {
                    self.start_attempt();
                }
            }
        }
        self.schedule_probe();
    }

    /// Sends what was held once a session opened. A `tools/call` held while
    /// no daemon was reached is answered instead: the CLI may have given up on
    /// it without the relay learning so, and it must not run late.
    fn send_held(&mut self) {
        for held in std::mem::take(&mut self.held) {
            match held {
                Held::Request(request) if request.method == TOOLS_CALL && request.absent => {
                    self.answer_error(&request.id, TOOLS_CALL, DAEMON_UNAVAILABLE, BACK_TEXT);
                }
                Held::Request(request) => {
                    self.carry(&request.frame);
                    self.sent.insert(request.seq, request);
                }
                Held::Notification(frame) => self.carry(&frame),
            }
        }
    }

    /// Answers each held request whose sequence number is below `before`
    /// with `word`, and tells whether any request is still held.
    fn answer_held(&mut self, word: &str, text: &str, before: u64) -> bool {
        let mut kept = VecDeque::new();
        for held in std::mem::take(&mut self.held) {
            match held {
                Held::Request(request) if request.seq < before => {
                    self.answer_error(&request.id, &request.method, word, text);
                }
                other => kept.push_back(other),
            }
        }
        self.held = kept;
        self.held_count(true) > 0
    }
}

fn is_result(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|answer| answer.get("result").is_some())
}
