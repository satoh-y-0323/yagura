//! The owner task: it carries out the transitions' actions, begins and
//! ends generations, and publishes the client's state.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::sync::{OwnedMutexGuard, mpsc, oneshot, watch};
use yagura_protocol::{ClientMessage, StatusBoot};

use super::super::{
    ConnectMode, Connection, DaemonStart, IpcError, MadeStart, MadeStartReached, PendingKind,
    SentDefaultPalettes, attach_restoring, lock_blocking, register_approver, restore_taken,
    revision_probe_outcome, revision_taken, speaks_this_wire,
};
use super::connect::{connect_after_spawn, connect_once, connect_retrying};
use super::generation::{DaemonConnection, Generation, StepReply};
use super::reader_link::{EndNews, EndSink};
use super::transition::{
    Action, ClientKind, Event, Failure, Lifecycle, Public, PublicState, Registration, Step,
    TakenAnswer, Until, WantId,
};
use crate::events::{DaemonConnectedPayload, EVENT_DAEMON_CONNECTED};
use crate::host::GuiHost;
use crate::message_dispatch::read_loop;
use crate::window_record::{LinkState, WindowRecord};

/// One message to a client's owner task.
#[derive(Debug)]
enum OwnerEvent {
    /// A caller asks for a connection, and waits on `reply`.
    Want {
        mode: ConnectMode,
        until: Until,
        reply: oneshot::Sender<Result<(), IpcError>>,
    },
    /// The generation's reader saw it end.
    Ended(EndNews),
    /// The client is let go of; `ack` is answered once nothing is open.
    Retire { ack: oneshot::Sender<()> },
}

/// Whether the owner task runs, and how to reach it.
enum OwnerSlot {
    /// No caller asked yet. Holds the publisher the owner takes when it starts.
    Unborn(watch::Sender<Published>),
    /// The owner runs and reads this sender's channel.
    Running(mpsc::UnboundedSender<OwnerEvent>),
    /// Let go of: nothing opens again.
    Retired,
}

/// What callers can tell about a client's connection, as its owner last
/// published it: the lifecycle's public state, and the generation it names
/// once that generation is open.
#[derive(Debug, Clone)]
pub(crate) struct Published {
    public: Public,
    open: Option<Arc<Generation>>,
}

impl Published {
    fn absent() -> Self {
        Self {
            public: Public {
                state: PublicState::Absent,
                daemon_answers: false,
            },
            open: None,
        }
    }

    /// The open generation, the only one a caller may write to.
    pub(crate) fn open(&self) -> Option<&Arc<Generation>> {
        self.open.as_ref()
    }

    pub(crate) fn public(&self) -> &Public {
        &self.public
    }

    /// The answer a want of `until` gets without asking the owner, if any.
    fn answers(&self, until: Until) -> Option<Result<(), IpcError>> {
        match (&self.public.state, until) {
            (PublicState::Retired, _) => Some(Err(IpcError::NotConnected)),
            (PublicState::Open { .. }, _) | (PublicState::Reached, Until::Reached) => Some(Ok(())),
            _ => None,
        }
    }
}

/// The parts of a client its owner task reads too.
pub(crate) struct Shared {
    kind: ClientKind,
    record: Option<Arc<WindowRecord>>,
    serial: u64,
    slot: std::sync::Mutex<OwnerSlot>,
    published: watch::Receiver<Published>,
    /// Keys requests across generations, so no key of this client is used twice.
    correlation_ids: Arc<AtomicU64>,
    daemon_start: std::sync::Mutex<DaemonStart>,
    palettes: Arc<tokio::sync::Mutex<SentDefaultPalettes>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("kind", &self.kind)
            .field("serial", &self.serial)
            .field("published", &*self.published.borrow())
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new(kind: ClientKind, record: Option<Arc<WindowRecord>>, serial: u64) -> Self {
        let (publisher, published) = watch::channel(Published::absent());
        Self {
            kind,
            record,
            serial,
            slot: std::sync::Mutex::new(OwnerSlot::Unborn(publisher)),
            published,
            correlation_ids: Arc::new(AtomicU64::new(0)),
            daemon_start: std::sync::Mutex::new(DaemonStart::default()),
            palettes: Arc::new(tokio::sync::Mutex::new(SentDefaultPalettes::default())),
        }
    }

