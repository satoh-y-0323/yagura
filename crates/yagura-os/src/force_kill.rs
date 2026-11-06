//! The decision behind [`crate::schedule_force_kill`], kept free of clocks
//! and threads so the caller supplies how to wait and how to kill.

use std::time::Duration;

use crate::ReapGate;

/// Longest single wait between two looks at the gate, so a child that dies
/// from the caller's first signal and is reaped stops the escalation long
/// before the deadline instead of at it.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Waits out `grace` through `wait`, in steps of at most [`POLL_INTERVAL`] and
/// outside the gate's lock, then calls `kill` under the lock -- unless the
/// child is seen reaped before a step or under the lock.
///
/// The grace counts the waits asked for, so a `wait` that oversleeps can only
/// delay the kill, never bring it forward.
pub(crate) fn escalate_after_grace(
    grace: Duration,
    gate: &ReapGate,
    mut wait: impl FnMut(Duration),
    kill: impl FnOnce(),
) {
    let mut remaining = grace;
    while !remaining.is_zero() {
        if gate.is_reaped() {
            return;
        }
        let step = remaining.min(POLL_INTERVAL);
        wait(step);
        remaining -= step;
    }
    let _ = gate.signal_unless_reaped(kill);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::escalate_after_grace;
    use crate::ReapGate;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Wait(Duration),
        Kill,
    }

    /// Fails instead of spinning when the decision never reaches its end.
    const MAX_WAITS: usize = 1_000_000;

    fn record_wait(events: &RefCell<Vec<Event>>, step: Duration) {
        let mut events = events.borrow_mut();
        assert!(
            events.len() < MAX_WAITS,
            "the escalation kept asking to wait without ever finishing"
        );
        events.push(Event::Wait(step));
    }

    fn waited(events: &[Event]) -> Duration {
        events
            .iter()
            .map(|event| match event {
                Event::Wait(step) => *step,
                Event::Kill => Duration::ZERO,
            })
            .sum()
    }

    fn kills(events: &[Event]) -> usize {
        events.iter().filter(|event| **event == Event::Kill).count()
    }

    #[test]
    fn an_exit_observed_before_the_call_is_never_killed() {
        let events = RefCell::new(Vec::new());
        let gate = ReapGate::new();
        gate.reap_with(|| {});

        escalate_after_grace(
            Duration::from_millis(100),
            &gate,
            |step| record_wait(&events, step),
            || events.borrow_mut().push(Event::Kill),
        );

        let events = events.into_inner();
        assert_eq!(kills(&events), 0, "events: {events:?}");
    }

    #[test]
    fn an_exit_observed_during_the_grace_stops_the_escalation_before_the_deadline() {
        let grace = Duration::from_secs(3600);
        let events = RefCell::new(Vec::new());
        let gate = ReapGate::new();

        escalate_after_grace(
            grace,
            &gate,
            |step| {
                record_wait(&events, step);
                gate.reap_with(|| {});
            },
            || events.borrow_mut().push(Event::Kill),
        );

        let events = events.into_inner();
        assert_eq!(kills(&events), 0, "events: {events:?}");
        assert!(
            waited(&events) < grace,
            "an exit seen during the grace must end the wait before the deadline; events: \
             {events:?}"
        );
    }

    #[test]
    fn an_exit_observed_by_the_last_wait_is_never_killed() {
        let grace = Duration::from_millis(100);
        let events = RefCell::new(Vec::new());
        let gate = ReapGate::new();

        escalate_after_grace(
            grace,
            &gate,
            |step| {
                record_wait(&events, step);
                if waited(&events.borrow()) >= grace {
                    gate.reap_with(|| {});
                }
            },
            || events.borrow_mut().push(Event::Kill),
        );

        let events = events.into_inner();
        assert!(
            waited(&events) >= grace,
            "precondition: the grace was waited out; events: {events:?}"
        );
        assert_eq!(
            kills(&events),
            0,
            "the gate must be looked at again right before the kill; events: {events:?}"
        );
    }

    #[test]
    fn a_zero_grace_kills_without_waiting() {
        let events = RefCell::new(Vec::new());
        let gate = ReapGate::new();

        escalate_after_grace(
            Duration::ZERO,
            &gate,
            |step| record_wait(&events, step),
            || events.borrow_mut().push(Event::Kill),
        );

        assert_eq!(events.into_inner(), vec![Event::Kill]);
    }

    #[test]
    fn the_kill_comes_once_and_only_after_the_whole_grace_has_been_waited() {
        let grace = Duration::from_millis(100);
        let events = RefCell::new(Vec::new());
        let gate = ReapGate::new();

        escalate_after_grace(
            grace,
            &gate,
            |step| record_wait(&events, step),
            || events.borrow_mut().push(Event::Kill),
        );

        let events = events.into_inner();
        assert_eq!(kills(&events), 1, "events: {events:?}");
        assert_eq!(
            events.last(),
            Some(&Event::Kill),
            "nothing may follow the kill; events: {events:?}"
        );
        assert!(
            waited(&events) >= grace,
            "the kill came before the grace was waited out; events: {events:?}"
        );
    }
}
