//! One connection's lifetime: its write half, the request waiting on it,
//! the attribution of answers and its end.

#[cfg(test)]
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;
use yagura_protocol::ClientMessage;

use super::super::{
    Connection, IpcError, PendingKind, Reply, answers_carry_correlation_ids, lock_blocking, wire,
};
use super::reader_link::{EndSink, ReaderLink};
use super::transition::TakenAnswer;

/// An async read plus a way to count and read, without waiting, the bytes
/// already delivered to this end.
pub(crate) trait GenerationRead: AsyncRead + Send + Unpin {
    /// How many bytes already delivered to this end were not read yet.
    ///
    /// # Errors
    /// The error counting them met.
    fn delivered_len(&mut self) -> std::io::Result<usize>;

    /// Reads into `into`, without waiting, up to `into.len()` of the bytes
    /// already delivered to this end, and returns how many; `Ok(0)` when
    /// none can be read without waiting.
    ///
    /// # Errors
    /// The error reading them met before any byte was read; one met after is
    /// returned by the next call.
    fn read_delivered(&mut self, into: &mut [u8]) -> std::io::Result<usize>;
}

impl GenerationRead for yagura_os::ClientReadHalf {
    fn delivered_len(&mut self) -> std::io::Result<usize> {
        yagura_os::ClientReadHalf::delivered_len(self)
    }

    fn read_delivered(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        yagura_os::ClientReadHalf::read_delivered(self, into)
    }
}

/// A [`GenerationRead`] in a box: the form a generation hands its reader.
pub(crate) type BoxedRead = Box<dyn GenerationRead>;

/// The write half a generation writes through.
type BoxedWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// A connection a generation is built on.
pub(crate) trait GenerationStream: Send + 'static {
    /// The half the generation's reader takes and the half it writes through.
    fn into_halves(self) -> (BoxedRead, BoxedWrite);
}

/// A connection to the daemon, as a generation is built on it.
pub(super) struct DaemonConnection(pub(super) Connection);

impl GenerationStream for DaemonConnection {
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = yagura_os::split_client(self.0);
        (Box::new(read), Box::new(write))
    }
}

#[cfg(test)]
impl GenerationStream for tokio::io::DuplexStream {
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = tokio::io::split(self);
        (Box::new(Polled::new(read)), Box::new(write))
    }
}

#[cfg(test)]
impl<R, W> GenerationStream for tokio::io::Join<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = self.into_inner();
        (Box::new(Polled::new(read)), Box::new(write))
    }
}

/// An in-memory connection whose read half also holds `late` bytes: bytes
/// that reach this end after a stopped reader has counted what was delivered.
#[cfg(test)]
pub(super) struct WithLateBytes<R, W> {
    pub(super) connection: tokio::io::Join<R, W>,
    pub(super) late: Vec<u8>,
}

#[cfg(test)]
impl<R, W> GenerationStream for WithLateBytes<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn into_halves(self) -> (BoxedRead, BoxedWrite) {
        let (read, write) = self.connection.into_inner();
        let mut read = Polled::new(read);
        read.late = self.late.into();
        (Box::new(read), Box::new(write))
    }
}

/// An in-memory read half: what polling until it would wait does not return
/// was not delivered. `delivered_len` takes the delivered bytes in, and they
/// are read before anything polled after them. `late` bytes are only ever
/// handed over by `read_delivered`, after the bytes taken in, and a finite
/// run of them ends in `Ok(0)`.
#[cfg(test)]
struct Polled<R> {
    inner: R,
    delivered: std::collections::VecDeque<u8>,
    late: std::collections::VecDeque<u8>,
}

#[cfg(test)]
impl<R> Polled<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            delivered: std::collections::VecDeque::new(),
            late: std::collections::VecDeque::new(),
        }
    }

    /// Moves up to `into.len()` of the bytes taken in to `into`.
    fn hand_over(&mut self, into: &mut [u8]) -> usize {
        let count = into.len().min(self.delivered.len());
        for (slot, byte) in into.iter_mut().zip(self.delivered.drain(..count)) {
            *slot = byte;
        }
        count
    }
}