    /// A control client open on `generation` with no owner: it asks for no
    /// connection, and is answered `NotConnected` when it does.
    #[cfg(test)]
    pub(crate) fn wired(generation: Arc<Generation>, serial: u64) -> Self {
        let correlation_ids = Arc::new(AtomicU64::new(0));
        let public = Public {
            state: PublicState::Open {
                generation: generation.number(),
                revision: generation
                    .revision()
                    .unwrap_or(yagura_protocol::PROTOCOL_REVISION),
                registration: Registration::Accepted,
            },
            daemon_answers: true,
        };
        let (_, published) = watch::channel(Published {
            public,
            open: Some(generation),
        });
        Self {
            kind: ClientKind::Control,
            record: None,
            serial,
            slot: std::sync::Mutex::new(OwnerSlot::Retired),
            published,
            correlation_ids,
            daemon_start: std::sync::Mutex::new(DaemonStart::default()),
            palettes: Arc::new(tokio::sync::Mutex::new(SentDefaultPalettes::default())),
        }
    }

    pub(crate) fn kind(&self) -> ClientKind {
        self.kind
    }

    pub(crate) fn record(&self) -> Option<&Arc<WindowRecord>> {
        self.record.as_ref()
    }

    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }

    pub(crate) fn published(&self) -> Published {
        self.published.borrow().clone()
    }

    /// The open generation.
    ///
    /// # Errors
    /// [`IpcError::NotConnected`] when none is open: nothing waits for one.
    pub(crate) fn open(&self) -> Result<Arc<Generation>, IpcError> {
        self.published
            .borrow()
            .open()
            .cloned()
            .ok_or(IpcError::NotConnected)
    }

    /// Whether this client may change `record`: it is the record's owner and
    /// has not been let go of.
    pub(crate) fn holds(&self, record: &WindowRecord) -> bool {
        record.owner() == self.serial && !matches!(*lock_blocking(&self.slot), OwnerSlot::Retired)
    }

    /// The daemon start this client last announced, `None` before the first.
    pub(crate) fn daemon_connection(&self) -> Option<DaemonConnectedPayload> {
        lock_blocking(&self.daemon_start).last.clone()
    }

    pub(crate) fn palettes(&self) -> &tokio::sync::Mutex<SentDefaultPalettes> {
        &self.palettes
    }

    /// Asks the owner for a connection of `mode` and waits until `until`
    /// holds, starting the owner on the first ask.
    ///
    /// # Errors
    /// The error of the attempt that covered this want, or
    /// [`IpcError::NotConnected`] once the client was let go of.
    pub(crate) async fn want<H: GuiHost>(
        self: &Arc<Self>,
        app: &H,
        mode: ConnectMode,
        until: Until,
    ) -> Result<(), IpcError> {
        if let Some(answer) = self.published().answers(until) {
            return answer;
        }
        let (reply, answered) = oneshot::channel();
        let born = {
            let mut slot = lock_blocking(&self.slot);
            match std::mem::replace(&mut *slot, OwnerSlot::Retired) {
                OwnerSlot::Retired => return Err(IpcError::NotConnected),
                OwnerSlot::Running(events) => {
                    let _ = events.send(OwnerEvent::Want { mode, until, reply });
                    *slot = OwnerSlot::Running(events);
                    None
                }
                // Sent under the slot's lock, so a retirement that follows
                // queues behind this want.
                OwnerSlot::Unborn(publisher) => {
                    let (events, inbox) = mpsc::unbounded_channel();
                    let weak = events.downgrade();
                    let _ = events.send(OwnerEvent::Want { mode, until, reply });
                    *slot = OwnerSlot::Running(events);
                    Some((publisher, inbox, weak))
                }
            }
        };
        if let Some((publisher, inbox, weak)) = born {
            app.spawn_task(run_owner(
                app.clone(),
                Arc::clone(self),
                publisher,
                inbox,
                weak,
            ));
        }
        answered.await.unwrap_or(Err(IpcError::NotConnected))
    }

    /// Lets the client go: once this returns nothing of it is open, and
    /// nothing opens again.
    pub(crate) async fn retire(&self) {
        // Under the record's lock, so no change this client makes to the
        // record is midway.
        let record = match &self.record {
            Some(record) => Some(record.lock().await),
            None => None,
        };
        let previous = std::mem::replace(&mut *lock_blocking(&self.slot), OwnerSlot::Retired);
        let acked = match previous {
            OwnerSlot::Running(events) => {
                let (ack, acked) = oneshot::channel();
                let _ = events.send(OwnerEvent::Retire { ack });
                Some(acked)
            }
            OwnerSlot::Unborn(_) | OwnerSlot::Retired => None,
        };
        drop(record);
        if let Some(acked) = acked {
            let _ = acked.await;
        }
    }

    /// Closes the owner's inbox, as the client it serves is gone.
    pub(crate) fn release(&self) {
        let mut slot = lock_blocking(&self.slot);
        if matches!(*slot, OwnerSlot::Running(_)) {
            *slot = OwnerSlot::Retired;
        }
    }
}

