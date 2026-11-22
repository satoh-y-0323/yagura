//! The lifetime of one client's connection, decided in one place.
//!
//! [`Lifecycle::on`] is a pure transition function: it takes the owner's
//! state and one event and returns the next state and the actions the owner
//! carries out. It does no I/O, takes no lock, reads no clock and spawns no
//! task, so every lifetime decision can be checked without a socket.
//!
//! The owner task carries those actions out, and is the only code that
//! connects, builds a [`Generation`], ends one or publishes the client's
//! state. Callers ask it for a connection and use the open generation; a
//! generation's reader alone tells it how that generation ended.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot, watch};
use yagura_os::{IpcTransport, PlatformIpc};
use yagura_protocol::{ClientMessage, StatusBoot};

use super::{
    ConnectMode, Connection, DaemonStart, IpcError, MadeStart, MadeStartReached, PendingKind,
    Reply, SentDefaultPalettes, answers_carry_correlation_ids, attach_restoring, lock_blocking,
    register_approver, restore_taken, revision_probe_outcome, revision_taken, wire,
};

use crate::constants::{
    CONNECT_RETRY_INTERVAL, CONNECT_RETRY_LIMIT, POST_SPAWN_CONNECT_RETRY_LIMIT,
    RECONNECT_ATTEMPT_LIMIT,
};
use crate::events::{DaemonConnectedPayload, EVENT_DAEMON_CONNECTED, LinkLostReason};
use crate::host::GuiHost;
use crate::message_dispatch::read_loop;
use crate::window_record::{LinkState, WindowRecord};
use reader_link::EndNews;
pub(crate) use reader_link::{Interrupt, ReaderLink};

/// Which kind of client a lifecycle belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientKind {
    /// The control connection: it registers as the approver while opening.
    Control,
    /// A window connection: a chain restores the window it was attached to.
    Window,
}

/// How far a want waits before it is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    /// Answered once an attempt's connection is accepted.
    Reached,
    /// Answered once a generation opens.
    Open,
}

/// Names one want so that its answer reaches the caller that sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WantId(pub(crate) u64);

/// The outcome of the control connection's approver registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registration {
    /// The daemon accepted the registration.
    Accepted,
    /// The daemon refused it with `auth_failed`; the generation opens anyway.
    Refused,
    /// No registration: a window connection never registers.
    Unregistered,
}

/// Why a want failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No connection could be reached or opened.
    NotConnected,
    /// The daemon could not be started.
    DaemonSpawn,
}

/// How a generation ended, as its reader saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndKind {
    /// The daemon closed the connection.
    Eof,
    /// Reading the connection failed.
    ReadError,
    /// The daemon sent a frame over the size limit.
    FrameTooLarge,
    /// A frame carrying a matching correlation id could not be read as an answer.
    UnreadableAnswer,
    /// A write failed in a way that does not say the daemon closed, and the
    /// bytes already delivered to the reader held no frame that ends it.
    WriteFailed,
    /// Another GUI took the daemon over.
    Superseded,
}

impl EndKind {
    /// Whether the end says the daemon stopped answering on the connection,
    /// as opposed to answering with something this client cannot use.
    fn is_connection_stage(self) -> bool {
        matches!(
            self,
            Self::Eof | Self::ReadError | Self::FrameTooLarge | Self::WriteFailed
        )
    }
}

/// An answer the daemon, or the revision gate, gave an opening step before
/// the generation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TakenAnswer {
    /// The revision round trip settled on this revision.
    Revision(u32),
    /// The approver registration settled with an outcome that opens.
    Registration(Registration),
    /// A window chain's attach was answered with an attach.
    Restore,
    /// The step was answered in a way that does not let the attempt go on.
    Refused,
}

/// One input to the lifecycle.
#[derive(Debug)]
pub(crate) enum Event {
    /// A caller asks for a connection.
    Want {
        id: WantId,
        mode: ConnectMode,
        until: Until,
    },
    /// The attempt numbered `attempt` brought a connection.
    Connected { attempt: u64 },
    /// The attempt numbered `attempt` could not bring a connection.
    ConnectFailed { attempt: u64, error: Failure },
    /// The opening revision round trip settled.
    Probed { generation: u64, revision: u32 },
    /// The opening approver registration settled with an outcome that opens.
    Registered {
        generation: u64,
        registration: Registration,
    },
    /// The daemon, or the revision gate, answered the step in flight in a way
    /// that does not let the attempt go on.
    StepFailed { generation: u64 },
    /// A window chain's attach was answered before the generation ended.
    Restored { generation: u64 },
    /// The generation ended. `window_id` is the ended generation's window, if
    /// it had one; the lifecycle keeps no window id of its own. `taken` is the
    /// last opening-step answer the generation took before it ended.
    Ended {
        generation: u64,
        kind: EndKind,
        window_id: Option<String>,
        taken: Option<TakenAnswer>,
    },
    /// The client is let go of; nothing opens after this.
    Retire,
}

/// One thing the owner does, in the order returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start connect attempt `attempt` of the given form.
    Attempt { attempt: u64, mode: ConnectMode },
    /// Drop the connection attempt `attempt` brought without opening it.
    Discard { attempt: u64 },
    /// Build generation `generation` on the connection attempt `attempt` brought.
    Begin { attempt: u64, generation: u64 },
    /// Run the revision round trip on the generation.
    ProbeRevision { generation: u64 },
    /// Register the control connection as the approver.
    Register { generation: u64 },
    /// Announce the daemon start for the generation.
    AnnounceStart { generation: u64 },
    /// Send the remembered default palettes to the generation.
    ResendPalettes { generation: u64 },
    /// Publish the generation as open.
    Open { generation: u64 },
    /// Attach the generation to the window again, as a chain does.
    RestoreAttach { generation: u64, window_id: String },
    /// End the generation.
    End { generation: u64 },
    /// Move the window's record to `to`. A move to `Connecting` applies only
    /// to a record that is still attached.
    MoveLink { window_id: String, to: LinkState },
    /// Answer a want. `opened` names the generation a caller reads as open
    /// once answered, so the answer waits until that generation is published.
    Answer {
        id: WantId,
        result: Result<(), Failure>,
        opened: Option<u64>,
    },
    /// Acknowledge the retirement.
    AckRetire,
}

/// What callers can tell about the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublicState {
    /// No connection: idle, or an attempt has not reached the daemon.
    Absent,
    /// A connection was accepted and its generation is opening.
    Reached,
    /// The generation is open.
    Open {
        generation: u64,
        revision: u32,
        registration: Registration,
    },
    /// The client was let go of.
    Retired,
}

/// The published state and what its readers return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Public {
    pub(crate) state: PublicState,
    /// Whether the daemon was last seen answering. A daemon that answered an
    /// opening step and still did not open stays "answering", so the recovery
    /// supervisor does not restart a live daemon; an end on the connection
    /// itself of the latest connection received lowers it, whenever that end
    /// arrives. Only the control connection is supervised, so a window's is
    /// always `false`.
    pub(crate) daemon_answers: bool,
}

impl Public {
    /// Whether a connection is reached or open, or the daemon still answers.
    /// A retired client is connected to nothing.
    pub(crate) fn is_connected(&self) -> bool {
        match self.state {
            PublicState::Reached | PublicState::Open { .. } => true,
            PublicState::Retired => false,
            PublicState::Absent => self.daemon_answers,
        }
    }

    /// Whether the open generation registered as the approver.
    pub(crate) fn is_approver_registered(&self) -> bool {
        matches!(
            self.state,
            PublicState::Open {
                registration: Registration::Accepted,
                ..
            }
        )
    }

    /// The open generation's protocol revision.
    #[cfg(test)]
    pub(crate) fn protocol_revision(&self) -> Option<u32> {
        match self.state {
            PublicState::Open { revision, .. } => Some(revision),
            _ => None,
        }
    }
}

/// The result of one event.
#[derive(Debug)]
pub(crate) struct Transition {
    pub(crate) actions: Vec<Action>,
    pub(crate) public: Public,
}

#[derive(Debug)]
struct Want {
    id: WantId,
    mode: ConnectMode,
    until: Until,
}

/// A reconnect chain started by one lost generation.
#[derive(Debug)]
struct Chain {
    attempts: u32,
    /// The lost generation's window, carried from its end event so that every
    /// attempt of the chain restores that window.
    restore: Option<String>,
}

#[derive(Debug)]
enum Phase {
    Idle,
    Connecting {
        attempt: u64,
        mode: ConnectMode,
    },
    Opening {
        mode: ConnectMode,
        generation: u64,
        revision: Option<u32>,
    },
    /// Open, while a window chain restores its attach: the chain's attempt
    /// has not finished yet.
    Restoring {
        mode: ConnectMode,
        generation: u64,
        revision: u32,
        registration: Registration,
    },
    Open {
        generation: u64,
        revision: u32,
        registration: Registration,
    },
    Retired,
}

/// The owner's decisions about one client's connection lifetime.
#[derive(Debug)]
pub(crate) struct Lifecycle {
    kind: ClientKind,
    phase: Phase,
    wants: Vec<Want>,
    chain: Option<Chain>,
    daemon_answers: bool,
    next_attempt: u64,
    next_generation: u64,
}

/// Whether an attempt of form `attempt` answers a want of form `want` when it
/// fails: the same form covers itself, and every form covers `Once`.
fn covers(attempt: ConnectMode, want: ConnectMode) -> bool {
    attempt == want || want == ConnectMode::Once
}

fn strength(mode: ConnectMode) -> u8 {
    match mode {
        ConnectMode::Once => 0,
        ConnectMode::Retry => 1,
        ConnectMode::Spawn => 2,
    }
}

impl Lifecycle {
    pub(crate) fn new(kind: ClientKind) -> Self {
        Self {
            kind,
            phase: Phase::Idle,
            wants: Vec::new(),
            chain: None,
            daemon_answers: false,
            next_attempt: 0,
            next_generation: 0,
        }
    }

    /// Applies one event and returns the actions to carry out, in order.
    pub(crate) fn on(&mut self, event: Event) -> Transition {
        let mut out = Vec::new();
        match event {
            Event::Want { id, mode, until } => self.on_want(Want { id, mode, until }, &mut out),
            Event::Connected { attempt } => self.on_connected(attempt, &mut out),
            Event::ConnectFailed { attempt, error } => {
                if let Phase::Connecting {
                    attempt: current,
                    mode,
                } = self.phase
                    && current == attempt
                {
                    self.daemon_answers = false;
                    self.attempt_failed(mode, error, &mut out);
                }
            }
            Event::Probed {
                generation,
                revision,
            } => self.on_probed(generation, revision, &mut out),
            Event::Registered {
                generation,
                registration,
            } => self.on_registered(generation, registration, &mut out),
            Event::StepFailed { generation } => self.on_step_failed(generation, &mut out),
            Event::Restored { generation } => self.on_restored(generation),
            Event::Ended {
                generation,
                kind,
                window_id,
                taken,
            } => self.on_ended(generation, kind, window_id, taken, &mut out),
            Event::Retire => {
                if let Some(generation) = self.current_generation() {
                    out.push(Action::End { generation });
                }
                self.answer_where(Err(Failure::NotConnected), None, |_| true, &mut out);
                self.chain = None;
                self.phase = Phase::Retired;
                out.push(Action::AckRetire);
            }
        }
        Transition {
            actions: out,
            public: self.public(),
        }
    }

    fn public(&self) -> Public {
        let state = match self.phase {
            Phase::Idle | Phase::Connecting { .. } => PublicState::Absent,
            Phase::Opening { .. } => PublicState::Reached,
            Phase::Restoring {
                generation,
                revision,
                registration,
                ..
            }
            | Phase::Open {
                generation,
                revision,
                registration,
            } => PublicState::Open {
                generation,
                revision,
                registration,
            },
            Phase::Retired => PublicState::Retired,
        };
        Public {
            state,
            daemon_answers: self.kind == ClientKind::Control && self.daemon_answers,
        }
    }

    fn current_generation(&self) -> Option<u64> {
        match self.phase {
            Phase::Opening { generation, .. }
            | Phase::Restoring { generation, .. }
            | Phase::Open { generation, .. } => Some(generation),
            Phase::Idle | Phase::Connecting { .. } | Phase::Retired => None,
        }
    }

    /// The form of the attempt that built `generation`, while that attempt has
    /// not finished yet.
    fn unfinished_attempt(&self, generation: u64) -> Option<ConnectMode> {
        match self.phase {
            Phase::Opening {
                mode,
                generation: current,
                ..
            }
            | Phase::Restoring {
                mode,
                generation: current,
                ..
            } if current == generation => Some(mode),
            _ => None,
        }
    }

    fn on_want(&mut self, want: Want, out: &mut Vec<Action>) {
        match self.phase {
            Phase::Retired => out.push(Action::Answer {
                id: want.id,
                result: Err(Failure::NotConnected),
                opened: None,
            }),
            Phase::Open { generation, .. } | Phase::Restoring { generation, .. } => {
                out.push(Action::Answer {
                    id: want.id,
                    result: Ok(()),
                    opened: Some(generation),
                });
            }
            Phase::Opening { .. } if want.until == Until::Reached => out.push(Action::Answer {
                id: want.id,
                result: Ok(()),
                opened: None,
            }),
            Phase::Opening { .. } | Phase::Connecting { .. } => self.wants.push(want),
            Phase::Idle => {
                let mode = want.mode;
                self.wants.push(want);
                self.start(mode, out);
            }
        }
    }

