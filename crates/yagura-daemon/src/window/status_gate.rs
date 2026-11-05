//! The one place a window's confirmed status is written.

use std::sync::Mutex;

use tokio::sync::{broadcast, watch};
use yagura_protocol::{DetectedStatus, UnknownReason};

use super::{STATUS_BROADCAST_CAPACITY, window_status_wire};
use crate::detector::engine::ConfirmedStatus;
use crate::handoff::{ReadyHoldLatch, StartupEvidence};
use crate::pty::lock_recover;
use crate::waiter::StatusAt;

/// What one write to a [`StatusGate`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Publication {
    /// A status other than `Exited` was published.
    Changed,
    /// `Exited` was published, for the first and only time.
    BecameExited,
    /// Nothing was published: the window had already exited, or the
    /// fail-safe status was already the current one.
    Dropped,
}

/// A window's confirmed-status sinks (the current value, the fan-out to
/// attach holders and the startup latch) behind one critical section.
///
/// Every write takes the section, so the fan-out order always matches the
/// generation order, and `Exited` is terminal: once published, every later
/// write is dropped before it touches any sink or logs.
pub(crate) struct StatusGate {
    current: watch::Sender<StatusAt>,
    changes: broadcast::Sender<StatusAt>,
    ready_hold: ReadyHoldLatch,
    // A std mutex: no write awaits, and none of it takes another lock.
    section: Mutex<()>,
}

impl StatusGate {
    /// A gate whose current value is `initial` as of `at`, generation 0.
    pub(crate) fn new(
        initial: DetectedStatus,
        at: tokio::time::Instant,
        ready_hold: ReadyHoldLatch,
    ) -> Self {
        let (current, _) = watch::channel(StatusAt {
            status: initial,
            changed_at: at,
            evidence_since: at,
            generation: 0,
        });
        let (changes, _) = broadcast::channel(STATUS_BROADCAST_CAPACITY);
        Self {
            current,
            changes,
            ready_hold,
            section: Mutex::new(()),
        }
    }

    /// Publishes one confirmed status, unless the window has already exited.
    ///
    /// The startup latch is offered the *outgoing* pair first, the only point
    /// that sees how long a status actually lasted. The status and the
    /// instant it changed then go out as one value on the current value and
    /// then the fan-out, so an observer that has seen the new status can
    /// never read the previous change's age. Logs one `window status
    /// confirmed` event per published transition.
    pub(crate) fn publish(&self, window_id: &str, confirmed: ConfirmedStatus) -> Publication {
        let _section = lock_recover(&self.section);
        if self.current.borrow().status == DetectedStatus::Exited {
            return Publication::Dropped;
        }
        self.publish_in_section(window_id, confirmed)
    }

    /// Fail-safe backstop for a window whose detection stopped reporting:
    /// publishes `Unknown(DetectorStopped)`, unless the window has already
    /// exited or already reports exactly that. A window at a *different*
    /// `Unknown` reason is still overwritten, so the reason this backstop
    /// fired is never masked by the one before it.
    pub(crate) fn publish_loss(&self, window_id: &str) -> Publication {
        let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
        let _section = lock_recover(&self.section);
        let current = self.current.borrow().status;
        if current == DetectedStatus::Exited || current == stopped {
            return Publication::Dropped;
        }
        tracing::warn!(
            window_id = %window_id,
            "the detection driver stopped reporting for a live window; failing its status back \
             to unknown"
        );
        self.publish_in_section(
            window_id,
            ConfirmedStatus {
                status: stopped,
                evidence_since: tokio::time::Instant::now(),
            },
        )
    }