/// What a stage brought back.
enum StageOutcome {
    Reached {
        attempt: u64,
        connection: Result<Connection, IpcError>,
    },
    Probed {
        generation: u64,
        settled: Option<(u32, Option<StatusBoot>)>,
    },
    Registered {
        generation: u64,
        registration: Option<Registration>,
    },
    /// The palettes' guard, held until the generation is published open.
    Palettes(OwnedMutexGuard<SentDefaultPalettes>),
    Restored {
        generation: u64,
        restored: bool,
    },
    /// An opening step brought no answer: its generation's end decides.
    Unanswered,
    Moved,
}

type Stage = Pin<Box<dyn Future<Output = StageOutcome> + Send>>;

/// The one task that changes a client's connection lifetime.
async fn run_owner<H: GuiHost>(
    app: H,
    shared: Arc<Shared>,
    publisher: watch::Sender<Published>,
    mut inbox: mpsc::UnboundedReceiver<OwnerEvent>,
    weak: mpsc::WeakUnboundedSender<OwnerEvent>,
) {
    let mut owner = Owner::new(app, shared, publisher, weak);
    let mut stage: Option<Stage> = None;
    let mut closed = false;
    loop {
        while stage.is_none() {
            let Some(step) = owner.queue.pop_front() else {
                break;
            };
            stage = owner.execute(step);
        }
        if closed && stage.is_none() {
            return;
        }
        // The stage is polled, never dropped midway: it may be writing.
        tokio::select! {
            outcome = poll_stage(&mut stage), if stage.is_some() => {
                stage = None;
                owner.finish(outcome);
            }
            event = inbox.recv(), if !closed => match event {
                Some(event) => owner.receive(event),
                None => {
                    closed = true;
                    owner.close();
                }
            },
        }
    }
}

async fn poll_stage(stage: &mut Option<Stage>) -> StageOutcome {
    match stage {
        Some(stage) => stage.await,
        None => std::future::pending().await,
    }
}

struct Owner<H> {
    app: H,
    shared: Arc<Shared>,
    publisher: watch::Sender<Published>,
    weak: mpsc::WeakUnboundedSender<OwnerEvent>,
    lifecycle: Lifecycle,
    public: Public,
    /// The generation published open, once its opening steps all ran.
    opened: Option<u64>,
    generations: HashMap<u64, Arc<Generation>>,
    connections: HashMap<u64, Connection>,
    replies: HashMap<u64, oneshot::Sender<Result<(), IpcError>>>,
    next_want: u64,
    acks: Vec<oneshot::Sender<()>>,
    boots: HashMap<u64, Option<StatusBoot>>,
    queue: VecDeque<Step<IpcError>>,
    palettes: Option<OwnedMutexGuard<SentDefaultPalettes>>,
}

