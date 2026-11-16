use tokio::time::Instant;
use yagura_protocol::{DetectedStatus, UnknownReason};

#[derive(Debug)]
pub(super) struct Raw {
    status: DetectedStatus,
    /// When `status` became the current judgement; the debounce runs from
    /// here.
    since: Instant,
    /// When the evidence behind `status` appeared: `since`, except for a
    /// judgement held back until its screen was finished, which dates from
    /// when that screen started to be drawn. Right after a confirmation,
    /// `status` is the confirmed status, so this is its evidence instant.
    evidence_at: Instant,
}

/// Every clock a turn is measured on, so a change of the raw judgement
/// cannot be made without moving them past the wait it ends.
pub(super) struct TurnClocks<'a> {
    pub(super) output_at: &'a mut Instant,
    pub(super) report_at: &'a mut Instant,
    pub(super) inherited_since: Option<&'a mut Instant>,
}

impl Raw {
    pub(super) fn new(status: DetectedStatus, now: Instant) -> Self {
        Self {
            status,
            since: now,
            evidence_at: now,
        }
    }

    pub(super) fn status(&self) -> DetectedStatus {
        self.status
    }

    pub(super) fn since(&self) -> Instant {
        self.since
    }

    pub(super) fn evidence_at(&self) -> Instant {
        self.evidence_at
    }

    /// Whether the screen waits on the user: an approval, or a screen the
    /// definition declares it cannot read (an approval it does not know,
    /// for one). That time is the user's, not the CLI's silence. A lost
    /// screen is not one: no turn is judged on it.
    pub(super) fn waits_on_user(&self) -> bool {
        matches!(
            self.status,
            DetectedStatus::WaitingApproval
                | DetectedStatus::Unknown(UnknownReason::DeclaredUnknown)
        )
    }

    /// The only writer of the raw judgement. Leaving a wait on the user
    /// stops `clocks` for its length: a clock set before the wait moves
    /// later by the wait, and one set during it moves to its end, so each
    /// counts only the time the screen did not wait.
    pub(super) fn replace(
        &mut self,
        status: DetectedStatus,
        now: Instant,
        evidence_at: Instant,
        clocks: TurnClocks<'_>,
    ) {
        if self.waits_on_user() {
            let waited = now.saturating_duration_since(self.since);
            let TurnClocks {
                output_at,
                report_at,
                inherited_since,
            } = clocks;
            for clock in [Some(output_at), Some(report_at), inherited_since]
                .into_iter()
                .flatten()
            {
                *clock = if *clock < self.since {
                    clock.checked_add(waited).unwrap_or(now)
                } else {
                    now
                };
            }
        }
        self.status = status;
        self.since = now;
        self.evidence_at = evidence_at;
    }
}
