//! Test doubles for [`GuiHost`] and for the daemon on the other side of it.
//!
//! The host records what the GUI asks of the application; the daemon is a real
//! endpoint, so a connection under test is a real connection that a script
//! answers frame by frame.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, DaemonMessage, PROTOCOL_REVISION, Palette, StatusBoot, TerminalSize, WindowInfo,
    WindowStatus, decode_correlated_client_message, encode_correlated_daemon_message,
    encode_daemon_message, frame_encode,
};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

use super::GuiHost;
use crate::connection_pool::ConnectionPool;
use crate::events::{EVENT_DAEMON_CONNECTED, EVENT_WINDOW_LINK};
use crate::frame_mailbox::FrameMailbox;
use crate::ipc_client::{ERROR_CODE_SUPERSEDED, IpcError};

const ENDPOINT_PREFIX: &str = "yagura-test-gui-host";

/// The wire key a request's answer carries its correlation id under.
const CORRELATION_ID: &str = "correlation_id";

/// One request as the daemon read it: the correlation id it carried, if any.
type Request = (Option<u64>, ClientMessage);

/// One frame a scripted daemon sends back for a request.
#[derive(Debug, Clone)]
pub(crate) enum SentFrame {
    /// The answer, carrying the request's correlation id as a daemon does.
    Answer(DaemonMessage),
    /// The message with no correlation id at all.
    Unkeyed(DaemonMessage),
    /// The message under a correlation id that is not the request's.
    OtherKey(DaemonMessage),
    /// A JSON object with the `type` tag and nothing else, which reads as no
    /// message of that type; it carries the request's correlation id when
    /// `keyed`, and no correlation id otherwise.
    Unreadable { tag: &'static str, keyed: bool },
    /// These bytes as one frame body, as they are.
    Raw(Vec<u8>),
}

impl SentFrame {
    fn body(&self, key: Option<u64>) -> Vec<u8> {
        match self {
            Self::Answer(message) => encode_correlated_daemon_message(message, key),
            Self::Unkeyed(message) => encode_daemon_message(message),
            Self::OtherKey(message) => {
                let other = key.map_or(u64::MAX, |key| key.wrapping_add(1));
                encode_correlated_daemon_message(message, Some(other))
            }
            Self::Unreadable { tag, keyed } => {
                let mut body = json!({ "type": tag });
                if let Some(key) = key.filter(|_| *keyed) {
                    body[CORRELATION_ID] = json!(key);
                }
                serde_json::to_vec(&body).expect("a JSON object serializes")
            }
            Self::Raw(bytes) => bytes.clone(),
        }
    }
}

/// What a scripted daemon does with one request: the frames to send back.
pub(crate) struct Reaction {
    send: Vec<SentFrame>,
    /// Told when the request has arrived, then waited on before answering.
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
    /// Whether a held answer is written from a task of its own while the
    /// connection goes on reading and answering.
    aside: bool,
    /// Whether every frame goes out in one write, as one run of bytes.
    together: bool,
}

impl Reaction {
    /// Answers with `message`.
    pub(crate) fn reply(message: DaemonMessage) -> Option<Self> {
        Self::replies(vec![message])
    }

    /// Answers with `messages`, in order.
    pub(crate) fn replies(messages: Vec<DaemonMessage>) -> Option<Self> {
        Self::sends(messages.into_iter().map(SentFrame::Answer).collect())
    }

    /// Sends `frames`, in order.
    pub(crate) fn sends(frames: Vec<SentFrame>) -> Option<Self> {
        Some(Self {
            send: frames,
            hold: None,
            aside: false,
            together: false,
        })
    }

    /// This reaction, its frames written in one write rather than one each.
    pub(crate) fn in_one_write(self) -> Self {
        Self {
            together: true,
            ..self
        }
    }

    /// Answers with `messages` only once the test opens `gate`, after telling
    /// `reached` that the request has arrived. Both use `notify_one`, which
    /// keeps a permit for a waiter that comes later.
    pub(crate) fn replies_after(
        reached: Arc<Notify>,
        gate: Arc<Notify>,
        messages: Vec<DaemonMessage>,
    ) -> Option<Self> {
        Self::replies(messages).map(|reaction| reaction.held(reached, gate))
    }

    /// This reaction, sent only once the test opens `gate`, after telling
    /// `reached` that the request has arrived.
    pub(crate) fn held(self, reached: Arc<Notify>, gate: Arc<Notify>) -> Self {
        Self {
            hold: Some((reached, gate)),
            ..self
        }
    }