impl<H: GuiHost> Owner<H> {
    fn new(
        app: H,
        shared: Arc<Shared>,
        publisher: watch::Sender<Published>,
        weak: mpsc::WeakUnboundedSender<OwnerEvent>,
    ) -> Self {
        let lifecycle = Lifecycle::new(shared.kind);
        let public = lifecycle.public();
        Self {
            app,
            shared,
            publisher,
            weak,
            lifecycle,
            public,
            opened: None,
            generations: HashMap::new(),
            connections: HashMap::new(),
            replies: HashMap::new(),
            next_want: 0,
            acks: Vec::new(),
            boots: HashMap::new(),
            queue: VecDeque::new(),
            palettes: None,
        }
    }

    fn receive(&mut self, event: OwnerEvent) {
        match event {
            OwnerEvent::Want { mode, until, reply } => {
                let id = WantId(self.next_want);
                self.next_want += 1;
                self.replies.insert(id.0, reply);
                self.feed(Event::Want { id, mode, until }, true, None);
            }
            OwnerEvent::Ended(news) => {
                let generation = news.generation();
                let window_id = self
                    .generations
                    .get(&generation)
                    .and_then(|ended| ended.window_id());
                // Ended before the lifecycle hears of it, so what the
                // generation took is settled and no later answer counts.
                let taken = self.end(generation);
                self.feed(
                    Event::Ended {
                        generation,
                        kind: news.kind(),
                        window_id,
                        taken,
                    },
                    true,
                    None,
                );
            }
            OwnerEvent::Retire { ack } => {
                self.acks.push(ack);
                self.feed(Event::Retire, true, None);
            }
        }
    }

    /// The client is gone: what it had open ends as a retirement does.
    fn close(&mut self) {
        if self.public.state != PublicState::Retired {
            self.feed(Event::Retire, true, None);
        }
    }

    fn finish(&mut self, outcome: StageOutcome) {
        match outcome {
            StageOutcome::Reached {
                attempt,
                connection: Ok(connection),
            } => {
                self.connections.insert(attempt, connection);
                self.feed(Event::Connected { attempt }, false, None);
            }
            StageOutcome::Reached {
                attempt,
                connection: Err(error),
            } => {
                let failure = failure_of(&error);
                self.feed(
                    Event::ConnectFailed {
                        attempt,
                        error: failure,
                    },
                    false,
                    Some(error),
                );
            }
            StageOutcome::Probed {
                generation,
                settled: Some((revision, boot)),
            } => {
                if let Some(probed) = self.generations.get(&generation) {
                    probed.record_revision(revision);
                    self.boots.insert(generation, boot);
                }
                self.feed(
                    Event::Probed {
                        generation,
                        revision,
                    },
                    false,
                    None,
                );
            }
            StageOutcome::Registered {
                generation,
                registration: Some(registration),
            } => self.feed(
                Event::Registered {
                    generation,
                    registration,
                },
                false,
                None,
            ),
            StageOutcome::Probed {
                generation,
                settled: None,
            }
            | StageOutcome::Registered {
                generation,
                registration: None,
            }
            | StageOutcome::Restored {
                generation,
                restored: false,
            } => self.feed(Event::StepFailed { generation }, false, None),
            StageOutcome::Restored {
                generation,
                restored: true,
            } => self.feed(Event::Restored { generation }, false, None),
            StageOutcome::Palettes(guard) => self.palettes = Some(guard),
            StageOutcome::Unanswered | StageOutcome::Moved => {}
        }
    }

