//! Structured status source: what a CLI's hooks say about its turns.
//!
//! A report is a fact about where a turn began or ended, not a snapshot of
//! what the screen shows, so it has no expiry: it stays true until the next
//! report. Only the latest fact is retained, and the `source` name is not
//! stored because validating it belongs to the IPC layer
//! (`yagura_protocol::is_valid_source`).

use yagura_protocol::WindowStatus;

/// What the most recent report says about the window's current turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnReport {
    /// No report stands: the window is judged by its screen alone.
    #[default]
    Unreported,
    /// A turn is in flight (a `Running` report).
    Running,
    /// A turn is in flight and waits for the user's approval (a
    /// `WaitingApproval` report).
    AwaitingApproval,
    /// The turn ended (`WaitingInput`, `Idle` or `Interrupted`), output
    /// stopped for so long that an open turn is presumed over, or the user
    /// pressed the key the definition's `[turn_end_key]` declares.
    Closed,
}

impl TurnReport {
    /// The fact a report of `status` leaves behind.
    ///
    /// `Unknown` and `Exited` withdraw the fact rather than state one: a hook
    /// that cannot say where the turn stands leaves the screen to decide.
    #[must_use]
    pub fn after_report(status: WindowStatus) -> Self {
        match status {
            WindowStatus::Running => Self::Running,
            WindowStatus::WaitingApproval => Self::AwaitingApproval,
            WindowStatus::WaitingInput | WindowStatus::Idle | WindowStatus::Interrupted => {
                Self::Closed
            }
            WindowStatus::Unknown | WindowStatus::Exited => Self::Unreported,
        }
    }

    /// Whether silence alone may end this turn. An approval dialog stays up
    /// without producing output, so a turn awaiting one ends only by a later
    /// report or the screen; a running turn's silence is not counted while
    /// the screen shows one either.
    #[must_use]
    pub fn closes_on_silence(self) -> bool {
        self == Self::Running
    }
}
