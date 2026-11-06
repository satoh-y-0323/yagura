//! A window's one attach: which connection holds it and where its frames go.
//! The view itself (its frames, their acknowledgements, the scroll and the
//! selection) is the holder's; the window's link hands each frame the holder
//! sends for the attach to the connection that holds it.

use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use yagura_protocol::Frame;
use yagura_protocol::holder::HolderViewNote;

use super::lock_recover;

/// Which attach of a window a connection holds; a later attach
/// supersedes it. Only the window makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachGeneration(u64);

impl AttachGeneration {
    /// The number the holder knows the attach by.
    pub(in crate::pty) fn number(self) -> u64 {
        self.0
    }
}

/// What a [`FrameSink`] did with a frame.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameSend {
    /// Queued for the connection.
    Sent,
    /// The connection's queue is full: its client stopped reading.
    Full,
    /// The connection is gone.
    Closed,
}

/// Lets only [`ViewSlot::deliver`], which checks the attach under the
/// slot's lock first, call [`FrameSink::try_send`]: nothing outside this
/// module can make one.
pub(crate) struct SendKey(());

/// Where an attached view's frames go: its connection's outbound queue.
pub(crate) trait FrameSink: Send + Sync {
    /// Encodes `frame` and queues it without waiting.
    fn try_send(&self, frame: Frame, key: &SendKey) -> FrameSend;
    /// Wakes the connection, which closes once a send failed.
    fn wake(&self);
}

/// What the attached connection tells its view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewNote {
    /// The client applied frame `seq`.
    Ack(u64),
    /// The client shows the view or no longer does.
    Visible(bool),
}

impl From<ViewNote> for HolderViewNote {
    fn from(note: ViewNote) -> Self {
        match note {
            ViewNote::Ack(seq) => Self::Ack { seq },
            ViewNote::Visible(visible) => Self::Visible { visible },
        }
    }
}

struct Attached {
    generation: AttachGeneration,
    kick: oneshot::Sender<()>,
    /// `None` until the view is shown, and again once a send failed.
    sink: Option<Arc<dyn FrameSink>>,
}

#[derive(Default)]
struct SlotState {
    last_generation: u64,
    attached: Option<Attached>,
}

impl SlotState {
    fn attached(&mut self, generation: AttachGeneration) -> Option<&mut Attached> {
        self.attached
            .as_mut()
            .filter(|attached| attached.generation == generation)
    }
}

/// The window's attach slot. Its lock is short and never waits on another.
#[derive(Default)]
pub(in crate::pty) struct ViewSlot(Mutex<SlotState>);

impl ViewSlot {
    pub(in crate::pty) fn attach(&self) -> (AttachGeneration, oneshot::Receiver<()>) {
        let mut state = lock_recover(&self.0);
        state.last_generation = state.last_generation.wrapping_add(1);
        let generation = AttachGeneration(state.last_generation);
        let (kick, kicked) = oneshot::channel();
        let previous = state.attached.replace(Attached {
            generation,
            kick,
            sink: None,
        });
        if let Some(previous) = previous {
            let _ = previous.kick.send(());
        }
        (generation, kicked)
    }

    /// Lets go of attach `generation`; returns whether it held the slot.
    pub(in crate::pty) fn detach(&self, generation: AttachGeneration) -> bool {
        let mut state = lock_recover(&self.0);
        let held = state.attached(generation).is_some();
        if held {
            state.attached = None;
        }
        held
    }

    pub(in crate::pty) fn is_attached(&self) -> bool {
        lock_recover(&self.0).attached.is_some()
    }

    pub(in crate::pty) fn holds(&self, generation: AttachGeneration) -> bool {
        lock_recover(&self.0).attached(generation).is_some()
    }

    /// Sends attach `generation`'s frames to `sink` from now on; returns
    /// whether `generation` still holds the slot.
    pub(in crate::pty) fn place(
        &self,
        generation: AttachGeneration,
        sink: Arc<dyn FrameSink>,
    ) -> bool {
        let mut state = lock_recover(&self.0);
        let Some(attached) = state.attached(generation) else {
            return false;
        };
        attached.sink = Some(sink);
        true
    }

    /// Queues `frame` for the connection of attach `generation`, only while
    /// that attach holds the slot: a frame for an attach that was superseded
    /// is never sent. A full or closed queue takes the sink off and wakes the
    /// connection, which then closes; returns `true` then, so the holder is
    /// told to let the attach go.
    pub(in crate::pty) fn deliver(&self, generation: u64, frame: Frame) -> bool {
        let failed = {
            let mut state = lock_recover(&self.0);
            let Some(attached) = state.attached(AttachGeneration(generation)) else {
                return false;
            };
            let Some(sink) = attached.sink.as_ref() else {
                return false;
            };
            match sink.try_send(frame, &SendKey(())) {
                FrameSend::Sent => None,
                FrameSend::Full | FrameSend::Closed => attached.sink.take(),
            }
        };
        let Some(sink) = failed else {
            return false;
        };
        sink.wake();
        true
    }
}
