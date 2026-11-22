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

    fn feed(&mut self, event: Event) -> Vec<Step<Failure>> {
        let transition = self.lifecycle.on(event);
        self.public = Some(transition.public);
        transition.actions
    }

    fn public(&self) -> &Public {
        self.public.as_ref().expect("an event was fed")
    }

    fn want(&mut self, mode: ConnectMode, until: Until) -> (WantId, Vec<Step<Failure>>) {
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
    fn reach(&mut self, attempt: u64) -> (u64, Vec<Step<Failure>>) {
        let actions = self.feed(Event::Connected { attempt });
        let generation = begun(&actions).expect("the attempt's connection begins a generation");
        (generation, actions)
    }

    /// Answers every opening step of `generation` so that it opens.
    fn settle(&mut self, generation: u64, registration: Registration) -> Vec<Step<Failure>> {
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
            actions.contains(&Step::Do(Action::Open { generation })),
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
        first: Vec<Step<Failure>>,
        mut fail: impl FnMut(&mut Run, u64) -> Vec<Step<Failure>>,
    ) -> (u32, Vec<Step<Failure>>) {
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

fn attempts(actions: &[Step<Failure>]) -> Vec<(u64, ConnectMode)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Step::Do(Action::Attempt { attempt, mode }) => Some((*attempt, *mode)),
            _ => None,
        })
        .collect()
}

fn attempt_in(actions: &[Step<Failure>]) -> Option<(u64, ConnectMode)> {
    let all = attempts(actions);
    assert!(all.len() <= 1, "one attempt at a time: {actions:?}");
    all.first().copied()
}

fn begun(actions: &[Step<Failure>]) -> Option<u64> {
    actions.iter().find_map(|action| match action {
        Step::Do(Action::Begin { generation, .. }) => Some(*generation),
        _ => None,
    })
}

