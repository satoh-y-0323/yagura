//! The queue of terminal-query replies and focus marks, and its writer thread.

use std::sync::{Condvar, Mutex};
use tokio::sync::watch;

use super::focus::FocusState;
use super::{PublishedInput, WriterSlot, lock_recover};
use crate::input_encode::InputModes;

/// Reply bytes waiting for a window's reply writer; a batch that would go
/// past this is dropped.
const MAX_QUEUED_REPLY_BYTES: usize = 64 * 1024;

/// What a [`ReplyQueue`] hands its writer thread: replies to write, and
/// whether the focus the child was told is to be brought up to date.
#[derive(Debug, PartialEq, Eq)]
struct Wake {
    bytes: Vec<u8>,
    check_focus: bool,
}

/// Replies to the child's terminal queries on their way to the PTY. The
/// reader only queues them, and one writer thread per window writes them, so
/// a child that stops reading its input never stops the reader.
///
/// The window's [`FocusState`] lives here too, because its reports are
/// written by the same thread and a change to it is a mark on this queue, not
/// bytes: marks are never dropped for want of room.
#[derive(Default)]
pub(super) struct ReplyQueue {
    state: Mutex<ReplyQueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct ReplyQueueState {
    bytes: Vec<u8>,
    closed: bool,
    /// Set while replies are being dropped, so a flood logs once.
    dropping: bool,
    /// The writer is to bring the child's focus up to date.
    check_focus: bool,
    focus: FocusState,
}

impl ReplyQueue {
    /// Queues `replies` whole, or drops them when the queue has no room;
    /// `dropped` replies were already dropped before reaching it.
    pub(super) fn push(&self, replies: Vec<u8>, dropped: usize) {
        if replies.is_empty() && dropped == 0 {
            return;
        }
        let mut state = lock_recover(&self.state);
        if state.closed {
            return;
        }
        let fits = state.bytes.len() + replies.len() <= MAX_QUEUED_REPLY_BYTES;
        if fits && dropped == 0 {
            state.dropping = false;
        } else if !state.dropping {
            state.dropping = true;
            tracing::warn!(
                "dropping replies to a child's terminal queries: the child is not reading its input"
            );
        }
        if fits && !replies.is_empty() {
            state.bytes.extend_from_slice(&replies);
            self.ready.notify_one();
        }
    }

    /// Asks the writer thread to bring the child's focus up to date.
    fn wake_for_focus(&self, state: &mut ReplyQueueState) {
        state.check_focus = true;
        self.ready.notify_one();
    }

    /// Ends the writer thread once it has nothing in hand.
    pub(super) fn close(&self) {
        let mut state = lock_recover(&self.state);
        state.closed = true;
        state.bytes.clear();
        self.ready.notify_one();
    }

    /// Waits for queued replies or a focus check; `None` once the queue is
    /// closed.
    fn next(&self) -> Option<Wake> {
        let mut state = lock_recover(&self.state);
        loop {
            if state.closed {
                return None;
            }
            if !state.bytes.is_empty() || state.check_focus {
                return Some(Wake {
                    bytes: std::mem::take(&mut state.bytes),
                    check_focus: std::mem::take(&mut state.check_focus),
                });
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(super) fn set_view_focus(&self, focused: bool) {
        let mut state = lock_recover(&self.state);
        state.focus.view = Some(focused);
        self.wake_for_focus(&mut state);
    }

    pub(super) fn open_focus_interval(&self) {
        let mut state = lock_recover(&self.state);
        state.focus.open_intervals += 1;
        self.wake_for_focus(&mut state);
    }

    pub(super) fn close_focus_interval(&self, outputs: &watch::Receiver<u64>) {
        let mut state = lock_recover(&self.state);
        state.focus.open_intervals = state.focus.open_intervals.saturating_sub(1);
        if state.focus.input_ended(*outputs.borrow()) {
            self.wake_for_focus(&mut state);
        }
    }

    /// Wakes the writer when output arrived while a focus-out waits for it.
    /// The writer reads the output count under this lock, so an arrival is
    /// either seen by that read or finds the wait marked.
    pub(super) fn note_output(&self) {
        let mut state = lock_recover(&self.state);
        if state.focus.out_deferred {
            self.wake_for_focus(&mut state);
        }
    }

    /// Under the writer's lock, before an input write: the report that tells
    /// the child it has the focus.
    pub(super) fn focus_before_input(
        &self,
        modes: InputModes,
        outputs: &watch::Receiver<u64>,
    ) -> Vec<u8> {
        lock_recover(&self.state)
            .focus
            .before_input(modes, *outputs.borrow())
    }

    /// After an input write ended and the writer's lock was let go.
    pub(super) fn focus_after_input(&self, outputs: &watch::Receiver<u64>) {
        let mut state = lock_recover(&self.state);
        if state.focus.input_ended(*outputs.borrow()) {
            self.wake_for_focus(&mut state);
        }
    }

    fn reconcile_focus(&self, modes: InputModes, outputs: &watch::Receiver<u64>) -> Vec<u8> {
        lock_recover(&self.state)
            .focus
            .reconcile(modes, *outputs.borrow())
    }

    /// The writer thread's body. Replies take the writer's lock like any other
    /// write, so they never land inside another write's bytes, but they are
    /// not counted in the write ledger: the child asked for them. So are the
    /// focus reports, which are worked out under that lock. A failed write
    /// leaves the PTY unusable, so the writer is dropped with it.
    pub(super) fn write_all_to(
        &self,
        writer: &WriterSlot,
        input: &PublishedInput,
        outputs: &watch::Receiver<u64>,
    ) {
        while let Some(wake) = self.next() {
            let mut guard = writer.lock();
            let Some(w) = guard.as_mut() else {
                break;
            };
            let mut bytes = wake.bytes;
            if wake.check_focus {
                bytes.extend(self.reconcile_focus(input.load().modes, outputs));
            }
            if !bytes.is_empty() && w.write_all(&bytes).and_then(|()| w.flush()).is_err() {
                *guard = None;
                break;
            }
            drop(guard);
            writer.release_if_requested();
        }
        self.close();
        writer.release_if_requested();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_reply_queue_hands_out_nothing_more() {
        let queue = ReplyQueue::default();
        queue.push(b"\x1b[?1;2c".to_vec(), 0);
        queue.close();
        assert_eq!(queue.next(), None);
    }

    #[test]
    fn a_reply_batch_that_would_go_past_the_bound_is_dropped_whole() {
        let queue = ReplyQueue::default();
        queue.push(vec![b'a'; MAX_QUEUED_REPLY_BYTES], 0);
        queue.push(vec![b'b'], 0);
        assert_eq!(
            queue.next().map(|wake| wake.bytes),
            Some(vec![b'a'; MAX_QUEUED_REPLY_BYTES])
        );
    }

    #[test]
    fn reply_batches_that_fit_together_are_all_kept_in_order() {
        let queue = ReplyQueue::default();
        queue.push(vec![b'a'; 100], 0);
        queue.push(vec![b'b'; 1000], 0);
        let mut expected = vec![b'a'; 100];
        expected.resize(1100, b'b');
        assert_eq!(queue.next().map(|wake| wake.bytes), Some(expected));
    }
}
