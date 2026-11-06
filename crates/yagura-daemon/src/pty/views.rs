//! A window's one attach: which connection holds it, its view, and where the
//! view's frames go. The window's feeder reads the frames; the attached
//! connection places the view and tells it what changed.

use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::lock_recover;
use yagura_term::screen_view::{FrameCapture, View};

/// Which attach of a window a connection holds; a later attach
/// supersedes it. Only the window makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachGeneration(u64);

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

/// A frame laid out and encoded for one connection.
pub(crate) struct EncodedFrame(pub(crate) Vec<Vec<u8>>);

/// Lets only [`ViewSlot::deliver`], which checks the attach under the
/// slot's lock first, call [`FrameSink::try_send`]: nothing outside this
/// module can make one.
pub(crate) struct SendKey(());

/// Where an attached view's frames go: its connection's outbound queue.
pub(crate) trait FrameSink: Send + Sync {
    /// Lays `capture` out and encodes it; called away from every lock.
    fn encode(&self, capture: FrameCapture) -> EncodedFrame;
    /// Queues `frame` without waiting.
    fn try_send(&self, frame: EncodedFrame, key: &SendKey) -> FrameSend;
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
    /// Something the view shows may have changed other than by output.
    Changed,
}

fn apply(view: &mut View, note: ViewNote) {
    match note {
        ViewNote::Ack(seq) => view.acknowledge(seq),
        ViewNote::Visible(visible) => view.set_visible(visible),
        ViewNote::Changed => view.note_change(),
    }
}

/// The notes that came while the feeder had the view out, each kind
/// folded into its latest.
#[derive(Default)]
struct Missed {
    ack: Option<u64>,
    visible: Option<bool>,
    changed: bool,
}

impl Missed {
    fn record(&mut self, note: ViewNote) {
        match note {
            ViewNote::Ack(seq) => self.ack = Some(seq),
            ViewNote::Visible(visible) => self.visible = Some(visible),
            ViewNote::Changed => self.changed = true,
        }
    }

    fn apply_to(self, view: &mut View) {
        if let Some(visible) = self.visible {
            apply(view, ViewNote::Visible(visible));
        }
        if let Some(seq) = self.ack {
            apply(view, ViewNote::Ack(seq));
        }
        if self.changed {
            apply(view, ViewNote::Changed);
        }
    }
}

enum Place {
    /// Not placed yet, or taken off after its connection failed.
    Empty,
    Here(View),
    /// The feeder is reading a frame off it.
    Out(Missed),
}

struct Attached {
    generation: AttachGeneration,
    kick: oneshot::Sender<()>,
    sink: Option<Arc<dyn FrameSink>>,
    view: Place,
}

#[derive(Default)]
struct SlotState {
    last_generation: u64,
    attached: Option<Attached>,
    /// A wake went to the feeder and it has not looked yet.
    wake_sent: bool,
}

impl SlotState {
    fn attached(&mut self, generation: AttachGeneration) -> Option<&mut Attached> {
        self.attached
            .as_mut()
            .filter(|attached| attached.generation == generation)
    }

    /// Whether the caller is to wake the feeder: one wake at a time.
    fn wake(&mut self) -> bool {
        !std::mem::replace(&mut self.wake_sent, true)
    }
}

/// A view the feeder took out to read a frame off the model.
pub(in crate::pty) struct ViewTaken {
    pub(in crate::pty) generation: AttachGeneration,
    pub(in crate::pty) view: View,
    pub(in crate::pty) sink: Arc<dyn FrameSink>,
}