    fn on_connected(&mut self, attempt: u64, out: &mut Vec<Action>) {
        let Phase::Connecting {
            attempt: current,
            mode,
        } = self.phase
        else {
            out.push(Action::Discard { attempt });
            return;
        };
        if current != attempt {
            out.push(Action::Discard { attempt });
            return;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.daemon_answers = true;
        out.push(Action::Begin {
            attempt,
            generation,
        });
        self.answer_where(Ok(()), None, |want| want.until == Until::Reached, out);
        out.push(Action::ProbeRevision { generation });
        self.phase = Phase::Opening {
            mode,
            generation,
            revision: None,
        };
    }

    fn on_probed(&mut self, generation: u64, revision: u32, out: &mut Vec<Action>) {
        let Phase::Opening {
            mode,
            generation: current,
            revision: None,
        } = self.phase
        else {
            return;
        };
        if current != generation {
            return;
        }
        match self.kind {
            ClientKind::Control => {
                self.phase = Phase::Opening {
                    mode,
                    generation,
                    revision: Some(revision),
                };
                out.push(Action::Register { generation });
            }
            ClientKind::Window => {
                self.finish_opening(mode, generation, revision, Registration::Unregistered, out);
            }
        }
    }

    fn on_registered(
        &mut self,
        generation: u64,
        registration: Registration,
        out: &mut Vec<Action>,
    ) {
        if let Phase::Opening {
            mode,
            generation: current,
            revision: Some(revision),
        } = self.phase
            && current == generation
            && self.kind == ClientKind::Control
        {
            self.finish_opening(mode, generation, revision, registration, out);
        }
    }

    fn on_step_failed(&mut self, generation: u64, out: &mut Vec<Action>) {
        if let Some(mode) = self.unfinished_attempt(generation) {
            self.failed_to_open(mode, generation, out);
        }
    }

    fn on_restored(&mut self, generation: u64) {
        if let Phase::Restoring {
            generation: current,
            revision,
            registration,
            ..
        } = self.phase
            && current == generation
        {
            self.chain = None;
            self.phase = Phase::Open {
                generation,
                revision,
                registration,
            };
        }
    }

    /// Applies an answer the generation took before it ended, as its step's
    /// own result is applied.
    fn take(&mut self, generation: u64, taken: TakenAnswer, out: &mut Vec<Action>) {
        match taken {
            TakenAnswer::Revision(revision) => self.on_probed(generation, revision, out),
            TakenAnswer::Registration(registration) => {
                self.on_registered(generation, registration, out);
            }
            TakenAnswer::Restore => self.on_restored(generation),
            TakenAnswer::Refused => self.on_step_failed(generation, out),
        }
    }

    fn finish_opening(
        &mut self,
        mode: ConnectMode,
        generation: u64,
        revision: u32,
        registration: Registration,
        out: &mut Vec<Action>,
    ) {
        out.push(Action::AnnounceStart { generation });
        out.push(Action::ResendPalettes { generation });
        out.push(Action::Open { generation });
        self.answer_where(Ok(()), Some(generation), |_| true, out);
        let restore = self.chain.as_ref().and_then(|chain| chain.restore.clone());
        match restore {
            Some(window_id) => {
                out.push(Action::RestoreAttach {
                    generation,
                    window_id,
                });
                self.phase = Phase::Restoring {
                    mode,
                    generation,
                    revision,
                    registration,
                };
            }
            None => {
                self.chain = None;
                self.phase = Phase::Open {
                    generation,
                    revision,
                    registration,
                };
            }
        }
    }

    fn on_ended(
        &mut self,
        generation: u64,
        kind: EndKind,
        window_id: Option<String>,
        taken: Option<TakenAnswer>,
        out: &mut Vec<Action>,
    ) {
        // Whether this end or a result of the generation arrives first, the
        // latest connection's end is what was last seen of the daemon.
        if kind.is_connection_stage() && generation + 1 == self.next_generation {
            self.daemon_answers = false;
        }
        if self.current_generation() != Some(generation) {
            return;
        }
        if let Some(taken) = taken {
            self.take(generation, taken, out);
            if self.current_generation() != Some(generation) {
                return;
            }
        }
        let unfinished = self.unfinished_attempt(generation);

        if kind == EndKind::Superseded {
            out.push(Action::End { generation });
            if let Some(window_id) = window_id {
                out.push(Action::MoveLink {
                    window_id,
                    to: LinkState::Superseded,
                });
            }
            // A superseded session must not fight the new GUI for the daemon.
            self.chain = None;
            self.phase = Phase::Idle;
            if let Some(mode) = unfinished {
                self.attempt_failed(mode, Failure::NotConnected, out);
            }
            return;
        }

        // The end's kind follows whether the generation's attempt finished,
        // never who reported it.
        if let Some(mode) = unfinished {
            self.failed_to_open(mode, generation, out);
            return;
        }

        out.push(Action::End { generation });
        self.phase = Phase::Idle;
        let restore = match (self.kind, window_id) {
            (ClientKind::Control, _) => None,
            (ClientKind::Window, Some(window_id)) => Some(window_id),
            // A window generation that holds no attach has nothing to restore.
            (ClientKind::Window, None) => return,
        };
        if let Some(window_id) = &restore {
            out.push(Action::MoveLink {
                window_id: window_id.clone(),
                to: LinkState::Connecting,
            });
        }
        self.chain = Some(Chain {
            attempts: 1,
            restore,
        });
        let mode = self.chain_mode();
        self.start(mode, out);
    }

    fn failed_to_open(&mut self, mode: ConnectMode, generation: u64, out: &mut Vec<Action>) {
        out.push(Action::End { generation });
        self.attempt_failed(mode, Failure::NotConnected, out);
    }

    /// Answers the wants the failed attempt covers and starts the next attempt:
    /// the chain's next one while it has budget, then one for the strongest
    /// want the failed attempt did not cover.
    fn attempt_failed(&mut self, mode: ConnectMode, error: Failure, out: &mut Vec<Action>) {
        self.answer_where(Err(error), None, |want| covers(mode, want.mode), out);
        self.phase = Phase::Idle;
        if let Some(chain) = self.chain.take() {
            if chain.attempts < RECONNECT_ATTEMPT_LIMIT {
                self.chain = Some(Chain {
                    attempts: chain.attempts + 1,
                    restore: chain.restore,
                });
                let mode = self.chain_mode();
                self.start(mode, out);
                return;
            }
            if let Some(window_id) = chain.restore {
                out.push(Action::MoveLink {
                    window_id,
                    to: LinkState::Lost(LinkLostReason::ReconnectFailed),
                });
            }
        }
        if let Some(mode) = self.strongest_want() {
            self.start(mode, out);
        }
    }

    /// A chain retries, and starts the daemon when a waiting want asks for it.
    fn chain_mode(&self) -> ConnectMode {
        match self.strongest_want() {
            Some(mode) if strength(mode) > strength(ConnectMode::Retry) => mode,
            _ => ConnectMode::Retry,
        }
    }

    fn strongest_want(&self) -> Option<ConnectMode> {
        self.wants
            .iter()
            .map(|want| want.mode)
            .max_by_key(|mode| strength(*mode))
    }

    fn start(&mut self, mode: ConnectMode, out: &mut Vec<Action>) {
        let attempt = self.next_attempt;
        self.next_attempt += 1;
        self.phase = Phase::Connecting { attempt, mode };
        out.push(Action::Attempt { attempt, mode });
    }

    fn answer_where(
        &mut self,
        result: Result<(), Failure>,
        opened: Option<u64>,
        answered: impl Fn(&Want) -> bool,
        out: &mut Vec<Action>,
    ) {
        let (now, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.wants)
            .into_iter()
            .partition(|want| answered(want));
        self.wants = kept;
        out.extend(now.into_iter().map(|want| Action::Answer {
            id: want.id,
            result,
            opened,
        }));
    }
}

/// The read half a generation's reader takes.
pub(crate) trait GenerationRead: AsyncRead + Send + Unpin {
    /// Appends to `into`, without waiting, the bytes already delivered to
    /// this end that were not read yet, and returns how many.
    ///
    /// # Errors
    /// The error reading them met; the bytes read before it stay appended.
    fn read_delivered(&mut self, into: &mut Vec<u8>) -> std::io::Result<usize>;
}

impl GenerationRead for yagura_os::ClientReadHalf {
    fn read_delivered(&mut self, into: &mut Vec<u8>) -> std::io::Result<usize> {
        yagura_os::ClientReadHalf::read_delivered(self, into)
    }
}

/// The read half a generation's reader takes.
pub(crate) type BoxedRead = Box<dyn GenerationRead>;

/// The write half a generation writes through.
type BoxedWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// A connection a generation is built on.
pub(crate) trait GenerationStream: Send + 'static {
    /// The half the generation's reader takes and the half it writes through.
    fn into_halves(self) -> (BoxedRead, BoxedWrite);
}

/// A connection to the daemon, as a generation is built on it.
struct DaemonConnection(Connection);

impl GenerationStream for DaemonConnection {
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = yagura_os::split_client(self.0);
        (Box::new(read), Box::new(write))
    }
}

#[cfg(test)]
impl GenerationStream for tokio::io::DuplexStream {
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = tokio::io::split(self);
        (Box::new(Polled(read)), Box::new(write))
    }
}

#[cfg(test)]
impl<R, W> GenerationStream for tokio::io::Join<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = self.into_inner();
        (Box::new(Polled(read)), Box::new(write))
    }
}

/// An in-memory read half: what one poll does not return was not delivered.
#[cfg(test)]
struct Polled<R>(R);

#[cfg(test)]
impl<R: AsyncRead + Unpin> AsyncRead for Polled<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

#[cfg(test)]
impl<R: AsyncRead + Send + Unpin> GenerationRead for Polled<R> {
    fn read_delivered(&mut self, into: &mut Vec<u8>) -> std::io::Result<usize> {
        use std::task::{Context, Poll, Waker};
        let mut chunk = [0u8; 4096];
        let mut added = 0;
        loop {
            let mut buf = tokio::io::ReadBuf::new(&mut chunk);
            // Outside the task's budget, so a spent budget hides no byte.
            let polled = {
                let mut read =
                    std::pin::pin!(tokio::task::unconstrained(std::future::poll_fn(|cx| {
                        Pin::new(&mut self.0).poll_read(cx, &mut buf)
                    })));
                read.as_mut().poll(&mut Context::from_waker(Waker::noop()))
            };
            match polled {
                Poll::Ready(Ok(())) if !buf.filled().is_empty() => {
                    into.extend_from_slice(buf.filled());
                    added += buf.filled().len();
                }
                Poll::Ready(Ok(())) | Poll::Pending => return Ok(added),
                Poll::Ready(Err(error)) => return Err(error),
            }
        }
    }
}

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

/// How an opening step reads an answer it was given, as the answer its
/// generation took. The same reading decides the step's own result.
pub(crate) type Take = fn(&Result<Reply, IpcError>) -> TakenAnswer;

/// What a request on a generation brought back.
#[derive(Debug)]
pub(crate) enum StepReply<T> {
    /// What the daemon, or the revision gate, answered.
    Answered(T),
    /// No answer: the connection failed under the request or the generation
    /// ended, and the generation's end tells how.
    Unanswered(IpcError),
}

impl<T> StepReply<T> {
    pub(crate) fn map<U>(self, read: impl FnOnce(T) -> U) -> StepReply<U> {
        match self {
            Self::Answered(answer) => StepReply::Answered(read(answer)),
            Self::Unanswered(error) => StepReply::Unanswered(error),
        }
    }
}

impl<T> StepReply<Result<T, IpcError>> {
    /// The answer, or the error no answer came with.
    pub(crate) fn into_result(self) -> Result<T, IpcError> {
        match self {
            Self::Answered(answer) => answer,
            Self::Unanswered(error) => Err(error),
        }
    }
}

/// The request waiting on a generation.
#[derive(Debug)]
struct Pending {
    kind: PendingKind,
    correlation_id: u64,
    sender: oneshot::Sender<Result<Reply, IpcError>>,
    /// Set on an opening step.
    take: Option<Take>,
}

/// What a generation's end takes in one step.
#[derive(Debug, Default)]
struct Slots {
    ended: bool,
    pending: Option<Pending>,
    /// The last answer an opening step took.
    taken: Option<TakenAnswer>,
    cancel_reader: Option<oneshot::Sender<()>>,
    stop_reader: Option<oneshot::Sender<()>>,
}

impl Slots {
    /// Records a refusal an opening step met before its request was written,
    /// as the step's answer. Called while the generation has not ended.
    fn record_refusal(&mut self, take: Option<Take>) {
        if take.is_some() {
            self.taken = Some(TakenAnswer::Refused);
        }
    }
}

/// The request a generation is waiting on, as its reader weighs a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Awaiting {
    pub(crate) kind: PendingKind,
    pub(crate) correlation_id: u64,
}

/// What became of an answer handed to [`Generation::settle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settled {
    /// It answered the request waiting.
    Answered,
    /// Nothing waiting is what it answers; it was dropped.
    Unanswered,
    /// It came under the waiting request's key but is of another kind: the
    /// daemon broke its word, so the generation must end.
    Broken,
}

/// One connection from the moment it was accepted until it ends: its write
/// half, the request waiting on it, and what opening it settled.
pub(crate) struct Generation {
    number: u64,
    writer: tokio::sync::Mutex<Option<BoxedWrite>>,
    slots: std::sync::Mutex<Slots>,
    /// One request waits for its answer on a connection at a time. Every
    /// request this client waits on is answered while the daemon handles its
    /// frame, so a second slot would be answered no sooner, and a single
    /// slot keeps the answers of a daemon that sends no correlation id
    /// attributable by their kind.
    request_lock: tokio::sync::Mutex<()>,
    revision: OnceLock<u32>,
    /// The window the reader routes frames to. An attach writes it before
    /// its request goes out, as the daemon may stream before this side reads
    /// the answer.
    window_id: std::sync::Mutex<Option<String>>,
    correlation_ids: Arc<AtomicU64>,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation")
            .field("number", &self.number)
            .field("revision", &self.revision.get())
            .finish_non_exhaustive()
    }
}

impl Generation {
    /// Builds generation `number` on `connection`. Its reader reports the end
    /// it sees to `owner`; nothing else does.
    fn begin(
        number: u64,
        connection: impl GenerationStream,
        correlation_ids: Arc<AtomicU64>,
        owner: Option<mpsc::WeakUnboundedSender<OwnerEvent>>,
    ) -> (Arc<Self>, BoxedRead, ReaderLink) {
        let (read, write) = connection.into_halves();
        let (cancel, cancelled) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let generation = Arc::new(Self {
            number,
            writer: tokio::sync::Mutex::new(Some(write)),
            slots: std::sync::Mutex::new(Slots {
                cancel_reader: Some(cancel),
                stop_reader: Some(stop),
                ..Slots::default()
            }),
            request_lock: tokio::sync::Mutex::new(()),
            revision: OnceLock::new(),
            window_id: std::sync::Mutex::new(None),
            correlation_ids,
        });
        let link = ReaderLink::new(number, cancelled, stopped, owner);
        (generation, read, link)
    }

    /// An open generation over `stream`, its reader running, with nobody
    /// owning it: nothing ends it but the stream.
    #[cfg(test)]
    pub(crate) fn wired(stream: impl GenerationStream) -> (Arc<Self>, WiredReader) {
        let (generation, reader, link) = Self::begin(0, stream, Arc::new(AtomicU64::new(0)), None);
        let _ = generation.revision.set(yagura_protocol::PROTOCOL_REVISION);
        let task = tokio::spawn(read_loop(
            reader,
            crate::host::fake::FakeHost::new("wired-generation"),
            Arc::clone(&generation),
            link,
        ));
        (generation, WiredReader(task))
    }

    /// The generation's number, unique within its client.
    pub(crate) fn number(&self) -> u64 {
        self.number
    }