    fn publish_in_section(&self, window_id: &str, confirmed: ConfirmedStatus) -> Publication {
        let now = tokio::time::Instant::now();
        let status = confirmed.status;
        // Never later than `changed_at`.
        let evidence_since = confirmed.evidence_since.min(now);
        self.ready_hold.observe(*self.current.borrow(), now);
        // `send_modify` (not `send`): no receiver is kept alive and `send`
        // no-ops when there are none, and the new generation is derived from
        // the value being replaced while that value's write lock is held.
        let mut generation = 0;
        self.current.send_modify(|slot| {
            generation = slot.generation.saturating_add(1);
            *slot = StatusAt {
                status,
                changed_at: now,
                evidence_since,
                generation,
            };
        });
        let _ = self.changes.send(StatusAt {
            status,
            changed_at: now,
            evidence_since,
            generation,
        });
        let wire_status = window_status_wire(status.status());
        match status.unknown_reason() {
            Some(reason) => tracing::info!(
                window_id = %window_id,
                status = %wire_status,
                unknown_reason = %reason.as_wire(),
                "window status confirmed"
            ),
            None => tracing::info!(
                window_id = %window_id,
                status = %wire_status,
                "window status confirmed"
            ),
        }
        if status == DetectedStatus::Exited {
            Publication::BecameExited
        } else {
            Publication::Changed
        }
    }

    /// The current pair, read as one value.
    pub(crate) fn snapshot(&self) -> StatusAt {
        *self.current.borrow()
    }

    /// A receiver that always observes the latest pair.
    pub(crate) fn watch(&self) -> watch::Receiver<StatusAt> {
        self.current.subscribe()
    }

