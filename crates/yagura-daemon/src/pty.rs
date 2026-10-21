//! PTY session management on top of [`yagura_os::pty`].

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Semaphore, watch};

use crate::child_env::ChildBaseEnv;
use crate::config::DaemonConfig;
use crate::detector::{SharedScreen, VtScreen};
use crate::input_encode::InputModes;
use crate::terminal_identity;

/// Error type for PTY operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PtyError {
    #[error("failed to spawn child process")]
    SpawnFailed,
    #[error("bundled ConPTY missing or not loaded; reinstall")]
    BundledConptyUnavailable,
    #[error("failed to write to PTY input")]
    WriteFailed,
    #[error("failed to resize PTY")]
    ResizeFailed,
    #[error("cols or rows outside the window size limits")]
    SizeOutOfRange,
    #[error("child process has exited")]
    ChildExited,
}

pub type PtyResult<T> = Result<T, PtyError>;

/// How long `reader_loop` keeps draining PTY output after the child's exit has
/// been confirmed, before it gives up waiting for an EOF that may never come.
///
/// Well above the graceful close budget (`close_grace_ms` + `close_settle_ms`,
/// 700 ms with both at their defaults), so an ordinary close is never cut
/// short by it; it only bounds the reader's lifetime when something else keeps
/// the PTY open and producing output after the child is gone.
const POST_EXIT_DRAIN: Duration = Duration::from_secs(2);

/// How long [`PtySession::terminate`] gives the child to act on the first,
/// refusable kill before asking the OS boundary to force it down
/// ([`yagura_os::schedule_force_kill`], which is a no-op on Windows because the
/// first kill there already cannot be refused).
///
/// Sized against the graceful close budget: the escalation must fire well
/// inside `close_grace_ms` while still leaving a child that handles the signal
/// a chance to shut itself down. A smaller `close_grace_ms` is allowed and
/// simply means the process-group teardown reaches the child first. Not
/// exposed as a setting: it describes the kill mechanism, not an operator's
/// policy.
const TERMINATE_ESCALATION_GRACE: Duration = Duration::from_millis(250);

/// Output is fed to the terminal model this many bytes at a time.
const MODEL_SLICE_BYTES: usize = 1024;

/// How long the reader keeps the model's lock before it publishes what it has
/// fed and offers the lock to a waiting reader. A hold is therefore never much
/// longer than this plus one slice's bounded work.
const MODEL_LOCK_HOLD: Duration = Duration::from_millis(2);

/// How many blocking threads may wait on one window's model at once. Every
/// one of them waits on the same lock, so more would only queue there.
const MODEL_ACCESS_PER_WINDOW: usize = 1;

/// Reply bytes waiting for a window's reply writer; a batch that would go
/// past this is dropped.
const MAX_QUEUED_REPLY_BYTES: usize = 64 * 1024;

/// Whether `cols` x `rows` is a size a window takes (see
/// [`yagura_protocol::MAX_WINDOW_DIMENSION`]).
pub(crate) fn size_within_limit(cols: u16, rows: u16) -> bool {
    use yagura_protocol::{MAX_WINDOW_DIMENSION, MIN_WINDOW_COLS, MIN_WINDOW_ROWS};
    (MIN_WINDOW_COLS..=MAX_WINDOW_DIMENSION).contains(&cols)
        && (MIN_WINDOW_ROWS..=MAX_WINDOW_DIMENSION).contains(&rows)
}

/// The raw bytes a window's child wrote, kept only for tests that must see
/// what was written rather than what the terminal shows (an echo and the
/// child's own print of the same line are two writes but one screen line).
/// Compiled to nothing without the `fault-injection` feature.
#[derive(Clone, Default)]
struct RawCapture {
    #[cfg(feature = "fault-injection")]
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl RawCapture {
    /// The most bytes kept; older ones are dropped.
    #[cfg(feature = "fault-injection")]
    const LIMIT: usize = 4 * 1024 * 1024;

    #[cfg(feature = "fault-injection")]
    fn record(&self, output: &[u8]) {
        let mut bytes = lock_recover(&self.bytes);
        bytes.extend_from_slice(output);
        if bytes.len() > Self::LIMIT {
            let excess = bytes.len() - Self::LIMIT;
            bytes.drain(..excess);
        }
    }

    #[cfg(not(feature = "fault-injection"))]
    fn record(&self, _output: &[u8]) {}
}

/// A window's model lock held by [`PtySession::hold_model`]; dropping it
/// releases the lock.
#[cfg(feature = "fault-injection")]
pub struct ModelHold {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "fault-injection")]
impl Drop for ModelHold {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Makes the next PTY resize fail, for tests of what a failed resize leaves
/// behind. Only the `fault-injection` feature can arm it; `take` is the same
/// in every build so the shipped resize path is the one the tests run.
#[derive(Default)]
struct ResizeFault {
    armed: AtomicBool,
}

impl ResizeFault {
    fn take(&self) -> bool {
        self.armed.swap(false, Ordering::SeqCst)
    }
}

/// Locks `mutex`, recovering the guard from poisoning instead of propagating
/// it.
///
/// A poisoned lock here only means a previous holder panicked while holding
/// it, which the workspace-wide `unwrap`/`panic!` ban makes exceptional; the
/// protected data itself is still structurally valid, so recovering the
/// guard (rather than failing the caller) is safe.
///
/// `pub(crate)` so [`crate::window`] can reuse it for its own
/// `std::sync::Mutex` fields rather than duplicating this recovery idiom.
pub(crate) fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The environment given to [`spawn_one_shot`]'s child.
///
/// `Debug` never shows a name or a value: an environment routinely carries
/// tokens.
pub enum CaptureChildEnv {
    /// The daemon's own environment, then [`ChildBaseEnv`], then `overlay`
    /// (a later entry for a name given more than once wins). Unlike a
    /// window's child, nothing is read from the Windows registry.
    Based {
        /// The daemon-wide base environment (see [`DaemonConfig::child_base_env`]).
        base: ChildBaseEnv,
        /// Variables layered on top of `base`.
        overlay: Vec<(OsString, OsString)>,
    },
    /// Only these variables; nothing inherited from the daemon and no base
    /// environment. Used by the login-shell read itself, which must start
    /// from exactly the environment it was given rather than the daemon's
    /// own.
    Replaced(Vec<(OsString, OsString)>),
}

impl std::fmt::Debug for CaptureChildEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Based { base, overlay } => f
                .debug_struct("CaptureChildEnv::Based")
                .field("base_len", &base.len())
                .field("overlay_len", &overlay.len())
                .finish(),
            Self::Replaced(vars) => f
                .debug_struct("CaptureChildEnv::Replaced")
                .field("len", &vars.len())
                .finish(),
        }
    }
}