    /// The wire revision opening settled, `None` while it is being probed.
    pub(crate) fn revision(&self) -> Option<u32> {
        self.revision.get().copied()
    }

    /// The window frames on this generation are routed to.
    pub(crate) fn window_id(&self) -> Option<String> {
        lock_blocking(&self.window_id).clone()
    }

    pub(crate) fn set_window_id(&self, window_id: Option<String>) {
        *lock_blocking(&self.window_id) = window_id;
    }

    /// Writes `message` under a fresh correlation id and waits for its answer.
    ///
    /// # Errors
    /// [`IpcError::NotConnected`] when the generation ended before the frame
    /// was written, [`IpcError::Unsupported`] when its revision cannot carry
    /// the frame, [`IpcError::Write`] when the write failed and
    /// [`IpcError::ConnectionLost`] when the generation ended after the frame
    /// was written; otherwise what the daemon answered.
    pub(crate) async fn request(
        &self,
        message: &ClientMessage,
        kind: PendingKind,
    ) -> Result<Reply, IpcError> {
        self.request_step(message, kind, None).await.into_result()
    }

    /// [`Generation::request`], telling an answer from none: the failures of
    /// the connection and of the generation's end come back as
    /// [`StepReply::Unanswered`]. With `take` the request is an opening step:
    /// the answer it is given, the revision gate's refusal included, is
    /// recorded as the step's answer in the same step that hands it over, so
    /// the generation's end carries it.
    pub(crate) async fn request_step(
        &self,
        message: &ClientMessage,
        kind: PendingKind,
        take: Option<Take>,
    ) -> StepReply<Result<Reply, IpcError>> {
        let _in_flight = self.request_lock.lock().await;
        let correlation_id = self.correlation_ids.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        let revision = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return StepReply::Unanswered(IpcError::NotConnected);
            }
            let revision = self.revision();
            if let Err(refusal) = wire::gate(message, revision) {
                slots.record_refusal(take);
                return StepReply::Answered(Err(refusal.into()));
            }
            slots.pending = Some(Pending {
                kind,
                correlation_id,
                sender,
                take,
            });
            revision
        };
        // The gate passed for this revision above, so a failure here is the
        // connection's.
        if let Err(error) = self
            .write_frame(message, Some(correlation_id), revision)
            .await
        {
            self.forget(correlation_id);
            return StepReply::Unanswered(error);
        }
        // No deadline: a daemon that has the frame may still carry it out, so
        // only its answer or the generation's end can settle the request.
        match receiver.await {
            Ok(result) => StepReply::Answered(result),
            // The end took the slot after the frame was written: the daemon
            // may have acted on it.
            Err(_) => StepReply::Unanswered(IpcError::ConnectionLost),
        }
    }

    /// A refusal a request met before it was written. On an opening step it
    /// is the step's answer only while the generation has not ended, and is
    /// recorded as such in the same step; after the end it is no answer.
    pub(crate) fn refuse(&self, take: Option<Take>, refusal: IpcError) -> StepReply<IpcError> {
        if take.is_some() {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return StepReply::Unanswered(IpcError::NotConnected);
            }
            slots.record_refusal(take);
        }
        StepReply::Answered(refusal)
    }

    /// Writes `message` with no correlation id: nothing waits for an answer.
    ///
    /// # Errors
    /// [`IpcError::NotConnected`] when the generation ended,
    /// [`IpcError::Unsupported`] when its revision cannot carry the frame,
    /// and [`IpcError::Write`] when the write failed.
    pub(crate) async fn write(&self, message: &ClientMessage) -> Result<(), IpcError> {
        self.write_frame(message, None, self.revision()).await
    }

    async fn write_frame(
        &self,
        message: &ClientMessage,
        correlation_id: Option<u64>,
        revision: Option<u32>,
    ) -> Result<(), IpcError> {
        let mut writer = self.writer.lock().await;
        if lock_blocking(&self.slots).ended {
            *writer = None;
        }
        let Some(half) = writer.as_mut() else {
            return Err(IpcError::NotConnected);
        };
        match wire::write_frame(half, message, correlation_id, revision).await {
            Err(wire::WriteError::Io(error)) => {
                // Taken away: a frame may be cut short, so nothing more is
                // written on this connection.
                *writer = None;
                // A peer that closed shows its reader every byte it sent and
                // then the end, so the reader tells how the connection ended.
                // Any other failure would leave a live peer's reader waiting.
                if !yagura_os::write_error_shows_peer_closed(&error) {
                    self.stop_reader();
                }
                Err(wire::WriteError::Io(error).into())
            }
            outcome => {
                // An end that came while this write held the half lets it go now.
                if lock_blocking(&self.slots).ended {
                    *writer = None;
                }
                outcome.map_err(IpcError::from)
            }
        }
    }

    /// Tells the reader to stop, once, while the generation has not ended.
    fn stop_reader(&self) {
        let stop = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                None
            } else {
                slots.stop_reader.take()
            }
        };
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
    }

    /// Hands `result` to the request waiting, when the answer is its own:
    /// under its correlation id once the daemon returns them, and by `kind`
    /// alone while opening and with an older daemon.
    pub(crate) fn settle(
        &self,
        correlation_id: Option<u64>,
        kind: PendingKind,
        result: Result<Reply, IpcError>,
    ) -> Settled {
        let mut slots = lock_blocking(&self.slots);
        let Some(pending) = slots.pending.as_ref() else {
            return Settled::Unanswered;
        };
        let answers = match correlation_id {
            // An error answers whatever request carried its key.
            Some(key) if key == pending.correlation_id => {
                if result.is_ok() && kind != pending.kind {
                    return Settled::Broken;
                }
                true
            }
            Some(_) => false,
            None => !answers_carry_correlation_ids(self.revision()) && kind == pending.kind,
        };
        if !answers {
            return Settled::Unanswered;
        }
        if let Some(pending) = slots.pending.take() {
            if let Some(take) = pending.take {
                slots.taken = Some(take(&result));
            }
            let _ = pending.sender.send(result);
        }
        Settled::Answered
    }

    /// The request waiting for an answer, if any.
    pub(crate) fn awaiting(&self) -> Option<Awaiting> {
        lock_blocking(&self.slots)
            .pending
            .as_ref()
            .map(|pending| Awaiting {
                kind: pending.kind,
                correlation_id: pending.correlation_id,
            })
    }

    /// Clears the slot of the request under `correlation_id`, which gave up.
    fn forget(&self, correlation_id: u64) {
        let mut slots = lock_blocking(&self.slots);
        if slots
            .pending
            .as_ref()
            .is_some_and(|pending| pending.correlation_id == correlation_id)
        {
            slots.pending = None;
        }
    }

    /// Ends the generation, in one step with no await: the request waiting
    /// hears of it, the reader stops and the write half is let go of, by a
    /// writer midway as soon as its write is done. Returns the last answer an
    /// opening step took before; later calls do nothing and return `None`.
    fn end(&self) -> Option<TakenAnswer> {
        let (pending, cancel, taken) = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return None;
            }
            slots.ended = true;
            (
                slots.pending.take(),
                slots.cancel_reader.take(),
                slots.taken,
            )
        };
        drop(pending);
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
        if let Ok(mut writer) = self.writer.try_lock() {
            *writer = None;
        }
        taken
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

/// The one way news of a generation's end reaches its owner: only a reader's
/// link, one per generation, can build it.
mod reader_link {
    use tokio::sync::{mpsc, oneshot};

    use super::{EndKind, OwnerEvent};

    /// A generation's end, as its reader saw it.
    #[derive(Debug)]
    pub(super) struct EndNews {
        generation: u64,
        kind: EndKind,
    }

    impl EndNews {
        pub(super) fn generation(&self) -> u64 {
            self.generation
        }

        pub(super) fn kind(&self) -> EndKind {
            self.kind
        }
    }

