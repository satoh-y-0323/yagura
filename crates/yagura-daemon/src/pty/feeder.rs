//! A window's feeder thread: the one thread that changes the window's model
//! on output and reads its view's frames off it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use yagura_protocol::holder::AckStep;

use super::holder_link::Feed;
use super::reply_queue::ReplyQueue;
use super::views::{AttachGeneration, FrameSink, ViewSlot, ViewTaken};
use super::{PublishedInput, RawCapture};
use crate::detector::SharedLocalScreen;
use yagura_term::VtScreen;
use yagura_term::screen_view::FrameCapture;

/// Output is fed to the terminal model this many bytes at a time.
const MODEL_SLICE_BYTES: usize = 1024;

/// How long the reader keeps the model's lock before it publishes what it has
/// fed and offers the lock to a waiting reader. A hold is therefore never much
/// longer than this plus one slice's bounded work.
const MODEL_LOCK_HOLD: Duration = Duration::from_millis(2);

/// Everything the feeder thread updates as output arrives, and the view it
/// reads frames for: the feeder is the one thread that changes the model on
/// output and reads the view's frames off it.
pub(super) struct ModelFeed {
    pub(super) screen: SharedLocalScreen,
    pub(super) input: Arc<PublishedInput>,
    /// Where the model's replies to the child's queries go.
    pub(super) replies: Arc<ReplyQueue>,
    pub(super) raw_capture: RawCapture,
    pub(super) arrivals: watch::Sender<u64>,
    pub(super) views: Arc<ViewSlot>,
    /// Published before the end reaches the feeder (see `holder_link::Sink`).
    pub(super) exited: Arc<AtomicBool>,
}

impl ModelFeed {
    /// Feeds every output in `outputs` to the model under one hold of its
    /// lock, one slice at a time, and reads the attached view's frame, if one
    /// is due, under the same hold; `ended` is the child's end, which may
    /// release a view a synchronized update holds. Empties `outputs`.
    ///
    /// Every release is fair: a reader waiting for the lock gets it before
    /// this thread can take it back, so a child that never stops writing
    /// cannot starve the readers. Mid-feed the lock is offered after a hold
    /// of [`MODEL_LOCK_HOLD`] and kept when nobody is waiting.
    ///
    /// The model's replies to queries in the output are taken under the lock
    /// and queued after it is released, never written here. The screen is
    /// carried into the window's copy once everything is fed, and the input
    /// state is published from that copy, so it never lags what a reader of
    /// the copy sees. The frame is encoded and sent after the release.
    ///
    /// The arrival is announced once both the copy and the raw capture hold
    /// the output, so whoever it wakes finds it in either.
    pub(super) fn take_in(&self, outputs: &mut Vec<Vec<u8>>, ended: bool) {
        let Self {
            screen,
            input,
            replies,
            raw_capture,
            arrivals,
            views,
            exited,
        } = self;
        let mut guard = screen.lock();
        let mut held_since = Instant::now();
        for slice in outputs
            .iter()
            .flat_map(|output| output.chunks(MODEL_SLICE_BYTES))
        {
            if held_since.elapsed() >= MODEL_LOCK_HOLD {
                parking_lot::MutexGuard::bump(&mut guard);
                held_since = Instant::now();
            }
            guard.feed_uncarried(slice);
        }
        let fed = !outputs.is_empty();
        // Counted, and carried into the copy, under the lock, so the count
        // announced never runs ahead of what the copy holds.
        let arrival = fed.then(|| guard.note_output());
        input.publish(guard.carried());
        let taken_replies = fed.then(|| guard.terminal_mut().take_replies());
        let frame = read_view_frame(
            views,
            guard.terminal_mut(),
            exited.load(Ordering::SeqCst),
            fed || ended,
        );
        parking_lot::MutexGuard::unlock_fair(guard);
        if let Some((queued, dropped)) = taken_replies {
            replies.push(queued, dropped);
        }
        for output in outputs.drain(..) {
            raw_capture.record(&output);
        }
        if let Some(arrival) = arrival {
            arrivals.send_replace(arrival);
            replies.note_output();
        }
        if let Some((generation, sink, capture)) = frame {
            views.deliver(generation, sink.as_ref(), capture);
        }
    }
}

/// Reads the attached view's next frame off `screen`, if one is due;
/// `changed` notes a change to the view first.
pub(super) fn read_view_frame(
    views: &ViewSlot,
    screen: &mut VtScreen,
    child_exited: bool,
    changed: bool,
) -> Option<(AttachGeneration, Arc<dyn FrameSink>, FrameCapture)> {
    let ViewTaken {
        generation,
        mut view,
        sink,
    } = views.take(changed)?;
    let capture = view.capture(screen, child_exited, false);
    views.put_back(generation, view);
    Some((generation, sink, capture?))
}