/// Starts a one-shot, non-PTY program with `env` layered as
/// [`CaptureChildEnv`] describes, on a blocking thread. Its output is read
/// through the returned [`yagura_os::CapturedChild`]'s
/// [`collect`](yagura_os::CapturedChild::collect) /
/// [`collect_until`](yagura_os::CapturedChild::collect_until).
///
/// `stdin` is handed to the child unchanged: `None` gives it a closed stdin,
/// `Some` a pipe fed those bytes and then closed (see
/// [`yagura_os::spawn_captured`]).
///
/// # Errors
///
/// Returns whatever [`yagura_os::spawn_captured`] returns, and an I/O error
/// if the blocking spawn task itself could not be joined.
// The one caller of `yagura_os::spawn_captured`, so every one-shot gets the
// daemon's environment layering.
#[allow(clippy::disallowed_methods)]
pub async fn spawn_one_shot(
    argv: Vec<String>,
    cwd: Option<PathBuf>,
    env: CaptureChildEnv,
    stdin: Option<Vec<u8>>,
) -> std::io::Result<yagura_os::CapturedChild> {
    tokio::task::spawn_blocking(move || {
        let (vars, replace): (Vec<(OsString, OsString)>, bool) = match env {
            CaptureChildEnv::Based { base, overlay } => {
                let mut vars: Vec<(OsString, OsString)> = base
                    .iter()
                    .map(|(name, value)| (name.to_os_string(), value.to_os_string()))
                    .collect();
                vars.extend(overlay);
                (vars, false)
            }
            CaptureChildEnv::Replaced(vars) => (vars, true),
        };
        let capture_env = if replace {
            yagura_os::CaptureEnv::Replace(&vars)
        } else {
            yagura_os::CaptureEnv::Inherit(&vars)
        };
        yagura_os::spawn_captured(&argv, cwd.as_deref(), capture_env, stdin)
    })
    .await
    .unwrap_or_else(|_join_error| {
        Err(std::io::Error::other(
            "the one-shot capture spawn task panicked",
        ))
    })
}

/// The count of a window's writes; see [`PtySession::write_input`].
/// Published as a watch so a waiter woken by a change reads the very count
/// that woke it.
struct WriteLedger {
    count: watch::Sender<u64>,
    /// The count of the latest write made with [`TakeBackPrefix::Withhold`].
    withheld: Mutex<Option<u64>>,
}

impl Default for WriteLedger {
    fn default() -> Self {
        Self {
            count: watch::Sender::new(0),
            withheld: Mutex::new(None),
        }
    }
}

impl WriteLedger {
    fn count(&self) {
        self.count
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    fn count_withheld(&self) {
        self.count();
        *lock_recover(&self.withheld) = Some(*self.count.borrow());
    }
}

/// Whether a plain write puts the registered take-back in front of its own
/// bytes; see [`PtySession::write_input_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeBackPrefix {
    /// Write the registered take-back first.
    Apply,
    /// Write no take-back; the registration is dropped all the same.
    Withhold,
}

/// What [`PtySession::write_input_if_unmoved`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardedWrite {
    /// The ledger was where the caller expected, and the bytes were written.
    Written,
    /// Another write had been counted since; nothing was written or counted.
    Moved,
}

/// One take-back call's generation, from [`PtySession::begin_take_back_call`].
/// A registration made under a call that has ended is never placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeBackCall(u64);

/// How a ledger-checked write leaves the window's take-back registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeBackRegistration {
    /// Leave the registration as it is.
    Keep,
    /// Remove the registration.
    Empty,
    /// Register `text` as the daemon's part of the composer, taken back with
    /// `clear` keys, unless `call` has already ended.
    Set {
        /// The call that owns the registration.
        call: TakeBackCall,
        /// The daemon's text in the composer.
        text: String,
        /// One take-back key.
        clear: Vec<u8>,
    },
}

/// The daemon's text in a composer, held as text and key rather than bytes so
/// the key count follows the width at the moment it is written.
struct Registered {
    call: TakeBackCall,
    text: String,
    clear: Vec<u8>,
}

impl Registered {
    fn keys(&self, cols: u16) -> Vec<u8> {
        self.clear
            .repeat(crate::input::take_back_key_count(&self.text, cols))
    }
}

#[derive(Default)]
struct TakeBackState {
    next_call: u64,
    live_calls: HashSet<u64>,
    registered: Option<Registered>,
}

/// The write end of a window's PTY, which teardown can hand back without
/// waiting for a writer that is stuck inside a write.
struct WriterSlot {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    release_requested: AtomicBool,
    /// Taken inside `writer`'s lock, never the other way round, and never held
    /// across a write, so a stuck PTY never holds it.
    take_back: Mutex<TakeBackState>,
}