fn answer_to(actions: &[Step<Failure>], id: WantId) -> Option<Result<(), Failure>> {
    let answers: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            Step::Answer {
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
fn opened_of(actions: &[Step<Failure>], id: WantId) -> Option<Option<u64>> {
    actions.iter().find_map(|action| match action {
        Step::Answer {
            id: answered,
            opened,
            ..
        } if *answered == id => Some(*opened),
        _ => None,
    })
}

/// The actions that open a generation, in the order they were returned.
fn opening(actions: &[Step<Failure>]) -> Vec<Action> {
    actions
        .iter()
        .filter_map(|step| match step {
            Step::Do(
                action @ (Action::Begin { .. }
                | Action::ProbeRevision { .. }
                | Action::Register { .. }
                | Action::AnnounceStart { .. }
                | Action::ResendPalettes { .. }
                | Action::Open { .. }
                | Action::RestoreAttach { .. }),
            ) => Some(action.clone()),
            _ => None,
        })
        .collect()
}

fn lost_link(window_id: &str) -> Step<Failure> {
    Step::Do(Action::MoveLink {
        window_id: window_id.to_owned(),
        to: LinkState::Lost(LinkLostReason::ReconnectFailed),
    })
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
    assert_eq!(actions, vec![Step::Do(Action::Discard { attempt: over })]);
    assert_eq!(run.public().state, PublicState::Absent);

    let (generation, _) = run.reach(current);
    run.settle(generation, Registration::Accepted);
    let actions = run.feed(Event::Connected { attempt: over });
    assert_eq!(actions, vec![Step::Do(Action::Discard { attempt: over })]);
    assert!(is_open(run.public()));
}

#[test]
fn a_generation_lost_after_its_attempt_finished_starts_exactly_one_chain() {
    for kind in LOST {
        for (label, mut run, generation, window) in finished_generations() {
            let actions = run.feed(ended(generation, kind, window));
            assert!(
                actions.contains(&Step::Do(Action::End { generation })),
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
            actions.contains(&Step::Do(Action::End { generation })),
            "{label}: {actions:?}"
        );
        assert_eq!(attempts(&actions), Vec::new(), "{label}");
        if let Some(window_id) = window {
            assert!(
                actions.contains(&Step::Do(Action::MoveLink {
                    window_id: window_id.to_owned(),
                    to: LinkState::Superseded,
                })),
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
            actions.contains(&Step::Do(Action::End { generation })),
            "{label}: {actions:?}"
        );
        assert!(
            actions.contains(&Step::Do(Action::AckRetire)),
            "{label}: {actions:?}"
        );
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
                    .any(|action| matches!(action, Step::Do(Action::MoveLink { .. }))),
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

    // A window of a daemon of another wire opens no generation: the
    // attempt ends and its want fails at the round trip.
    for other in [REVISION - 1, REVISION + 1] {
        let mut run = Run::window();
        let (id, attempt) = run.start(ConnectMode::Retry, Until::Open);
        let (generation, _) = run.reach(attempt);
        let probed = run.feed(Event::Probed {
            generation,
            revision: other,
        });
        assert!(
            probed.contains(&Step::Do(Action::End { generation })),
            "revision {other}: {probed:?}"
        );
        assert_eq!(
            answer_to(&probed, id),
            Some(Err(Failure::NotConnected)),
            "revision {other}"
        );
        assert_eq!(opened_of(&probed, id), Some(None), "revision {other}");
        assert_eq!(
            opening(&probed),
            Vec::new(),
            "revision {other}: no generation opens"
        );
        assert!(!is_open(run.public()), "revision {other}");
    }
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
fn an_end_before_its_attempt_took_a_finishing_answer_fails_it_and_one_after_starts_a_new_chain() {
    for kind in [EndKind::Eof, EndKind::UnreadableAnswer] {
        let (mut run, id, generation) = opening_control();
        let actions = run.feed(ended(generation, kind, None));
        assert!(
            actions.contains(&Step::Do(Action::End { generation })),
            "{kind:?}"
        );
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
            actions.contains(&Step::Do(Action::MoveLink {
                window_id: WINDOW.to_owned(),
                to: LinkState::Connecting,
            })),
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
                    let result =
                        result.map_or_else(|| step_failed(generation), |event| event(generation));
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
                        .filter(|action| **action == Step::Do(Action::End { generation }))
                        .count(),
                    1,
                    "{what}: {actions:?}"
                );

                let moves_to = |to: LinkState| {
                    window.is_some_and(|window_id| {
                        actions.contains(&Step::Do(Action::MoveLink {
                            window_id: window_id.to_owned(),
                            to,
                        }))
                    })
                };
                let lost = window.is_some_and(|window_id| actions.contains(&lost_link(window_id)));
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
                                .any(|action| matches!(action, Step::Do(Action::MoveLink { .. }))),
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
    assert!(actions.contains(&Step::Do(Action::AckRetire)));
    assert_eq!(run.public().state, PublicState::Retired);
    let (id, actions) = run.want(ConnectMode::Retry, Until::Open);
    assert_eq!(answer_to(&actions, id), Some(Err(Failure::NotConnected)));
    assert_eq!(attempts(&actions), Vec::new());

    let mut run = Run::window();
    let (waiting, attempt) = run.start(ConnectMode::Retry, Until::Open);
    let actions = run.feed(Event::Retire);
    assert!(actions.contains(&Step::Do(Action::AckRetire)));
    assert_eq!(
        answer_to(&actions, waiting),
        Some(Err(Failure::NotConnected))
    );
    let actions = run.feed(Event::Connected { attempt });
    assert_eq!(actions, vec![Step::Do(Action::Discard { attempt })]);
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
        actions.contains(&Step::Do(Action::RestoreAttach {
            generation,
            window_id: OTHER_WINDOW.to_owned(),
        })),
        "{actions:?}"
    );
    run.feed(Event::Restored { generation });

    let actions = run.feed(ended(generation, EndKind::Eof, None));
    assert!(
        actions.contains(&Step::Do(Action::End { generation })),
        "{actions:?}"
    );
    assert_eq!(attempts(&actions), Vec::new());
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Step::Do(Action::MoveLink { .. }))),
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
