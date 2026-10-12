//! The Tauri layer's mailbox between the daemon's frames and the WebView.
//!
//! Frames reach the WebView as the response of one pending `next_frames`
//! request rather than as events: an event is delivered by evaluating a script
//! whose text differs every time, and the WebView does not reclaim what that
//! leaves behind in proportion to the output. A response body is plain data.
//!
//! The daemon sends one frame per acknowledgement, so what piles up here is
//! bounded by that flow control, not by a cap of this module.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use tokio::sync::Notify;
use yagura_protocol::Frame;

#[derive(Debug, Default)]
struct State {
    /// Undelivered parts per window, oldest first.
    queues: BTreeMap<String, Vec<Frame>>,
    /// Which `take` is the current one; a newer `take` retires an older one.
    taker: u64,
}

/// Undelivered frame parts of every window, and the single `take` waiting for
/// them.
#[derive(Debug, Default)]
pub struct FrameMailbox {
    state: Mutex<State>,
    changed: Notify,
}

impl FrameMailbox {
    /// Creates an empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one frame part of `window_id`. A part that resets the grid makes
    /// the window's earlier undelivered parts obsolete, so they are dropped.
    pub fn push(&self, window_id: &str, frame: Frame) {
        {
            let mut state = self.lock();
            let queue = state.queues.entry(window_id.to_owned()).or_default();
            if frame.reset {
                queue.clear();
            }
            queue.push(frame);
        }
        self.changed.notify_waiters();
    }

    /// Drops what a window has not had delivered, for a window that is gone.
    pub fn discard_window(&self, window_id: &str) {
        self.lock().queues.remove(window_id);
    }

    /// Waits until some window has an undelivered part and hands over every
    /// part of every window, each window's in the order it was pushed.
    ///
    /// Returns empty when a newer `take` has taken over, so the request that
    /// is superseded (a reloaded page) does not stay pending for good.
    pub async fn take(&self) -> Vec<(String, Frame)> {
        let mine = {
            let mut state = self.lock();
            state.taker += 1;
            state.taker
        };
        self.changed.notify_waiters();
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Registered before the state is read, so a push between the read
            // and the await still wakes this task.
            notified.as_mut().enable();
            {
                let mut state = self.lock();
                if state.taker != mine {
                    return Vec::new();
                }
                if state.queues.values().any(|queue| !queue.is_empty()) {
                    let queues = std::mem::take(&mut state.queues);
                    return queues
                        .into_iter()
                        .flat_map(|(window_id, frames)| {
                            frames
                                .into_iter()
                                .map(move |frame| (window_id.clone(), frame))
                        })
                        .collect();
                }
            }
            notified.await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state is a map and a counter; a panicking holder cannot leave
        // them inconsistent.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