    /// Answers with `messages` under the request's key once the test opens
    /// `gate`, after telling `reached` that the request has arrived, while
    /// the connection goes on reading and answering other requests: the way
    /// a daemon holds a request for an approver. The request counts as
    /// arrived once it is set aside, before its answer is written.
    pub(crate) fn replies_aside(
        reached: Arc<Notify>,
        gate: Arc<Notify>,
        messages: Vec<DaemonMessage>,
    ) -> Option<Self> {
        Self::replies_after(reached, gate, messages).map(|reaction| Self {
            aside: true,
            ..reaction
        })
    }
}

/// An error frame with `code` and no reason.
pub(crate) fn error_frame(code: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_owned(),
        message: String::new(),
        rejected_argument: None,
    }
}

/// This build's revision answer from a daemon running in `boot`.
pub(crate) fn protocol_revision(boot: Option<StatusBoot>) -> DaemonMessage {
    protocol_revision_at(PROTOCOL_REVISION, boot)
}

/// The revision answer of a daemon that speaks `revision`, running in `boot`.
pub(crate) fn protocol_revision_at(revision: u32, boot: Option<StatusBoot>) -> DaemonMessage {
    DaemonMessage::ProtocolRevision { revision, boot }
}

/// Whether `request` is the first thing a daemon hears on a new connection.
fn begins_connection(request: &ClientMessage) -> bool {
    matches!(request, ClientMessage::GetProtocolRevision)
}

/// The requests of the connection that began with the `nth` revision probe
/// (counted from 0), up to the next connection's probe.
pub(crate) fn connection(received: &[ClientMessage], nth: usize) -> &[ClientMessage] {
    let starts: Vec<usize> = received
        .iter()
        .enumerate()
        .filter(|(_, request)| begins_connection(request))
        .map(|(index, _)| index)
        .collect();
    let start = *starts.get(nth).expect("that connection was made");
    let end = starts.get(nth + 1).copied().unwrap_or(received.len());
    &received[start..end]
}

/// A script part that answers the `n`th revision probe, counted across
/// connections from 0, from a daemon running in `boots[n]`, and every probe
/// after the last one in the last boot.
pub(crate) fn probes_answered_in(
    boots: Vec<Option<StatusBoot>>,
) -> impl Fn(&ClientMessage) -> Option<Reaction> + Send + Sync + 'static {
    let probes = AtomicUsize::new(0);
    move |request| match request {
        ClientMessage::GetProtocolRevision => {
            let nth = probes.fetch_add(1, Ordering::SeqCst);
            let boot = boots.get(nth).or(boots.last()).copied().flatten();
            Reaction::reply(protocol_revision(boot))
        }
        _ => None,
    }
}

/// A script part that answers the `n`th revision probe, counted across
/// connections from 0, as a daemon speaking `revisions[n]` does, and every
/// probe after the last one at the last revision.
pub(crate) fn probes_answered_at(
    revisions: Vec<u32>,
) -> impl Fn(&ClientMessage) -> Option<Reaction> + Send + Sync + 'static {
    let probes = AtomicUsize::new(0);
    move |request| match request {
        ClientMessage::GetProtocolRevision => {
            let nth = probes.fetch_add(1, Ordering::SeqCst);
            let revision = revisions
                .get(nth)
                .or(revisions.last())
                .copied()
                .expect("a revision to answer with");
            Reaction::reply(protocol_revision_at(revision, None))
        }
        _ => None,
    }
}

/// The daemon-connected payload the WebView reads: all keys always present.
pub(crate) fn connected(boot: Option<StatusBoot>, spawn_warning: Option<&str>) -> Value {
    json!({ "status_boot": boot, "spawn_warning": spawn_warning, "compatible": true })
}

/// The payloads of the daemon-connected events among `events`, oldest first.
pub(crate) fn announced(events: &[RecordedEvent]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event.name == EVENT_DAEMON_CONNECTED)
        .map(|event| event.payload.clone())
        .collect()
}