/// The window's attach slot. Its lock is short: never held while the
/// model is read or a frame is encoded, and never waits on another lock.
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
            view: Place::Empty,
        });
        if let Some(previous) = previous {
            let _ = previous.kick.send(());
        }
        (generation, kicked)
    }

    pub(in crate::pty) fn detach(&self, generation: AttachGeneration) {
        let mut state = lock_recover(&self.0);
        if state.attached(generation).is_some() {
            state.attached = None;
        }
    }

    pub(in crate::pty) fn is_attached(&self) -> bool {
        lock_recover(&self.0).attached.is_some()
    }

    /// Places `view` for attach `generation`; called under the model's
    /// lock its rows were read under. Returns whether to wake the feeder.
    pub(in crate::pty) fn place(
        &self,
        generation: AttachGeneration,
        view: View,
        sink: Arc<dyn FrameSink>,
    ) -> bool {
        let mut state = lock_recover(&self.0);
        let wants = view.wants_frame();
        let Some(attached) = state.attached(generation) else {
            return false;
        };
        attached.sink = Some(sink);
        attached.view = Place::Here(view);
        wants && state.wake()
    }

    /// Returns whether to wake the feeder.
    pub(in crate::pty) fn note(&self, generation: AttachGeneration, note: ViewNote) -> bool {
        let mut state = lock_recover(&self.0);
        let Some(attached) = state.attached(generation) else {
            return false;
        };
        let wants = match &mut attached.view {
            Place::Here(view) => {
                apply(view, note);
                view.wants_frame()
            }
            Place::Out(missed) => {
                missed.record(note);
                true
            }
            Place::Empty => false,
        };
        wants && state.wake()
    }

    /// A write to the child, which may end a synchronized update that
    /// holds the view. Returns whether to wake the feeder.
    pub(in crate::pty) fn note_write(&self) -> bool {
        let mut state = lock_recover(&self.0);
        let Some(attached) = state.attached.as_mut() else {
            return false;
        };
        let wants = match &mut attached.view {
            Place::Here(view) if view.is_held() => {
                view.note_change();
                view.wants_frame()
            }
            Place::Out(missed) => {
                missed.changed = true;
                true
            }
            Place::Here(_) | Place::Empty => false,
        };
        wants && state.wake()
    }

    /// Takes the view out for the feeder when it wants a frame, after
    /// noting a change when `changed`.
    pub(in crate::pty) fn take(&self, changed: bool) -> Option<ViewTaken> {
        let mut state = lock_recover(&self.0);
        state.wake_sent = false;
        let attached = state.attached.as_mut()?;
        let sink = attached.sink.clone()?;
        let Place::Here(view) = &mut attached.view else {
            return None;
        };
        if changed {
            view.note_change();
        }
        if !view.wants_frame() {
            return None;
        }
        let Place::Here(view) =
            std::mem::replace(&mut attached.view, Place::Out(Missed::default()))
        else {
            return None;
        };
        Some(ViewTaken {
            generation: attached.generation,
            view,
            sink,
        })
    }

    /// Puts a taken view back with what it missed; drops it when
    /// another attach took the slot meanwhile.
    pub(in crate::pty) fn put_back(&self, generation: AttachGeneration, mut view: View) {
        let mut state = lock_recover(&self.0);
        let Some(attached) = state.attached(generation) else {
            return;
        };
        if let Place::Out(missed) = std::mem::replace(&mut attached.view, Place::Empty) {
            missed.apply_to(&mut view);
            attached.view = Place::Here(view);
        }
    }

    /// Encodes `capture` and queues it, only while `generation` still
    /// holds the slot: a frame read for an attach that was superseded is
    /// never sent. A full or closed queue takes the view off and wakes
    /// the connection, which then closes.
    pub(in crate::pty) fn deliver(
        &self,
        generation: AttachGeneration,
        encoder: &dyn FrameSink,
        capture: FrameCapture,
    ) {
        let frame = encoder.encode(capture);
        let failed = {
            let mut state = lock_recover(&self.0);
            let Some(attached) = state.attached(generation) else {
                return;
            };
            let Some(sink) = attached.sink.as_ref() else {
                return;
            };
            match sink.try_send(frame, &SendKey(())) {
                FrameSend::Sent => None,
                FrameSend::Full | FrameSend::Closed => {
                    attached.view = Place::Empty;
                    attached.sink.take()
                }
            }
        };
        if let Some(sink) = failed {
            sink.wake();
        }
    }
}