#[cfg(test)]
impl<R: AsyncRead + Unpin> AsyncRead for Polled<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.delivered.is_empty() {
            return Pin::new(&mut self.inner).poll_read(cx, buf);
        }
        let count = self.hand_over(buf.initialize_unfilled());
        buf.advance(count);
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
impl<R: AsyncRead + Send + Unpin> GenerationRead for Polled<R> {
    fn delivered_len(&mut self) -> std::io::Result<usize> {
        use std::task::{Context, Poll, Waker};
        let mut chunk = [0u8; 4096];
        loop {
            let mut buf = tokio::io::ReadBuf::new(&mut chunk);
            // Outside the task's budget, so a spent budget hides no byte.
            let polled = {
                let mut read =
                    std::pin::pin!(tokio::task::unconstrained(std::future::poll_fn(|cx| {
                        Pin::new(&mut self.inner).poll_read(cx, &mut buf)
                    })));
                read.as_mut().poll(&mut Context::from_waker(Waker::noop()))
            };
            match polled {
                Poll::Ready(Ok(())) if !buf.filled().is_empty() => {
                    self.delivered.extend(buf.filled());
                }
                Poll::Ready(Ok(())) | Poll::Pending => return Ok(self.delivered.len()),
                Poll::Ready(Err(error)) => return Err(error),
            }
        }
    }

    fn read_delivered(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        let taken = self.hand_over(into);
        let room = (into.len() - taken).min(self.late.len());
        for (slot, byte) in into[taken..].iter_mut().zip(self.late.drain(..room)) {
            *slot = byte;
        }
        Ok(taken + room)
    }
}

/// How an opening step reads an answer it was given, as the answer its
/// generation took. The same reading decides the step's own result.
pub(crate) type Take = fn(&Result<Reply, IpcError>) -> TakenAnswer;

/// What a request on a generation brought back.
#[derive(Debug)]
pub(crate) enum StepReply<T> {
    /// What the daemon, or the wire gate, answered.
    Answered(T),
    /// No answer: the connection failed under the request or the generation
    /// ended, and the generation's end tells how.
    Unanswered(IpcError),
}

impl<T> StepReply<T> {
    pub(crate) fn map<U>(self, read: impl FnOnce(T) -> U) -> StepReply<U> {
        match self {
            Self::Answered(answer) => StepReply::Answered(read(answer)),
            Self::Unanswered(error) => StepReply::Unanswered(error),
        }
    }
}

impl<T> StepReply<Result<T, IpcError>> {
    /// The answer, or the error no answer came with.
    pub(crate) fn into_result(self) -> Result<T, IpcError> {
        match self {
            Self::Answered(answer) => answer,
            Self::Unanswered(error) => Err(error),
        }
    }
}

/// The request waiting on a generation.
#[derive(Debug)]
struct Pending {
    kind: PendingKind,
    correlation_id: u64,
    sender: oneshot::Sender<Result<Reply, IpcError>>,
    /// Set on an opening step.
    take: Option<Take>,
}

/// What a generation's end takes in one step.
#[derive(Debug, Default)]
struct Slots {
    ended: bool,
    pending: Option<Pending>,
    /// The last answer an opening step took.
    taken: Option<TakenAnswer>,
    cancel_reader: Option<oneshot::Sender<()>>,
    stop_reader: Option<oneshot::Sender<()>>,
}

impl Slots {
    /// Records a refusal an opening step met before its request was written,
    /// read as the step reads any answer. Called while the generation has not
    /// ended.
    fn record_refusal(&mut self, take: Option<Take>, refusal: &IpcError) {
        if let Some(take) = take {
            self.taken = Some(take(&Err(refusal.duplicate())));
        }
    }
}

/// The request a generation is waiting on, as its reader weighs a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Awaiting {
    pub(crate) kind: PendingKind,
    pub(crate) correlation_id: u64,
}

/// What became of an answer handed to [`Generation::settle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settled {
    /// It answered the request waiting.
    Answered,
    /// Nothing waiting is what it answers; it was dropped.
    Unanswered,
    /// It came under the waiting request's key but is of another kind: the
    /// daemon broke its word, so the generation must end.
    Broken,
}

/// One connection from the moment it was accepted until it ends: its write
/// half, the request waiting on it, and what opening it settled.
pub(crate) struct Generation {
    number: u64,
    writer: tokio::sync::Mutex<Option<BoxedWrite>>,
    slots: std::sync::Mutex<Slots>,
    /// One request waits for its answer on a connection at a time. Every
    /// request this client waits on is answered while the daemon handles its
    /// frame, so a second slot would be answered no sooner, and a single
    /// slot keeps the answers of a daemon that sends no correlation id
    /// attributable by their kind.
    request_lock: tokio::sync::Mutex<()>,
    revision: OnceLock<u32>,
    /// The window the reader routes frames to. An attach writes it before
    /// its request goes out, as the daemon may stream before this side reads
    /// the answer.
    window_id: std::sync::Mutex<Option<String>>,
    correlation_ids: Arc<AtomicU64>,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation")
            .field("number", &self.number)
            .field("revision", &self.revision.get())
            .finish_non_exhaustive()
    }
}