/// A window as the daemon lists it, with nothing but its id and status set.
pub(crate) fn window_info(window_id: &str, status: WindowStatus) -> WindowInfo {
    WindowInfo {
        window_id: window_id.to_owned(),
        name: window_id.to_owned(),
        vendor: None,
        cwd: "/work".to_owned(),
        status,
        exit_code: None,
        layout_json: None,
        room: None,
        unknown_reason: None,
        detection_warning: None,
        status_rev: None,
        status_boot: None,
        mcp_connected: None,
        status_reports_missing: None,
        queued_count: None,
        task_report: None,
    }
}

/// A palette that holds the 16 ANSI colors a window palette needs.
pub(crate) fn sample_palette() -> Palette {
    let color: yagura_protocol::Rgb = [1, 2, 3];
    Palette {
        foreground: color,
        background: color,
        cursor: color,
        ansi: vec![color; 16],
    }
}

/// Attaches `window_id` through `pool` the way a pane does: the window is
/// listed first, a hidden pane says so, and then it attaches at `size`.
pub(crate) async fn attach_pane(
    pool: &ConnectionPool,
    host: &FakeHost,
    window_id: &str,
    size: Option<TerminalSize>,
    visible: bool,
) -> Result<(), IpcError> {
    pool.track_window_list(&[window_info(window_id, WindowStatus::Running)])
        .await;
    if !visible {
        pool.set_window_visible(window_id, false).await?;
    }
    pool.attach_window(host, window_id, size).await
}

/// The daemon's answer to an attach, with no exit code.
pub(crate) fn attached(child_exited: bool) -> DaemonMessage {
    DaemonMessage::Attached {
        child_exited,
        exit_code: None,
    }
}

/// A text input of `text`.
pub(crate) fn text(text: &str) -> ClientMessage {
    ClientMessage::Text {
        text: text.to_owned(),
    }
}

/// A request as a failure message shows it: a paste by its size and first
/// character rather than in full.
pub(crate) fn brief(request: &ClientMessage) -> String {
    match request {
        ClientMessage::Paste { text } => {
            format!("Paste of {} bytes of {:?}", text.len(), text.chars().next())
        }
        other => format!("{other:?}"),
    }
}

type Script = dyn Fn(&ClientMessage) -> Option<Reaction> + Send + Sync;

struct DaemonState {
    script: Box<Script>,
    received: Mutex<Vec<Request>>,
    /// The number of requests read so far, answered or not.
    recorded: watch::Sender<usize>,
    /// The number of requests received so far, for waiting on the next one.
    arrived: watch::Sender<usize>,
    /// Bumped to end every connection currently being served.
    hang_up: watch::Sender<u64>,
    /// Frame bodies written, unasked, on every connection being served.
    pushes: broadcast::Sender<Vec<u8>>,
}

impl DaemonState {
    /// Records `request` and returns how many have been read with it.
    fn record(&self, request: &Request) -> usize {
        let count = {
            let mut received = self.received.lock().expect("received log");
            received.push(request.clone());
            received.len()
        };
        self.recorded.send_replace(count);
        count
    }
}

/// A daemon that answers each request with what its script says. A request
/// the script has no answer for gets the answer of the two connection
/// housekeeping requests (approver registration, revision probe), or none.
#[derive(Clone)]
pub(crate) struct ScriptedDaemon {
    state: Arc<DaemonState>,
}

