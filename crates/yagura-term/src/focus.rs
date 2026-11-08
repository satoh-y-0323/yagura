//! The focus a window's child is told.

use yagura_protocol::InputModes;

/// The focus a window's child is told, derived from the view's focus and from
/// the daemon's own input.
///
/// A real terminal delivers keys to the focused window only, so the child is
/// told it has focus whenever the daemon types into it, however the view
/// stands. It is told the view's focus again once the input is over, and
/// only after the child has produced output, so the report never lands in
/// the same read as the last input byte.
///
/// Whoever writes the child's input keeps this, so a report is decided and
/// written in the order of the writes around it.
#[derive(Debug, Default)]
pub struct FocusState {
    /// The last focus the view reported; `None` before any.
    view: Option<bool>,
    /// Open stretches of deciding on or making writes.
    open_intervals: usize,
    /// The focus the child was last told; `None` before any report, when a
    /// child assumes it has the focus.
    told: Option<bool>,
    /// A focus-out is held back until the output count passes this value.
    out_after: Option<u64>,
    /// A focus-out is held back now, so the next output is to bring the
    /// child up to date.
    out_deferred: bool,
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

    /// Records the focus the view reported.
    pub fn set_view(&mut self, focused: bool) {
        self.view = Some(focused);
    }

    /// Opens a stretch of deciding on or making writes.
    pub fn open_interval(&mut self) {
        self.open_intervals += 1;
    }

    /// Closes a stretch opened by [`FocusState::open_interval`]. Returns
    /// whether a focus-out is due.
    ///
    /// The focus-out still waits only for output past the last input write;
    /// closing does not move that mark, so output that followed the input
    /// while the stretch was open lets the focus-out out at once.
    pub fn close_interval(&mut self) -> bool {
        self.open_intervals = self.open_intervals.saturating_sub(1);
        self.out_of_target()
    }

    /// Whether the child holds more focus than the target allows.
    fn out_of_target(&self) -> bool {
        self.target() == Some(false) && self.told != Some(false)
    }

    /// Whether a focus-out waits for the child's next output.
    #[must_use]
    pub fn out_deferred(&self) -> bool {
        self.out_deferred
    }

    /// Notes that an input write ended with `outputs` outputs seen. Returns
    /// whether the child holds more focus than the target allows, so a
    /// focus-out is due.
    pub fn input_ended(&mut self, outputs: u64) -> bool {
        self.out_after = Some(outputs);
        self.out_of_target()
    }

    /// The report that brings the child to the target; empty when the child
    /// already holds it, when it asked for no focus reports, or when a
    /// focus-out must wait for output.
    pub fn reconcile(&mut self, modes: InputModes, outputs: u64) -> Vec<u8> {
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
        focus_report(target)
    }

    /// The report that goes in front of an input write.
    /// `outputs` is the output count as the input is written; a focus-out
    /// waits for output past it, so it never follows the input's bytes.
    pub fn before_input(&mut self, modes: InputModes, outputs: u64) -> Vec<u8> {
        self.out_deferred = false;
        self.out_after = Some(outputs);
        if !modes.focus_reports || self.told == Some(true) {
            return Vec::new();
        }
        self.told = Some(true);
        focus_report(true)
    }
}

/// The focus report a child that turned on focus reports (`CSI ? 1004 h`)
/// reads.
fn focus_report(focused: bool) -> Vec<u8> {
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
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

    /// An unfocused window the holder's writer typed into under `intervals`
    /// open intervals, with `outputs` outputs seen as the input was written.
    fn typed_under_intervals(intervals: usize, outputs: u64) -> FocusState {
        let modes = reporting();
        let mut focus = unfocused_view();
        assert_eq!(focus.reconcile(modes, 0), b"\x1b[O");
        let mut opened = Vec::new();
        for _ in 0..intervals {
            focus.open_interval();
            opened.extend(focus.reconcile(modes, outputs));
        }
        assert_eq!(opened, b"\x1b[I");
        assert_eq!(focus.before_input(modes, outputs), b"");
        assert!(!focus.input_ended(outputs));
        assert_eq!(focus.reconcile(modes, outputs), b"");
        focus
    }

    #[test]
    fn closing_an_interval_explicitly_lets_out_output_that_followed_the_input() {
        let modes = reporting();

        let mut quiet = typed_under_intervals(1, 5);
        assert!(quiet.close_interval());
        assert_eq!(quiet.reconcile(modes, 5), b"");
        assert!(quiet.out_deferred());
        assert_eq!(quiet.reconcile(modes, 6), b"\x1b[O");

        let mut answered = typed_under_intervals(1, 5);
        assert!(answered.close_interval());
        assert_eq!(
            answered.reconcile(modes, 6),
            b"\x1b[O",
            "output after the input came before the interval closed, so the focus-out is due now"
        );
        assert!(!answered.out_deferred());
    }

    #[test]
    fn closing_intervals_a_lost_connection_left_lets_out_output_that_followed_the_input() {
        let modes = reporting();

        let mut quiet = typed_under_intervals(2, 5);
        assert!(!quiet.close_interval());
        assert!(quiet.close_interval());
        assert_eq!(quiet.reconcile(modes, 5), b"");
        assert!(quiet.out_deferred());
        assert_eq!(quiet.reconcile(modes, 6), b"\x1b[O");

        let mut answered = typed_under_intervals(2, 5);
        assert!(!answered.close_interval());
        assert!(answered.close_interval());
        assert_eq!(
            answered.reconcile(modes, 6),
            b"\x1b[O",
            "output after the input came while the connection was lost, so the focus-out is due now"
        );
        assert!(!answered.out_deferred());
    }
}
