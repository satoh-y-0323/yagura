//! A window's terminal as the daemon holds it: the terminal model and the
//! order of writes, with the PTY and the child kept by the window's holder
//! (`yagura-holder`, reached through [`holder_link`]).

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{Semaphore, oneshot, watch};
use yagura_protocol::holder::is_plain_window_id;

use crate::child_env::ChildBaseEnv;
use crate::config::DaemonConfig;
use crate::detector::{LocalScreen, SharedLocalScreen};
use crate::holder_host::HolderHost;
#[cfg(feature = "fault-injection")]
use yagura_protocol::ScreenCopy;
use yagura_protocol::{InputModes, ScreenPush};
use yagura_term::screen_view::View;

mod env;
mod feeder;
mod focus;
mod holder_link;
mod reply_queue;
mod views;

use feeder::{ModelFeed, feed_model};
pub use focus::FocusInterval;
use holder_link::{ExitReport, Feed, Link};
use reply_queue::ReplyQueue;
pub use views::AttachGeneration;
use views::ViewSlot;
pub(crate) use views::{EncodedFrame, FrameSend, FrameSink, SendKey, ViewNote};

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

/// How many blocking threads may wait on one window's model at once. Every
/// one of them waits on the same lock, so more would only queue there.
const MODEL_ACCESS_PER_WINDOW: usize = 1;

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
    fn of(copy: &ScreenPush) -> Self {
        let published = Self(AtomicU64::new(0));
        published.publish(copy);
        published
    }

    /// Publishes the modes and size of the copy `copy` closed.
    fn publish(&self, copy: &ScreenPush) {
        let bits = u64::from(copy.input_modes.to_bits())
            | u64::from(copy.cols) << 16
            | u64::from(copy.rows) << 32;
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

/// The window's attach slot and the way to wake its feeder for it.
#[derive(Clone)]
struct Views {
    slot: Arc<ViewSlot>,
    feed: std::sync::mpsc::Sender<Feed>,
}

impl Views {
    fn wake_if(&self, wake: bool) {
        if wake {
            let _ = self.feed.send(Feed::View);
        }
    }
}

/// What [`PtySession::spawn_window`] opens.
pub(crate) struct WindowSpawn<'a> {
    /// Names the holder's files and endpoint: a
    /// [plain](is_plain_window_id) identifier.
    pub(crate) window_id: &'a str,
    pub(crate) argv: &'a [&'a str],
    /// `None`, or a directory that does not exist, is the user's home
    /// directory (see [`yagura_os::pty::PtyCommand::cwd`]).
    pub(crate) cwd: Option<&'a Path>,
    pub(crate) extra_env: &'a [(&'a str, &'a str)],
    pub(crate) removed_env: &'a [&'a str],
    /// Answers color queries from the child's first output on; `None` leaves
    /// them unanswered until a palette is set.
    pub(crate) palette: Option<yagura_protocol::Palette>,
}

/// A window's terminal: its model and the order of writes in the daemon, its
/// PTY and child in the window's holder process.
pub struct PtySession {
    /// Test seam; see [`RawCapture`].
    #[cfg(feature = "fault-injection")]
    raw_capture: RawCapture,

    /// Test seam; see [`ResizeFault`].
    resize_fault: ResizeFault,

    /// The output as a terminal shows it, at the PTY's size, and the one
    /// owner of that size. Locked only on blocking threads (see
    /// [`PtySession::with_screen`]), never on the async runtime's.
    screen: SharedLocalScreen,

    /// Admits the blocking threads that wait on `screen` for async callers.
    model_access: Arc<Semaphore>,

    /// `screen`'s input state, readable without its lock.
    input: Arc<PublishedInput>,

    /// Whether the window's program flattens console key records, decided
    /// from its `argv[0]` when it opens (see
    /// [`InputModes::key_records_flattened`]). The child's output never
    /// changes it, so it lives here rather than in `input`'s bits.
    key_records_flattened: bool,

    /// Orders resizes, so the model and the PTY always end at the size of the
    /// same request. Held across the holder's answer, which readers never
    /// wait on.
    resize_order: tokio::sync::Mutex<()>,

    /// Counts the chunks of output fed to `screen`; changes once the screen
    /// already shows them.
    output_arrivals: watch::Receiver<u64>,

    /// Counts the writes to the child's input, before each reaches it;
    /// `screen` follows it (see [`yagura_term::VtScreen::follow_writes`]).
    input_writes: Arc<watch::Sender<u64>>,

    /// The window's attach and its view, whose frames the feeder reads.
    views: Views,

    /// The way to the window's holder.
    link: Link,