    /// Applies `event` and lines up its steps. An `urgent` event arrived
    /// while a stage runs: the steps that only end or answer run at once, so
    /// the stage that waits on the daemon is not what holds them up.
    fn feed(&mut self, event: Event, urgent: bool, attempt_error: Option<IpcError>) {
        let transition = self.lifecycle.on(event);
        self.public = transition.public;
        self.publish();
        for step in transition.actions {
            let step = match step {
                Step::Do(action) => Step::Do(action),
                Step::Answer { id, result, opened } => Step::Answer {
                    id,
                    opened,
                    result: result
                        .map_err(|failure| failure_error(failure, attempt_error.as_ref())),
                },
            };
            if urgent && self.runs_at_once(&step) {
                let _ = self.execute(step);
            } else {
                self.queue.push_back(step);
            }
        }
    }

    fn runs_at_once(&self, step: &Step<IpcError>) -> bool {
        match step {
            Step::Do(Action::End { .. } | Action::AckRetire | Action::Discard { .. }) => true,
            Step::Do(_) => false,
            Step::Answer { result: Err(_), .. } | Step::Answer { opened: None, .. } => true,
            Step::Answer {
                opened: Some(generation),
                ..
            } => self.opened == Some(*generation),
        }
    }

    fn publish(&self) {
        let mut public = self.public.clone();
        let open = match public.state {
            PublicState::Open { generation, .. } if self.opened == Some(generation) => {
                self.generations.get(&generation).cloned()
            }
            _ => None,
        };
        // Open only once its opening steps ran: until then callers read it
        // as reached.
        if open.is_none() && matches!(public.state, PublicState::Open { .. }) {
            public.state = PublicState::Reached;
        }
        self.publisher.send_replace(Published { public, open });
    }

    fn execute(&mut self, step: Step<IpcError>) -> Option<Stage> {
        match step {
            Step::Answer { id, result, opened } => {
                self.answer(id, result, opened);
                None
            }
            Step::Do(action) => self.act(action),
        }
    }

    fn answer(&mut self, id: WantId, result: Result<(), IpcError>, opened_by: Option<u64>) {
        let Some(reply) = self.replies.remove(&id.0) else {
            return;
        };
        // The generation it waited for ended before opening: dropped unsent,
        // the caller hears it was not connected.
        if opened_by.is_some_and(|generation| !self.generations.contains_key(&generation)) {
            return;
        }
        let _ = reply.send(result);
    }

    fn act(&mut self, action: Action) -> Option<Stage> {
        match action {
            Action::Attempt { attempt, mode } => Some(attempt_stage(
                self.app.clone(),
                Arc::clone(&self.shared),
                attempt,
                mode,
            )),
            Action::Discard { attempt } => {
                self.connections.remove(&attempt);
                None
            }
            Action::Begin {
                attempt,
                generation,
            } => {
                self.begin(attempt, generation);
                None
            }
            Action::ProbeRevision { generation } => self
                .live(generation)
                .map(|probed| probe_stage(generation, probed)),
            Action::Register { generation } => self
                .live(generation)
                .map(|registering| register_stage(self.app.clone(), generation, registering)),
            Action::AnnounceStart { generation } => {
                self.announce(generation);
                None
            }
            Action::ResendPalettes { generation } => {
                let palettes = Arc::clone(&self.shared.palettes);
                self.live(generation)
                    .map(|opening| palettes_stage(palettes, generation, opening))
            }
            Action::Open { generation } => {
                self.open(generation);
                None
            }
            Action::RestoreAttach {
                generation,
                window_id,
            } => self.live(generation).map(|restoring| {
                restore_stage(
                    self.app.clone(),
                    Arc::clone(&self.shared),
                    generation,
                    restoring,
                    window_id,
                )
            }),
            Action::End { generation } => {
                let _ = self.end(generation);
                None
            }
            Action::MoveLink { window_id, to } => Some(move_stage(
                self.app.clone(),
                Arc::clone(&self.shared),
                window_id,
                to,
            )),
            Action::AckRetire => {
                for ack in self.acks.drain(..) {
                    let _ = ack.send(());
                }
                None
            }
        }
    }

