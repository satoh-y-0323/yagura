//! Where a task window's last report to its plan's owner stands: still
//! waiting on a live owner, released by a new instruction, or left with no
//! reader.

use std::sync::Mutex;

use yagura_protocol::{DetectedStatus, ReportState, TaskReportMark};

use crate::pty::lock_recover;
use crate::waiter::StatusAt;

/// A change to what a task report waits on, made under its plan's lock.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReportChange<'a> {
    /// The window's task binding ended: the report has no reader for good.
    Unbound,
    /// The plan named a new owner (alive when named): the report now waits
    /// on it.
    NewOwner(&'a str),
}

/// One window's count of new instructions, its typed-ahead mark and the mark
/// its last task report left. Memory only: a daemon boot starts with none.
///
/// A leaf lock: nothing else is locked while it is held.
#[derive(Debug, Default)]
pub(crate) struct TaskReport {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// New instructions that reached the window's CLI. Never decreases.
    instructions: u64,
    /// One past the status generation an instruction last arrived at while
    /// the window was running or unknown; 0 for none.
    ahead_at: u64,
    /// One past the status generation the window last came to rest at
    /// (waiting for input or idle); 0 for none.
    rest_at: u64,
    mark: Option<Mark>,
}

/// What one task report left.
#[derive(Debug, Clone)]
struct Mark {
    /// The report's number in this window for this daemon boot, from 1.
    seq: u64,
    /// The owner the report was delivered to; `None` when none was alive.
    owner: Option<String>,
    /// [`State::instructions`] when the report was sent.
    instructions: u64,
    /// Why the report no longer stands whatever happens next; `None` while
    /// it may still stand.
    released: Option<Released>,
}

/// Why a report was released for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Released {
    /// An instruction typed ahead was still to be read when it was sent.
    TypedAhead,
    /// No owner was alive when it was sent, or the task binding ended.
    NoReader,
}

impl TaskReport {
    /// Counts a new instruction handed to the CLI as it asked for one (a door
    /// read), which is never ahead of the CLI's turn.
    pub(crate) fn instruction_taken(&self) {
        let mut state = lock_recover(&self.state);
        state.instructions = state.instructions.saturating_add(1);
    }

    /// Counts new work handed to the window's CLI (a submitted line, a
    /// message or a push; never an interrupt) while its confirmed status was
    /// `at`. One that arrives while the window runs, or while its status is
    /// unknown, may be read by the CLI only after a report sent later in the
    /// same turn, so that report is born released.
    pub(crate) fn instruction_arrived(&self, at: &StatusAt) {
        let mut state = lock_recover(&self.state);
        state.instructions = state.instructions.saturating_add(1);
        if matches!(
            at.status,
            DetectedStatus::Running | DetectedStatus::Unknown(_)
        ) {
            state.ahead_at = state.ahead_at.max(at.generation.saturating_add(1));
        }
    }

    /// Records that the window's confirmed status came to rest at
    /// `generation`, which ends every instruction typed ahead before it.
    pub(crate) fn rested(&self, generation: u64) {
        let mut state = lock_recover(&self.state);
        state.rest_at = state.rest_at.max(generation.saturating_add(1));
    }

    /// Leaves the mark of a task report delivered to `owner`, or to no one
    /// (`None`: the owner was not alive), under the next `seq`.
    pub(crate) fn place(&self, owner: Option<&str>) {
        let mut state = lock_recover(&self.state);
        let seq = state
            .mark
            .as_ref()
            .map_or(1, |mark| mark.seq.saturating_add(1));
        let typed_ahead = state.ahead_at > state.rest_at;
        let instructions = state.instructions;
        state.mark = Some(Mark {
            seq,
            owner: owner.map(str::to_string),
            instructions,
            // A report sent behind unread work is released by that work,
            // whether or not anyone would have read it.
            released: if typed_ahead {
                Some(Released::TypedAhead)
            } else if owner.is_none() {
                Some(Released::NoReader)
            } else {
                None
            },
        });
    }

    /// Applies `change` to the mark, if any. `owner_alive` tells whether the
    /// owner the mark waits on now is alive; it is called with no lock held.
    ///
    /// A new owner leaves a released report released. When it makes a report
    /// that did not stand stand again, the mark moves to the next `seq`, so
    /// a reader that saw it released accepts the change.
    pub(crate) fn apply(&self, change: ReportChange<'_>, owner_alive: impl FnOnce(&str) -> bool) {
        let previous_owner = {
            let mut state = lock_recover(&self.state);
            let Some(mark) = state.mark.as_mut() else {
                return;
            };
            if mark.released.is_some() {
                return;
            }
            if let ReportChange::Unbound = change {
                mark.released = Some(Released::NoReader);
                return;
            }
            mark.owner.clone()
        };
        let ReportChange::NewOwner(owner) = change else {
            return;
        };
        let was_alive = previous_owner.as_deref().is_some_and(owner_alive);
        let mut state = lock_recover(&self.state);
        let instructions = state.instructions;
        let Some(mark) = state.mark.as_mut() else {
            return;
        };
        let stood = was_alive && mark.instructions == instructions;
        mark.owner = Some(owner.to_string());
        if !stood && mark.instructions == instructions {
            mark.seq = mark.seq.saturating_add(1);
        }
    }

    /// The fact as it reads now, `owner_alive` telling whether the owner the
    /// report waits on is alive; `None` for a window that sent no task
    /// report in this daemon boot.
    pub(crate) fn fact(&self, owner_alive: impl FnOnce(&str) -> bool) -> Option<TaskReportMark> {
        let (mark, instructions) = {
            let state = lock_recover(&self.state);
            (state.mark.clone()?, state.instructions)
        };
        let instructed =
            mark.released == Some(Released::TypedAhead) || mark.instructions != instructions;
        // Checked first: a report an instruction released no longer waits on
        // anyone, so its reader's end changes nothing.
        let state = if instructed {
            ReportState::Instruction
        } else if mark.released.is_some() || !mark.owner.as_deref().is_some_and(owner_alive) {
            ReportState::NoReader
        } else {
            ReportState::Standing
        };
        Some(TaskReportMark {
            seq: mark.seq,
            state,
        })
    }
}