/// The feeder thread: takes in every item waiting at once, feeds the outputs
/// to the model in the order the holder sent them and, once the model holds a
/// step of them, acknowledges it, so the holder reads on only as fast as the
/// model takes it in; `ack` sends the acknowledgement to the holder. Answers
/// each settle once everything before it has been taken in and acknowledged.
/// Marks `drained` once the end arrived after the last output, and goes on
/// reading the view's frames until both the link and the window have let go
/// of `items`.
pub(super) fn feed_model(
    feed: &ModelFeed,
    items: &std::sync::mpsc::Receiver<Feed>,
    ack: &dyn Fn(u64),
    mut ack_step: AckStep,
    drained: &watch::Sender<bool>,
) {
    let mut outputs = Vec::new();
    let mut settles = Vec::new();
    while let Ok(first) = items.recv() {
        let mut to_ack = None;
        let mut ended = false;
        let mut next = Some(first);
        while let Some(item) = next {
            match item {
                Feed::Output { seq, bytes } => {
                    if let Some(seq) = ack_step.on_output(seq) {
                        to_ack = Some(seq);
                    }
                    outputs.push(bytes);
                }
                Feed::End => ended = true,
                // Read for by `take_in` below.
                Feed::View => {}
                Feed::Settle(settle) => settles.push(settle),
            }
            next = items.try_recv().ok();
        }
        feed.take_in(&mut outputs, ended);
        if let Some(seq) = to_ack {
            ack(seq);
        }
        for settle in settles.drain(..) {
            let _ = settle.send(());
        }
        if ended {
            let _ = drained.send(true);
        }
    }
    let _ = drained.send(true);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver, Sender};

    use tokio::sync::oneshot;
    use tokio::sync::oneshot::error::TryRecvError;

    use super::*;
    use crate::detector::LocalScreen;
    use crate::pty::views::{EncodedFrame, FrameSend, SendKey};
    use yagura_term::screen_view::View;

    /// A safety bound only: reaching it fails the test.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        Frame,
        Ack(u64),
    }

    /// Holds whoever passes it until the test lets it go.
    struct Gate {
        entered: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl Gate {
        fn new() -> (Self, Receiver<()>, Sender<()>) {
            let (entered, entered_rx) = mpsc::channel();
            let (release_tx, release) = mpsc::channel();
            let gate = Self {
                entered,
                release: Mutex::new(release),
            };
            (gate, entered_rx, release_tx)
        }

        fn pass(&self) {
            let _ = self.entered.send(());
            let _ = self.release.lock().unwrap().recv();
        }
    }

    /// Records each frame it is sent, then holds the sender at its gate.
    struct HeldSink {
        seen: Arc<Mutex<Vec<Seen>>>,
        gate: Gate,
    }

    impl FrameSink for HeldSink {
        fn encode(&self, _capture: FrameCapture) -> EncodedFrame {
            EncodedFrame(Vec::new())
        }

        fn try_send(&self, _frame: EncodedFrame, _key: &SendKey) -> FrameSend {
            self.seen.lock().unwrap().push(Seen::Frame);
            self.gate.pass();
            FrameSend::Sent
        }

        fn wake(&self) {}
    }

    #[test]
    fn a_settle_is_answered_only_after_the_frame_and_the_ack_for_the_output_before_it() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (frame_gate, frame_entered, frame_release) = Gate::new();
        let (ack_gate, ack_entered, ack_release) = Gate::new();

        let screen = LocalScreen::new(20, 5).into_shared();
        let views = Arc::new(ViewSlot::default());
        let (generation, _kicked) = views.attach();
        let sink: Arc<dyn FrameSink> = Arc::new(HeldSink {
            seen: Arc::clone(&seen),
            gate: frame_gate,
        });
        let _ = views.place(generation, View::new(), sink);
        let input = Arc::new(PublishedInput::of(screen.lock().carried()));
        let feed = ModelFeed {
            screen,
            input,
            replies: Arc::new(ReplyQueue::default()),
            raw_capture: RawCapture::default(),
            arrivals: watch::channel(0).0,
            views,
            exited: Arc::default(),
        };

        let (items_tx, items) = mpsc::channel();
        let (settle_tx, mut settled) = oneshot::channel();
        items_tx
            .send(Feed::Output {
                seq: 1,
                bytes: b"hello".to_vec(),
            })
            .unwrap();
        items_tx.send(Feed::Settle(settle_tx)).unwrap();
        drop(items_tx);

        let (done_tx, done) = mpsc::channel();
        let feeder = std::thread::spawn({
            let seen = Arc::clone(&seen);
            move || {
                let (drained, _drained_rx) = watch::channel(false);
                let ack = |seq: u64| {
                    seen.lock().unwrap().push(Seen::Ack(seq));
                    ack_gate.pass();
                };
                feed_model(&feed, &items, &ack, AckStep::new(None), &drained);
                let _ = done_tx.send(());
            }
        });

        frame_entered
            .recv_timeout(HANG_GUARD)
            .expect("the output before the settle sends a frame");
        assert!(
            matches!(settled.try_recv(), Err(TryRecvError::Empty)),
            "the settle is answered while the frame for the output before it is still being sent"
        );
        frame_release.send(()).unwrap();

        ack_entered
            .recv_timeout(HANG_GUARD)
            .expect("the output before the settle is acked");
        assert!(
            matches!(settled.try_recv(), Err(TryRecvError::Empty)),
            "the settle is answered before the output before it is acked"
        );
        ack_release.send(()).unwrap();

        done.recv_timeout(HANG_GUARD)
            .expect("the feeder ends once its items are dropped");
        feeder.join().unwrap();
        assert_eq!(
            settled.try_recv(),
            Ok(()),
            "a settle read before the end is answered, not dropped"
        );
        assert_eq!(*seen.lock().unwrap(), [Seen::Frame, Seen::Ack(1)]);
    }
}