    fn live(&self, generation: u64) -> Option<Arc<Generation>> {
        self.generations.get(&generation).cloned()
    }

    fn begin(&mut self, attempt: u64, generation: u64) {
        let Some(connection) = self.connections.remove(&attempt) else {
            return;
        };
        let (begun, reader, link) = Generation::begin(
            generation,
            DaemonConnection(connection),
            Arc::clone(&self.shared.correlation_ids),
            Some(end_sink(self.weak.clone())),
        );
        self.app.spawn_task(read_loop(
            reader,
            self.app.clone(),
            Arc::clone(&begun),
            link,
        ));
        self.generations.insert(generation, begun);
    }

    fn announce(&mut self, generation: u64) {
        let Some(announced) = self.generations.get(&generation) else {
            return;
        };
        let compatible = speaks_this_wire(announced.revision());
        let boot = self.boots.get(&generation).copied().flatten();
        let announcement = {
            let mut start = lock_blocking(&self.shared.daemon_start);
            let (next, announcement) = start.first_settled(boot, self.shared.kind, compatible);
            *start = next;
            announcement
        };
        if let Some(payload) = announcement {
            self.app.emit_event(EVENT_DAEMON_CONNECTED, payload);
        }
    }

    fn open(&mut self, generation: u64) {
        let opens = matches!(
            self.public.state,
            PublicState::Open { generation: current, .. } if current == generation
        );
        if opens && self.generations.contains_key(&generation) {
            self.opened = Some(generation);
            self.publish();
        }
        // Published open under the palettes' guard, so a palette asked for
        // meanwhile is sent either by the opening or by its caller.
        self.palettes = None;
    }

    /// Ends `generation`, and returns the last answer an opening step of it
    /// took: `None` for one ended before or never begun.
    fn end(&mut self, generation: u64) -> Option<TakenAnswer> {
        let taken = self
            .generations
            .remove(&generation)
            .and_then(|ended| ended.end());
        if self.opened == Some(generation) {
            self.opened = None;
        }
        self.boots.remove(&generation);
        taken
    }
}

/// The lifecycle tells a failed attempt apart only this far; the error itself
/// reaches the wants the attempt covered.
fn failure_of(error: &IpcError) -> Failure {
    match error {
        IpcError::NotConnected => Failure::NotConnected,
        _ => Failure::DaemonSpawn,
    }
}

fn failure_error(failure: Failure, attempt_error: Option<&IpcError>) -> IpcError {
    match (attempt_error, failure) {
        (Some(error), _) => error.duplicate(),
        (None, Failure::NotConnected) => IpcError::NotConnected,
        (None, Failure::DaemonSpawn) => IpcError::DaemonSpawn,
    }
}

fn attempt_stage<H: GuiHost>(
    app: H,
    shared: Arc<Shared>,
    attempt: u64,
    mode: ConnectMode,
) -> Stage {
    Box::pin(async move {
        let connection = reach(&app, &shared, mode).await;
        StageOutcome::Reached {
            attempt,
            connection,
        }
    })
}

/// One connect attempt of `mode`.
async fn reach<H: GuiHost>(
    app: &H,
    shared: &Shared,
    mode: ConnectMode,
) -> Result<Connection, IpcError> {
    let endpoint = app.endpoint()?;
    let reached = match mode {
        // One attempt decides whether a daemon has to be started, so this
        // path does not spend the retry budget first.
        ConnectMode::Once | ConnectMode::Spawn => connect_once(&endpoint).await,
        ConnectMode::Retry => connect_retrying(&endpoint).await,
    };
    match reached {
        Some(connection) => Ok(connection),
        None if mode == ConnectMode::Spawn => {
            let warning = app.spawn_daemon()?;
            lock_blocking(&shared.daemon_start).made = Some(MadeStart {
                warning,
                reached: MadeStartReached::NotYet,
            });
            connect_after_spawn(&endpoint).await
        }
        None => Err(IpcError::NotConnected),
    }
}

