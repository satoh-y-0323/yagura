//! The pure transition function of a client's connection lifetime: no I/O,
//! no lock, no clock and no task.

use super::super::{ConnectMode, speaks_this_wire};
use crate::constants::RECONNECT_ATTEMPT_LIMIT;
use crate::events::LinkLostReason;
use crate::window_record::LinkState;

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
    /// No registration: a window connection never registers, and a control
    /// connection to a daemon of another wire revision is not let register.
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

/// An answer the daemon, or the wire gate, gave an opening step before
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
    /// The daemon, or the wire gate, answered the step in flight in a way
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

/// One step the lifecycle returns, in order: work for the owner to carry
/// out, or the answer to a want. `E` tells an answer's failure: the
/// lifecycle's [`Failure`], and the owner's
/// [`IpcError`](super::super::IpcError) once the failed
/// attempt's error is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step<E> {
    Do(Action),
    /// `opened` names the generation a caller reads as open once answered,
    /// so the answer waits until that generation is published.
    Answer {
        id: WantId,
        result: Result<(), E>,
        opened: Option<u64>,
    },
}

/// One thing the owner does. Answers are not actions, so carrying out an
/// action cannot drop one.
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
    pub(crate) actions: Vec<Step<Failure>>,
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
    /// The weakest form the chain's attempts take.
    floor: ConnectMode,
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
                    out.push(Step::Do(Action::End { generation }));
                }
                self.answer_where(Err(Failure::NotConnected), None, |_| true, &mut out);
                self.chain = None;
                self.phase = Phase::Retired;
                out.push(Step::Do(Action::AckRetire));
            }
        }
        Transition {
            actions: out,
            public: self.public(),
        }
    }

    pub(super) fn public(&self) -> Public {
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

    fn on_want(&mut self, want: Want, out: &mut Vec<Step<Failure>>) {
        match self.phase {
            Phase::Retired => out.push(Step::Answer {
                id: want.id,
                result: Err(Failure::NotConnected),
                opened: None,
            }),
            Phase::Open { generation, .. } | Phase::Restoring { generation, .. } => {
                out.push(Step::Answer {
                    id: want.id,
                    result: Ok(()),
                    opened: Some(generation),
                });
            }
            Phase::Opening { .. } if want.until == Until::Reached => out.push(Step::Answer {
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

    fn on_connected(&mut self, attempt: u64, out: &mut Vec<Step<Failure>>) {
        let Phase::Connecting {
            attempt: current,
            mode,
        } = self.phase
        else {
            out.push(Step::Do(Action::Discard { attempt }));
            return;
        };
        if current != attempt {
            out.push(Step::Do(Action::Discard { attempt }));
            return;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.daemon_answers = true;
        out.push(Step::Do(Action::Begin {
            attempt,
            generation,
        }));
        self.answer_where(Ok(()), None, |want| want.until == Until::Reached, out);
        out.push(Step::Do(Action::ProbeRevision { generation }));
        self.phase = Phase::Opening {
            mode,
            generation,
            revision: None,
        };
    }

    fn on_probed(&mut self, generation: u64, revision: u32, out: &mut Vec<Step<Failure>>) {
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
                out.push(Step::Do(Action::Register { generation }));
            }
            // A window of a daemon of another wire is not opened: nothing
            // but the probe may be written to it.
            ClientKind::Window if !speaks_this_wire(Some(revision)) => {
                self.failed_to_open(mode, generation, out);
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
        out: &mut Vec<Step<Failure>>,
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

    fn on_step_failed(&mut self, generation: u64, out: &mut Vec<Step<Failure>>) {
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
    fn take(&mut self, generation: u64, taken: TakenAnswer, out: &mut Vec<Step<Failure>>) {
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
        out: &mut Vec<Step<Failure>>,
    ) {
        out.push(Step::Do(Action::AnnounceStart { generation }));
        out.push(Step::Do(Action::ResendPalettes { generation }));
        out.push(Step::Do(Action::Open { generation }));
        self.answer_where(Ok(()), Some(generation), |_| true, out);
        let restore = self.chain.as_ref().and_then(|chain| chain.restore.clone());
        match restore {
            Some(window_id) => {
                out.push(Step::Do(Action::RestoreAttach {
                    generation,
                    window_id,
                }));
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
        out: &mut Vec<Step<Failure>>,
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
            out.push(Step::Do(Action::End { generation }));
            if let Some(window_id) = window_id {
                out.push(Step::Do(Action::MoveLink {
                    window_id,
                    to: LinkState::Superseded,
                }));
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

        // A daemon of another wire is one this GUI cannot use, so once it is
        // gone the chain starts one of this build's when none answers.
        let floor = match self.phase {
            Phase::Open { revision, .. } if !speaks_this_wire(Some(revision)) => ConnectMode::Spawn,
            _ => ConnectMode::Retry,
        };
        out.push(Step::Do(Action::End { generation }));
        self.phase = Phase::Idle;
        let restore = match (self.kind, window_id) {
            (ClientKind::Control, _) => None,
            (ClientKind::Window, Some(window_id)) => Some(window_id),
            // A window generation that holds no attach has nothing to restore.
            (ClientKind::Window, None) => return,
        };
        if let Some(window_id) = &restore {
            out.push(Step::Do(Action::MoveLink {
                window_id: window_id.clone(),
                to: LinkState::Connecting,
            }));
        }
        self.chain = Some(Chain {
            attempts: 1,
            restore,
            floor,
        });
        let mode = self.chain_mode();
        self.start(mode, out);
    }

    fn failed_to_open(&mut self, mode: ConnectMode, generation: u64, out: &mut Vec<Step<Failure>>) {
        out.push(Step::Do(Action::End { generation }));
        self.attempt_failed(mode, Failure::NotConnected, out);
    }

    /// Answers the wants the failed attempt covers and starts the next attempt:
    /// the chain's next one while it has budget, then one for the strongest
    /// want the failed attempt did not cover.
    fn attempt_failed(&mut self, mode: ConnectMode, error: Failure, out: &mut Vec<Step<Failure>>) {
        self.answer_where(Err(error), None, |want| covers(mode, want.mode), out);
        self.phase = Phase::Idle;
        if let Some(chain) = self.chain.take() {
            if chain.attempts < RECONNECT_ATTEMPT_LIMIT {
                self.chain = Some(Chain {
                    attempts: chain.attempts + 1,
                    restore: chain.restore,
                    floor: chain.floor,
                });
                let mode = self.chain_mode();
                self.start(mode, out);
                return;
            }
            if let Some(window_id) = chain.restore {
                out.push(Step::Do(Action::MoveLink {
                    window_id,
                    to: LinkState::Lost(LinkLostReason::ReconnectFailed),
                }));
            }
        }
        if let Some(mode) = self.strongest_want() {
            self.start(mode, out);
        }
    }

    /// A chain takes its floor, and starts the daemon when a waiting want
    /// asks for it.
    fn chain_mode(&self) -> ConnectMode {
        let floor = self
            .chain
            .as_ref()
            .map_or(ConnectMode::Retry, |chain| chain.floor);
        match self.strongest_want() {
            Some(mode) if strength(mode) > strength(floor) => mode,
            _ => floor,
        }
    }

    fn strongest_want(&self) -> Option<ConnectMode> {
        self.wants
            .iter()
            .map(|want| want.mode)
            .max_by_key(|mode| strength(*mode))
    }

    fn start(&mut self, mode: ConnectMode, out: &mut Vec<Step<Failure>>) {
        let attempt = self.next_attempt;
        self.next_attempt += 1;
        self.phase = Phase::Connecting { attempt, mode };
        out.push(Step::Do(Action::Attempt { attempt, mode }));
    }

    fn answer_where(
        &mut self,
        result: Result<(), Failure>,
        opened: Option<u64>,
        answered: impl Fn(&Want) -> bool,
        out: &mut Vec<Step<Failure>>,
    ) {
        let (now, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.wants)
            .into_iter()
            .partition(|want| answered(want));
        self.wants = kept;
        out.extend(now.into_iter().map(|want| Step::Answer {
            id: want.id,
            result,
            opened,
        }));
    }
}

#[cfg(test)]
mod tests;
