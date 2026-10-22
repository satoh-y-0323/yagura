//! The focus a window's child is told, and the guard that holds it open.

use std::sync::Arc;
use tokio::sync::watch;

use super::reply_queue::ReplyQueue;
use crate::input_encode::{self, InputModes};

/// The focus a window's child is told, derived from the view's focus and from
/// the daemon's own input.
///
/// A real terminal delivers keys to the focused window only, so the child is
/// told it has focus whenever the daemon types into it, however the view
/// stands. It is told the view's focus again once the input is over, and
/// only after the child has produced output, so the report never lands in
/// the same read as the last input byte.
///
/// Every field is read and written under the queue's lock. The reports that
/// follow from it are written under the writer's lock, which is what orders
/// them against input bytes.
#[derive(Default)]
pub(super) struct FocusState {
    /// The last focus the view reported; `None` before any.
    pub(super) view: Option<bool>,
    /// Open stretches of deciding on or making writes; see [`FocusInterval`].
    pub(super) open_intervals: usize,
    /// The focus the child was last told; `None` before any report, when a
    /// child assumes it has the focus.
    told: Option<bool>,
    /// A focus-out is held back until the output count passes this value.
    out_after: Option<u64>,
    /// A focus-out is held back now, so the next output is to wake the writer.
    pub(super) out_deferred: bool,
}

impl FocusState {
    /// The focus the child should hold; `None` when nothing claims one.
    fn target(&self) -> Option<bool> {
        if self.open_intervals > 0 {
            Some(true)
        } else {
            self.view
        }
    }

    /// Notes that an input write, or an interval of waiting to write, ended
    /// with `outputs` outputs seen. Returns whether the child holds more focus
    /// than the target allows, so a focus-out is due.
    pub(super) fn input_ended(&mut self, outputs: u64) -> bool {
        self.out_after = Some(outputs);
        self.target() == Some(false) && self.told != Some(false)
    }

    /// The report that brings the child to the target; empty when the child
    /// already holds it, when it asked for no focus reports, or when a
    /// focus-out must wait for output.
    pub(super) fn reconcile(&mut self, modes: InputModes, outputs: u64) -> Vec<u8> {
        self.out_deferred = false;
        if !modes.focus_reports {
            return Vec::new();
        }
        let Some(target) = self.target() else {
            return Vec::new();
        };
        if self.told == Some(target) {
            return Vec::new();
        }
        if !target && self.out_after.is_some_and(|after| outputs <= after) {
            self.out_deferred = true;
            return Vec::new();
        }
        self.told = Some(target);
        self.out_after = None;
        input_encode::encode_focus(target, modes)
    }

    /// The report that goes in front of an input write.
    /// `outputs` is the output count as the input is written; a focus-out
    /// waits for output past it, so it never follows the input's bytes.
    pub(super) fn before_input(&mut self, modes: InputModes, outputs: u64) -> Vec<u8> {
        self.out_deferred = false;
        self.out_after = Some(outputs);
        if !modes.focus_reports || self.told == Some(true) {
            return Vec::new();
        }
        self.told = Some(true);
        input_encode::encode_focus(true, modes)
    }
}

/// An open stretch in which the daemon is deciding on, or making, writes to a
/// window; see [`PtySession::open_focus_interval`]. Ends when dropped.
#[must_use = "the interval ends when this is dropped"]
pub struct FocusInterval {
    pub(super) replies: Arc<ReplyQueue>,
    pub(super) outputs: watch::Receiver<u64>,
}

impl Drop for FocusInterval {
    fn drop(&mut self) {
        self.replies.close_focus_interval(&self.outputs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporting() -> InputModes {
        InputModes {
            focus_reports: true,
            ..InputModes::default()
        }
    }

    fn unfocused_view() -> FocusState {
        FocusState {
            view: Some(false),
            ..FocusState::default()
        }
    }

    #[test]
    fn focus_out_waits_for_output_after_the_input_written_under_the_lock() {
        let modes = reporting();

        let mut told_out = unfocused_view();
        assert_eq!(told_out.reconcile(modes, 0), b"\x1b[O");
        assert_eq!(told_out.before_input(modes, 5), b"\x1b[I");
        assert_eq!(told_out.reconcile(modes, 5), b"");
        assert_eq!(told_out.reconcile(modes, 6), b"\x1b[O");

        let mut stale = unfocused_view();
        assert_eq!(stale.before_input(modes, 3), b"\x1b[I");
        let _ = stale.input_ended(3);
        assert_eq!(stale.reconcile(modes, 3), b"");
        assert_eq!(stale.before_input(modes, 8), b"");
        assert_eq!(stale.reconcile(modes, 8), b"");
    }
}