fn probe_stage(generation: u64, probed: Arc<Generation>) -> Stage {
    Box::pin(async move {
        let reply = probed
            .request_step(
                &ClientMessage::GetProtocolRevision,
                PendingKind::GetProtocolRevision,
                Some(revision_taken),
            )
            .await;
        match reply {
            StepReply::Answered(answer) => StageOutcome::Probed {
                generation,
                settled: revision_probe_outcome(&answer),
            },
            StepReply::Unanswered(_) => StageOutcome::Unanswered,
        }
    })
}

fn register_stage<H: GuiHost>(app: H, generation: u64, registering: Arc<Generation>) -> Stage {
    Box::pin(async move {
        match register_approver(&app, &registering).await {
            StepReply::Answered(registration) => StageOutcome::Registered {
                generation,
                registration,
            },
            StepReply::Unanswered(_) => StageOutcome::Unanswered,
        }
    })
}

/// Sends the remembered default palettes to the generation opening, and keeps
/// their guard for the opening that follows.
fn palettes_stage(
    palettes: Arc<tokio::sync::Mutex<SentDefaultPalettes>>,
    generation: u64,
    opening: Arc<Generation>,
) -> Stage {
    Box::pin(async move {
        let mut sent = palettes.lock_owned().await;
        if let Some(latest) = sent.latest.clone()
            && sent.sent_on != Some(generation)
            // A failed write ends the generation, and its reader tells the
            // owner how.
            && opening
                .write(&ClientMessage::SetDefaultPalettes(latest))
                .await
                .is_ok()
        {
            sent.sent_on = Some(generation);
        }
        StageOutcome::Palettes(sent)
    })
}

fn restore_stage<H: GuiHost>(
    app: H,
    shared: Arc<Shared>,
    generation: u64,
    restoring: Arc<Generation>,
    window_id: String,
) -> Stage {
    Box::pin(async move {
        let reply = attach_restoring(
            &app,
            &shared,
            &restoring,
            &window_id,
            None,
            Some(restore_taken),
        )
        .await;
        match reply {
            StepReply::Answered(attach) => StageOutcome::Restored {
                generation,
                restored: attach.is_ok(),
            },
            StepReply::Unanswered(_) => StageOutcome::Unanswered,
        }
    })
}

fn move_stage<H: GuiHost>(app: H, shared: Arc<Shared>, window_id: String, to: LinkState) -> Stage {
    Box::pin(async move {
        if let Some(record) = shared.record() {
            let mut state = record.lock().await;
            if shared.holds(record)
                && (to != LinkState::Connecting || state.link == LinkState::Attached)
            {
                state.move_to(&app, &window_id, to);
            }
        }
        StageOutcome::Moved
    })
}

/// The way a generation's reader tells the owner how it ended: the owner
/// hears of it only if it still runs when the end is reported.
fn end_sink(owner: mpsc::WeakUnboundedSender<OwnerEvent>) -> EndSink {
    Box::new(move |news| {
        if let Some(owner) = owner.upgrade() {
            let _ = owner.send(OwnerEvent::Ended(news));
        }
    })
}

#[cfg(test)]
impl Generation {
    /// An open generation over `stream`, its reader running, with nobody
    /// owning it: nothing ends it but the stream.
    pub(crate) fn wired(
        stream: impl super::generation::GenerationStream,
    ) -> (Arc<Self>, WiredReader) {
        let (generation, reader, link) = Self::begin(0, stream, Arc::new(AtomicU64::new(0)), None);
        generation.record_revision(yagura_protocol::PROTOCOL_REVISION);
        let task = tokio::spawn(read_loop(
            reader,
            crate::host::fake::FakeHost::new("wired-generation"),
            Arc::clone(&generation),
            link,
        ));
        (generation, WiredReader(task))
    }
}

/// The reader task of a wired generation; dropping it stops the reader.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct WiredReader(tokio::task::JoinHandle<()>);

#[cfg(test)]
impl Drop for WiredReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests;