impl Generation {
    /// Builds generation `number` on `connection`. Its reader reports the end
    /// it sees to `owner`; nothing else does.
    pub(super) fn begin(
        number: u64,
        connection: impl GenerationStream,
        correlation_ids: Arc<AtomicU64>,
        owner: Option<EndSink>,
    ) -> (Arc<Self>, BoxedRead, ReaderLink) {
        let (read, write) = connection.into_halves();
        let (cancel, cancelled) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let generation = Arc::new(Self {
            number,
            writer: tokio::sync::Mutex::new(Some(write)),
            slots: std::sync::Mutex::new(Slots {
                cancel_reader: Some(cancel),
                stop_reader: Some(stop),
                ..Slots::default()
            }),
            request_lock: tokio::sync::Mutex::new(()),
            revision: OnceLock::new(),
            window_id: std::sync::Mutex::new(None),
            correlation_ids,
        });
        let link = ReaderLink::new(number, cancelled, stopped, owner);
        (generation, read, link)
    }

    /// The generation's number, unique within its client.
    pub(crate) fn number(&self) -> u64 {
        self.number
    }

    /// The wire revision opening settled, `None` while it is being probed.
    pub(crate) fn revision(&self) -> Option<u32> {
        self.revision.get().copied()
    }

    /// Records the wire revision opening settled; once one is recorded, a
    /// later call changes nothing.
    pub(super) fn record_revision(&self, revision: u32) {
        let _ = self.revision.set(revision);
    }

    /// The window frames on this generation are routed to.
    pub(crate) fn window_id(&self) -> Option<String> {
        lock_blocking(&self.window_id).clone()
    }

    pub(crate) fn set_window_id(&self, window_id: Option<String>) {
        *lock_blocking(&self.window_id) = window_id;
    }

    /// Writes `message` under a fresh correlation id and waits for its answer.
    ///
    /// # Errors
    /// [`IpcError::NotConnected`] when the generation ended before the frame
    /// was written, [`IpcError::Unsupported`] when its daemon speaks another
    /// wire revision, [`IpcError::Write`] when the write failed and
    /// [`IpcError::ConnectionLost`] when the generation ended after the frame
    /// was written; otherwise what the daemon answered.
    pub(crate) async fn request(
        &self,
        message: &ClientMessage,
        kind: PendingKind,
    ) -> Result<Reply, IpcError> {
        self.request_step(message, kind, None).await.into_result()
    }