    /// The child's input, written through the holder.
    writer: Arc<WriterSlot>,

    /// Every write [`PtySession::write_input`] made, counted while holding
    /// `writer`, so the count's order is the order the bytes reach the child.
    write_ledger: Arc<WriteLedger>,

    /// Replies to the child's terminal queries, written by their own thread.
    replies: Arc<ReplyQueue>,

    /// OS process ID of the child, as the holder's record names it.
    child_process_id: u32,

    /// Exit status, published as soon as the holder reports it, whatever the
    /// model is doing.
    exit_code: Arc<AtomicI32>,
    is_exited: Arc<AtomicBool>,

    /// `true` once the child has exited (see [`PtySession::exit_notified`]).
    exit_notify_rx: watch::Receiver<bool>,

    /// `true` once every output the holder sent before the end is in the
    /// model.
    drained: watch::Receiver<bool>,

    /// `true` once the holder's process has ended.
    released: watch::Receiver<bool>,

    /// Outlives every window opened through it.
    _host: Arc<HolderHost>,
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
    ///
    /// # Errors
    ///
    /// As [`PtySession::spawn_with_command_in_env`].
    pub async fn spawn_with_command(
        argv: &[&str],
        config: &DaemonConfig,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        Self::spawn_with_command_in(argv, None, config, host).await
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
    ///
    /// # Errors
    ///
    /// As [`PtySession::spawn_with_command_in_env`].
    pub async fn spawn_with_command_in(
        argv: &[&str],
        cwd: Option<&Path>,
        config: &DaemonConfig,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        Self::spawn_with_command_in_env(argv, cwd, &[], config, host).await
    }

    /// Spawn a child process with the given command in a new PTY kept by a
    /// holder of `host`, optionally in a specific working directory, with
    /// additional environment variables merged into the child's environment;
    /// this is how `YAGURA_STATUS_TOKEN` reaches a spawned window's child
    /// process.
    ///
    /// The child's environment is layered, later layers winning for a name
    /// they share: the holder's own (the daemon's, plus the registry's
    /// variables on Windows), then [`DaemonConfig::child_base_env`], then
    /// the launching terminal's own variables taken away and
    /// [`crate::terminal_identity::DECLARED_TERMINAL`] set, then `extra_env`, where
    /// a name given more than once keeps its last value.
    ///
    /// # Errors
    ///
    /// [`PtyError::BundledConptyUnavailable`] when the holder cannot use the
    /// pseudoconsole bundled beside it; [`PtyError::SpawnFailed`] when the
    /// holder or the child cannot be started.
    pub async fn spawn_with_command_in_env(
        argv: &[&str],
        cwd: Option<&Path>,
        extra_env: &[(&str, &str)],
        config: &DaemonConfig,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        let window_id = host.anonymous_window_id();
        Self::spawn_window(
            WindowSpawn {
                window_id: &window_id,
                argv,
                cwd,
                extra_env,
                removed_env: &[],
                palette: None,
            },
            config,
            host,
        )
        .await
    }

    /// Opens `spawn` through a holder of `host`. The model is ready, palette
    /// included, before the holder is let in, so the child's first query is
    /// answered too.
    pub(crate) async fn spawn_window(
        spawn: WindowSpawn<'_>,
        config: &DaemonConfig,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        let Some(program) = spawn.argv.first() else {
            return Err(PtyError::SpawnFailed);
        };
        if !is_plain_window_id(spawn.window_id) {
            return Err(PtyError::SpawnFailed);
        }
        let key_records_flattened = config.host_os.flattens_key_records(program);
        let launch = holder_link::Launch {
            window_id: spawn.window_id.to_owned(),
            argv: spawn.argv.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: spawn.cwd.map(Path::to_path_buf),
            env: env::window_env(&config.child_base_env, spawn.extra_env, spawn.removed_env),
            cols: Self::INITIAL_COLS,
            rows: Self::INITIAL_ROWS,
        };
        let context = host.link_context().await?;

        let (input_writes, writes_seen) = watch::channel(0u64);
        let mut screen = LocalScreen::with_history(
            Self::INITIAL_COLS,
            Self::INITIAL_ROWS,
            config.read_output_max_lines,
        );
        screen
            .terminal_mut()
            .follow_writes(move || *writes_seen.borrow());
        if let Some(palette) = spawn.palette {
            screen.terminal_mut().set_palette(palette);
        }
        let input = Arc::new(PublishedInput::of(screen.carried()));
        let screen = screen.into_shared();
        let (arrivals_tx, output_arrivals) = watch::channel(0u64);

        let exit_code = Arc::new(AtomicI32::new(0));
        let is_exited = Arc::new(AtomicBool::new(false));
        let (exit_notify_tx, exit_notify_rx) = watch::channel(false);
        let (feed_tx, feed_rx) = std::sync::mpsc::channel();
        let views = Views {
            slot: Arc::default(),
            feed: feed_tx.clone(),
        };
        let holder_link::Started {
            link,
            opened,
            released,
        } = holder_link::start(
            context,
            launch,
            holder_link::Sink {
                feed: feed_tx,
                exit: ExitReport {
                    code: Arc::clone(&exit_code),
                    exited: Arc::clone(&is_exited),
                    notify: exit_notify_tx,
                },
            },
        )?;
        let holder_link::Opened {
            child_pid: child_process_id,
            ack_step,
        } = opened.await.map_err(|_| PtyError::SpawnFailed)??;

        let raw_capture = RawCapture::default();
        let writer = Arc::new(WriterSlot::new(link.writer()));
        let replies = Arc::new(ReplyQueue::default());
        let (drained_tx, drained) = watch::channel(false);
        let feed = ModelFeed {
            screen: screen.clone(),
            input: Arc::clone(&input),
            replies: Arc::clone(&replies),
            raw_capture: raw_capture.clone(),
            arrivals: arrivals_tx,
            views: Arc::clone(&views.slot),
            exited: Arc::clone(&is_exited),
        };
        let started = spawn_thread("window-replies", {
            let replies = Arc::clone(&replies);
            let writer = Arc::clone(&writer);
            let input = Arc::clone(&input);
            let outputs = output_arrivals.clone();
            move || replies.write_all_to(&writer, &input, &outputs)
        })
        .and_then(|()| {
            spawn_thread("window-feed", {
                let link = link.clone();
                move || feed_model(&feed, &feed_rx, &|seq| link.ack(seq), ack_step, &drained_tx)
            })
        });
        if started.is_err() {
            replies.close();
            link.terminate(true);
            return Err(PtyError::SpawnFailed);
        }

        Ok(Self {
            #[cfg(feature = "fault-injection")]
            raw_capture,
            resize_fault: ResizeFault::default(),
            screen,
            model_access: Arc::new(Semaphore::new(MODEL_ACCESS_PER_WINDOW)),
            input,
            key_records_flattened,
            resize_order: tokio::sync::Mutex::new(()),
            output_arrivals,
            input_writes: Arc::new(input_writes),
            views,
            link,
            writer,
            write_ledger: Arc::new(WriteLedger::default()),
            replies,
            child_process_id,
            exit_code,
            is_exited,
            exit_notify_rx,
            drained,
            released,
            _host: Arc::clone(host),
        })
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
        access: impl FnOnce(&SharedLocalScreen) -> R + Send + 'static,
    ) -> Option<R> {
        // Once the exit is known, a read sees everything the child wrote
        // before it.
        if self.is_exited.load(Ordering::SeqCst) {
            let mut drained = self.drained.clone();
            let _ = drained.wait_for(|drained| *drained).await;
        }
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
        read: impl FnOnce(&mut LocalScreen) -> R + Send + 'static,
    ) -> Option<R> {
        self.with_screen(move |screen| read(&mut screen.lock()))
            .await
    }

    /// The window's text as a terminal shows it: see [`yagura_term::VtScreen::text_tail`].
    ///
    /// Returns `None` when the blocking read could not run to completion
    /// (the runtime is shutting down).
    pub async fn text_tail(&self, max_lines: usize, max_bytes: usize) -> Option<(String, bool)> {
        self.with_model(move |model| model.terminal_mut().text_tail(max_lines, max_bytes))
            .await
    }

    /// The window's terminal model, for its
    /// [`DetectionEngine`](crate::detector::DetectionEngine) alone, which
    /// [`DetectorHandle`](crate::detector::DetectorHandle) drives on blocking
    /// threads.
    pub(crate) fn model_for_detection(&self) -> SharedLocalScreen {
        self.screen.clone()
    }

    /// A receiver whose value changes each time output has been fed to the
    /// model. Its first `borrow_and_update` tells whether any output arrived
    /// before the call.
    pub fn output_arrivals(&self) -> watch::Receiver<u64> {
        self.output_arrivals.clone()
    }

    /// Counts one write to the child's input, before it is made; the write
    /// may end a synchronized update that holds the view.
    fn count_input_write(input_writes: &watch::Sender<u64>, views: &Views) {
        input_writes.send_modify(|count| *count = count.wrapping_add(1));
        views.wake_if(views.slot.note_write());
    }

    /// Takes the window's attach for a connection, superseding the one
    /// before, whose receiver then resolves.
    pub(crate) fn attach_view(&self) -> (AttachGeneration, oneshot::Receiver<()>) {
        self.views.slot.attach()
    }

    /// Lets go of the attach if `generation` still holds it.
    pub(crate) fn detach_view(&self, generation: AttachGeneration) {
        self.views.slot.detach(generation);
    }

    /// Resolves once the feeder has taken in everything sent to it before
    /// this call: the output received, and the notes and writes told to the
    /// view, with the frames for them handed to the view's connection. Ends
    /// unanswered (an error) at once when the feeder has ended, since no
    /// frame comes from it after that.
    pub(crate) fn settled(&self) -> oneshot::Receiver<()> {
        let (settle, settled) = oneshot::channel();
        let _ = self.views.feed.send(Feed::Settle(settle));
        settled
    }

    /// Whether some connection holds the window's attach.
    pub(crate) fn is_view_attached(&self) -> bool {
        self.views.slot.is_attached()
    }

    /// Tells the view of attach `generation` what changed; a note from an
    /// attach that was superseded is dropped.
    pub(crate) fn note_view(&self, generation: AttachGeneration, note: ViewNote) {
        self.views.wake_if(self.views.slot.note(generation, note));
    }

    /// Gives attach `generation` a new view and sends `sink` every row of it
    /// (whatever was unacknowledged or held before). The view is placed
    /// under the same hold of the model's lock the rows are read under, so
    /// the feeder's next frame for it waits for this one's acknowledgement.
    pub(crate) async fn show_view(&self, generation: AttachGeneration, sink: Arc<dyn FrameSink>) {
        let placed = {
            let slot = Arc::clone(&self.views.slot);
            let sink = Arc::clone(&sink);
            let exited = Arc::clone(&self.is_exited);
            self.with_model(move |screen| {
                // Read under the model's lock: an exit not yet seen here
                // reaches the feeder's end after this view is placed.
                let child_exited = exited.load(Ordering::SeqCst);
                let mut view = View::new();
                let capture = view.capture(screen.terminal_mut(), child_exited, true);
                (capture, slot.place(generation, view, sink))
            })
            .await
        };
        let Some((capture, wake)) = placed else {
            return;
        };
        self.views.wake_if(wake);
        if let Some(capture) = capture {
            let slot = Arc::clone(&self.views.slot);
            let _ = tokio::task::spawn_blocking(move || {
                slot.deliver(generation, sink.as_ref(), capture);
            })
            .await;
        }
    }

    /// Runs `read` against the copy of the window's screen as it stands, the
    /// way the detector reads it (see [`SharedCopy::read`]). A test seam (the
    /// `fault-injection` feature only).
    ///
    /// Returns `None` when `read` panicked or could not run to completion
    /// (the runtime is shutting down).
    ///
    /// [`SharedCopy::read`]: crate::detector::SharedCopy::read
    #[cfg(feature = "fault-injection")]
    pub async fn read_screen<R: Send + 'static>(
        &self,
        read: impl FnOnce(&ScreenCopy) -> R + Send + 'static,
    ) -> Option<R> {
        self.with_screen(move |screen| screen.copy().read(read).value)
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

    /// Sends the child a report it turned on that carries no key or button,
    /// such as a pointer move, the way replies to its terminal queries go:
    /// ordered with every other write but not counted in the write ledger,
    /// since a report never reaches the composer. Dropped once the child is
    /// gone. Focus is not sent this way; see [`PtySession::set_view_focus`].
    pub fn send_report(&self, report: Vec<u8>) {
        self.replies.push(report, 0);
    }

    /// Records the focus the window's view reported. The child is told it
    /// when it asked for focus reports and the daemon is not typing into the
    /// window (see [`PtySession::open_focus_interval`]); only a change is
    /// ever sent, and never counted in the write ledger.
    pub fn set_view_focus(&self, focused: bool) {
        self.replies.set_view_focus(focused);
    }

    /// Opens a stretch in which the daemon decides on, or makes, writes to the
    /// child, such as one delivery from its start to its end. While any is
    /// open, and while a write is being made, the child is told it has the
    /// focus, whatever the view reported: a daemon that types into a window
    /// the user is not looking at would otherwise type into a child that
    /// believes it has no focus, and some of those draw nothing until they
    /// have it. Once the last one ends, the view's focus is told again, after
    /// the child's next output. The stretch ends when the guard is dropped.
    pub fn open_focus_interval(&self) -> FocusInterval {
        self.replies.open_focus_interval();
        FocusInterval {
            replies: Arc::clone(&self.replies),
            outputs: self.output_arrivals.clone(),
        }
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

        Self::count_input_write(&self.input_writes, &self.views);
        let data = data.to_vec();
        let writer = self.writer.clone();
        let ledger = Arc::clone(&self.write_ledger);
        let input = Arc::clone(&self.input);
        let replies = Arc::clone(&self.replies);
        let outputs = self.output_arrivals.clone();
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock();
            let registered = writer.take_registration();
            let mut lead = replies.focus_before_input(input.load().modes, &outputs);
            if let (TakeBackPrefix::Apply, Some(registered)) = (prefix, registered) {
                lead.extend(registered.keys(input.load().cols));
            }
            match prefix {
                TakeBackPrefix::Apply => ledger.count(),
                TakeBackPrefix::Withhold => ledger.count_withheld(),
            }
            let written = match writer_guard.as_mut() {
                Some(w) => w.write_all(&lead).and_then(|()| w.write_all(&data)),
                None => Ok(()),
            };
            drop(writer_guard);
            writer.release_if_requested();
            replies.focus_after_input(&outputs);
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
        let views = self.views.clone();
        let input = Arc::clone(&self.input);
        let replies = Arc::clone(&self.replies);
        let outputs = self.output_arrivals.clone();
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock();
            let mut entered = false;
            let outcome = if *ledger.count.borrow() == expected_seq {
                before_write();
                Self::count_input_write(&input_writes, &views);
                ledger.count();
                entered = true;
                let lead = replies.focus_before_input(input.load().modes, &outputs);
                let written = match writer_guard.as_mut() {
                    Some(w) => w.write_all(&lead).and_then(|()| w.write_all(&data)),
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
            if entered {
                replies.focus_after_input(&outputs);
            }
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
        if !yagura_protocol::window_size_within_limit(cols, rows) {
            return Err(PtyError::SizeOutOfRange);
        }
        if self.is_exited.load(Ordering::SeqCst) {
            return Err(PtyError::ChildExited);
        }

        let _order = self.resize_order.lock().await;
        let input = Arc::clone(&self.input);
        let previous = self
            .with_model(move |model| {
                let previous = model.terminal().size();
                model.resize(cols, rows);
                input.publish(model.carried());
                previous
            })
            .await
            .ok_or(PtyError::ResizeFailed)?;

        // Outside the model's permit: readers of the model never wait on the
        // holder's answer, and output keeps flowing into the model meanwhile.
        let refused = self.resize_fault.take() || !self.link.resize(cols, rows).await;
        if refused {
            let input = Arc::clone(&self.input);
            self.with_model(move |model| {
                model.resize(previous.0, previous.1);
                input.publish(model.carried());
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
        let mut state = self.input.load();
        state.modes.key_records_flattened = self.key_records_flattened;
        state
    }

    /// The window's size as `(cols, rows)`, as its terminal model has it.
    ///
    /// Returns `None` when the blocking read could not run to completion
    /// (the runtime is shutting down).
    pub async fn size(&self) -> Option<(u16, u16)> {
        self.with_model(|model| model.terminal().size()).await
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
    /// Asks the holder to end the child and releases the writer. Takes `&self`
    /// so it can run from `Drop` and from synchronous close handlers; nothing
    /// here waits.
    ///
    /// The holder ends the child's whole tree once the child has gone and
    /// reports the exit after the child's last output, which is when
    /// [`PtySession::exit_notified`] turns `true`;
    /// [`PtySession::terminate_tree`] ends the tree without waiting for the
    /// child. A writer stuck inside a write is not waited for: its write fails
    /// once the child or the holder is gone.
    pub fn terminate(&self) {
        if !self.is_exited.load(Ordering::SeqCst) {
            self.link.terminate(false);
        }
        self.replies.close();
        self.writer.request_release();
    }

    /// Ends the child's whole process tree now: the child and everything it
    /// started. Idempotent. The holder does the same once the child has
    /// exited, so this is only needed to end the tree while the child is
    /// still running. Nothing can fail here: the request goes to the holder,
    /// and a holder that is already gone has ended the tree with the child.
    pub fn terminate_tree(&self) {
        self.link.terminate(true);
    }

    /// Waits, at most `limit`, until the window's holder process has ended;
    /// returns whether it got there.
    pub async fn wait_until_released(&self, limit: Duration) -> bool {
        let mut released = self.released.clone();
        matches!(
            tokio::time::timeout(limit, released.wait_for(|released| *released)).await,
            Ok(Ok(_))
        )
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.terminate();
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

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map(drop)
        .inspect_err(|error| tracing::error!(%error, "cannot start a window's thread"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