impl WriterSlot {
    fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(Some(writer)),
            release_requested: AtomicBool::new(false),
            take_back: Mutex::new(TakeBackState::default()),
        }
    }

    fn begin_take_back_call(&self) -> TakeBackCall {
        let mut state = lock_recover(&self.take_back);
        state.next_call = state.next_call.wrapping_add(1);
        let call = state.next_call;
        state.live_calls.insert(call);
        TakeBackCall(call)
    }

    fn end_take_back_call(&self, call: TakeBackCall) {
        let mut state = lock_recover(&self.take_back);
        state.live_calls.remove(&call.0);
        if state.registered.as_ref().is_some_and(|r| r.call == call) {
            state.registered = None;
        }
    }

    fn take_registration(&self) -> Option<Registered> {
        lock_recover(&self.take_back).registered.take()
    }

    fn update_registration(&self, update: TakeBackRegistration) {
        let mut state = lock_recover(&self.take_back);
        match update {
            TakeBackRegistration::Keep => {}
            TakeBackRegistration::Empty => state.registered = None,
            TakeBackRegistration::Set { call, text, clear } => {
                if state.live_calls.contains(&call.0) {
                    state.registered = Some(Registered { call, text, clear });
                }
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Box<dyn Write + Send>>> {
        lock_recover(&self.writer)
    }

    /// Drops the writer now when nobody holds it; otherwise the holder drops
    /// it through [`WriterSlot::release_if_requested`] once its write ends.
    fn request_release(&self) {
        self.release_requested.store(true, Ordering::SeqCst);
        self.release_if_requested();
    }

    /// Called by every writer after it let go of the lock, so a release
    /// requested while it was inside a write is not lost.
    fn release_if_requested(&self) {
        if !self.release_requested.load(Ordering::SeqCst) {
            return;
        }
        match self.writer.try_lock() {
            Ok(mut guard) => *guard = None,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => *poisoned.into_inner() = None,
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
    }
}

/// Replies to the child's terminal queries on their way to the PTY. The
/// reader only queues them, and one writer thread per window writes them, so
/// a child that stops reading its input never stops the reader.
#[derive(Default)]
struct ReplyQueue {
    state: Mutex<ReplyQueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct ReplyQueueState {
    bytes: Vec<u8>,
    closed: bool,
    /// Set while replies are being dropped, so a flood logs once.
    dropping: bool,
}

impl ReplyQueue {
    /// Queues `replies` whole, or drops them when the queue has no room;
    /// `dropped` replies were already dropped before reaching it.
    fn push(&self, replies: Vec<u8>, dropped: usize) {
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

    /// Ends the writer thread once it has nothing in hand.
    fn close(&self) {
        let mut state = lock_recover(&self.state);
        state.closed = true;
        state.bytes.clear();
        self.ready.notify_one();
    }

    /// Waits for queued replies; `None` once the queue is closed.
    fn next(&self) -> Option<Vec<u8>> {
        let mut state = lock_recover(&self.state);
        loop {
            if state.closed {
                return None;
            }
            if !state.bytes.is_empty() {
                return Some(std::mem::take(&mut state.bytes));
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// The writer thread's body. Replies take the writer's lock like any other
    /// write, so they never land inside another write's bytes, but they are
    /// not counted in the write ledger: the child asked for them. A failed
    /// write leaves the PTY unusable, so the writer is dropped with it.
    fn write_all_to(&self, writer: &WriterSlot) {
        while let Some(bytes) = self.next() {
            let mut guard = writer.lock();
            let Some(w) = guard.as_mut() else {
                break;
            };
            if w.write_all(&bytes).and_then(|()| w.flush()).is_err() {
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

/// What input to a window is encoded against: the modes its child set and
/// the size of its screen, as the model last had them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputState {
    /// The modes that change what input becomes.
    pub modes: InputModes,
    /// The screen's column count.
    pub cols: u16,
    /// The screen's row count.
    pub rows: u16,
}

impl InputState {
    /// Whether `(col, row)`, counted from 0, is on the screen.
    #[must_use]
    pub fn contains_cell(&self, col: u16, row: u16) -> bool {
        col < self.cols && row < self.rows
    }
}

/// The model's [`InputState`], published by whoever changes the model while
/// it holds the model's lock, and read without taking that lock: encoding a
/// key never waits behind a child that floods its window.
struct PublishedInput(AtomicU64);

impl PublishedInput {
    fn of(screen: &VtScreen) -> Self {
        let published = Self(AtomicU64::new(0));
        published.publish(screen);
        published
    }

    fn publish(&self, screen: &VtScreen) {
        let (cols, rows) = screen.size();
        let bits = u64::from(screen.input_modes().to_bits())
            | u64::from(cols) << 16
            | u64::from(rows) << 32;
        self.0.store(bits, Ordering::Release);
    }

    fn load(&self) -> InputState {
        let bits = self.0.load(Ordering::Acquire);
        let field = |shift: u32| u16::try_from((bits >> shift) & 0xffff).unwrap_or(0);
        InputState {
            modes: InputModes::from_bits(field(0)),
            cols: field(16),
            rows: field(32),
        }
    }
}

/// Everything the reader thread updates as output arrives.
struct ModelFeed {
    screen: SharedScreen,
    input: Arc<PublishedInput>,
    /// Where the model's replies to the child's queries go.
    replies: Arc<ReplyQueue>,
    raw_capture: RawCapture,
    arrivals: watch::Sender<u64>,
}

impl ModelFeed {
    /// Feeds `output` to the model one slice at a time.
    ///
    /// Every release is fair: a reader waiting for the lock gets it before
    /// this thread can take it back for the next chunk, so a child that never
    /// stops writing cannot starve the readers. Mid-chunk the lock is offered
    /// after a hold of [`MODEL_LOCK_HOLD`] and kept when nobody is waiting.
    ///
    /// The model's replies to queries in `output` are taken under the lock and
    /// queued after it is released, never written here. The input state is
    /// published before every release, so it never lags what a reader that
    /// takes the lock sees.
    ///
    /// The arrival is announced last, once both the model and the raw capture
    /// hold `output`, so whoever it wakes finds the chunk in either.
    fn feed(&self, output: &[u8]) {
        let Self {
            screen,
            input,
            replies,
            raw_capture,
            arrivals,
        } = self;
        let mut guard = screen.lock();
        let mut held_since = Instant::now();
        for slice in output.chunks(MODEL_SLICE_BYTES) {
            if held_since.elapsed() >= MODEL_LOCK_HOLD {
                input.publish(&guard);
                parking_lot::MutexGuard::bump(&mut guard);
                held_since = Instant::now();
            }
            guard.feed(slice);
        }
        // Counted under the lock, so the count announced never runs ahead of
        // what the screen holds.
        let arrival = guard.note_output();
        input.publish(&guard);
        let (queued, dropped) = guard.take_replies();
        parking_lot::MutexGuard::unlock_fair(guard);
        replies.push(queued, dropped);
        raw_capture.record(output);
        arrivals.send_replace(arrival);
    }
}

/// Represents a PTY session holding a child process.
pub struct PtySession {
    /// Test seam; see [`RawCapture`].
    #[cfg(feature = "fault-injection")]
    raw_capture: RawCapture,

    /// Test seam; see [`ResizeFault`].
    resize_fault: ResizeFault,

    /// The output as a terminal shows it, at the PTY's size, and the one
    /// owner of that size. Locked only on blocking threads (see
    /// [`PtySession::with_screen`]), never on the async runtime's.
    screen: SharedScreen,

    /// Admits the blocking threads that wait on `screen` for async callers.
    model_access: Arc<Semaphore>,

    /// `screen`'s input state, readable without its lock.
    input: Arc<PublishedInput>,

    /// Orders resizes, so the model and the PTY always end at the size of the
    /// same request. Held across the PTY call, which readers never wait on.
    resize_order: tokio::sync::Mutex<()>,

    /// Counts the chunks of output fed to `screen`; changes once the screen
    /// already shows them.
    output_arrivals: watch::Receiver<u64>,

    /// Counts the writes to the child's input, before each reaches it;
    /// `screen` follows it (see [`VtScreen::follow_writes`]).
    input_writes: Arc<watch::Sender<u64>>,

    /// Resizes and closes the PTY. The exit monitor closes it once the child
    /// has exited.
    control: Arc<yagura_os::pty::PtyControl>,

    /// The child's process tree, which exists from before the child runs its
    /// first instruction. Shared with the exit monitor, which ends it when
    /// the child exits.
    group: Arc<yagura_os::ProcessGroup>,

    /// Writer handle to PTY slave
    writer: Arc<WriterSlot>,

    /// Every write [`PtySession::write_input`] made, counted while holding
    /// `writer`, so the count's order is the order the bytes reach the child.
    write_ledger: Arc<WriteLedger>,

    /// Replies to the child's terminal queries, written by their own thread.
    replies: Arc<ReplyQueue>,

    /// Kills the child.
    ///
    /// Kill and wait are held through *separate* handles: sharing one owner
    /// would let whichever of [`PtySession::terminate`] and the exit-monitor
    /// thread reached it first take the child from the other, so a terminate
    /// that lost that race would send no kill at all. The killer and the
    /// waiting child act on the same process without either owning it
    /// exclusively.
    killer: yagura_os::pty::PtyKiller,

    /// OS process ID of the child, captured at spawn time.
    child_process_id: u32,

    /// Exit status
    exit_code: Arc<AtomicI32>,
    is_exited: Arc<AtomicBool>,

    /// Set once [`PtySession::terminate`] has scheduled the
    /// [`yagura_os::schedule_force_kill`] escalation, so a repeat call (e.g.
    /// from both an explicit close and `Drop`) does not keep spawning more of
    /// its detached escalation threads. `terminate`'s own kill is re-sent
    /// unconditionally on every call regardless of this flag, since it is
    /// cheap and idempotent.
    force_kill_scheduled: AtomicBool,

    /// Child-exit notification, published once the child-monitor thread has
    /// finalized `exit_code` / `is_exited`.
    ///
    /// The monitor thread is the authoritative send point because it is the
    /// only place the exit code becomes known; `reader_loop` observes EOF
    /// without an exit code and therefore never sends here.
    exit_notify_rx: watch::Receiver<bool>,

    /// Reader thread handle
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl PtySession {
    /// Initial PTY size.
    const INITIAL_COLS: u16 = 80;
    const INITIAL_ROWS: u16 = 24;

    /// Spawn a child process with the given command in a new PTY, in the
    /// user's home directory.
    ///
    /// Convenience wrapper around [`PtySession::spawn_with_command_in`] with
    /// `cwd = None`.
    pub async fn spawn_with_command(argv: &[&str], config: &DaemonConfig) -> PtyResult<Self> {
        Self::spawn_with_command_in(argv, None, config).await
    }

    /// Spawn a child process with the given command in a new PTY, optionally
    /// in a specific working directory.
    ///
    /// A `cwd` that is `None`, or is not a directory, starts the child in the
    /// user's home directory (see [`yagura_os::pty::PtyCommand::cwd`]); the
    /// daemon's own working directory is not inherited.
    /// [`crate::window::WindowRegistry::open`] validates that a `Some` `cwd`
    /// exists and is a directory at its entry point, before this function is
    /// ever reached.
    pub async fn spawn_with_command_in(
        argv: &[&str],
        cwd: Option<&Path>,
        config: &DaemonConfig,
    ) -> PtyResult<Self> {
        Self::spawn_with_command_in_env(argv, cwd, &[], config).await
    }

    /// Spawn a child process with the given command in a new PTY, optionally
    /// in a specific working directory, with additional environment
    /// variables merged into the child's environment; this is how
    /// `YAGURA_STATUS_TOKEN` reaches a spawned window's child process.
    ///
    /// The child's environment is layered, later layers winning for a name
    /// they share: the base environment of [`yagura_os::pty::spawn`] (the
    /// daemon's own, plus the registry's variables on Windows), then
    /// [`DaemonConfig::child_base_env`], then
    /// the launching terminal's own variables taken away and
    /// [`terminal_identity::DECLARED_TERMINAL`] set, then `extra_env`, where
    /// a name given more than once keeps its last value.
    /// [`PtySession::spawn_with_command_in`] is this function with an empty
    /// `extra_env`.
    pub async fn spawn_with_command_in_env(
        argv: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        config: &DaemonConfig,
    ) -> PtyResult<Self> {
        Self::spawn_with_palette(argv, cwd, extra_env, None, config).await
    }

    /// [`PtySession::spawn_with_command_in_env`], with the window's terminal
    /// answering color queries with `palette` from the child's first output
    /// on. With `None` it leaves them unanswered until a palette is set.
    pub async fn spawn_with_palette(
        argv: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        palette: Option<yagura_protocol::Palette>,
        config: &DaemonConfig,
    ) -> PtyResult<Self> {
        if argv.is_empty() {
            return Err(PtyError::SpawnFailed);
        }

        let spawned = tokio::task::spawn_blocking({
            let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            let cwd = cwd.map(Path::to_path_buf);
            let extra_env: Vec<(String, String)> = extra_env
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            let child_base_env = config.child_base_env.clone();
            move || {
                use yagura_os::pty::EnvOp;

                // The window's terminal is yagura's, so what the launching
                // terminal or a login shell says about one goes, while an
                // explicit `extra_env` entry still wins.
                let mut env: Vec<EnvOp> = child_base_env
                    .iter()
                    .map(|(name, value)| EnvOp::Set(name.to_os_string(), value.to_os_string()))
                    .collect();
                let inherited: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
                let launcher_names = terminal_identity::launcher_terminal_names(
                    inherited
                        .iter()
                        .map(OsString::as_os_str)
                        .chain(child_base_env.iter().map(|(name, _)| name)),
                );
                env.extend(
                    launcher_names
                        .iter()
                        .map(|name| EnvOp::Remove(name.to_os_string())),
                );
                env.extend(
                    terminal_identity::DECLARED_TERMINAL
                        .iter()
                        .map(|(name, value)| {
                            EnvOp::Set(OsString::from(name), OsString::from(value))
                        }),
                );
                env.extend(
                    extra_env
                        .iter()
                        .map(|(key, value)| EnvOp::Set(OsString::from(key), OsString::from(value))),
                );

                yagura_os::pty::spawn(
                    &yagura_os::pty::PtyCommand { argv, cwd, env },
                    yagura_os::pty::PtySize {
                        cols: Self::INITIAL_COLS,
                        rows: Self::INITIAL_ROWS,
                    },
                )
                .map_err(|error| match error {
                    yagura_os::pty::PtySpawnError::BundledConptyUnavailable(reason) => {
                        tracing::error!(
                            %reason,
                            "bundled ConPTY unavailable; refusing to open a window"
                        );
                        PtyError::BundledConptyUnavailable
                    }
                    yagura_os::pty::PtySpawnError::Spawn(error) => {
                        tracing::debug!(%error, "failed to start the window's child");
                        PtyError::SpawnFailed
                    }
                })
            }
        })
        .await
        .map_err(|_| PtyError::SpawnFailed)??;
        let yagura_os::pty::PtySpawn {
            child,
            killer,
            group,
            control,
            reader,
            writer,
        } = spawned;

        let exit_code = Arc::new(AtomicI32::new(0));
        let is_exited = Arc::new(AtomicBool::new(false));

        let raw_capture = RawCapture::default();

        let (input_writes, writes_seen) = watch::channel(0u64);
        let mut screen = VtScreen::with_history(
            Self::INITIAL_COLS,
            Self::INITIAL_ROWS,
            config.read_output_max_lines,
        );
        screen.follow_writes(writes_seen);
        // Before the reader thread starts, so no query the child already
        // wrote is read without it.
        if let Some(palette) = palette {
            screen.set_palette(palette);
        }
        let screen = screen.into_shared();
        let input = Arc::new(PublishedInput::of(&screen.lock()));
        let (arrivals_tx, output_arrivals) = watch::channel(0u64);

        let control = Arc::new(control);
        let group = Arc::new(group);

        let writer = Arc::new(WriterSlot::new(writer));

        let replies = Arc::new(ReplyQueue::default());
        {
            let replies = Arc::clone(&replies);
            let writer = Arc::clone(&writer);
            thread::spawn(move || replies.write_all_to(&writer));
        }

        let reader_thread = {
            let feed = ModelFeed {
                screen: screen.clone(),
                input: Arc::clone(&input),
                replies: Arc::clone(&replies),
                raw_capture: raw_capture.clone(),
                arrivals: arrivals_tx,
            };
            let is_exited = is_exited.clone();

            thread::spawn(move || {
                Self::reader_loop(&feed, reader, &is_exited);
            })
        };

        let child_process_id = child.process_id();

        let (exit_notify_tx, exit_notify_rx) = watch::channel(false);

        let _child_monitor_thread = {
            let exit_code = exit_code.clone();
            let is_exited = is_exited.clone();
            let group = Arc::clone(&group);
            let control = Arc::clone(&control);

            // This thread is the child's only waiter; terminate() reaches the same
            // process through the killer. After the exit the wind-down order is
            // fixed: publish the exit, end the whole tree, then close the PTY.
            // The close comes last so the kill, not the close, decides the exit
            // code; the reader keeps reading (or is dropped), which lets the PTY's
            // host finish.
            thread::spawn(move || {
                let code = child.wait().unwrap_or(-1);
                exit_code.store(code, Ordering::SeqCst);
                is_exited.store(true, Ordering::SeqCst);
                let _ = exit_notify_tx.send(true);
                if let Err(error) = group.terminate() {
                    tracing::warn!(%error, "failed to end the exited child's process tree");
                }
                control.close();
            })
        };

        Ok(Self {
            #[cfg(feature = "fault-injection")]
            raw_capture,
            resize_fault: ResizeFault::default(),
            screen,
            model_access: Arc::new(Semaphore::new(MODEL_ACCESS_PER_WINDOW)),
            input,
            resize_order: tokio::sync::Mutex::new(()),
            output_arrivals,
            input_writes: Arc::new(input_writes),
            control,
            group,
            writer,
            write_ledger: Arc::new(WriteLedger::default()),
            replies,
            killer,
            child_process_id,
            exit_code,
            is_exited,
            force_kill_scheduled: AtomicBool::new(false),
            exit_notify_rx,
            _reader_thread: Some(reader_thread),
        })
    }

    /// Reader thread: reads PTY output and feeds it to the terminal model.
    ///
    /// Reads `is_exited` but writes neither it nor `exit_code`: the
    /// child-monitor thread is the sole writer of both, so their
    /// `exit_code` -> `is_exited` store order is never raced by this thread
    /// observing PTY EOF on its own.
    fn reader_loop(
        feed: &ModelFeed,
        mut reader: Box<dyn Read + Send>,
        is_exited: &Arc<AtomicBool>,
    ) {
        let mut buf = vec![0u8; 8192];
        let mut drain_deadline: Option<Instant> = None;

        loop {
            // Never stop on a termination request alone: on macOS a force-killed
            // PTY session leader stalls in the kernel's exit path unless the master
            // is drained. Only a confirmed exit starts the bounded POST_EXIT_DRAIN.
            if is_exited.load(Ordering::SeqCst) {
                let deadline =
                    *drain_deadline.get_or_insert_with(|| Instant::now() + POST_EXIT_DRAIN);
                if Instant::now() >= deadline {
                    break;
                }
            }

            match reader.read(&mut buf) {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    feed.feed(&buf[0..n]);
                }
                Err(_) => {
                    break;
                }
            }
        }
    }

    /// Runs `access` with the window's model on a blocking thread, admitted
    /// by [`MODEL_ACCESS_PER_WINDOW`]: the one way async code reaches the
    /// model, so waiting for its lock never holds up the async runtime. The
    /// permit stays with the blocking thread, so a caller that stops waiting
    /// does not free a slot while the thread still waits on the lock.
    ///
    /// Returns `None` when `access` could not run to completion (the runtime
    /// is shutting down).
    async fn with_screen<R: Send + 'static>(
        &self,
        access: impl FnOnce(&SharedScreen) -> R + Send + 'static,
    ) -> Option<R> {
        let permit = Arc::clone(&self.model_access).acquire_owned().await.ok()?;
        let screen = self.screen.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            access(&screen)
        })
        .await
        .ok()
    }

    /// [`PtySession::with_screen`] for an access that holds the lock
    /// throughout.
    pub(crate) async fn with_model<R: Send + 'static>(
        &self,
        read: impl FnOnce(&mut VtScreen) -> R + Send + 'static,
    ) -> Option<R> {
        self.with_screen(move |screen| read(&mut screen.lock()))
            .await
    }

    /// The window's text as a terminal shows it: see [`VtScreen::text_tail`].
    ///
    /// Returns `None` when the blocking read could not run to completion
    /// (the runtime is shutting down).
    pub async fn text_tail(&self, max_lines: usize, max_bytes: usize) -> Option<(String, bool)> {
        self.with_model(move |model| model.text_tail(max_lines, max_bytes))
            .await
    }

    /// The window's terminal model, for its
    /// [`DetectionEngine`](crate::detector::DetectionEngine) alone, which
    /// [`DetectorHandle`](crate::detector::DetectorHandle) drives on blocking
    /// threads.
    pub(crate) fn model_for_detection(&self) -> SharedScreen {
        self.screen.clone()
    }

    /// A receiver whose value changes each time output has been fed to the
    /// model. Its first `borrow_and_update` tells whether any output arrived
    /// before the call.
    pub fn output_arrivals(&self) -> watch::Receiver<u64> {
        self.output_arrivals.clone()
    }

    /// A receiver whose value changes each time something is written to the
    /// child's input ([`PtySession::write_input`]).
    pub(crate) fn input_writes(&self) -> watch::Receiver<u64> {
        self.input_writes.subscribe()
    }

    /// Counts one write to the child's input, before it is made.
    fn count_input_write(input_writes: &watch::Sender<u64>) {
        input_writes.send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Runs `read` against the window's model as it stands, the way a reader
    /// that shares it does (see [`VtScreen::read_guarded`]). A test seam (the
    /// `fault-injection` feature only).
    ///
    /// Returns `None` when `read` panicked or could not run to completion
    /// (the runtime is shutting down).
    #[cfg(feature = "fault-injection")]
    pub async fn read_screen<R: Send + 'static>(
        &self,
        read: impl FnOnce(&VtScreen) -> R + Send + 'static,
    ) -> Option<R> {
        self.with_model(move |model| model.read_guarded(read))
            .await
            .flatten()
    }

    /// Up to the last 4 MiB of the raw bytes the child wrote, escape sequences
    /// and all. A test seam (the `fault-injection` feature only): the daemon
    /// itself keeps no raw output.
    #[cfg(feature = "fault-injection")]
    pub fn captured_output(&self) -> Vec<u8> {
        lock_recover(&self.raw_capture.bytes).clone()
    }

    /// Makes the next [`PtySession::resize`] fail as if the PTY refused the
    /// size. A test seam (the `fault-injection` feature only).
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_resize(&self) {
        self.resize_fault.armed.store(true, Ordering::SeqCst);
    }

    /// Holds the window's model lock on a thread of its own until the returned
    /// [`ModelHold`] is dropped, for tests of what must not wait on that lock.
    /// Returns once the lock is held. A test seam (the `fault-injection`
    /// feature only).
    #[cfg(feature = "fault-injection")]
    #[must_use]
    pub fn hold_model(&self) -> ModelHold {
        let screen = self.screen.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _guard = screen.lock();
            let _ = held_tx.send(());
            let _ = release_rx.recv();
        });
        let _ = held_rx.recv();
        ModelHold {
            release: Some(release_tx),
            thread: Some(thread),
        }
    }

    /// Get a watch receiver that holds `true` once the child has exited.
    ///
    /// The value is already `true` when the child exited before this call, so
    /// callers must inspect [`watch::Receiver::borrow`] before awaiting
    /// [`watch::Receiver::changed`] to avoid missing an early exit.
    pub fn exit_notified(&self) -> watch::Receiver<bool> {
        self.exit_notify_rx.clone()
    }

    /// Sends the child a report it turned on, such as a focus change, the way
    /// replies to its terminal queries go: ordered with every other write but
    /// not counted in the write ledger, since a report never reaches the
    /// composer. Dropped once the child is gone.
    pub fn send_report(&self, report: Vec<u8>) {
        self.replies.push(report, 0);
    }

    /// [`PtySession::write_input_with`] with [`TakeBackPrefix::Apply`].
    ///
    /// # Errors
    ///
    /// As [`PtySession::write_input_with`].
    pub async fn write_input(&self, data: &[u8]) -> PtyResult<()> {
        self.write_input_with(data, TakeBackPrefix::Apply).await
    }

    /// Writes `data` to the child's input, counting it in this window's write
    /// ledger under the same lock that orders the bytes, so
    /// [`PtySession::writes_recorded`] never counts a write ahead of one whose
    /// bytes reached the child first. A write that then fails still counts.
    ///
    /// Under the same lock, the take-back registered by
    /// [`PtySession::write_input_if_unmoved`] is removed and, with
    /// [`TakeBackPrefix::Apply`], written ahead of `data`, its key count taken
    /// at the window's width at that moment, so a daemon's unsent text never
    /// ends up in front of another writer's bytes.
    ///
    /// # Errors
    ///
    /// [`PtyError::ChildExited`] once the child is gone (nothing is counted
    /// and the registration stays); [`PtyError::WriteFailed`] when the write
    /// itself fails.
    pub async fn write_input_with(&self, data: &[u8], prefix: TakeBackPrefix) -> PtyResult<()> {
        if self.is_exited.load(Ordering::SeqCst) {
            return Err(PtyError::ChildExited);
        }

        Self::count_input_write(&self.input_writes);
        let data = data.to_vec();
        let writer = self.writer.clone();
        let ledger = Arc::clone(&self.write_ledger);
        let input = Arc::clone(&self.input);
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock();
            let registered = writer.take_registration();
            let take_back = match prefix {
                TakeBackPrefix::Apply => registered.map(|r| r.keys(input.load().cols)),
                TakeBackPrefix::Withhold => None,
            };
            match prefix {
                TakeBackPrefix::Apply => ledger.count(),
                TakeBackPrefix::Withhold => ledger.count_withheld(),
            }
            let written = match writer_guard.as_mut() {
                Some(w) => take_back
                    .map_or(Ok(()), |keys| w.write_all(&keys))
                    .and_then(|()| w.write_all(&data)),
                None => Ok(()),
            };
            drop(writer_guard);
            writer.release_if_requested();
            written
        })
        .await
        .map_err(|_| PtyError::WriteFailed)?
        .map_err(|_| PtyError::WriteFailed)
    }

    /// Writes `data` only if the write ledger still reads `expected_seq`,
    /// checked under the lock that orders every write, so a decision made on
    /// a composer nobody else has written to since is never acted on after
    /// someone has. On a match, `before_write` runs, the write is counted and
    /// made, and `registration` is applied; otherwise nothing runs, nothing is
    /// counted, and [`GuardedWrite::Moved`] is returned.
    ///
    /// # Errors
    ///
    /// [`PtyError::ChildExited`] once the child is gone (nothing is counted);
    /// [`PtyError::WriteFailed`] when the write itself fails or
    /// `before_write` panics.
    pub async fn write_input_if_unmoved(
        &self,
        data: &[u8],
        expected_seq: u64,
        registration: TakeBackRegistration,
        before_write: impl FnOnce() + Send + 'static,
    ) -> PtyResult<GuardedWrite> {
        if self.is_exited.load(Ordering::SeqCst) {
            return Err(PtyError::ChildExited);
        }

        let data = data.to_vec();
        let writer = self.writer.clone();
        let ledger = Arc::clone(&self.write_ledger);
        let input_writes = Arc::clone(&self.input_writes);
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock();
            let outcome = if *ledger.count.borrow() == expected_seq {
                before_write();
                Self::count_input_write(&input_writes);
                ledger.count();
                let written = match writer_guard.as_mut() {
                    Some(w) => w.write_all(&data),
                    None => Ok(()),
                };
                // Applied even when the write failed: part of `data` may have
                // reached the composer, and extra take-back keys are harmless.
                writer.update_registration(registration);
                written.map(|()| GuardedWrite::Written)
            } else {
                Ok(GuardedWrite::Moved)
            };
            drop(writer_guard);
            writer.release_if_requested();
            outcome
        })
        .await
        .map_err(|_| PtyError::WriteFailed)?
        .map_err(|_| PtyError::WriteFailed)
    }

    /// Starts a take-back call: registrations made under the returned
    /// generation are placed until [`PtySession::end_take_back_call`].
    #[must_use]
    pub fn begin_take_back_call(&self) -> TakeBackCall {
        self.writer.begin_take_back_call()
    }

    /// Ends `call`: removes its registration, if it is the one in place, and
    /// places none of its later ones.
    pub fn end_take_back_call(&self, call: TakeBackCall) {
        self.writer.end_take_back_call(call);
    }

    /// How many writes [`PtySession::write_input_with`] and
    /// [`PtySession::write_input_if_unmoved`] have counted.
    #[must_use]
    pub fn writes_recorded(&self) -> u64 {
        *self.write_ledger.count.borrow()
    }

    /// The [`PtySession::writes_recorded`] count of the latest write made
    /// with [`TakeBackPrefix::Withhold`], if any.
    #[must_use]
    pub fn latest_withheld_write(&self) -> Option<u64> {
        *lock_recover(&self.write_ledger.withheld)
    }

    /// A receiver of [`PtySession::writes_recorded`]'s value, changing as each
    /// write is counted.
    #[must_use]
    pub fn subscribe_writes_recorded(&self) -> watch::Receiver<u64> {
        self.write_ledger.count.subscribe()
    }

    /// Resize the window to `cols` x `rows`: the terminal model first, then
    /// the PTY, the order a terminal view follows, so the child's redraw for
    /// the new size lands on a model that already has it.
    ///
    /// # Errors
    ///
    /// [`PtyError::SizeOutOfRange`] when either side is outside the limits
    /// in [`yagura_protocol`] (`MIN_WINDOW_COLS`, `MIN_WINDOW_ROWS`,
    /// `MAX_WINDOW_DIMENSION`), with nothing changed;
    /// [`PtyError::ChildExited`] once the child is gone; and
    /// [`PtyError::ResizeFailed`] when the PTY refuses the size, in which case
    /// the model is put back to its previous size.
    pub async fn resize(&self, cols: u16, rows: u16) -> PtyResult<()> {
        if !size_within_limit(cols, rows) {
            return Err(PtyError::SizeOutOfRange);
        }
        if self.is_exited.load(Ordering::SeqCst) {
            return Err(PtyError::ChildExited);
        }

        let _order = self.resize_order.lock().await;
        let input = Arc::clone(&self.input);
        let previous = self
            .with_model(move |model| {
                let previous = model.size();
                model.resize(cols, rows);
                input.publish(model);
                previous
            })
            .await
            .ok_or(PtyError::ResizeFailed)?;

        // Outside the model's permit: readers of the model never wait on the
        // PTY call, and output keeps flowing into the model meanwhile.
        let control = Arc::clone(&self.control);
        let injected_failure = self.resize_fault.take();
        let refused = tokio::task::spawn_blocking(move || {
            injected_failure || control.resize(cols, rows).is_err()
        })
        .await
        .unwrap_or(true);
        if refused {
            let input = Arc::clone(&self.input);
            self.with_model(move |model| {
                model.resize(previous.0, previous.1);
                input.publish(model);
            })
            .await;
            return Err(PtyError::ResizeFailed);
        }
        Ok(())
    }

    /// The modes and size input to this window is encoded against, as the
    /// model last published them. Never waits on the model's lock.
    #[must_use]
    pub fn input_state(&self) -> InputState {
        self.input.load()
    }

    /// The window's size as `(cols, rows)`, as its terminal model has it.
    ///
    /// Returns `None` when the blocking read could not run to completion
    /// (the runtime is shutting down).
    pub async fn size(&self) -> Option<(u16, u16)> {
        self.with_model(|model| model.size()).await
    }

    /// Get the OS process ID of the child, as captured when it was spawned.
    ///
    /// The value is not cleared when the child exits, so callers must not
    /// treat it as a liveness indicator; use [`PtySession::exit_status`] for
    /// that.
    pub fn child_process_id(&self) -> u32 {
        self.child_process_id
    }

    /// Get the exit status of the child process.
    /// Returns `None` if running, `Some(code)` if exited; the status is
    /// retained indefinitely.
    ///
    /// The code is whatever the child actually exited with, including when
    /// [`PtySession::terminate`] is what ended it -- `1` for a killed child
    /// (`TerminateProcess(handle, 1)` on Windows, a signal exit mapped to `1`
    /// on Unix). A killed `1` and a child that genuinely exited with `1` are
    /// therefore indistinguishable here; callers that need to tell them apart
    /// have to track the terminate themselves.
    pub fn exit_status(&self) -> Option<i32> {
        if self.is_exited.load(Ordering::SeqCst) {
            Some(self.exit_code.load(Ordering::SeqCst))
        } else {
            None
        }
    }
}

impl PtySession {
    /// Terminate the child process and release the PTY writer. Takes `&self` so
    /// it can run from `Drop` and from synchronous close handlers; nothing
    /// here sleeps or waits for a writer lock.
    ///
    /// The reader thread keeps running (it ends on EOF or a read error only),
    /// so the master stays drained while the child is killed: a force-killed
    /// PTY session leader otherwise stalls in the macOS kernel's exit path.
    ///
    /// The kill is sent explicitly and *before* the writer is closed, because
    /// closing the writer is itself a console-close kill on Windows. Ordering
    /// the explicit kill first keeps it, not the console close, deciding the
    /// exit code, and a console close is not guaranteed to reach a child under
    /// load. Once the child is gone the monitor thread ends its whole process
    /// tree and closes the PTY, so this reaches descendants too;
    /// [`PtySession::terminate_tree`] ends the tree without waiting for that.
    ///
    /// Returns as soon as the kill has been *sent*; the exit itself is observed
    /// by the monitor thread, and on Unix the escalation to an uncatchable
    /// signal happens on a thread inside [`yagura_os::schedule_force_kill`].
    pub fn terminate(&self) {
        // A child whose exit has already been observed must not be signalled:
        // its process id may by then belong to an unrelated process. On Unix a
        // narrow window remains, inherent to addressing a process by id.
        if !self.is_exited.load(Ordering::SeqCst) {
            // The result is discarded: a failed signal on Unix means the child
            // is already gone.
            let _ = self.killer.kill();

            // Scheduled once per session: a repeat terminate() call (Drop
            // after an explicit close, for example) must not keep spawning
            // more detached escalation threads for the same child.
            if !self.force_kill_scheduled.swap(true, Ordering::SeqCst) {
                yagura_os::schedule_force_kill(
                    self.child_process_id,
                    TERMINATE_ESCALATION_GRACE,
                    self.is_exited.clone(),
                );
            }
        }

        self.replies.close();
        // Released after the kill above (even when the child had already
        // exited) so that the kill, not this close, decides the exit code on
        // Windows. A writer stuck inside a write is not waited for: it drops
        // the writer when its write fails or ends, which happens once the
        // process tree is gone.
        self.writer.request_release();
    }
}

impl PtySession {
    /// Ends the child's whole process tree now: the child and everything it
    /// started. Idempotent. The monitor thread does the same once the child
    /// has exited, so this is only needed to end the tree while the child is
    /// still running.
    ///
    /// # Errors
    ///
    /// Returns the OS boundary's error when the tree cannot be ended; a later
    /// call tries again.
    pub fn terminate_tree(&self) -> Result<(), yagura_os::OsError> {
        self.group.terminate()
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.terminate();

        // Detach the reader thread handle without blocking: the child was
        // killed above, so the reader ends on the resulting EOF.
        let _ = self._reader_thread.take();
    }
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("child_process_id", &self.child_process_id)
            .field("is_exited", &self.is_exited.load(Ordering::SeqCst))
            .field("exit_code", &self.exit_code.load(Ordering::SeqCst))
            .finish_non_exhaustive()
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
        assert_eq!(queue.next(), Some(vec![b'a'; MAX_QUEUED_REPLY_BYTES]));
    }

    #[test]
    fn reply_batches_that_fit_together_are_all_kept_in_order() {
        let queue = ReplyQueue::default();
        queue.push(vec![b'a'; 100], 0);
        queue.push(vec![b'b'; 1000], 0);
        let mut expected = vec![b'a'; 100];
        expected.resize(1100, b'b');
        assert_eq!(queue.next(), Some(expected));
    }

    #[test]
    fn a_cell_is_on_the_screen_only_below_both_the_column_and_the_row_count() {
        let input = InputState {
            modes: InputModes::default(),
            cols: 80,
            rows: 24,
        };
        assert!(input.contains_cell(79, 23));
        assert!(!input.contains_cell(80, 0));
        assert!(!input.contains_cell(0, 24));
    }

    #[test]
    fn the_reader_still_feeds_output_left_in_the_pty_once_the_exit_is_known() {
        let screen = VtScreen::with_history(80, 24, 100).into_shared();
        let input = Arc::new(PublishedInput::of(&screen.lock()));
        let (arrivals, _arrivals_rx) = watch::channel(0u64);
        let feed = ModelFeed {
            screen: screen.clone(),
            input,
            replies: Arc::new(ReplyQueue::default()),
            raw_capture: RawCapture::default(),
            arrivals,
        };
        // The child's last words are still unread when its exit is observed.
        let is_exited = Arc::new(AtomicBool::new(true));
        let left_in_pty: Box<dyn Read + Send> =
            Box::new(std::io::Cursor::new(b"LAST-WORDS".to_vec()));

        PtySession::reader_loop(&feed, left_in_pty, &is_exited);

        let (text, _) = screen.lock().text_tail(24, 4096);
        assert!(
            text.contains("LAST-WORDS"),
            "output written before the exit must still reach the model; it shows {text:?}"
        );
    }
}