impl ScriptedDaemon {
    pub(crate) fn new(
        script: impl Fn(&ClientMessage) -> Option<Reaction> + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::new(DaemonState {
                script: Box::new(script),
                received: Mutex::new(Vec::new()),
                recorded: watch::channel(0).0,
                arrived: watch::channel(0).0,
                hang_up: watch::channel(0).0,
                pushes: broadcast::channel(16).0,
            }),
        }
    }

    /// A daemon that answers only the connection housekeeping.
    pub(crate) fn quiet() -> Self {
        Self::new(|_| None)
    }

    /// Binds `endpoint` and serves every connection made to it.
    pub(crate) async fn listen(&self, endpoint: &str) {
        let ipc = PlatformIpc::default();
        let security = ipc.endpoint_security().expect("endpoint security");
        let mut listener = ipc.bind(endpoint, security).await.expect("bind");
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            while let Ok(connection) = ipc.accept(&mut listener).await {
                let hang_up = state.hang_up.subscribe();
                let pushes = state.pushes.subscribe();
                tokio::spawn(serve(connection, Arc::clone(&state), hang_up, pushes));
            }
        });
    }

    /// Serves `stream` as one connection, whatever carries it.
    pub(crate) fn serve_stream(&self, stream: impl AsyncRead + AsyncWrite + Send + 'static) {
        let state = Arc::clone(&self.state);
        let hang_up = state.hang_up.subscribe();
        let pushes = state.pushes.subscribe();
        tokio::spawn(serve(stream, state, hang_up, pushes));
    }

    /// Every request received so far, in arrival order across connections.
    pub(crate) fn received(&self) -> Vec<ClientMessage> {
        self.received_correlated()
            .into_iter()
            .map(|(_, request)| request)
            .collect()
    }

    /// How many revision probes, and so how many connections, were heard.
    pub(crate) fn probes(&self) -> usize {
        self.received()
            .iter()
            .filter(|request| begins_connection(request))
            .count()
    }

    /// Every request received so far with the correlation id it carried, in
    /// arrival order across connections.
    pub(crate) fn received_correlated(&self) -> Vec<(Option<u64>, ClientMessage)> {
        self.state.received.lock().expect("received log").clone()
    }

    /// Waits until at least `count` requests have arrived and returns them all.
    pub(crate) async fn received_at_least(&self, count: usize) -> Vec<ClientMessage> {
        let mut arrived = self.state.arrived.subscribe();
        reply_within("daemon requests", arrived.wait_for(|n| *n >= count))
            .await
            .expect("the daemon state outlives the wait");
        self.received()
    }

    /// Waits until at least `count` requests have been read, whether or not
    /// they have been answered yet, and returns them all.
    pub(crate) async fn recorded_at_least(&self, count: usize) -> Vec<ClientMessage> {
        let mut recorded = self.state.recorded.subscribe();
        reply_within("daemon requests read", recorded.wait_for(|n| *n >= count))
            .await
            .expect("the daemon state outlives the wait");
        self.received()
    }

    /// Ends every connection being served; later connections are served as
    /// usual.
    pub(crate) fn hang_up(&self) {
        self.state
            .hang_up
            .send_modify(|generation| *generation += 1);
    }

    /// Tells every connection being served that another GUI took the daemon
    /// over, with the error frame a daemon sends unasked: no correlation id.
    pub(crate) fn supersede(&self) {
        let body = encode_daemon_message(&error_frame(ERROR_CODE_SUPERSEDED));
        // No connection being served is no one to tell.
        let _ = self.state.pushes.send(body);
    }
}

type Reading<R> = Pin<Box<dyn Future<Output = (R, Option<Request>)> + Send>>;

/// The read half of a served connection. A request is read only when the
/// daemon asks for the next one, as a daemon that is busy reads nothing.
struct Requests<R> {
    idle: Option<R>,
    reading: Option<Reading<R>>,
}

impl<R: AsyncRead + Unpin + Send + 'static> Requests<R> {
    /// Starts reading the next request, unless one is being read already.
    fn pull(&mut self) {
        if let Some(mut reader) = self.idle.take() {
            self.reading = Some(Box::pin(async move {
                let request = read_request(&mut reader).await;
                (reader, request)
            }));
        }
    }

    /// The request being read, `None` once the connection ends. Pending while
    /// nothing is being read. Dropping it mid-read keeps the read for the next
    /// call, so it can wait beside other events.
    async fn next(&mut self) -> Option<Request> {
        let Some(reading) = self.reading.as_mut() else {
            return std::future::pending().await;
        };
        let (reader, request) = reading.await;
        self.reading = None;
        self.idle = Some(reader);
        request
    }
}

