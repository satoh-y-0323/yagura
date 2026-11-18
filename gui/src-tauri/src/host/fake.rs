//! Test doubles for [`GuiHost`] and for the daemon on the other side of it.
//!
//! The host records what the GUI asks of the application; the daemon is a real
//! endpoint, so a connection under test is a real connection that a script
//! answers frame by frame.

use std::future::Future;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, watch};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, DaemonMessage, PROTOCOL_REVISION, Palette, TerminalSize, WindowInfo,
    WindowStatus, decode_client_message, encode_daemon_message, frame_encode,
};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

use super::GuiHost;
use crate::connection_pool::ConnectionPool;
use crate::events::EVENT_WINDOW_LINK;
use crate::frame_mailbox::FrameMailbox;
use crate::ipc_client::IpcError;

const ENDPOINT_PREFIX: &str = "yagura-test-gui-host";

type PeerConnection = <PlatformIpc as PeerAuth>::Connection;

/// What a scripted daemon does with one request: the frames to send back.
pub(crate) struct Reaction {
    send: Vec<DaemonMessage>,
    /// Told when the request has arrived, then waited on before answering.
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl Reaction {
    /// Answers with `message`.
    pub(crate) fn reply(message: DaemonMessage) -> Option<Self> {
        Self::replies(vec![message])
    }

    /// Answers with `messages`, in order.
    pub(crate) fn replies(messages: Vec<DaemonMessage>) -> Option<Self> {
        Some(Self {
            send: messages,
            hold: None,
        })
    }

    /// Answers with `messages` only once the test opens `gate`, after telling
    /// `reached` that the request has arrived. Both use `notify_one`, which
    /// keeps a permit for a waiter that comes later.
    pub(crate) fn replies_after(
        reached: Arc<Notify>,
        gate: Arc<Notify>,
        messages: Vec<DaemonMessage>,
    ) -> Option<Self> {
        Some(Self {
            send: messages,
            hold: Some((reached, gate)),
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
    received: Mutex<Vec<ClientMessage>>,
    /// The number of requests received so far, for waiting on the next one.
    arrived: watch::Sender<usize>,
    /// Bumped to end every connection currently being served.
    hang_up: watch::Sender<u64>,
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
                arrived: watch::channel(0).0,
                hang_up: watch::channel(0).0,
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
                tokio::spawn(serve(connection, Arc::clone(&state), hang_up));
            }
        });
    }

    /// Every request received so far, in arrival order across connections.
    pub(crate) fn received(&self) -> Vec<ClientMessage> {
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

    /// Ends every connection being served; later connections are served as
    /// usual.
    pub(crate) fn hang_up(&self) {
        self.state
            .hang_up
            .send_modify(|generation| *generation += 1);
    }
}

async fn serve(
    mut connection: PeerConnection,
    state: Arc<DaemonState>,
    mut hang_up: watch::Receiver<u64>,
) {
    loop {
        let request = tokio::select! {
            request = read_request(&mut connection) => match request {
                Some(request) => request,
                None => return,
            },
            _ = hang_up.changed() => return,
        };
        let count = {
            let mut received = state.received.lock().expect("received log");
            received.push(request.clone());
            received.len()
        };
        let reaction = (state.script)(&request).or_else(|| housekeeping(&request));
        if let Some(reaction) = reaction {
            if let Some((reached, gate)) = &reaction.hold {
                reached.notify_one();
                tokio::select! {
                    () = gate.notified() => {}
                    _ = hang_up.changed() => return,
                }
            }
            for message in &reaction.send {
                let frame = frame_encode(&encode_daemon_message(message));
                if connection.write_all(&frame).await.is_err() {
                    return;
                }
            }
        }
        // Announced after the answer is written, so a waiter that wakes on
        // this request finds its reply already on the wire.
        state.arrived.send_replace(count);
    }
}

async fn read_request(connection: &mut PeerConnection) -> Option<ClientMessage> {
    let mut length = [0u8; 4];
    connection.read_exact(&mut length).await.ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
    connection.read_exact(&mut body).await.ok()?;
    decode_client_message(&body).ok()
}

/// What a daemon answers without being told to: the two requests every
/// connection makes of its own accord.
fn housekeeping(request: &ClientMessage) -> Option<Reaction> {
    match request {
        ClientMessage::RegisterApprover => Reaction::reply(DaemonMessage::ApproverRegistered {
            pending: Vec::new(),
        }),
        ClientMessage::GetProtocolRevision => Reaction::reply(DaemonMessage::ProtocolRevision {
            revision: PROTOCOL_REVISION,
        }),
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
    /// The daemon a spawn brings up; `None` makes a spawn fail.
    daemon_on_spawn: Mutex<Option<ScriptedDaemon>>,
    spawns: watch::Sender<usize>,
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
                daemon_on_spawn: Mutex::new(None),
                spawns: watch::channel(0).0,
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

    /// Makes a daemon start bring `daemon` up on the host's endpoint.
    pub(crate) fn starts_daemon(&self, daemon: &ScriptedDaemon) {
        *self.state.daemon_on_spawn.lock().expect("spawn behaviour") = Some(daemon.clone());
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
            .ok_or(IpcError::NotConnected)
    }

    fn spawn_daemon(&self) -> Result<(), IpcError> {
        self.state.spawns.send_modify(|count| *count += 1);
        let daemon = self
            .state
            .daemon_on_spawn
            .lock()
            .expect("spawn behaviour")
            .clone()
            .ok_or(IpcError::DaemonSpawn)?;
        let endpoint = self.endpoint_name();
        tokio::spawn(async move { daemon.listen(&endpoint).await });
        Ok(())
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
        tokio::spawn(task);
    }
}