    /// [`Generation::request`], telling an answer from none: the failures of
    /// the connection and of the generation's end come back as
    /// [`StepReply::Unanswered`]. With `take` the request is an opening step:
    /// the answer it is given, the wire gate's refusal included, is
    /// recorded as the step's answer in the same step that hands it over, so
    /// the generation's end carries it.
    pub(crate) async fn request_step(
        &self,
        message: &ClientMessage,
        kind: PendingKind,
        take: Option<Take>,
    ) -> StepReply<Result<Reply, IpcError>> {
        let _in_flight = self.request_lock.lock().await;
        let correlation_id = self.correlation_ids.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        let revision = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return StepReply::Unanswered(IpcError::NotConnected);
            }
            let revision = self.revision();
            if let Err(refusal) = wire::gate(message, revision) {
                let refusal = IpcError::from(refusal);
                slots.record_refusal(take, &refusal);
                return StepReply::Answered(Err(refusal));
            }
            slots.pending = Some(Pending {
                kind,
                correlation_id,
                sender,
                take,
            });
            revision
        };
        // The gate passed for this revision above, so a failure here is the
        // connection's.
        if let Err(error) = self
            .write_frame(message, Some(correlation_id), revision)
            .await
        {
            self.forget(correlation_id);
            return StepReply::Unanswered(error);
        }
        // No deadline: a daemon that has the frame may still carry it out, so
        // only its answer or the generation's end can settle the request.
        match receiver.await {
            Ok(result) => StepReply::Answered(result),
            // The end took the slot after the frame was written: the daemon
            // may have acted on it.
            Err(_) => StepReply::Unanswered(IpcError::ConnectionLost),
        }
    }

    /// A refusal a request met before it was written. On an opening step it
    /// is the step's answer only while the generation has not ended, and is
    /// recorded as such in the same step; after the end it is no answer.
    pub(crate) fn refuse(&self, take: Option<Take>, refusal: IpcError) -> StepReply<IpcError> {
        if take.is_some() {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return StepReply::Unanswered(IpcError::NotConnected);
            }
            slots.record_refusal(take, &refusal);
        }
        StepReply::Answered(refusal)
    }

    /// Writes `message` with no correlation id: nothing waits for an answer.
    ///
    /// # Errors
    /// [`IpcError::NotConnected`] when the generation ended,
    /// [`IpcError::Unsupported`] when its daemon speaks another wire revision,
    /// and [`IpcError::Write`] when the write failed.
    pub(crate) async fn write(&self, message: &ClientMessage) -> Result<(), IpcError> {
        self.write_frame(message, None, self.revision()).await
    }

    async fn write_frame(
        &self,
        message: &ClientMessage,
        correlation_id: Option<u64>,
        revision: Option<u32>,
    ) -> Result<(), IpcError> {
        let mut writer = self.writer.lock().await;
        if lock_blocking(&self.slots).ended {
            *writer = None;
        }
        let Some(half) = writer.as_mut() else {
            return Err(IpcError::NotConnected);
        };
        match wire::write_frame(half, message, correlation_id, revision).await {
            Err(wire::WriteError::Io(error)) => {
                // Taken away: a frame may be cut short, so nothing more is
                // written on this connection.
                *writer = None;
                // A peer that closed shows its reader every byte it sent and
                // then the end, so the reader tells how the connection ended.
                // Any other failure would leave a live peer's reader waiting.
                if !yagura_os::write_error_shows_peer_closed(&error) {
                    self.stop_reader();
                }
                Err(wire::WriteError::Io(error).into())
            }
            outcome => {
                // An end that came while this write held the half lets it go now.
                if lock_blocking(&self.slots).ended {
                    *writer = None;
                }
                outcome.map_err(IpcError::from)
            }
        }
    }

    /// Tells the reader to stop, once, while the generation has not ended.
    fn stop_reader(&self) {
        let stop = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                None
            } else {
                slots.stop_reader.take()
            }
        };
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
    }

    /// Hands `result` to the request waiting, when the answer is its own:
    /// under its correlation id once the daemon returns them, and by `kind`
    /// alone while opening and with an older daemon.
    pub(crate) fn settle(
        &self,
        correlation_id: Option<u64>,
        kind: PendingKind,
        result: Result<Reply, IpcError>,
    ) -> Settled {
        let mut slots = lock_blocking(&self.slots);
        let Some(pending) = slots.pending.as_ref() else {
            return Settled::Unanswered;
        };
        let answers = match correlation_id {
            // An error answers whatever request carried its key.
            Some(key) if key == pending.correlation_id => {
                if result.is_ok() && kind != pending.kind {
                    return Settled::Broken;
                }
                true
            }
            Some(_) => false,
            None => !answers_carry_correlation_ids(self.revision()) && kind == pending.kind,
        };
        if !answers {
            return Settled::Unanswered;
        }
        if let Some(pending) = slots.pending.take() {
            if let Some(take) = pending.take {
                slots.taken = Some(take(&result));
            }
            let _ = pending.sender.send(result);
        }
        Settled::Answered
    }

    /// The request waiting for an answer, if any.
    pub(crate) fn awaiting(&self) -> Option<Awaiting> {
        lock_blocking(&self.slots)
            .pending
            .as_ref()
            .map(|pending| Awaiting {
                kind: pending.kind,
                correlation_id: pending.correlation_id,
            })
    }

    /// Clears the slot of the request under `correlation_id`, which gave up.
    fn forget(&self, correlation_id: u64) {
        let mut slots = lock_blocking(&self.slots);
        if slots
            .pending
            .as_ref()
            .is_some_and(|pending| pending.correlation_id == correlation_id)
        {
            slots.pending = None;
        }
    }

    /// Ends the generation, in one step with no await: the request waiting
    /// hears of it, the reader stops and the write half is let go of, by a
    /// writer midway as soon as its write is done. Returns the last answer an
    /// opening step took before; later calls do nothing and return `None`.
    pub(super) fn end(&self) -> Option<TakenAnswer> {
        let (pending, cancel, taken) = {
            let mut slots = lock_blocking(&self.slots);
            if slots.ended {
                return None;
            }
            slots.ended = true;
            (
                slots.pending.take(),
                slots.cancel_reader.take(),
                slots.taken,
            )
        };
        drop(pending);
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
        if let Ok(mut writer) = self.writer.try_lock() {
            *writer = None;
        }
        taken
    }
}

#[cfg(test)]
mod request_tests;