async fn serve(
    connection: impl AsyncRead + AsyncWrite + Send + 'static,
    state: Arc<DaemonState>,
    mut hang_up: watch::Receiver<u64>,
    mut pushes: broadcast::Receiver<Vec<u8>>,
) {
    let (reader, writer) = tokio::io::split(connection);
    // Shared with the tasks that write answers held aside.
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut requests = Requests {
        idle: Some(reader),
        reading: None,
    };
    loop {
        requests.pull();
        let (count, (key, request)) = tokio::select! {
            request = requests.next() => match request {
                Some(request) => (state.record(&request), request),
                None => return,
            },
            _ = hang_up.changed() => return,
            Ok(body) = pushes.recv() => {
                if writer.lock().await.write_all(&frame_encode(&body)).await.is_err() {
                    return;
                }
                continue;
            }
        };
        let reaction = (state.script)(&request).or_else(|| housekeeping(&request));
        if let Some(reaction) = reaction {
            let mut writes: Vec<Vec<u8>> = reaction
                .send
                .iter()
                .map(|frame| frame_encode(&frame.body(key)))
                .collect();
            if reaction.together {
                writes = vec![writes.concat()];
            }
            match &reaction.hold {
                Some((reached, gate)) if reaction.aside => {
                    reached.notify_one();
                    let (gate, writer, mut hang_up) =
                        (Arc::clone(gate), Arc::clone(&writer), hang_up.clone());
                    tokio::spawn(async move {
                        tokio::select! {
                            () = gate.notified() => {}
                            _ = hang_up.changed() => return,
                        }
                        let mut writer = writer.lock().await;
                        for bytes in writes {
                            if writer.write_all(&bytes).await.is_err() {
                                return;
                            }
                        }
                    });
                    state.arrived.send_replace(count);
                    continue;
                }
                Some((reached, gate)) => {
                    reached.notify_one();
                    let opened = gate.notified();
                    tokio::pin!(opened);
                    loop {
                        tokio::select! {
                            () = &mut opened => break,
                            _ = hang_up.changed() => return,
                            Ok(body) = pushes.recv() => {
                                if writer.lock().await.write_all(&frame_encode(&body)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                None => {}
            }
            let mut writer = writer.lock().await;
            for bytes in writes {
                if writer.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        }
        // Announced after the answer is written, so a waiter that wakes on
        // this request finds its reply already on the wire.
        state.arrived.send_replace(count);
    }
}

async fn read_request(connection: &mut (impl AsyncRead + Unpin)) -> Option<Request> {
    let mut length = [0u8; 4];
    connection.read_exact(&mut length).await.ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
    connection.read_exact(&mut body).await.ok()?;
    decode_correlated_client_message(&body).ok()
}

/// What a daemon answers without being told to: the two requests every
/// connection makes of its own accord.
fn housekeeping(request: &ClientMessage) -> Option<Reaction> {
    match request {
        ClientMessage::RegisterApprover => Reaction::reply(DaemonMessage::ApproverRegistered {
            pending: Vec::new(),
        }),
        ClientMessage::GetProtocolRevision => Reaction::reply(protocol_revision(None)),
        _ => None,
    }
}

/// One window-link report as the WebView reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) state: String,
    /// Present on `lost` only; any other state carries no `reason` key.
    pub(crate) reason: Option<String>,
    pub(crate) epoch: u64,
}

impl Link {
    /// Reads a window-link payload, failing the test when it is not the shape
    /// the WebView narrows.
    pub(crate) fn from_payload(payload: &Value) -> Self {
        let state = payload["state"]
            .as_str()
            .unwrap_or_else(|| panic!("a link report names its state: {payload}"))
            .to_owned();
        let reason = payload.get("reason").map(|reason| {
            reason
                .as_str()
                .unwrap_or_else(|| panic!("a link reason is a code: {payload}"))
                .to_owned()
        });
        assert_eq!(
            reason.is_some(),
            state == "lost",
            "only a lost link carries a reason: {payload}"
        );
        let epoch = payload["epoch"]
            .as_u64()
            .unwrap_or_else(|| panic!("a link report carries its epoch: {payload}"));
        Self {
            state,
            reason,
            epoch,
        }
    }
}

/// One event the GUI emitted to the WebView.
#[derive(Debug)]
pub(crate) struct RecordedEvent {
    pub(crate) name: String,
    pub(crate) payload: Value,
}

struct HostState {
    /// `None` makes the endpoint unresolvable, as a failed start-up does.
    endpoint: Mutex<Option<String>>,
    /// The failure an unresolvable endpoint reports.
    endpoint_failure: Mutex<fn() -> IpcError>,
    /// The daemon a spawn brings up; `None` makes a spawn fail.
    daemon_on_spawn: Mutex<Option<ScriptedDaemon>>,
    /// The failure a spawn that brings no daemon up reports.
    spawn_failure: Mutex<fn() -> IpcError>,
    /// The warning a spawn that brings the daemon up hands back.
    spawn_warning: Mutex<Option<String>>,
    spawns: watch::Sender<usize>,
    /// The pause the next task spawned on the host is run under.
    pause_next: Mutex<Option<TaskPause>>,
    /// How many tasks spawned on the host ran to their end.
    tasks_ended: watch::Sender<usize>,
    events: mpsc::UnboundedSender<RecordedEvent>,
    recorded: tokio::sync::Mutex<mpsc::UnboundedReceiver<RecordedEvent>>,
    mailbox: Arc<FrameMailbox>,
}

/// A [`GuiHost`] that records instead of acting: events are kept, a daemon
/// start is counted (and brings up the daemon it was told to, or fails), and
/// the endpoint is one chosen by the test.
#[derive(Clone)]
pub(crate) struct FakeHost {
    state: Arc<HostState>,
}

impl FakeHost {
    /// A host whose endpoint nobody listens on and whose daemon start fails.
    pub(crate) fn new(tag: &str) -> Self {
        let (events, recorded) = mpsc::unbounded_channel();
        Self {
            state: Arc::new(HostState {
                endpoint: Mutex::new(Some(unique_endpoint_name(ENDPOINT_PREFIX, tag))),
                endpoint_failure: Mutex::new(|| IpcError::NotConnected),
                daemon_on_spawn: Mutex::new(None),
                spawn_failure: Mutex::new(|| IpcError::DaemonSpawn),
                spawn_warning: Mutex::new(None),
                spawns: watch::channel(0).0,
                pause_next: Mutex::new(None),
                tasks_ended: watch::channel(0).0,
                events,
                recorded: tokio::sync::Mutex::new(recorded),
                mailbox: Arc::new(FrameMailbox::new()),
            }),
        }
    }

    /// A host whose endpoint `daemon` already serves.
    pub(crate) async fn served_by(tag: &str, daemon: &ScriptedDaemon) -> Self {
        let host = Self::new(tag);
        daemon.listen(&host.endpoint_name()).await;
        host
    }

    /// The endpoint the host resolves to.
    pub(crate) fn endpoint_name(&self) -> String {
        self.state
            .endpoint
            .lock()
            .expect("endpoint")
            .clone()
            .expect("the host has an endpoint")
    }

    /// Replaces the endpoint; `None` makes it unresolvable.
    pub(crate) fn set_endpoint(&self, endpoint: Option<String>) {
        *self.state.endpoint.lock().expect("endpoint") = endpoint;
    }

    /// Makes the endpoint unresolvable for the reason `failure` names.
    pub(crate) fn fails_endpoint_with(&self, failure: fn() -> IpcError) {
        *self
            .state
            .endpoint_failure
            .lock()
            .expect("endpoint failure") = failure;
        self.set_endpoint(None);
    }

    /// Makes a daemon start that brings no daemon up fail for the reason
    /// `failure` names.
    pub(crate) fn fails_spawns_with(&self, failure: fn() -> IpcError) {
        *self.state.spawn_failure.lock().expect("spawn failure") = failure;
    }

    /// Makes a daemon start bring `daemon` up on the host's endpoint.
    pub(crate) fn starts_daemon(&self, daemon: &ScriptedDaemon) {
        *self.state.daemon_on_spawn.lock().expect("spawn behaviour") = Some(daemon.clone());
    }

    /// Makes a daemon start bring `daemon` up the weaker way, handing back
    /// `warning`.
    pub(crate) fn starts_daemon_with_warning(&self, daemon: &ScriptedDaemon, warning: &str) {
        self.starts_daemon(daemon);
        *self.state.spawn_warning.lock().expect("spawn warning") = Some(warning.to_owned());
    }

    /// How many times the daemon was asked to start.
    pub(crate) fn spawn_count(&self) -> usize {
        *self.state.spawns.borrow()
    }

    /// Waits until the daemon has been asked to start `count` times.
    pub(crate) async fn wait_for_spawns(&self, count: usize) {
        let mut spawns = self.state.spawns.subscribe();
        reply_within("daemon start", spawns.wait_for(|n| *n >= count))
            .await
            .expect("the host state outlives the wait");
    }

    /// Runs the next task spawned on the host under `pause`. A client's owner
    /// is the first task its first ask for a connection spawns.
    pub(crate) fn pauses_next_task(&self, pause: &TaskPause) {
        *self.state.pause_next.lock().expect("pause") = Some(pause.clone());
    }

    /// Waits until `count` tasks spawned on the host have run to their end.
    pub(crate) async fn wait_for_ended_tasks(&self, count: usize) {
        let mut ended = self.state.tasks_ended.subscribe();
        reply_within("tasks ended", ended.wait_for(|n| *n >= count))
            .await
            .expect("the host state outlives the wait");
    }

    /// The next window-link report about `window_id`, skipping every other
    /// event.
    pub(crate) async fn next_link(&self, window_id: &str) -> Link {
        loop {
            let payload = self.next_event(EVENT_WINDOW_LINK).await;
            if payload["window_id"] == window_id {
                return Link::from_payload(&payload);
            }
        }
    }

    /// The payload of the next event named `name`, skipping the others.
    pub(crate) async fn next_event(&self, name: &str) -> Value {
        reply_within(name, async {
            let mut recorded = self.state.recorded.lock().await;
            loop {
                let event = recorded.recv().await.expect("the host keeps its sender");
                if event.name == name {
                    return event.payload;
                }
            }
        })
        .await
    }

    /// Every event up to and including the next one named `name`, oldest
    /// first, so a test can see what came before it as well.
    pub(crate) async fn events_through(&self, name: &str) -> Vec<RecordedEvent> {
        reply_within(name, async {
            let mut recorded = self.state.recorded.lock().await;
            let mut events = Vec::new();
            loop {
                let event = recorded.recv().await.expect("the host keeps its sender");
                let found = event.name == name;
                events.push(event);
                if found {
                    return events;
                }
            }
        })
        .await
    }

    /// Every event emitted and not yet taken, oldest first.
    pub(crate) fn take_events(&self) -> Vec<RecordedEvent> {
        let mut recorded = self
            .state
            .recorded
            .try_lock()
            .expect("nobody is waiting for an event");
        let mut events = Vec::new();
        while let Ok(event) = recorded.try_recv() {
            events.push(event);
        }
        events
    }

    /// The mailbox the host's frames go to.
    pub(crate) fn mailbox(&self) -> &Arc<FrameMailbox> {
        &self.state.mailbox
    }
}

impl GuiHost for FakeHost {
    fn endpoint(&self) -> Result<String, IpcError> {
        self.state
            .endpoint
            .lock()
            .expect("endpoint")
            .clone()
            .ok_or_else(|| {
                (self
                    .state
                    .endpoint_failure
                    .lock()
                    .expect("endpoint failure"))()
            })
    }

    fn spawn_daemon(&self) -> Result<Option<String>, IpcError> {
        self.state.spawns.send_modify(|count| *count += 1);
        let daemon = self
            .state
            .daemon_on_spawn
            .lock()
            .expect("spawn behaviour")
            .clone()
            .ok_or_else(|| (self.state.spawn_failure.lock().expect("spawn failure"))())?;
        let endpoint = self.endpoint_name();
        tokio::spawn(async move { daemon.listen(&endpoint).await });
        Ok(self
            .state
            .spawn_warning
            .lock()
            .expect("spawn warning")
            .clone())
    }

    fn emit_event<P: Serialize + Clone>(&self, event: &str, payload: P) {
        let _ = self.state.events.send(RecordedEvent {
            name: event.to_owned(),
            payload: serde_json::to_value(payload).expect("an event payload serializes"),
        });
    }

    fn frame_mailbox(&self) -> Option<&Arc<FrameMailbox>> {
        Some(&self.state.mailbox)
    }

    fn spawn_task<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let pause = self.state.pause_next.lock().expect("pause").take();
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut task = std::pin::pin!(task);
            std::future::poll_fn(|cx| match &pause {
                Some(pause) if pause.holds(cx) => std::task::Poll::Pending,
                _ => task.as_mut().poll(cx),
            })
            .await;
            state.tasks_ended.send_modify(|count| *count += 1);
        });
    }
}

/// Stops a task spawned on a host from being polled while it is held: what
/// woke the task meanwhile is acted on once the pause is released.
#[derive(Clone, Default)]
pub(crate) struct TaskPause(Arc<Mutex<PauseState>>);

#[derive(Default)]
struct PauseState {
    held: bool,
    /// The task woken while held, woken again on the release.
    waker: Option<std::task::Waker>,
}

impl TaskPause {
    pub(crate) fn hold(&self) {
        self.0.lock().expect("pause").held = true;
    }

    pub(crate) fn release(&self) {
        let waker = {
            let mut state = self.0.lock().expect("pause");
            state.held = false;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Whether the task must not be polled now; keeps `cx`'s waker for the
    /// release when it must not.
    fn holds(&self, cx: &std::task::Context<'_>) -> bool {
        let mut state = self.0.lock().expect("pause");
        if state.held {
            state.waker = Some(cx.waker().clone());
        }
        state.held
    }
}