    /// What stops a reader short of its connection's end.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Interrupt {
        /// The owner ended the generation: there is nothing to tell it.
        Cancelled,
        /// A write failed in a way that does not say the peer closed.
        Stopped,
    }

    /// What ties a reader to its generation's owner: what stops it, and the
    /// way it reports the end it saw.
    #[derive(Debug)]
    pub(crate) struct ReaderLink {
        generation: u64,
        cancelled: oneshot::Receiver<()>,
        stopped: Option<oneshot::Receiver<()>>,
        owner: Option<mpsc::WeakUnboundedSender<OwnerEvent>>,
    }

    impl ReaderLink {
        pub(super) fn new(
            generation: u64,
            cancelled: oneshot::Receiver<()>,
            stopped: oneshot::Receiver<()>,
            owner: Option<mpsc::WeakUnboundedSender<OwnerEvent>>,
        ) -> Self {
            Self {
                generation,
                cancelled,
                stopped: Some(stopped),
                owner,
            }
        }

        /// Resolves once the reader is to stop, the cancellation first. A
        /// stop whose sender is gone is never told, so the reader reads on;
        /// only the cancellation stops it without telling the owner. Safe to
        /// drop and call again until it resolved.
        pub(crate) async fn interrupted(&mut self) -> Interrupt {
            let cancelled = &mut self.cancelled;
            let stopped = &mut self.stopped;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut *cancelled => return Interrupt::Cancelled,
                    told = told(stopped) => {
                        *stopped = None;
                        if told {
                            return Interrupt::Stopped;
                        }
                    }
                }
            }
        }

        /// Tells the owner the generation ended as `kind`, unless the owner
        /// ended it first.
        pub(crate) fn report(mut self, kind: EndKind) {
            if !matches!(
                self.cancelled.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ) {
                return;
            }
            if let Some(owner) = self
                .owner
                .as_ref()
                .and_then(mpsc::WeakUnboundedSender::upgrade)
            {
                let _ = owner.send(OwnerEvent::Ended(EndNews {
                    generation: self.generation,
                    kind,
                }));
            }
        }
    }

    /// Whether the stop was told; `false` once its sender is gone.
    async fn told(stopped: &mut Option<oneshot::Receiver<()>>) -> bool {
        match stopped {
            Some(receiver) => receiver.await.is_ok(),
            None => std::future::pending().await,
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
        let correlation_ids = Arc::clone(&generation.correlation_ids);
        let public = Public {
            state: PublicState::Open {
                generation: generation.number,
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

/// One of the owner's steps, in the order the lifecycle returned them.
enum Step {
    Do(Action),
    /// `opened_by` names the generation whose opening the answer waits for.
    Answer {
        id: WantId,
        result: Result<(), IpcError>,
        opened_by: Option<u64>,
    },
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
    queue: VecDeque<Step>,
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
                    let _ = probed.revision.set(revision);
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
        for action in transition.actions {
            let step = match action {
                Action::Answer { id, result, opened } => Step::Answer {
                    id,
                    opened_by: opened,
                    result: result
                        .map_err(|failure| failure_error(failure, attempt_error.as_ref())),
                },
                other => Step::Do(other),
            };
            if urgent && self.runs_at_once(&step) {
                let _ = self.execute(step);
            } else {
                self.queue.push_back(step);
            }
        }
    }

    fn runs_at_once(&self, step: &Step) -> bool {
        match step {
            Step::Do(Action::End { .. } | Action::AckRetire | Action::Discard { .. }) => true,
            Step::Do(_) => false,
            Step::Answer { result: Err(_), .. }
            | Step::Answer {
                opened_by: None, ..
            } => true,
            Step::Answer {
                opened_by: Some(generation),
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

    fn execute(&mut self, step: Step) -> Option<Stage> {
        match step {
            Step::Answer {
                id,
                result,
                opened_by,
            } => {
                self.answer(id, result, opened_by);
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
            // `feed` lines every answer up as a `Step::Answer`, with its error.
            Action::Answer { .. } => None,
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
            Some(self.weak.clone()),
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
        if !self.generations.contains_key(&generation) {
            return;
        }
        let boot = self.boots.get(&generation).copied().flatten();
        let announcement = {
            let mut start = lock_blocking(&self.shared.daemon_start);
            let (next, announcement) = start.first_settled(boot, self.shared.kind);
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

async fn connect_once(endpoint: &str) -> Option<Connection> {
    PlatformIpc::default().connect(endpoint).await.ok()
}

/// Connects to a running daemon, retrying a refused endpoint up to `limit`
/// times. The first attempt precedes any wait, so an endpoint that is free
/// costs no delay (waiting first would cost one interval per window).
async fn connect_retrying_with_limit(endpoint: &str, limit: u32) -> Option<Connection> {
    for attempt in 0..limit {
        if attempt > 0 {
            tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
        }
        if let Some(connection) = connect_once(endpoint).await {
            return Some(connection);
        }
    }
    None
}

/// [`connect_retrying_with_limit`] with the ordinary [`CONNECT_RETRY_LIMIT`]
/// budget.
async fn connect_retrying(endpoint: &str) -> Option<Connection> {
    connect_retrying_with_limit(endpoint, CONNECT_RETRY_LIMIT).await
}

/// Connects to a daemon that was just spawned. Unlike [`connect_retrying`]
/// this waits before the first attempt, because the daemon still has to bind
/// its endpoint -- reading the login shell's environment first on Unix, so
/// the budget ([`POST_SPAWN_CONNECT_RETRY_LIMIT`]) is longer than the ordinary one.
async fn connect_after_spawn(endpoint: &str) -> Result<Connection, IpcError> {
    tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
    connect_retrying_with_limit(endpoint, POST_SPAWN_CONNECT_RETRY_LIMIT)
        .await
        .ok_or(IpcError::ConnectTimeout)
}

#[cfg(test)]
mod tests {
    //! The transition function alone: no clock, no socket, no task. Every
    //! result is read from what `Lifecycle::on` returns.

    use super::*;
    use crate::constants::RECONNECT_ATTEMPT_LIMIT;
    use crate::events::LinkLostReason;
    use crate::window_record::LinkState;

    /// A daemon of the current wire revision.
    const REVISION: u32 = yagura_protocol::PROTOCOL_REVISION;
    const WINDOW: &str = "w-1";
    const OTHER_WINDOW: &str = "w-2";

    /// Every end a reader reports that is not a takeover.
    const LOST: [EndKind; 5] = [
        EndKind::Eof,
        EndKind::ReadError,
        EndKind::FrameTooLarge,
        EndKind::UnreadableAnswer,
        EndKind::WriteFailed,
    ];

    /// The ends that happen on the connection itself, not in an answer.
    const CONNECTION_STAGE: [EndKind; 4] = [
        EndKind::Eof,
        EndKind::ReadError,
        EndKind::FrameTooLarge,
        EndKind::WriteFailed,
    ];

    struct Run {
        lifecycle: Lifecycle,
        control: bool,
        public: Option<Public>,
        wants: u64,
    }

    impl Run {
        fn control() -> Self {
            Self::new(ClientKind::Control, true)
        }

        fn window() -> Self {
            Self::new(ClientKind::Window, false)
        }

        fn new(kind: ClientKind, control: bool) -> Self {
            Self {
                lifecycle: Lifecycle::new(kind),
                control,
                public: None,
                wants: 0,
            }
        }

        fn feed(&mut self, event: Event) -> Vec<Action> {
            let transition = self.lifecycle.on(event);
            self.public = Some(transition.public);
            transition.actions
        }

        fn public(&self) -> &Public {
            self.public.as_ref().expect("an event was fed")
        }

        fn want(&mut self, mode: ConnectMode, until: Until) -> (WantId, Vec<Action>) {
            self.wants += 1;
            let id = WantId(self.wants);
            (id, self.feed(Event::Want { id, mode, until }))
        }

        /// Starts an attempt for a want of `mode` and returns the attempt.
        fn start(&mut self, mode: ConnectMode, until: Until) -> (WantId, u64) {
            let (id, actions) = self.want(mode, until);
            let (attempt, _) =
                attempt_in(&actions).expect("a want on an idle lifecycle starts an attempt");
            (id, attempt)
        }

        /// Hands over the connection `attempt` brought; returns the generation.
        fn reach(&mut self, attempt: u64) -> (u64, Vec<Action>) {
            let actions = self.feed(Event::Connected { attempt });
            let generation = begun(&actions).expect("the attempt's connection begins a generation");
            (generation, actions)
        }

        /// Answers every opening step of `generation` so that it opens.
        fn settle(&mut self, generation: u64, registration: Registration) -> Vec<Action> {
            let mut actions = self.feed(Event::Probed {
                generation,
                revision: REVISION,
            });
            if self.control {
                actions.extend(self.feed(Event::Registered {
                    generation,
                    registration,
                }));
            }
            actions
        }

        /// Opens a generation for a want of `mode`; returns its number.
        fn open(&mut self, mode: ConnectMode) -> u64 {
            let (_, attempt) = self.start(mode, Until::Open);
            let (generation, _) = self.reach(attempt);
            let actions = self.settle(generation, Registration::Accepted);
            assert!(
                actions.contains(&Action::Open { generation }),
                "{actions:?}"
            );
            generation
        }

        /// Loses `generation` on its reader; returns the chain's first attempt.
        fn lose(&mut self, generation: u64, window_id: Option<&str>) -> (u64, ConnectMode) {
            let actions = self.feed(ended(generation, EndKind::Eof, window_id));
            attempt_in(&actions).expect("a lost generation starts the chain")
        }

        /// Runs a chain attempt until it finished: opened, and on a window
        /// with its attach restored too.
        fn finish_chain_attempt(&mut self, attempt: u64) -> u64 {
            let (generation, _) = self.reach(attempt);
            self.settle(generation, Registration::Accepted);
            if !self.control {
                self.feed(Event::Restored { generation });
            }
            generation
        }

        /// Fails each chain attempt with `fail` until no next one is started.
        /// Returns how many attempts ran and the last transition's actions.
        fn exhaust(
            &mut self,
            first: Vec<Action>,
            mut fail: impl FnMut(&mut Run, u64) -> Vec<Action>,
        ) -> (u32, Vec<Action>) {
            let mut tried = 0;
            let mut last = first;
            while let Some((attempt, mode)) = attempt_in(&last) {
                tried += 1;
                assert!(
                    tried <= RECONNECT_ATTEMPT_LIMIT,
                    "the chain went past its limit: {last:?}"
                );
                assert_eq!(mode, ConnectMode::Retry);
                last = fail(self, attempt);
            }
            (tried, last)
        }
    }

    fn ended(generation: u64, kind: EndKind, window_id: Option<&str>) -> Event {
        ended_taking(generation, kind, window_id, None)
    }

    /// An end that carries the last opening-step answer its generation took
    /// before it ended.
    fn ended_taking(
        generation: u64,
        kind: EndKind,
        window_id: Option<&str>,
        taken: Option<TakenAnswer>,
    ) -> Event {
        Event::Ended {
            generation,
            kind,
            window_id: window_id.map(str::to_owned),
            taken,
        }
    }

    fn attempts(actions: &[Action]) -> Vec<(u64, ConnectMode)> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Attempt { attempt, mode } => Some((*attempt, *mode)),
                _ => None,
            })
            .collect()
    }

    fn attempt_in(actions: &[Action]) -> Option<(u64, ConnectMode)> {
        let all = attempts(actions);
        assert!(all.len() <= 1, "one attempt at a time: {actions:?}");
        all.first().copied()
    }

    fn begun(actions: &[Action]) -> Option<u64> {
        actions.iter().find_map(|action| match action {
            Action::Begin { generation, .. } => Some(*generation),
            _ => None,
        })
    }

    fn answer_to(actions: &[Action], id: WantId) -> Option<Result<(), Failure>> {
        let answers: Vec<_> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Answer {
                    id: answered,
                    result,
                    ..
                } if *answered == id => Some(*result),
                _ => None,
            })
            .collect();
        assert!(answers.len() <= 1, "a want is answered once: {actions:?}");
        answers.first().copied()
    }

    /// The generation the answer to `id` waits to see open, when it is answered.
    fn opened_of(actions: &[Action], id: WantId) -> Option<Option<u64>> {
        actions.iter().find_map(|action| match action {
            Action::Answer {
                id: answered,
                opened,
                ..
            } if *answered == id => Some(*opened),
            _ => None,
        })
    }

    /// The actions that open a generation, in the order they were returned.
    fn opening(actions: &[Action]) -> Vec<Action> {
        actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    Action::Begin { .. }
                        | Action::ProbeRevision { .. }
                        | Action::Register { .. }
                        | Action::AnnounceStart { .. }
                        | Action::ResendPalettes { .. }
                        | Action::Open { .. }
                        | Action::RestoreAttach { .. }
                )
            })
            .cloned()
            .collect()
    }

    fn lost_link(window_id: &str) -> Action {
        Action::MoveLink {
            window_id: window_id.to_owned(),
            to: LinkState::Lost(LinkLostReason::ReconnectFailed),
        }
    }

    fn is_open(public: &Public) -> bool {
        matches!(public.state, PublicState::Open { .. })
    }

    /// Every way a generation's attempt finishes, with the window id its end
    /// carries.
    fn finished_generations() -> Vec<(&'static str, Run, u64, Option<&'static str>)> {
        let mut control = Run::control();
        let opened = control.open(ConnectMode::Spawn);

        let mut chained_control = Run::control();
        let first = chained_control.open(ConnectMode::Spawn);
        let (attempt, _) = chained_control.lose(first, None);
        let chained = chained_control.finish_chain_attempt(attempt);

        let mut window = Run::window();
        let attached = window.open(ConnectMode::Retry);

        let mut restored_window = Run::window();
        let first = restored_window.open(ConnectMode::Retry);
        let (attempt, _) = restored_window.lose(first, Some(WINDOW));
        let restored = restored_window.finish_chain_attempt(attempt);

        vec![
            ("control", control, opened, None),
            (
                "control opened by its chain",
                chained_control,
                chained,
                None,
            ),
            ("window", window, attached, Some(WINDOW)),
            (
                "window restored by its chain",
                restored_window,
                restored,
                Some(WINDOW),
            ),
        ]
    }

    /// A control lifecycle whose generation is in its opening steps.
    fn opening_control() -> (Run, WantId, u64) {
        let mut run = Run::control();
        let (id, attempt) = run.start(ConnectMode::Spawn, Until::Open);
        let (generation, _) = run.reach(attempt);
        (run, id, generation)
    }

    #[test]
    fn an_end_of_a_generation_that_is_not_the_current_one_does_nothing() {
        let mut run = Run::control();
        let first = run.open(ConnectMode::Spawn);
        let (attempt, _) = run.lose(first, None);
        let current = run.finish_chain_attempt(attempt);
        assert_ne!(first, current);
        let before = run.public().clone();
        for kind in LOST.into_iter().chain([EndKind::Superseded]) {
            for stale in [first, current + 1] {
                let actions = run.feed(ended(stale, kind, None));
                assert_eq!(actions, Vec::new(), "{kind:?} of generation {stale}");
                assert_eq!(run.public(), &before, "{kind:?} of generation {stale}");
            }
        }
    }

    #[test]
    fn wants_that_arrive_during_an_attempt_join_it_instead_of_starting_another() {
        let mut run = Run::control();
        let (poll, first) = run.start(ConnectMode::Once, Until::Open);
        let (again, actions) = run.want(ConnectMode::Once, Until::Open);
        assert_eq!(attempts(&actions), Vec::new());
        let (user, actions) = run.want(ConnectMode::Spawn, Until::Open);
        assert_eq!(attempts(&actions), Vec::new());

        // Once does not cover Spawn.
        let actions = run.feed(Event::ConnectFailed {
            attempt: first,
            error: Failure::NotConnected,
        });
        assert_eq!(answer_to(&actions, poll), Some(Err(Failure::NotConnected)));
        assert_eq!(answer_to(&actions, again), Some(Err(Failure::NotConnected)));
        assert_eq!(answer_to(&actions, user), None);
        let (second, mode) =
            attempt_in(&actions).expect("the uncovered want gets an attempt of its own form");
        assert_eq!(mode, ConnectMode::Spawn);

        // Spawn covers Once.
        let (late, actions) = run.want(ConnectMode::Once, Until::Open);
        assert_eq!(attempts(&actions), Vec::new());
        let actions = run.feed(Event::ConnectFailed {
            attempt: second,
            error: Failure::DaemonSpawn,
        });
        assert_eq!(answer_to(&actions, user), Some(Err(Failure::DaemonSpawn)));
        assert_eq!(answer_to(&actions, late), Some(Err(Failure::DaemonSpawn)));
        assert_eq!(attempts(&actions), Vec::new());

        // The opening steps belong to the attempt too.
        let (_, third) = run.start(ConnectMode::Once, Until::Open);
        let (generation, _) = run.reach(third);
        for mode in [ConnectMode::Once, ConnectMode::Spawn] {
            let (_, actions) = run.want(mode, Until::Open);
            assert_eq!(attempts(&actions), Vec::new(), "{mode:?} while opening");
        }
        let actions = run.settle(generation, Registration::Accepted);
        assert_eq!(attempts(&actions), Vec::new());

        // The same form covers itself.
        let mut run = Run::window();
        let (one, attempt) = run.start(ConnectMode::Retry, Until::Open);
        let (other, actions) = run.want(ConnectMode::Retry, Until::Open);
        assert_eq!(attempts(&actions), Vec::new());
        let actions = run.feed(Event::ConnectFailed {
            attempt,
            error: Failure::NotConnected,
        });
        assert_eq!(answer_to(&actions, one), Some(Err(Failure::NotConnected)));
        assert_eq!(answer_to(&actions, other), Some(Err(Failure::NotConnected)));
        assert_eq!(attempts(&actions), Vec::new());
    }

    #[test]
    fn a_connection_brought_by_an_attempt_that_is_over_is_dropped_unopened() {
        let mut run = Run::control();
        let (_, over) = run.start(ConnectMode::Once, Until::Open);
        run.feed(Event::ConnectFailed {
            attempt: over,
            error: Failure::NotConnected,
        });
        let (_, current) = run.start(ConnectMode::Once, Until::Open);

        let actions = run.feed(Event::Connected { attempt: over });
        assert_eq!(actions, vec![Action::Discard { attempt: over }]);
        assert_eq!(run.public().state, PublicState::Absent);

        let (generation, _) = run.reach(current);
        run.settle(generation, Registration::Accepted);
        let actions = run.feed(Event::Connected { attempt: over });
        assert_eq!(actions, vec![Action::Discard { attempt: over }]);
        assert!(is_open(run.public()));
    }

    #[test]
    fn a_generation_lost_after_its_attempt_finished_starts_exactly_one_chain() {
        for kind in LOST {
            for (label, mut run, generation, window) in finished_generations() {
                let actions = run.feed(ended(generation, kind, window));
                assert!(
                    actions.contains(&Action::End { generation }),
                    "{label} {kind:?}: {actions:?}"
                );
                let (_, mode) = attempt_in(&actions)
                    .unwrap_or_else(|| panic!("{label} {kind:?} starts the chain: {actions:?}"));
                assert_eq!(mode, ConnectMode::Retry, "{label} {kind:?}");
                for second in LOST.into_iter().chain([EndKind::Superseded]) {
                    assert_eq!(
                        run.feed(ended(generation, second, window)),
                        Vec::new(),
                        "{label} {kind:?} then {second:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn superseded_and_retired_generations_start_no_chain() {
        for (label, mut run, generation, window) in finished_generations() {
            let actions = run.feed(ended(generation, EndKind::Superseded, window));
            assert!(
                actions.contains(&Action::End { generation }),
                "{label}: {actions:?}"
            );
            assert_eq!(attempts(&actions), Vec::new(), "{label}");
            if let Some(window_id) = window {
                assert!(
                    actions.contains(&Action::MoveLink {
                        window_id: window_id.to_owned(),
                        to: LinkState::Superseded,
                    }),
                    "{label}: {actions:?}"
                );
            }
            assert_eq!(run.public().state, PublicState::Absent, "{label}");
            if window.is_none() {
                let (_, actions) = run.want(ConnectMode::Spawn, Until::Open);
                assert_eq!(attempts(&actions).len(), 1, "{label}: only a want reopens");
            }
        }
        for (label, mut run, generation, _) in finished_generations() {
            let actions = run.feed(Event::Retire);
            assert!(
                actions.contains(&Action::End { generation }),
                "{label}: {actions:?}"
            );
            assert!(actions.contains(&Action::AckRetire), "{label}: {actions:?}");
            assert_eq!(attempts(&actions), Vec::new(), "{label}");
            assert_eq!(run.public().state, PublicState::Retired, "{label}");
        }
    }

    #[test]
    fn a_chain_gives_up_after_its_limit_and_then_each_want_tries_once() {
        for (mut run, mode, window) in [
            (Run::window(), ConnectMode::Retry, Some(WINDOW)),
            (Run::control(), ConnectMode::Spawn, None),
        ] {
            let generation = run.open(mode);
            let first = run.feed(ended(generation, EndKind::Eof, window));
            let (tried, last) = run.exhaust(first, |run, attempt| {
                run.feed(Event::ConnectFailed {
                    attempt,
                    error: Failure::NotConnected,
                })
            });
            assert_eq!(tried, RECONNECT_ATTEMPT_LIMIT, "{window:?}");
            match window {
                Some(window_id) => assert!(last.contains(&lost_link(window_id)), "{last:?}"),
                None => assert!(
                    !last
                        .iter()
                        .any(|action| matches!(action, Action::MoveLink { .. })),
                    "{last:?}"
                ),
            }

            for _ in 0..2 {
                let (id, attempt) = run.start(mode, Until::Open);
                let actions = run.feed(Event::ConnectFailed {
                    attempt,
                    error: Failure::NotConnected,
                });
                assert_eq!(answer_to(&actions, id), Some(Err(Failure::NotConnected)));
                assert_eq!(attempts(&actions), Vec::new(), "{window:?}");
            }
        }
    }

    #[test]
    fn opening_settles_the_revision_then_registers_then_announces_then_resends_the_palettes_then_opens()
     {
        for registration in [Registration::Accepted, Registration::Refused] {
            let mut run = Run::control();
            let (_, attempt) = run.start(ConnectMode::Spawn, Until::Open);
            let (generation, connected) = run.reach(attempt);
            assert_eq!(
                opening(&connected),
                vec![
                    Action::Begin {
                        attempt,
                        generation
                    },
                    Action::ProbeRevision { generation },
                ]
            );
            let probed = run.feed(Event::Probed {
                generation,
                revision: REVISION,
            });
            assert_eq!(opening(&probed), vec![Action::Register { generation }]);
            let registered = run.feed(Event::Registered {
                generation,
                registration,
            });
            assert_eq!(
                opening(&registered),
                vec![
                    Action::AnnounceStart { generation },
                    Action::ResendPalettes { generation },
                    Action::Open { generation },
                ],
                "{registration:?}"
            );
        }

        let mut run = Run::window();
        let (_, attempt) = run.start(ConnectMode::Retry, Until::Open);
        let (generation, _) = run.reach(attempt);
        let probed = run.feed(Event::Probed {
            generation,
            revision: REVISION,
        });
        assert_eq!(
            opening(&probed),
            vec![
                Action::AnnounceStart { generation },
                Action::ResendPalettes { generation },
                Action::Open { generation },
            ]
        );
    }

    #[test]
    fn the_public_state_and_its_readers_follow_the_generation() {
        let mut run = Run::control();
        let (_, attempt) = run.start(ConnectMode::Spawn, Until::Open);
        let public = run.public();
        assert_eq!(public.state, PublicState::Absent);
        assert!(!public.is_connected());
        assert!(!public.is_approver_registered());
        assert_eq!(public.protocol_revision(), None);

        let (generation, _) = run.reach(attempt);
        let public = run.public();
        assert_eq!(public.state, PublicState::Reached);
        assert!(public.is_connected());
        assert!(!public.is_approver_registered());
        assert_eq!(public.protocol_revision(), None);

        run.feed(Event::Probed {
            generation,
            revision: REVISION,
        });
        assert_eq!(run.public().state, PublicState::Reached);
        assert_eq!(run.public().protocol_revision(), None);

        run.feed(Event::Registered {
            generation,
            registration: Registration::Accepted,
        });
        let public = run.public();
        assert!(is_open(public));
        assert!(public.is_connected());
        assert!(public.is_approver_registered());
        assert_eq!(public.protocol_revision(), Some(REVISION));

        run.feed(ended(generation, EndKind::Eof, None));
        let public = run.public();
        assert_eq!(public.state, PublicState::Absent);
        assert!(!public.is_connected());
        assert!(!public.is_approver_registered());
        assert_eq!(public.protocol_revision(), None);

        let (mut run, _, generation) = opening_control();
        run.feed(Event::Probed {
            generation,
            revision: REVISION,
        });
        run.feed(Event::Registered {
            generation,
            registration: Registration::Refused,
        });
        assert!(is_open(run.public()));
        assert!(!run.public().is_approver_registered());
        assert_eq!(run.public().protocol_revision(), Some(REVISION));

        let mut run = Run::window();
        run.open(ConnectMode::Retry);
        assert!(run.public().is_connected());
        assert!(!run.public().is_approver_registered());
        assert_eq!(run.public().protocol_revision(), Some(REVISION));

        // The daemon answered and still did not open: connected for the
        // supervisor, nothing to read for anyone else.
        let (mut run, _, generation) = opening_control();
        run.feed(Event::StepFailed { generation });
        let public = run.public();
        assert_eq!(public.state, PublicState::Absent);
        assert!(public.daemon_answers);
        assert!(public.is_connected());
        assert_eq!(public.protocol_revision(), None);

        let mut run = Run::control();
        run.open(ConnectMode::Spawn);
        run.feed(Event::Retire);
        let public = run.public();
        assert_eq!(public.state, PublicState::Retired);
        assert!(!public.is_approver_registered());
        assert_eq!(public.protocol_revision(), None);
    }

    #[test]
    fn an_end_before_its_attempt_took_a_finishing_answer_fails_it_and_one_after_starts_a_new_chain()
    {
        for kind in [EndKind::Eof, EndKind::UnreadableAnswer] {
            let (mut run, id, generation) = opening_control();
            let actions = run.feed(ended(generation, kind, None));
            assert!(actions.contains(&Action::End { generation }), "{kind:?}");
            assert_eq!(answer_to(&actions, id), Some(Err(Failure::NotConnected)));
            assert_eq!(attempts(&actions), Vec::new(), "{kind:?}");

            let mut run = Run::control();
            let generation = run.open(ConnectMode::Spawn);
            let first = run.feed(ended(generation, EndKind::Eof, None));
            let (tried, _) = run.exhaust(first, |run, attempt| {
                let (generation, _) = run.reach(attempt);
                run.feed(ended(generation, kind, None))
            });
            assert_eq!(tried, RECONNECT_ATTEMPT_LIMIT, "control {kind:?}");

            // A window chain's attempt finishes only once its attach is restored.
            let mut run = Run::window();
            let generation = run.open(ConnectMode::Retry);
            let first = run.feed(ended(generation, EndKind::Eof, Some(WINDOW)));
            let (tried, last) = run.exhaust(first, |run, attempt| {
                let (generation, _) = run.reach(attempt);
                run.settle(generation, Registration::Unregistered);
                run.feed(ended_taking(
                    generation,
                    kind,
                    Some(WINDOW),
                    Some(TakenAnswer::Revision(REVISION)),
                ))
            });
            assert_eq!(tried, RECONNECT_ATTEMPT_LIMIT, "window {kind:?}");
            assert!(last.contains(&lost_link(WINDOW)), "{kind:?}: {last:?}");

            // An end after the attach was answered is a generation that opened
            // and was lost: it starts a chain with a budget of its own.
            let mut run = Run::window();
            let generation = run.open(ConnectMode::Retry);
            let first = run.feed(ended(generation, EndKind::Eof, Some(WINDOW)));
            let (attempt, _) = attempt_in(&first).expect("the lost window starts the chain");
            let (generation, _) = run.reach(attempt);
            run.settle(generation, Registration::Unregistered);
            let actions = run.feed(ended_taking(
                generation,
                kind,
                Some(WINDOW),
                Some(TakenAnswer::Restore),
            ));
            assert!(
                actions.contains(&Action::MoveLink {
                    window_id: WINDOW.to_owned(),
                    to: LinkState::Connecting,
                }),
                "{kind:?}: {actions:?}"
            );
            let (_, mode) = attempt_in(&actions).expect("a restored window that is lost chains");
            assert_eq!(mode, ConnectMode::Retry, "{kind:?}");
            assert!(
                !actions.contains(&lost_link(WINDOW)),
                "{kind:?}: {actions:?}"
            );
            let (tried, last) = run.exhaust(actions, |run, attempt| {
                let (generation, _) = run.reach(attempt);
                run.settle(generation, Registration::Unregistered);
                run.feed(ended_taking(
                    generation,
                    kind,
                    Some(WINDOW),
                    Some(TakenAnswer::Revision(REVISION)),
                ))
            });
            assert_eq!(tried, RECONNECT_ATTEMPT_LIMIT, "new chain {kind:?}");
            assert!(last.contains(&lost_link(WINDOW)), "{kind:?}: {last:?}");
        }
    }

    /// A generation whose attempt has not finished, the number of the
    /// generation, and the window its end carries.
    type InFlight = (Run, u64, Option<&'static str>);

    fn control_probing() -> InFlight {
        let (run, _, generation) = opening_control();
        (run, generation, None)
    }

    fn control_registering() -> InFlight {
        let (mut run, _, generation) = opening_control();
        run.feed(probed(generation));
        (run, generation, None)
    }

    fn window_probing() -> InFlight {
        let mut run = Run::window();
        let (_, attempt) = run.start(ConnectMode::Retry, Until::Open);
        let (generation, _) = run.reach(attempt);
        (run, generation, None)
    }

    fn control_chain_probing() -> InFlight {
        let mut run = Run::control();
        let lost = run.open(ConnectMode::Spawn);
        let (attempt, _) = run.lose(lost, None);
        let (generation, _) = run.reach(attempt);
        (run, generation, None)
    }

    fn control_chain_registering() -> InFlight {
        let (mut run, generation, window) = control_chain_probing();
        run.feed(probed(generation));
        (run, generation, window)
    }

    fn window_chain_probing() -> InFlight {
        let mut run = Run::window();
        let lost = run.open(ConnectMode::Retry);
        let (attempt, _) = run.lose(lost, Some(WINDOW));
        let (generation, _) = run.reach(attempt);
        (run, generation, Some(WINDOW))
    }

    fn window_chain_restoring() -> InFlight {
        let (mut run, generation, window) = window_chain_probing();
        run.settle(generation, Registration::Unregistered);
        (run, generation, window)
    }

    fn probed(generation: u64) -> Event {
        Event::Probed {
            generation,
            revision: REVISION,
        }
    }

    fn registered(generation: u64) -> Event {
        Event::Registered {
            generation,
            registration: Registration::Accepted,
        }
    }

    fn registration_refused(generation: u64) -> Event {
        Event::Registered {
            generation,
            registration: Registration::Refused,
        }
    }

    fn restored(generation: u64) -> Event {
        Event::Restored { generation }
    }

    fn step_failed(generation: u64) -> Event {
        Event::StepFailed { generation }
    }

    /// How a step can settle: the event it brings, and the answer the
    /// generation took that an end carries.
    type Settling = (&'static str, fn(u64) -> Event, TakenAnswer);

    const PROBE_SETTLES: [Settling; 1] = [("probed", probed, TakenAnswer::Revision(REVISION))];
    const REGISTRATION_SETTLES: [Settling; 2] = [
        (
            "registered",
            registered,
            TakenAnswer::Registration(Registration::Accepted),
        ),
        (
            "registration refused",
            registration_refused,
            TakenAnswer::Registration(Registration::Refused),
        ),
    ];
    const RESTORE_SETTLES: [Settling; 1] = [("restored", restored, TakenAnswer::Restore)];

    #[test]
    fn an_end_and_the_result_of_the_step_in_flight_act_the_same_in_either_order() {
        // Every generation whose attempt has not finished, with the form of
        // the want that waits for it and how its step can settle.
        type Scene = (
            &'static str,
            fn() -> InFlight,
            ConnectMode,
            &'static [Settling],
        );
        let scenes: [Scene; 7] = [
            (
                "control probing",
                control_probing,
                ConnectMode::Once,
                &PROBE_SETTLES,
            ),
            (
                "control registering",
                control_registering,
                ConnectMode::Once,
                &REGISTRATION_SETTLES,
            ),
            (
                "window probing",
                window_probing,
                ConnectMode::Retry,
                &PROBE_SETTLES,
            ),
            (
                "control chain probing",
                control_chain_probing,
                ConnectMode::Once,
                &PROBE_SETTLES,
            ),
            (
                "control chain registering",
                control_chain_registering,
                ConnectMode::Once,
                &REGISTRATION_SETTLES,
            ),
            (
                "window chain probing",
                window_chain_probing,
                ConnectMode::Retry,
                &PROBE_SETTLES,
            ),
            (
                "window chain restoring",
                window_chain_restoring,
                ConnectMode::Retry,
                &RESTORE_SETTLES,
            ),
        ];
        for (label, setup, wants, settles) in scenes {
            // A step the daemon refused is recorded before the end, as a refusal.
            let settlings = settles
                .iter()
                .map(|(name, event, taken)| (*name, Some(*event), *taken))
                .chain([("step failed", None, TakenAnswer::Refused)]);
            for (settled, result, taken) in settlings {
                for kind in LOST.into_iter().chain([EndKind::Superseded]) {
                    let play = |end_first: bool| {
                        let (mut run, generation, window) = setup();
                        run.want(wants, Until::Open);
                        let result = result
                            .map_or_else(|| step_failed(generation), |event| event(generation));
                        let end = ended_taking(generation, kind, window, Some(taken));
                        let mut actions = Vec::new();
                        if end_first {
                            actions.extend(run.feed(end));
                            actions.extend(run.feed(result));
                        } else {
                            actions.extend(run.feed(result));
                            actions.extend(run.feed(end));
                        }
                        // A want still waiting means a generation is on its way.
                        assert!(
                            run.lifecycle.wants.is_empty()
                                || matches!(
                                    run.lifecycle.phase,
                                    Phase::Connecting { .. } | Phase::Opening { .. }
                                ),
                            "{label} {settled} {kind:?}: a want waits with no attempt on its way"
                        );
                        (generation, window, format!("{:?}", run.lifecycle), actions)
                    };
                    let (generation, window, state, actions) = play(false);
                    let (_, _, reversed_state, reversed) = play(true);
                    let what = format!("{label} {settled} {kind:?}");

                    assert_eq!(state, reversed_state, "{what}");
                    assert_eq!(actions, reversed, "{what}");
                    assert_eq!(
                        actions
                            .iter()
                            .filter(|action| **action == Action::End { generation })
                            .count(),
                        1,
                        "{what}: {actions:?}"
                    );

                    let moves_to = |to: LinkState| {
                        window.is_some_and(|window_id| {
                            actions.contains(&Action::MoveLink {
                                window_id: window_id.to_owned(),
                                to,
                            })
                        })
                    };
                    let lost =
                        window.is_some_and(|window_id| actions.contains(&lost_link(window_id)));
                    let tries = !attempts(&actions).is_empty();
                    let succeeded = result.is_some();
                    match (window, kind, succeeded) {
                        // An attach that was answered, then lost: the chain starts again.
                        (Some(_), kind, true)
                            if settled == "restored" && kind != EndKind::Superseded =>
                        {
                            assert!(moves_to(LinkState::Connecting), "{what}: {actions:?}");
                            assert!(
                                attempts(&actions)
                                    .iter()
                                    .any(|(_, mode)| *mode == ConnectMode::Retry),
                                "{what}: {actions:?}"
                            );
                            assert!(!lost, "{what}: {actions:?}");
                        }
                        // A takeover is the window's end, whatever the step did.
                        (Some(_), EndKind::Superseded, true) => {
                            assert!(moves_to(LinkState::Superseded), "{what}: {actions:?}");
                            assert!(!lost, "{what}: {actions:?}");
                            assert!(!tries, "{what}: {actions:?}");
                        }
                        (None, EndKind::Superseded, true) if wants == ConnectMode::Retry => {
                            assert!(
                                !actions
                                    .iter()
                                    .any(|action| matches!(action, Action::MoveLink { .. })),
                                "{what}: {actions:?}"
                            );
                            assert!(!tries, "{what}: {actions:?}");
                        }
                        // A refusal ends the attempt, and a takeover that follows it does not undo that.
                        (Some(_), _, false) => {
                            assert!(lost, "{what}: {actions:?}");
                            assert!(!moves_to(LinkState::Superseded), "{what}: {actions:?}");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn a_want_is_answered_by_the_first_result_that_meets_its_until() {
        let mut run = Run::control();
        let (poll, attempt) = run.start(ConnectMode::Once, Until::Open);
        let (supervisor, _) = run.want(ConnectMode::Spawn, Until::Reached);
        let (generation, connected) = run.reach(attempt);
        assert_eq!(answer_to(&connected, supervisor), Some(Ok(())));
        assert_eq!(answer_to(&connected, poll), None);
        // Reaching the daemon is not waiting for a generation to open.
        assert_eq!(opened_of(&connected, supervisor), Some(None));

        let (reached, actions) = run.want(ConnectMode::Spawn, Until::Reached);
        assert_eq!(answer_to(&actions, reached), Some(Ok(())));
        assert_eq!(opened_of(&actions, reached), Some(None));
        let (opened, actions) = run.want(ConnectMode::Once, Until::Open);
        assert_eq!(answer_to(&actions, opened), None);

        let actions = run.settle(generation, Registration::Accepted);
        assert_eq!(answer_to(&actions, poll), Some(Ok(())));
        assert_eq!(answer_to(&actions, opened), Some(Ok(())));
        assert_eq!(opened_of(&actions, poll), Some(Some(generation)));
        assert_eq!(opened_of(&actions, opened), Some(Some(generation)));

        for until in [Until::Reached, Until::Open] {
            let (id, actions) = run.want(ConnectMode::Spawn, until);
            assert_eq!(
                answer_to(&actions, id),
                Some(Ok(())),
                "{until:?} while open"
            );
            assert_eq!(opened_of(&actions, id), Some(Some(generation)), "{until:?}");
            assert_eq!(attempts(&actions), Vec::new());
        }

        // A window that restores its attach is open too: a want that arrives
        // meanwhile is answered once the generation is published open.
        let mut window = Run::window();
        let lost = window.open(ConnectMode::Retry);
        let (attempt, _) = window.lose(lost, Some(WINDOW));
        let (generation, _) = window.reach(attempt);
        window.settle(generation, Registration::Unregistered);
        let (id, actions) = window.want(ConnectMode::Retry, Until::Open);
        assert_eq!(answer_to(&actions, id), Some(Ok(())));
        assert_eq!(opened_of(&actions, id), Some(Some(generation)));

        // Failing an attempt that does not cover a want leaves the want to
        // the next attempt.
        let mut run = Run::control();
        let (poll, attempt) = run.start(ConnectMode::Once, Until::Open);
        let (supervisor, _) = run.want(ConnectMode::Spawn, Until::Reached);
        let actions = run.feed(Event::ConnectFailed {
            attempt,
            error: Failure::NotConnected,
        });
        assert_eq!(answer_to(&actions, poll), Some(Err(Failure::NotConnected)));
        assert_eq!(opened_of(&actions, poll), Some(None));
        assert_eq!(answer_to(&actions, supervisor), None);
        let (next, mode) = attempt_in(&actions).expect("the want keeps an attempt");
        assert_eq!(mode, ConnectMode::Spawn);
        let actions = run.feed(Event::Connected { attempt: next });
        assert_eq!(answer_to(&actions, supervisor), Some(Ok(())));
    }

    #[test]
    fn a_retired_lifecycle_answers_wants_with_not_connected_and_opens_nothing() {
        let mut run = Run::window();
        let actions = run.feed(Event::Retire);
        assert!(actions.contains(&Action::AckRetire));
        assert_eq!(run.public().state, PublicState::Retired);
        let (id, actions) = run.want(ConnectMode::Retry, Until::Open);
        assert_eq!(answer_to(&actions, id), Some(Err(Failure::NotConnected)));
        assert_eq!(attempts(&actions), Vec::new());

        let mut run = Run::window();
        let (waiting, attempt) = run.start(ConnectMode::Retry, Until::Open);
        let actions = run.feed(Event::Retire);
        assert!(actions.contains(&Action::AckRetire));
        assert_eq!(
            answer_to(&actions, waiting),
            Some(Err(Failure::NotConnected))
        );
        let actions = run.feed(Event::Connected { attempt });
        assert_eq!(actions, vec![Action::Discard { attempt }]);
        assert_eq!(run.public().state, PublicState::Retired);
        let (id, actions) = run.want(ConnectMode::Retry, Until::Open);
        assert_eq!(answer_to(&actions, id), Some(Err(Failure::NotConnected)));
        assert_eq!(attempts(&actions), Vec::new());
    }

    #[test]
    fn a_window_chain_restores_the_window_its_end_carried_and_none_without_one() {
        let mut run = Run::window();
        let generation = run.open(ConnectMode::Retry);
        let (attempt, _) = run.lose(generation, Some(WINDOW));
        let (generation, _) = run.reach(attempt);
        let actions = run.settle(generation, Registration::Unregistered);
        assert_eq!(
            opening(&actions),
            vec![
                Action::AnnounceStart { generation },
                Action::ResendPalettes { generation },
                Action::Open { generation },
                Action::RestoreAttach {
                    generation,
                    window_id: WINDOW.to_owned(),
                },
            ]
        );
        run.feed(Event::Restored { generation });

        // The window id comes from each end, not from an earlier one.
        let (attempt, _) = run.lose(generation, Some(OTHER_WINDOW));
        let (generation, _) = run.reach(attempt);
        let actions = run.settle(generation, Registration::Unregistered);
        assert!(
            actions.contains(&Action::RestoreAttach {
                generation,
                window_id: OTHER_WINDOW.to_owned(),
            }),
            "{actions:?}"
        );
        run.feed(Event::Restored { generation });

        let actions = run.feed(ended(generation, EndKind::Eof, None));
        assert!(actions.contains(&Action::End { generation }), "{actions:?}");
        assert_eq!(attempts(&actions), Vec::new());
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::MoveLink { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn a_want_the_running_attempt_does_not_cover_waits_for_an_attempt_of_its_own_form() {
        let mut run = Run::control();
        let generation = run.open(ConnectMode::Spawn);
        let (chain, mode) = run.lose(generation, None);
        assert_eq!(mode, ConnectMode::Retry);
        let (supervisor, _) = run.want(ConnectMode::Spawn, Until::Reached);
        let (poll, _) = run.want(ConnectMode::Once, Until::Open);

        let actions = run.feed(Event::ConnectFailed {
            attempt: chain,
            error: Failure::NotConnected,
        });
        assert_eq!(answer_to(&actions, poll), Some(Err(Failure::NotConnected)));
        assert_eq!(answer_to(&actions, supervisor), None);
        let (spawn, mode) = attempt_in(&actions).expect("the supervisor's want keeps an attempt");
        assert_eq!(mode, ConnectMode::Spawn);

        let actions = run.feed(Event::Connected { attempt: spawn });
        assert_eq!(answer_to(&actions, supervisor), Some(Ok(())));
    }

    #[test]
    fn daemon_answers_changes_only_on_the_outcomes_that_tell_whether_the_daemon_answers() {
        let (run, _, _) = opening_control();
        assert!(run.public().daemon_answers, "an accepted connection");

        let answered: [fn(u64) -> Event; 2] = [
            |generation| Event::StepFailed { generation },
            |generation| ended(generation, EndKind::UnreadableAnswer, None),
        ];
        for answer in answered {
            let (mut run, _, generation) = opening_control();
            let event = answer(generation);
            let label = format!("{event:?}");
            run.feed(event);
            assert!(run.public().daemon_answers, "{label}");
            run.want(ConnectMode::Once, Until::Open);
            assert!(run.public().daemon_answers, "{label}, then an attempt");
        }

        for kind in CONNECTION_STAGE {
            let (mut run, _, generation) = opening_control();
            run.feed(ended(generation, kind, None));
            assert!(!run.public().daemon_answers, "{kind:?} while opening");
            assert!(!run.public().is_connected(), "{kind:?} while opening");
            run.want(ConnectMode::Once, Until::Open);
            assert!(!run.public().daemon_answers, "{kind:?}, then an attempt");

            let mut run = Run::control();
            let generation = run.open(ConnectMode::Spawn);
            let actions = run.feed(ended(generation, kind, None));
            assert!(attempt_in(&actions).is_some(), "{kind:?} after opening");
            assert!(!run.public().daemon_answers, "{kind:?} after opening");
            assert!(!run.public().is_connected(), "{kind:?} after opening");
        }

        let mut run = Run::control();
        let generation = run.open(ConnectMode::Spawn);
        run.feed(ended(generation, EndKind::Superseded, None));
        assert!(run.public().daemon_answers, "superseded");
        let (_, attempt) = run.start(ConnectMode::Spawn, Until::Open);
        assert!(run.public().daemon_answers, "superseded, then an attempt");
        run.feed(Event::ConnectFailed {
            attempt,
            error: Failure::DaemonSpawn,
        });
        assert!(!run.public().daemon_answers, "a failed attempt");
    }
}

#[cfg(test)]
mod connect_tests {
    //! The connect functions an attempt uses, against a real endpoint.

    use yagura_os::PeerAuth;

    use super::*;

    const ENDPOINT_PREFIX: &str = "yagura-test-gui-ipc-client";

    async fn listening(
        tag: &str,
    ) -> (PlatformIpc, <PlatformIpc as IpcTransport>::Listener, String) {
        let ipc = PlatformIpc::default();
        let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, tag);
        let security = ipc.endpoint_security().expect("endpoint security");
        let listener = ipc.bind(&endpoint, security).await.expect("bind");
        (ipc, listener, endpoint)
    }

    #[tokio::test]
    async fn every_connect_form_reaches_a_listening_endpoint() {
        let (ipc, mut listener, endpoint) = listening("connect-once").await;
        let (accepted, reached) = tokio::join!(ipc.accept(&mut listener), connect_once(&endpoint));
        assert!(accepted.is_ok() && reached.is_some(), "connect_once");

        let (ipc, mut listener, endpoint) = listening("connect-limited").await;
        let (accepted, reached) = tokio::join!(
            ipc.accept(&mut listener),
            connect_retrying_with_limit(&endpoint, 1)
        );
        assert!(
            accepted.is_ok() && reached.is_some(),
            "connect_retrying_with_limit"
        );

        let (ipc, mut listener, endpoint) = listening("connect-retrying").await;
        let (accepted, reached) =
            tokio::join!(ipc.accept(&mut listener), connect_retrying(&endpoint));
        assert!(accepted.is_ok() && reached.is_some(), "connect_retrying");
    }

    // The paused clock moves only when the retry loop sleeps, and only to that
    // sleep's deadline, so the elapsed time counts the sleeps exactly: one
    // interval between each pair of attempts and none before the first.
    #[tokio::test(start_paused = true)]
    async fn a_refused_endpoint_is_retried_an_interval_apart() {
        let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, "nobody");

        let started = tokio::time::Instant::now();
        assert!(connect_retrying_with_limit(&endpoint, 1).await.is_none());
        assert_eq!(
            started.elapsed(),
            std::time::Duration::ZERO,
            "a single attempt waits"
        );

        let started = tokio::time::Instant::now();
        assert!(connect_retrying_with_limit(&endpoint, 3).await.is_none());
        assert_eq!(started.elapsed(), CONNECT_RETRY_INTERVAL * 2);
    }
}

#[cfg(test)]
mod request_tests {
    //! A request ends with its answer or with its generation, and with
    //! nothing else, so no time that passes can turn "not answered yet" into a
    //! failure. The connection is in memory so the paused clock moves in one
    //! order everywhere.

    use std::time::Duration;

    use tokio::sync::Notify;
    use yagura_protocol::DaemonMessage;

    use super::*;
    use crate::host::fake::{Reaction, ScriptedDaemon};
    use crate::pool_outcomes::{DecideApprovalOutcome, map_decide_approval_result};

    // Longer than any finite limit the product could hold; the value is not
    // tied to one, as a limit that cannot be named cannot be tuned away.
    const A_DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[tokio::test(start_paused = true)]
    async fn a_decision_the_daemon_has_not_answered_stays_pending_however_long_it_takes() {
        let reached = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let daemon = ScriptedDaemon::new({
            let reached = Arc::clone(&reached);
            let gate = Arc::clone(&gate);
            move |request| match request {
                ClientMessage::ApprovalDecision { request_id, .. } => Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::clone(&gate),
                    vec![DaemonMessage::ApprovalResolved {
                        request_id: request_id.clone(),
                    }],
                ),
                _ => None,
            }
        });
        let (near, far) = tokio::io::duplex(1 << 16);
        daemon.serve_stream(far);
        let (generation, _reader) = Generation::wired(near);
        let decision = ClientMessage::ApprovalDecision {
            request_id: "r1".to_owned(),
            approved: true,
        };

        let (answered, ()) = tokio::join!(
            generation.request(&decision, PendingKind::DecideApproval),
            async {
                yagura_test_support::ipc::reply_within("the request", reached.notified()).await;
                tokio::time::sleep(A_DAY).await;
                gate.notify_one();
            }
        );

        let decided = map_decide_approval_result(answered.and_then(|reply| match reply {
            Reply::Done => Ok(()),
            _ => Err(IpcError::UnexpectedReply),
        }));
        assert!(
            matches!(decided, Ok(DecideApprovalOutcome::Decided)),
            "got {decided:?}"
        );
    }
}

#[cfg(test)]
mod owner_tests {
    //! The owner, driven through the wiring it has in production: its stages
    //! write to and read from real generations over in-memory streams, and the
    //! news a generation ends with comes from its reader alone. Nothing
    //! here reads a clock, opens a socket, spawns a task or sleeps: the order
    //! in which results reach the owner is the order the test hands them over.

    use std::sync::atomic::AtomicBool;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, Join, duplex};
    use yagura_protocol::{
        ClientMessage, DaemonMessage, decode_correlated_client_message, encode_daemon_message,
        frame_encode,
    };

    use super::*;
    use crate::events::AttachResponse;
    use crate::host::fake::{FakeHost, error_frame};
    use crate::ipc_client::ERROR_CODE_SUPERSEDED;

    const PIPE: usize = 64 * 1024;
    /// A daemon of the current wire revision.
    const REVISION: u32 = yagura_protocol::PROTOCOL_REVISION;
    /// Older than the first revision that carries frames.
    const BEFORE_FRAMES: u32 = 18;
    const WINDOW: &str = "w1";

    type Outcome = <Stage as Future>::Output;

    /// The write half of a connection: it writes until told to fail, and then
    /// fails as an allocation failure does, with an error that does not say the
    /// peer closed.
    struct FlakyWriter {
        inner: DuplexStream,
        failing: Arc<AtomicBool>,
    }

    impl AsyncWrite for FlakyWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.failing.load(Ordering::SeqCst) {
                return Poll::Ready(Err(std::io::ErrorKind::OutOfMemory.into()));
            }
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// The daemon's end of a connection.
    struct Peer {
        to_client: Option<DuplexStream>,
        from_client: Option<DuplexStream>,
        failing: Arc<AtomicBool>,
    }

    impl Peer {
        /// What the client wrote, as one request.
        async fn read_request(&mut self) -> (Option<u64>, ClientMessage) {
            let from_client = self.from_client.as_mut().expect("the daemon still reads");
            let mut length = [0u8; 4];
            from_client.read_exact(&mut length).await.expect("a length");
            let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
            from_client.read_exact(&mut body).await.expect("a body");
            decode_correlated_client_message(&body).expect("a request the client wrote")
        }

        async fn send(&mut self, message: &DaemonMessage) {
            let to_client = self.to_client.as_mut().expect("the daemon still writes");
            to_client
                .write_all(&frame_encode(&encode_daemon_message(message)))
                .await
                .expect("the client's end is open");
        }

        /// The daemon closes: whatever the client writes next fails as a write
        /// to a closed peer does.
        fn stop_reading(&mut self) {
            self.from_client = None;
        }

        /// The daemon lets go of the connection it was writing to.
        fn stop_writing(&mut self) {
            self.to_client = None;
        }

        /// From now on a write fails in a way that does not say the peer closed.
        fn make_writes_fail(&self) {
            self.failing.store(true, Ordering::SeqCst);
        }
    }

    type Stream = Join<DuplexStream, FlakyWriter>;

    fn connection() -> (Stream, Peer) {
        let (client_read, to_client) = duplex(PIPE);
        let (client_write, from_client) = duplex(PIPE);
        let failing = Arc::new(AtomicBool::new(false));
        let stream = tokio::io::join(
            client_read,
            FlakyWriter {
                inner: client_write,
                failing: Arc::clone(&failing),
            },
        );
        let peer = Peer {
            to_client: Some(to_client),
            from_client: Some(from_client),
            failing,
        };
        (stream, peer)
    }

    /// A generation's reader, polled by hand.
    struct Reader {
        task: Pin<Box<dyn Future<Output = ()> + Send>>,
        done: bool,
    }

    /// One generation of a scene, with the daemon's end and either its own
    /// reader (polled by the test) or the link its reader would report through.
    struct Conn {
        generation: Arc<Generation>,
        peer: Peer,
        reader: Option<Reader>,
        link: Option<ReaderLink>,
    }

    impl Conn {
        /// Polls the reader once, and tells whether it has ended.
        async fn reader_ended(&mut self) -> bool {
            let reader = self.reader.as_mut().expect("this connection has a reader");
            if !reader.done {
                let ready = std::future::poll_fn(|cx| {
                    Poll::Ready(reader.task.as_mut().poll(cx).is_ready())
                })
                .await;
                reader.done = ready;
            }
            reader.done
        }

        /// The reader reports that the connection ended as `kind`.
        fn report(&mut self, kind: EndKind) {
            self.link
                .take()
                .expect("this connection's reader is reported for")
                .report(kind);
        }
    }

    /// Reads the request the generation wrote and answers it under the key it
    /// carried.
    async fn answer_next(
        generation: &Generation,
        peer: &mut Peer,
        kind: PendingKind,
        answer: Result<Reply, IpcError>,
    ) {
        let (key, _request) = peer.read_request().await;
        let awaiting = generation
            .awaiting()
            .expect("the request written is the one awaiting");
        assert_eq!(key, Some(awaiting.correlation_id));
        assert_eq!(
            generation.settle(Some(awaiting.correlation_id), kind, answer),
            Settled::Answered
        );
    }

    fn describe(step: &Step) -> String {
        match step {
            Step::Do(action) => format!("{action:?}"),
            Step::Answer { id, result, .. } => format!("Answer({id:?}, {result:?})"),
        }
    }

    enum Item {
        Result(Outcome),
        End(OwnerEvent),
    }

    /// The `index`th arrangement of `pool`, in the order of the permutations'
    /// factorial number system.
    fn arrangement<T>(mut pool: Vec<T>, mut index: usize) -> Vec<T> {
        let mut arranged = Vec::with_capacity(pool.len());
        while !pool.is_empty() {
            let size = factorial(pool.len() - 1);
            arranged.push(pool.remove(index / size));
            index %= size;
        }
        arranged
    }

    fn factorial(n: usize) -> usize {
        (1..=n).product()
    }

    /// A client's owner with the inbox its generations report to.
    struct Rig {
        owner: Owner<FakeHost>,
        shared: Arc<Shared>,
        app: FakeHost,
        events: mpsc::UnboundedSender<OwnerEvent>,
        inbox: mpsc::UnboundedReceiver<OwnerEvent>,
        /// What reached the inbox and was not handed to the owner yet.
        news: Vec<OwnerEvent>,
    }

    impl Rig {
        fn new(kind: ClientKind) -> Self {
            let app = FakeHost::new("owner-scenes");
            let record = (kind == ClientKind::Window).then(|| Arc::new(WindowRecord::new()));
            let shared = Arc::new(Shared::new(kind, record, 0));
            let (events, inbox) = mpsc::unbounded_channel();
            let OwnerSlot::Unborn(publisher) = std::mem::replace(
                &mut *lock_blocking(&shared.slot),
                OwnerSlot::Running(events.clone()),
            ) else {
                panic!("a new client has no owner yet");
            };
            let owner = Owner::new(
                app.clone(),
                Arc::clone(&shared),
                publisher,
                events.downgrade(),
            );
            Self {
                owner,
                shared,
                app,
                events,
                inbox,
                news: Vec::new(),
            }
        }

        fn want(&mut self, mode: ConnectMode, until: Until) {
            let (reply, _answered) = oneshot::channel();
            self.owner.receive(OwnerEvent::Want { mode, until, reply });
        }

        fn pop(&mut self, what: &str) -> Step {
            self.owner
                .queue
                .pop_front()
                .unwrap_or_else(|| panic!("{what} is queued"))
        }

        /// The number of the attempt the owner started, which the scene does
        /// not run.
        fn next_attempt(&mut self) -> u64 {
            attempt_of(self.pop("an attempt"))
        }

        /// Hands the owner the connection of `attempt` and builds the
        /// generation it begins over a fresh stream. With a real reader the
        /// test polls it; without one the test reports its ends.
        fn begin(&mut self, attempt: u64, real_reader: bool) -> Conn {
            self.owner.feed(Event::Connected { attempt }, false, None);
            let step = self.pop("the begin of a generation");
            let Step::Do(Action::Begin { generation, .. }) = step else {
                panic!("expected a begin, got {}", describe(&step));
            };
            let (stream, peer) = connection();
            let (begun, reader, link) = Generation::begin(
                generation,
                stream,
                Arc::clone(&self.shared.correlation_ids),
                Some(self.events.downgrade()),
            );
            self.owner
                .generations
                .insert(generation, Arc::clone(&begun));
            if real_reader {
                let task = Box::pin(read_loop(
                    reader,
                    self.app.clone(),
                    Arc::clone(&begun),
                    link,
                ));
                Conn {
                    generation: begun,
                    peer,
                    reader: Some(Reader { task, done: false }),
                    link: None,
                }
            } else {
                drop(reader);
                Conn {
                    generation: begun,
                    peer,
                    reader: None,
                    link: Some(link),
                }
            }
        }

        /// Runs the revision round trip the generation's opening starts, with
        /// the daemon answering `revision`.
        async fn probe(&mut self, conn: &mut Conn, revision: u32) {
            let step = self.pop("the revision round trip");
            assert!(
                matches!(step, Step::Do(Action::ProbeRevision { .. })),
                "{}",
                describe(&step)
            );
            let stage = self.owner.execute(step).expect("the round trip is a stage");
            let answer = Ok(Reply::ProtocolRevision {
                revision,
                boot: None,
            });
            let (outcome, ()) = tokio::join!(
                stage,
                answer_next(
                    &conn.generation,
                    &mut conn.peer,
                    PendingKind::GetProtocolRevision,
                    answer
                )
            );
            self.owner.finish(outcome);
        }

        /// Carries out what is queued, except the steps that start something
        /// the scene runs itself: those are returned.
        async fn carry_out(&mut self) -> Vec<Step> {
            let mut held = Vec::new();
            while let Some(step) = self.owner.queue.pop_front() {
                if matches!(
                    step,
                    Step::Do(
                        Action::Attempt { .. }
                            | Action::Register { .. }
                            | Action::RestoreAttach { .. }
                    )
                ) {
                    held.push(step);
                } else if let Some(stage) = self.owner.execute(step) {
                    let outcome = stage.await;
                    self.owner.finish(outcome);
                }
            }
            held
        }

        fn collect(&mut self) {
            while let Ok(event) = self.inbox.try_recv() {
                self.news.push(event);
            }
        }

        /// The reader of `conn` reports `kind` and the owner hears it.
        fn lose(&mut self, conn: &mut Conn, kind: EndKind) {
            conn.report(kind);
            self.collect();
            for event in std::mem::take(&mut self.news) {
                self.owner.receive(event);
            }
        }

        /// How many ends of a generation reached the inbox, and how many of
        /// them are `kind`. The inbox holds one generation's news at a time in
        /// these scenes, and an end is told apart by how it prints, so that
        /// the test does not depend on how the news is built.
        fn ends(&mut self, kind: EndKind) -> (usize, usize) {
            self.collect();
            let ended: Vec<String> = self
                .news
                .iter()
                .map(|event| format!("{event:?}"))
                .filter(|printed| printed.starts_with("Ended"))
                .collect();
            let named = format!("{kind:?}");
            let of_kind = ended
                .iter()
                .filter(|printed| printed.contains(&named))
                .count();
            (ended.len(), of_kind)
        }

        /// Hands the owner the items in order, after emptying the queue so
        /// that only what they bring is read.
        fn deliver(&mut self, items: Vec<Item>) {
            self.owner.queue.clear();
            for item in items {
                match item {
                    Item::Result(outcome) => self.owner.finish(outcome),
                    Item::End(event) => self.owner.receive(event),
                }
            }
        }

        /// What the delivery left. A step that ends a generation is not read:
        /// an urgent event runs it at once and a result queues it.
        fn observe(&self, items: usize, notes: Vec<String>) -> Observed {
            let mut actions = Vec::new();
            let mut steps = Vec::new();
            for step in &self.owner.queue {
                match step {
                    Step::Do(Action::End { .. }) => {}
                    Step::Do(action) => {
                        actions.push(action.clone());
                        steps.push(describe(step));
                    }
                    Step::Answer { .. } => steps.push(describe(step)),
                }
            }
            Observed {
                items,
                actions,
                steps,
                public: format!("{:?}", self.owner.public),
                notes,
            }
        }
    }

    fn attempt_of(step: Step) -> u64 {
        match step {
            Step::Do(Action::Attempt { attempt, .. }) => attempt,
            other => panic!("expected an attempt, got {}", describe(&other)),
        }
    }

    /// What one scene left, in one order of delivery.
    struct Observed {
        /// How many things were handed to the owner.
        items: usize,
        actions: Vec<Action>,
        steps: Vec<String>,
        public: String,
        /// What went against the scene before anything was handed over.
        notes: Vec<String>,
    }

    impl Observed {
        fn moves_to(&self, to: LinkState) -> bool {
            self.actions.contains(&Action::MoveLink {
                window_id: WINDOW.to_owned(),
                to,
            })
        }

        fn lost(&self) -> bool {
            self.moves_to(LinkState::Lost(LinkLostReason::ReconnectFailed))
        }

        fn tries(&self) -> bool {
            self.actions
                .iter()
                .any(|action| matches!(action, Action::Attempt { .. }))
        }

        fn tries_in_retry(&self) -> bool {
            self.actions.iter().any(|action| {
                matches!(
                    action,
                    Action::Attempt {
                        mode: ConnectMode::Retry,
                        ..
                    }
                )
            })
        }
    }

    fn check(notes: &mut Vec<String>, held: bool, what: &str) {
        if !held {
            notes.push(what.to_owned());
        }
    }

    fn require(held: bool, what: &str) -> Result<(), String> {
        if held { Ok(()) } else { Err(what.to_owned()) }
    }

    fn all(checks: Vec<Result<(), String>>) -> Result<(), String> {
        let problems: Vec<String> = checks.into_iter().filter_map(Result::err).collect();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n  "))
        }
    }

    /// Runs `scene` in every order its news and results can reach the owner,
    /// and tells whether every order met `expect` and left the same.
    async fn across_orders<F, Fut>(
        scene: F,
        expect: fn(&Observed) -> Result<(), String>,
    ) -> Result<(), String>
    where
        F: Fn(usize) -> Fut,
        Fut: Future<Output = Observed>,
    {
        let first = scene(0).await;
        let mut problems = first.notes.clone();
        let mut add = |problem: String| {
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        };
        if let Err(problem) = expect(&first) {
            add(problem);
        }
        for order in 1..factorial(first.items) {
            let other = scene(order).await;
            if other.steps != first.steps || other.public != first.public {
                add(format!(
                    "order {order} of {} leaves {:?} and {}, order 0 leaves {:?} and {}",
                    factorial(first.items),
                    other.steps,
                    other.public,
                    first.steps,
                    first.public
                ));
            }
            if let Err(problem) = expect(&other) {
                add(problem);
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n  "))
        }
    }

    /// A window client whose generation 0 was open on window `w1` and ended
    /// on its reader, and whose chain's generation 1 is restoring: the step
    /// that attaches it again is returned, not yet run.
    async fn chain_restoring(real_reader: bool, revision: u32) -> (Rig, Conn, Stage) {
        let mut rig = Rig::new(ClientKind::Window);
        rig.want(ConnectMode::Retry, Until::Open);
        let attempt = rig.next_attempt();
        let mut lost = rig.begin(attempt, false);
        rig.probe(&mut lost, REVISION).await;
        assert!(rig.carry_out().await.is_empty());
        lost.generation.set_window_id(Some(WINDOW.to_owned()));
        rig.lose(&mut lost, EndKind::ReadError);
        let mut held = rig.carry_out().await;
        let attempt = attempt_of(held.pop().expect("the chain's first attempt"));
        assert!(held.is_empty());

        let mut restoring = rig.begin(attempt, real_reader);
        rig.probe(&mut restoring, revision).await;
        let mut held = rig.carry_out().await;
        let step = held.pop().expect("the restoring attach");
        assert!(held.is_empty());
        assert!(
            matches!(step, Step::Do(Action::RestoreAttach { .. })),
            "{}",
            describe(&step)
        );
        let stage = rig.owner.execute(step).expect("the attach is a stage");
        (rig, restoring, stage)
    }

    /// A window client whose generation 0 is open on window `w1`, with its
    /// own reader.
    async fn open_window() -> (Rig, Conn) {
        let mut rig = Rig::new(ClientKind::Window);
        rig.want(ConnectMode::Retry, Until::Open);
        let attempt = rig.next_attempt();
        let mut open = rig.begin(attempt, true);
        rig.probe(&mut open, REVISION).await;
        assert!(rig.carry_out().await.is_empty());
        open.generation.set_window_id(Some(WINDOW.to_owned()));
        (rig, open)
    }

    fn superseded_frame() -> DaemonMessage {
        error_frame(ERROR_CODE_SUPERSEDED)
    }

    fn attached_reply() -> Result<Reply, IpcError> {
        Ok(Reply::Attached(AttachResponse {
            child_exited: false,
            exit_code: None,
        }))
    }

    /// (a) The attach was answered before the connection ended.
    async fn answered_before_the_end() -> Observed {
        let (mut rig, mut restoring, stage) = chain_restoring(false, REVISION).await;
        let (outcome, ()) = tokio::join!(
            stage,
            answer_next(
                &restoring.generation,
                &mut restoring.peer,
                PendingKind::Attach,
                attached_reply()
            )
        );
        rig.owner.queue.clear();
        rig.lose(&mut restoring, EndKind::ReadError);
        rig.owner.finish(outcome);
        rig.observe(2, Vec::new())
    }

    /// (b) An answer to a request that is not the step does not count, and the
    /// connection ended before the step was answered.
    async fn ended_before_the_step_was_answered() -> (Observed, bool) {
        let (mut rig, mut restoring, mut stage) = chain_restoring(false, REVISION).await;
        let attach = ClientMessage::AttachView {
            window_id: WINDOW.to_owned(),
            size: None,
        };
        let user_attach = restoring.generation.request(&attach, PendingKind::Attach);
        let (answered, ()) = tokio::join!(
            user_attach,
            answer_next(
                &restoring.generation,
                &mut restoring.peer,
                PendingKind::Attach,
                attached_reply()
            )
        );
        assert!(answered.is_ok(), "{answered:?}");

        // The step's request is written and not answered.
        tokio::select! {
            _ = &mut stage => panic!("the step ended before its request was answered"),
            _ = restoring.peer.read_request() => {}
        }
        let awaiting = restoring
            .generation
            .awaiting()
            .expect("the step's request awaits");
        rig.owner.queue.clear();
        rig.lose(&mut restoring, EndKind::ReadError);
        let outcome = stage.await;
        rig.owner.finish(outcome);
        let late = restoring.generation.settle(
            Some(awaiting.correlation_id),
            PendingKind::Attach,
            attached_reply(),
        );
        (rig.observe(2, Vec::new()), late == Settled::Unanswered)
    }

    /// G: the control connection's approver registration was answered before
    /// the connection ended.
    async fn registered_before_the_end() -> Observed {
        let mut rig = Rig::new(ClientKind::Control);
        rig.want(ConnectMode::Once, Until::Open);
        let attempt = rig.next_attempt();
        let mut conn = rig.begin(attempt, false);
        rig.probe(&mut conn, REVISION).await;
        let mut held = rig.carry_out().await;
        let step = held.pop().expect("the registration");
        assert!(
            matches!(step, Step::Do(Action::Register { .. })),
            "{}",
            describe(&step)
        );
        assert!(held.is_empty());
        let stage = rig
            .owner
            .execute(step)
            .expect("the registration is a stage");
        let (outcome, ()) = tokio::join!(
            stage,
            answer_next(
                &conn.generation,
                &mut conn.peer,
                PendingKind::RegisterApprover,
                Ok(Reply::ApproverRegistered(Vec::new()))
            )
        );
        rig.owner.queue.clear();
        rig.lose(&mut conn, EndKind::ReadError);
        rig.owner.finish(outcome);
        rig.observe(2, Vec::new())
    }

    /// (c)/(d) The peer closed, so the step's write failed, and the daemon
    /// had sent `superseded` before it closed. With `earlier_write`, an input
    /// that is not the step failed first.
    async fn closed_peer_then_superseded(order: usize, earlier_write: bool) -> Observed {
        let (mut rig, mut restoring, stage) = chain_restoring(true, REVISION).await;
        restoring.peer.stop_reading();
        if earlier_write {
            let write = restoring
                .generation
                .write(&ClientMessage::Resize { cols: 80, rows: 24 })
                .await;
            assert!(write.is_err(), "the peer is closed");
        }
        let outcome = stage.await;
        let mut notes = Vec::new();
        check(
            &mut notes,
            rig.ends(EndKind::WriteFailed).0 == 0,
            "a write that failed because the peer closed told the owner the generation ended, before its reader saw an end",
        );
        check(
            &mut notes,
            !restoring.reader_ended().await,
            "the reader ended before the daemon sent or closed anything",
        );
        restoring.peer.send(&superseded_frame()).await;
        restoring.peer.stop_writing();
        check(
            &mut notes,
            restoring.reader_ended().await,
            "the reader did not end on the superseded frame and the close",
        );
        let (ended, superseded) = rig.ends(EndKind::Superseded);
        check(
            &mut notes,
            ended == 1 && superseded == 1,
            &format!(
                "{ended} ends reached the inbox, {superseded} of them superseded; wanted exactly one, superseded"
            ),
        );
        let mut items: Vec<Item> = vec![Item::Result(outcome)];
        items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
        let count = items.len();
        eprintln!(
            "window of the generation when the owner hears of its end: {:?}",
            restoring.generation.window_id()
        );
        rig.deliver(arrangement(items, order));
        rig.observe(count, notes)
    }

    /// (e)/(f) The step was refused (by the daemon, or by the revision's
    /// gate) and the connection was taken over.
    async fn refused_then_superseded(order: usize, gate: bool) -> Observed {
        let revision = if gate { BEFORE_FRAMES } else { REVISION };
        let (mut rig, mut restoring, stage) = chain_restoring(false, revision).await;
        let outcome = if gate {
            stage.await
        } else {
            let refusal = Err(IpcError::Rejected {
                code: "window_not_found".to_owned(),
                message: String::new(),
                rejected_argument: None,
            });
            let (outcome, ()) = tokio::join!(
                stage,
                answer_next(
                    &restoring.generation,
                    &mut restoring.peer,
                    PendingKind::Attach,
                    refusal
                )
            );
            outcome
        };
        restoring.report(EndKind::Superseded);
        rig.collect();
        let mut items: Vec<Item> = vec![Item::Result(outcome)];
        items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
        let count = items.len();
        rig.deliver(arrangement(items, order));
        rig.observe(count, Vec::new())
    }

    /// (f) alone: the revision gate's refusal is the step's answer, so the
    /// attach fails on it with no news of the connection's end.
    async fn refused_by_the_gate_and_never_ended() -> Observed {
        let (mut rig, _restoring, stage) = chain_restoring(false, BEFORE_FRAMES).await;
        let outcome = stage.await;
        rig.deliver(vec![Item::Result(outcome)]);
        rig.observe(1, Vec::new())
    }

    /// (g) The step's write failed on this side while the daemon is alive.
    async fn failed_write_with_a_live_peer(order: usize) -> Observed {
        let (mut rig, mut restoring, stage) = chain_restoring(true, REVISION).await;
        restoring.peer.make_writes_fail();
        let outcome = stage.await;
        let mut notes = Vec::new();
        check(
            &mut notes,
            rig.ends(EndKind::WriteFailed).0 == 0,
            "a write that failed here told the owner the generation ended, before its reader saw an end",
        );
        check(
            &mut notes,
            restoring.reader_ended().await,
            "the reader did not end after a write that failed in a way that does not say the peer closed",
        );
        let (ended, write_failed) = rig.ends(EndKind::WriteFailed);
        check(
            &mut notes,
            ended == 1 && write_failed == 1,
            &format!(
                "{ended} ends reached the inbox, {write_failed} of them a failed write; wanted exactly one"
            ),
        );
        let mut items: Vec<Item> = vec![Item::Result(outcome)];
        items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
        let count = items.len();
        rig.deliver(arrangement(items, order));
        rig.observe(count, notes)
    }

    /// (h)/(i) An open window whose input failed to write, with `superseded`
    /// already sent by the daemon. In (h) the peer closed; in (i) the write
    /// failed here and the daemon wrote `superseded` before it.
    async fn input_failed_then_superseded(order: usize, closed_peer: bool) -> Observed {
        let (mut rig, mut open) = open_window().await;
        let mut notes = Vec::new();
        if closed_peer {
            open.peer.stop_reading();
        } else {
            check(
                &mut notes,
                !open.reader_ended().await,
                "the reader ended before the daemon sent or closed anything",
            );
            open.peer.send(&superseded_frame()).await;
            open.peer.make_writes_fail();
        }
        let write = open
            .generation
            .write(&ClientMessage::Resize { cols: 80, rows: 24 })
            .await;
        assert!(write.is_err(), "the write was made to fail");
        check(
            &mut notes,
            rig.ends(EndKind::WriteFailed).0 == 0,
            "a failed input write told the owner the generation ended, before its reader saw an end",
        );
        if closed_peer {
            check(
                &mut notes,
                !open.reader_ended().await,
                "the reader ended before the daemon sent or closed anything",
            );
            open.peer.send(&superseded_frame()).await;
            open.peer.stop_writing();
        }
        check(
            &mut notes,
            open.reader_ended().await,
            "the reader did not end on the superseded frame",
        );
        let (ended, superseded) = rig.ends(EndKind::Superseded);
        check(
            &mut notes,
            ended == 1 && superseded == 1,
            &format!(
                "{ended} ends reached the inbox, {superseded} of them superseded; wanted exactly one, superseded"
            ),
        );
        let items: Vec<Item> = std::mem::take(&mut rig.news)
            .into_iter()
            .map(Item::End)
            .collect();
        let count = items.len();
        rig.deliver(arrangement(items, order));
        rig.observe(count, notes)
    }

    fn superseded_without_a_chain(observed: &Observed) -> Result<(), String> {
        all(vec![
            require(
                observed.moves_to(LinkState::Superseded),
                "the window was not reported as taken over by another GUI",
            ),
            require(
                !observed.moves_to(LinkState::Connecting),
                "the window was reported as reconnecting after it was taken over",
            ),
            require(
                !observed.actions.iter().any(|action| {
                    matches!(
                        action,
                        Action::MoveLink {
                            to: LinkState::Lost(_),
                            ..
                        }
                    )
                }),
                "the window was reported as lost after it was taken over",
            ),
            require(
                !observed.tries(),
                "a reconnect started after the window was taken over",
            ),
        ])
    }

    fn lost_to_a_failed_reconnect(observed: &Observed) -> Result<(), String> {
        all(vec![
            require(
                observed.lost(),
                "the window was not reported as lost to a failed reconnect",
            ),
            require(
                !observed.moves_to(LinkState::Superseded),
                "a refused attach was reported as a takeover",
            ),
            require(
                !observed.tries(),
                "a reconnect started after the chain's budget was spent",
            ),
        ])
    }

    #[tokio::test]
    async fn the_owner_settles_a_generation_the_same_whichever_news_of_it_arrives_first() {
        let a = answered_before_the_end().await;
        let (b, late_answer_dropped) = ended_before_the_step_was_answered().await;
        let g = registered_before_the_end().await;
        let results: Vec<(&str, Result<(), String>)> = vec![
            (
                "(a) answered before the end",
                all(vec![
                    require(
                        a.moves_to(LinkState::Connecting) && a.tries_in_retry(),
                        "a window whose restoring attach was answered before the connection ended did not start a new chain",
                    ),
                    require(
                        !a.actions.iter().any(|action| {
                            matches!(
                                action,
                                Action::MoveLink {
                                    to: LinkState::Lost(_),
                                    ..
                                }
                            )
                        }),
                        "a window whose restoring attach was answered was reported as lost",
                    ),
                ]),
            ),
            (
                "(b) ended before the step was answered",
                all(vec![
                    require(
                        b.lost() && !b.tries(),
                        "an attach that no answer reached did not spend the chain's budget",
                    ),
                    require(
                        late_answer_dropped,
                        "an answer after the generation ended was not dropped",
                    ),
                ]),
            ),
            (
                "(G) registered before the end",
                require(
                    g.tries(),
                    "a control connection that registered before it ended did not start a new chain",
                ),
            ),
            (
                "(c) closed peer, then superseded",
                across_orders(
                    |order| closed_peer_then_superseded(order, false),
                    superseded_without_a_chain,
                )
                .await,
            ),
            (
                "(d) earlier failed write, then superseded",
                across_orders(
                    |order| closed_peer_then_superseded(order, true),
                    superseded_without_a_chain,
                )
                .await,
            ),
            (
                "(e) refused by the daemon, then superseded",
                across_orders(
                    |order| refused_then_superseded(order, false),
                    lost_to_a_failed_reconnect,
                )
                .await,
            ),
            (
                "(f) refused by the revision gate, then superseded",
                all(vec![
                    across_orders(
                        |order| refused_then_superseded(order, true),
                        lost_to_a_failed_reconnect,
                    )
                    .await,
                    lost_to_a_failed_reconnect(&refused_by_the_gate_and_never_ended().await),
                ]),
            ),
            (
                "(g) failed write with a live peer",
                across_orders(failed_write_with_a_live_peer, lost_to_a_failed_reconnect).await,
            ),
            (
                "(h) input failed, peer closed, then superseded",
                across_orders(
                    |order| input_failed_then_superseded(order, true),
                    superseded_without_a_chain,
                )
                .await,
            ),
            (
                "(i) input failed here, superseded already sent",
                across_orders(
                    |order| input_failed_then_superseded(order, false),
                    superseded_without_a_chain,
                )
                .await,
            ),
        ];
        let mut failed = Vec::new();
        for (name, result) in &results {
            match result {
                Ok(()) => eprintln!("scene passed: {name}"),
                Err(problem) => {
                    eprintln!("scene FAILED: {name}\n  {problem}");
                    failed.push(*name);
                }
            }
        }
        assert!(failed.is_empty(), "scenes that failed: {failed:?}");
    }
}