    /// A receiver of every published pair, in generation order.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<StatusAt> {
        self.changes.subscribe()
    }

    /// What the current pair says about the startup phase, latching if it
    /// already proves it.
    pub(crate) fn startup_evidence(&self) -> StartupEvidence {
        self.ready_hold
            .observe(self.snapshot(), tokio::time::Instant::now())
    }

    /// Records that the window is past its startup phase.
    pub(crate) fn note_left_startup_phase(&self) {
        self.ready_hold.latch();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A gate plus a subscriber on its fan-out, its pair set well in the past
    /// so that "the timestamp moved" is decidable whatever the clock's
    /// resolution.
    struct Probe {
        gate: StatusGate,
        fanout: broadcast::Receiver<StatusAt>,
        initial_changed_at: tokio::time::Instant,
    }

    impl Probe {
        fn new(status: DetectedStatus, hold: Duration) -> Self {
            let initial_changed_at = tokio::time::Instant::now()
                .checked_sub(Duration::from_secs(60))
                .expect("the test clock must support a 60s offset into the past");
            let gate = StatusGate::new(status, initial_changed_at, ReadyHoldLatch::new(hold));
            let fanout = gate.subscribe();
            Self {
                gate,
                fanout,
                initial_changed_at,
            }
        }

        fn publish(&self, status: DetectedStatus) -> Publication {
            self.gate.publish(
                "w-test",
                ConfirmedStatus {
                    status,
                    evidence_since: tokio::time::Instant::now(),
                },
            )
        }

        fn fanned_out_status(&mut self) -> Option<DetectedStatus> {
            self.fanout.try_recv().ok().map(|at| at.status)
        }

        fn timestamp_moved(&self) -> bool {
            self.gate.snapshot().changed_at > self.initial_changed_at
        }
    }

    #[test]
    fn publish_loss_fails_a_live_window_back_to_unknown() {
        let mut probe = Probe::new(DetectedStatus::Running, Duration::ZERO);

        assert_eq!(probe.gate.publish_loss("w-test"), Publication::Changed);

        let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
        assert_eq!(probe.gate.snapshot().status, stopped);
        assert_eq!(probe.fanned_out_status(), Some(stopped));
        assert!(
            probe.timestamp_moved(),
            "the fail-safe transition is a status change and must move the change instant"
        );
    }

    #[test]
    fn publish_loss_never_overwrites_an_exited_window() {
        let mut probe = Probe::new(DetectedStatus::Exited, Duration::ZERO);

        assert_eq!(probe.gate.publish_loss("w-test"), Publication::Dropped);

        assert_eq!(probe.gate.snapshot().status, DetectedStatus::Exited);
        assert_eq!(probe.fanned_out_status(), None, "nothing may be fanned out");
        assert!(!probe.timestamp_moved(), "the change instant must stay put");
    }

    #[test]
    fn publish_loss_is_idempotent_once_the_window_is_already_unknown() {
        let mut probe = Probe::new(DetectedStatus::Running, Duration::ZERO);

        probe.gate.publish_loss("w-test");
        let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
        assert_eq!(probe.fanned_out_status(), Some(stopped));

        assert_eq!(probe.gate.publish_loss("w-test"), Publication::Dropped);

        assert_eq!(probe.gate.snapshot().status, stopped);
        assert_eq!(
            probe.fanned_out_status(),
            None,
            "a second call at Unknown(DetectorStopped) must fan out nothing"
        );
    }

    #[test]
    fn publish_passes_the_outgoing_pair_to_the_ready_hold_latch() {
        let hold = Duration::from_millis(1000);
        let past = tokio::time::Instant::now()
            .checked_sub(Duration::from_millis(1500))
            .expect("the test clock must support a 1500ms offset into the past");
        let gate = StatusGate::new(
            DetectedStatus::WaitingInput,
            past,
            ReadyHoldLatch::new(hold),
        );

        gate.publish(
            "w-test",
            ConfirmedStatus {
                status: DetectedStatus::Running,
                evidence_since: tokio::time::Instant::now(),
            },
        );

        assert!(
            gate.ready_hold.is_latched(),
            "publish must observe the outgoing pair against the ready-hold latch before \
             publishing the new status, so a window that held ready for the full duration \
             latches even though the new status is not itself ready"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn publish_restarts_the_age_at_every_transition_on_the_paused_clock() {
        let opened_at = tokio::time::Instant::now();
        let gate = StatusGate::new(
            DetectedStatus::Running,
            opened_at,
            ReadyHoldLatch::new(Duration::ZERO),
        );
        let mut fanout = gate.subscribe();
        let publish = |status: DetectedStatus| {
            gate.publish(
                "w-test",
                ConfirmedStatus {
                    status,
                    evidence_since: tokio::time::Instant::now(),
                },
            );
        };

        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            gate.snapshot().since_ms(tokio::time::Instant::now()),
            5_000,
            "the age of the opening status grows with the clock"
        );

        publish(DetectedStatus::Idle);
        assert_eq!(gate.snapshot().since_ms(tokio::time::Instant::now()), 0);
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(gate.snapshot().since_ms(tokio::time::Instant::now()), 250);

        publish(DetectedStatus::Running);
        assert_eq!(
            gate.snapshot().since_ms(tokio::time::Instant::now()),
            0,
            "a new transition must restart the age"
        );
        let fanned_out = fanout.try_recv().and_then(|_| fanout.try_recv());
        assert_eq!(
            fanned_out.map(|at| at.since_ms(tokio::time::Instant::now())),
            Ok(0),
            "the fan-out value must carry the same restarted age"
        );
    }

    #[test]
    fn nothing_published_after_exited_reaches_any_sink() {
        let mut probe = Probe::new(DetectedStatus::Exited, Duration::ZERO);
        let before = probe.gate.snapshot();

        assert_eq!(probe.publish(DetectedStatus::Running), Publication::Dropped);
        assert_eq!(
            probe.publish(DetectedStatus::WaitingInput),
            Publication::Dropped
        );
        assert_eq!(probe.gate.publish_loss("w-test"), Publication::Dropped);

        let after = probe.gate.snapshot();
        assert_eq!(after.status, DetectedStatus::Exited);
        assert_eq!(after.changed_at, before.changed_at);
        assert_eq!(after.generation, before.generation);
        assert_eq!(probe.fanned_out_status(), None, "nothing may be fanned out");
    }

    #[test]
    fn exited_is_published_once_and_a_second_one_fans_out_nothing() {
        let mut probe = Probe::new(DetectedStatus::Running, Duration::ZERO);

        assert_eq!(
            probe.publish(DetectedStatus::Exited),
            Publication::BecameExited
        );
        assert_eq!(probe.fanned_out_status(), Some(DetectedStatus::Exited));
        let generation = probe.gate.snapshot().generation;

        assert_eq!(probe.publish(DetectedStatus::Exited), Publication::Dropped);

        assert_eq!(
            probe.fanned_out_status(),
            None,
            "a second Exited must fan out nothing"
        );
        assert_eq!(probe.gate.snapshot().generation, generation);
    }
}
