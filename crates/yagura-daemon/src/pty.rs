//! A window's terminal as the daemon reaches it: the copy of its screen and
//! the order of writes, with the PTY, the child and the terminal model kept
//! by the window's holder (`yagura-holder`, reached through [`holder_link`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{oneshot, watch};
use yagura_protocol::holder::{HolderRecord, HolderViewOp, is_plain_window_id};
use yagura_protocol::{InputModes, Palette, ScreenCopy, ScreenPush};

use crate::child_env::ChildBaseEnv;
use crate::config::DaemonConfig;
use crate::detector::SharedCopy;
use crate::holder_host::HolderHost;

mod env;
mod holder_link;
#[cfg(test)]
pub(crate) use holder_link::tests::{bind_holder_endpoint, played::Holder as PlayedHolder};
mod views;

use holder_link::{ExitReport, Link, Writer};
pub use views::AttachGeneration;
use views::ViewSlot;
pub(crate) use views::{FrameSend, FrameSink, SendKey, ViewNote};

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

/// How a window's child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    /// The code the child exited with; `None` when it could not be determined
    /// (the OS gave none, or the connection to the holder was lost).
    pub code: Option<i32>,
}

/// Marks a window's row lost once its holder is found gone with the window
/// still open; run at most once, from the window's link.
pub(crate) type LostNotice = Box<dyn FnOnce() + Send>;

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

/// The write end of a window's child input, which teardown can hand back
/// without waiting for a writer that is stuck inside a write.
struct WriterSlot {
    writer: Mutex<Option<Writer>>,
    release_requested: AtomicBool,
}

impl WriterSlot {
    fn new(writer: Writer) -> Self {
        Self {
            writer: Mutex::new(Some(writer)),
            release_requested: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Writer>> {
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
/// the size of its screen, as the copy of its screen last showed them.
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

/// The [`InputState`] of the copy last applied, published by the window's
/// link and read without a lock.
struct PublishedInput(AtomicU64);

impl PublishedInput {
    fn new(cols: u16, rows: u16) -> Self {
        Self(AtomicU64::new(Self::bits(
            InputModes::default(),
            cols,
            rows,
        )))
    }

    fn bits(modes: InputModes, cols: u16, rows: u16) -> u64 {
        u64::from(modes.to_bits()) | u64::from(cols) << 16 | u64::from(rows) << 32
    }

    /// Publishes the modes and size of the copy `copy` closed.
    fn publish(&self, copy: &ScreenPush) {
        self.0.store(
            Self::bits(copy.input_modes, copy.cols, copy.rows),
            Ordering::Release,
        );
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
    pub(crate) palette: Option<Palette>,
    /// The launch record the holder keeps in memory only and returns in its
    /// `Welcome`.
    pub(crate) record: String,
    pub(crate) lost: LostNotice,
    /// The scope names the window's row was written with; `None` asks the
    /// holder for none.
    pub(crate) scopes: Option<crate::window_table::RowScopes>,
}

/// A window a previous daemon of the instance left running, to be taken back
/// by [`PtySession::take_back`].
pub(crate) struct TakeBackSpawn<'a> {
    pub(crate) window_id: &'a str,
    /// The command the window was opened with, as its row keeps it.
    pub(crate) command: &'a [String],
    /// The holder's record.
    pub(crate) record: HolderRecord,
    /// The holder's process, opened by its id and start; `None` when the OS
    /// would not tell whether it runs.
    pub(crate) holder: Option<yagura_os::ProcessHandle>,
    /// Turns `true` once the daemon accepts connections.
    pub(crate) begin: watch::Receiver<bool>,
    /// Given the launch record the holder's `Welcome` returns; dropped
    /// uncalled as soon as the take-back fails or the window is closed first.
    pub(crate) on_welcome: Box<dyn FnOnce(&str) + Send>,
    pub(crate) lost: LostNotice,
}

/// What a session shares with its link, made before the link starts.
struct Shared {
    copy: SharedCopy,
    input: Arc<PublishedInput>,
    output_arrivals: watch::Receiver<u64>,
    views: Arc<ViewSlot>,
    exit: Arc<OnceLock<ChildExit>>,
    exit_notify_rx: watch::Receiver<bool>,
}

impl Shared {
    fn new(cols: u16, rows: u16) -> (Self, holder_link::Sink) {
        let copy = SharedCopy::default();
        let input = Arc::new(PublishedInput::new(cols, rows));
        let (arrivals, output_arrivals) = watch::channel(0u64);
        let views = Arc::new(ViewSlot::default());
        let exit = Arc::new(OnceLock::new());
        let (exit_notify_tx, exit_notify_rx) = watch::channel(false);
        let sink = holder_link::Sink {
            copy: copy.clone(),
            input: Arc::clone(&input),
            arrivals,
            views: Arc::clone(&views),
            exit: ExitReport {
                exit: Arc::clone(&exit),
                notify: exit_notify_tx,
            },
        };
        let shared = Self {
            copy,
            input,
            output_arrivals,
            views,
            exit,
            exit_notify_rx,
        };
        (shared, sink)
    }
}

/// A window's terminal: the copy of its screen and the order of writes in the
/// daemon; its PTY, child and terminal model in the window's holder process.
pub struct PtySession {
    /// Test seam; see [`ResizeFault`].
    resize_fault: ResizeFault,

    /// The copy of the screen the holder last sent, at the PTY's size.
    copy: SharedCopy,

    /// `copy`'s input state, readable without a lock.
    input: Arc<PublishedInput>,

    /// Whether the window's program flattens console key records, decided
    /// from its `argv[0]` when it opens (see
    /// [`InputModes::key_records_flattened`]). The child's output never
    /// changes it, so it lives here rather than in `input`'s bits.
    key_records_flattened: bool,

    /// The holder's count of the chunks of output its model took in, as of
    /// the copy last applied; changes once the copy shows them.
    output_arrivals: watch::Receiver<u64>,

    /// The window's attach.
    views: Arc<ViewSlot>,

    /// The way to the window's holder.
    link: Link,

    /// The child's input, written through the holder.
    writer: Arc<WriterSlot>,

    /// How many writes [`PtySession::write_input`] made, counted while holding
    /// `writer`.
    writes: Arc<AtomicU64>,

    /// Exit status, published once the holder reports it, after the copy
    /// holding the child's last output.
    exit: Arc<OnceLock<ChildExit>>,

    /// `true` once the child has exited (see [`PtySession::exit_notified`]).
    exit_notify_rx: watch::Receiver<bool>,

    /// `true` once the link has let go of the holder: its process ended, or
    /// a closed window's holder could not be named, leaving it running.
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
                record: String::new(),
                lost: Box::new(|| {}),
                scopes: None,
            },
            config,
            host,
        )
        .await
    }

    /// Opens `spawn` through a holder of `host`. The holder's model has the
    /// palette from its start, so the child's first query is answered too.
    /// Returns once the holder let the daemon in and its first copy of the
    /// screen was taken in.
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
            history_rows: config.read_output_max_lines,
            palette: spawn.palette,
            fault: config.launch_fault(),
            record: spawn.record,
            scopes: spawn.scopes,
        };
        #[cfg(test)]
        if let Some(endpoint) = host.take_played() {
            // A test plays this window's holder in its own process. Skipping
            // `link_context` and `spawn.lost`, it measures neither the holder's
            // start nor its loss.
            let (shared, sink) = Shared::new(Self::INITIAL_COLS, Self::INITIAL_ROWS);
            let started = holder_link::start_over(
                endpoint,
                std::process::id(),
                (Self::INITIAL_COLS, Self::INITIAL_ROWS),
                sink,
            )?;
            return Self::open(started, shared, key_records_flattened, host).await;
        }
        let context = host.link_context().await?;
        let (shared, sink) = Shared::new(Self::INITIAL_COLS, Self::INITIAL_ROWS);
        let started = holder_link::start(context, launch, sink, spawn.lost)?;
        Self::open(started, shared, key_records_flattened, host).await
    }

    /// The window `spawn` names, which a previous daemon of the instance left
    /// running: returned at once, not connected, with no screen and its
    /// status to be judged. Once [`TakeBackSpawn::begin`] turns `true` its
    /// link lets itself into the holder; until then, and until the holder
    /// lets it in, writes and resizes wait. A take-back that fails ends
    /// nothing: the window stays unconnected, with its holder running until
    /// the window is closed.
    ///
    /// # Errors
    ///
    /// [`PtyError::SpawnFailed`] when `host` keeps no holders that outlive a
    /// daemon, the window id is not plain, or the link cannot start.
    pub(crate) fn take_back(
        spawn: TakeBackSpawn<'_>,
        config: &DaemonConfig,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        if !is_plain_window_id(spawn.window_id) {
            return Err(PtyError::SpawnFailed);
        }
        let Some(context) = host.take_back_context() else {
            return Err(PtyError::SpawnFailed);
        };
        let key_records_flattened = spawn
            .command
            .first()
            .is_some_and(|program| config.host_os.flattens_key_records(program));
        let (shared, sink) = Shared::new(Self::INITIAL_COLS, Self::INITIAL_ROWS);
        let take = holder_link::TakeBack {
            window_id: spawn.window_id.to_owned(),
            endpoint: spawn.record.endpoint,
            holder_pid: spawn.record.holder_pid,
            holder: spawn.holder,
            cols: Self::INITIAL_COLS,
            rows: Self::INITIAL_ROWS,
            begin: spawn.begin,
            on_welcome: spawn.on_welcome,
            lost: spawn.lost,
        };
        let (link, released) = holder_link::start_take_back(context, take, sink)?;
        Ok(Self::from_parts(
            link,
            released,
            shared,
            key_records_flattened,
            host,
        ))
    }

    /// A window over a holder already serving `endpoint` from process
    /// `holder_pid`, without starting one: the daemon's side of the link
    /// from `Hello` on.
    #[cfg(test)]
    pub(crate) async fn over_holder_endpoint(endpoint: &str, holder_pid: u32) -> PtyResult<Self> {
        let host = HolderHost::from_config(&DaemonConfig::default());
        let (shared, sink) = Shared::new(Self::INITIAL_COLS, Self::INITIAL_ROWS);
        let started = holder_link::start_over(
            endpoint.to_owned(),
            holder_pid,
            (Self::INITIAL_COLS, Self::INITIAL_ROWS),
            sink,
        )?;
        Self::open(started, shared, false, &host).await
    }

    async fn open(
        started: holder_link::Started,
        shared: Shared,
        key_records_flattened: bool,
        host: &Arc<HolderHost>,
    ) -> PtyResult<Self> {
        let holder_link::Started {
            link,
            opened,
            released,
        } = started;
        let holder_link::Opened = opened.await.map_err(|_| PtyError::SpawnFailed)??;
        Ok(Self::from_parts(
            link,
            released,
            shared,
            key_records_flattened,
            host,
        ))
    }

    fn from_parts(
        link: Link,
        released: watch::Receiver<bool>,
        shared: Shared,
        key_records_flattened: bool,
        host: &Arc<HolderHost>,
    ) -> Self {
        let Shared {
            copy,
            input,
            output_arrivals,
            views,
            exit,
            exit_notify_rx,
        } = shared;
        Self {
            resize_fault: ResizeFault::default(),
            copy,
            input,
            key_records_flattened,
            output_arrivals,
            views,
            writer: Arc::new(WriterSlot::new(link.writer())),
            link,
            writes: Arc::new(AtomicU64::new(0)),
            exit,
            exit_notify_rx,
            released,
            _host: Arc::clone(host),
        }
    }

    /// The window's text as a terminal shows it, its last `max_lines`
    /// logical lines cut to `max_bytes`, as the holder answers it; the flag
    /// reports that text was left out. Also read after the child exited.
    ///
    /// Returns `None` once the holder is gone.
    pub async fn text_tail(&self, max_lines: usize, max_bytes: usize) -> Option<(String, bool)> {
        self.link.read_tail(max_lines, max_bytes).await
    }

    /// The copy of the window's screen, for its
    /// [`DetectionEngine`](crate::detector::DetectionEngine).
    pub(crate) fn model_for_detection(&self) -> SharedCopy {
        self.copy.clone()
    }

    /// A receiver whose value changes each time a copy holding more of the
    /// child's output was applied, and each time a copy was refused. Its
    /// first `borrow_and_update` tells whether any output arrived before the
    /// call.
    pub fn output_arrivals(&self) -> watch::Receiver<u64> {
        self.output_arrivals.clone()
    }

    /// Takes the window's attach for a connection, superseding the one
    /// before, whose receiver then resolves.
    pub(crate) fn attach_view(&self) -> (AttachGeneration, oneshot::Receiver<()>) {
        self.views.attach()
    }

    /// Lets go of the attach if `generation` still holds it.
    pub(crate) fn detach_view(&self, generation: AttachGeneration) {
        if self.views.detach(generation) {
            self.link.detach(generation.number());
        }
    }

    /// Resolves once the holder has taken in the output it read before this
    /// call and every frame it sent for it has been handed to the view's
    /// connection. Ends unanswered (an error) once the holder is gone, since
    /// no frame comes from it after that.
    pub(crate) fn settled(&self) -> oneshot::Receiver<()> {
        self.link.settle()
    }

    /// Whether some connection holds the window's attach.
    pub(crate) fn is_view_attached(&self) -> bool {
        self.views.is_attached()
    }

    /// Tells the view of attach `generation` what changed; a note from an
    /// attach that was superseded is dropped.
    pub(crate) fn note_view(&self, generation: AttachGeneration, note: ViewNote) {
        if self.views.holds(generation) {
            self.link.view_note(generation.number(), note.into());
        }
    }

    /// Shows attach `generation` a view of the window: the holder sends its
    /// frames, the first with every row, and they go to `sink`. The holder
    /// takes the attach after every resize asked for before it, so the first
    /// frame has the size the window was last asked for.
    pub(crate) fn show_view(&self, generation: AttachGeneration, sink: Arc<dyn FrameSink>) {
        if self.views.place(generation, sink) {
            self.link.attach(generation.number());
        }
    }

    /// Applies `op` to the window's view, without waiting for it.
    pub(crate) fn view_op(&self, op: HolderViewOp) {
        self.link.view_op(op);
    }

    /// The view's selected text; `None` when nothing is selected or the
    /// holder is gone.
    pub(crate) async fn selected_text(&self) -> Option<String> {
        self.link.selected_text().await
    }

    /// The colors the child's color queries are answered from from now on.
    pub(crate) fn set_palette(&self, palette: Palette) {
        self.link.set_palette(palette);
    }

    /// Runs `read` against the copy of the window's screen as it stands, the
    /// way the detector reads it (see [`SharedCopy::read`]). Once the exit is
    /// known, the copy holds everything the child wrote before it.
    ///
    /// Returns `None` before the first copy, or when `read` panicked.
    pub async fn read_screen<R: Send + 'static>(
        &self,
        read: impl FnOnce(&ScreenCopy) -> R + Send + 'static,
    ) -> Option<R> {
        self.copy.read(read).value
    }

    /// Makes the next [`PtySession::resize`] fail as if the PTY refused the
    /// size. A test seam (the `fault-injection` feature only).
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_resize(&self) {
        self.resize_fault.armed.store(true, Ordering::SeqCst);
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
    /// such as a pointer move, ordered with every other write. The holder
    /// drops it once the child is gone. Focus is not sent this way; see
    /// [`PtySession::set_view_focus`].
    pub fn send_report(&self, report: Vec<u8>) {
        self.link.report(report);
    }

    /// Records the focus the window's view reported. The holder tells the
    /// child when it asked for focus reports; only a change is ever sent.
    pub fn set_view_focus(&self, focused: bool) {
        self.link.set_view_focus(focused);
    }

    /// Writes `data` to the child's input, after every earlier write, and
    /// counts it in [`PtySession::writes_recorded`]; a write that then fails
    /// still counts. A write to a window being taken back waits for its
    /// holder.
    ///
    /// # Errors
    ///
    /// [`PtyError::ChildExited`] once the child is gone (nothing is counted);
    /// [`PtyError::WriteFailed`] when the write itself fails.
    pub async fn write_input(&self, data: &[u8]) -> PtyResult<()> {
        if self.exit.get().is_some() {
            return Err(PtyError::ChildExited);
        }

        let data = data.to_vec();
        let writer = self.writer.clone();
        let writes = Arc::clone(&self.writes);
        tokio::task::spawn_blocking(move || {
            let mut writer_guard = writer.lock();
            writes.fetch_add(1, Ordering::SeqCst);
            let written = match writer_guard.as_mut() {
                Some(w) => w.write(&data),
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

    /// How many writes [`PtySession::write_input`] has counted: a report
    /// ([`PtySession::send_report`]) and a focus change are not among them.
    #[must_use]
    pub fn writes_recorded(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    /// Resize the window to `cols` x `rows`: the holder resizes its terminal
    /// model first, then the PTY, the order a terminal view follows, so the
    /// child's redraw for the new size lands on a model that already has it.
    /// Returns once the holder answered; a copy of the new size has been
    /// applied by then. One size is asked for at a time.
    ///
    /// # Errors
    ///
    /// [`PtyError::SizeOutOfRange`] when either side is outside the limits
    /// in [`yagura_protocol`] (`MIN_WINDOW_COLS`, `MIN_WINDOW_ROWS`,
    /// `MAX_WINDOW_DIMENSION`), with nothing changed;
    /// [`PtyError::ChildExited`] once the child is gone; and
    /// [`PtyError::ResizeFailed`] when the PTY refuses the size, in which case
    /// the holder keeps the previous size.
    pub async fn resize(&self, cols: u16, rows: u16) -> PtyResult<()> {
        if !yagura_protocol::window_size_within_limit(cols, rows) {
            return Err(PtyError::SizeOutOfRange);
        }
        if self.exit.get().is_some() {
            return Err(PtyError::ChildExited);
        }
        if self.resize_fault.take() || !self.link.resize(cols, rows).await {
            return Err(PtyError::ResizeFailed);
        }
        Ok(())
    }

    /// The modes and size input to this window is encoded against, as the
    /// copy of its screen last showed them. Never waits.
    #[must_use]
    pub fn input_state(&self) -> InputState {
        let mut state = self.input.load();
        state.modes.key_records_flattened = self.key_records_flattened;
        state
    }

    /// The window's size as `(cols, rows)`, as its holder last confirmed it.
    ///
    /// Always `Some`; the option is kept for callers that read a size that
    /// may be missing.
    pub async fn size(&self) -> Option<(u16, u16)> {
        Some(self.link.confirmed_size())
    }

    /// Get the OS process ID of the child, as the holder's `Welcome` named
    /// it; `0` before the `Welcome`, when it named none, or when the child had
    /// already exited by then.
    ///
    /// The value is not cleared when the child exits, so callers must not
    /// treat it as a liveness indicator; use [`PtySession::exit_status`] for
    /// that.
    pub fn child_process_id(&self) -> u32 {
        self.link.child_pid().unwrap_or(0)
    }

    /// The child the holder's `Welcome` named running, with the start the
    /// holder read for it: what a window's tree is judged from. `None` when
    /// the `Welcome` did not name both, or the child had exited by then; it
    /// is not cleared when the child exits later.
    pub(crate) fn child_anchor(&self) -> Option<(u32, yagura_os::ProcessStart)> {
        self.link.child_anchor()
    }

    /// Get the exit status of the child process.
    /// Returns `None` if running, `Some(exit)` if exited; the status is
    /// retained indefinitely.
    ///
    /// The code is whatever the child actually exited with, including when
    /// [`PtySession::terminate`] is what ended it -- `1` for a killed child
    /// (`TerminateProcess(handle, 1)` on Windows, a signal exit mapped to `1`
    /// on Unix). A killed `1` and a child that genuinely exited with `1` are
    /// therefore indistinguishable here; callers that need to tell them apart
    /// have to track the terminate themselves.
    pub fn exit_status(&self) -> Option<ChildExit> {
        self.exit.get().copied()
    }

    /// Records `exit` as observed without announcing it, so nothing confirms
    /// the window `Exited`: the gap between the two, held open for a test.
    #[cfg(test)]
    pub(crate) fn observe_exit_unannounced(&self, exit: ChildExit) {
        let _ = self.exit.set(exit);
    }
}

impl PtySession {
    /// Asks the holder to end the child, and then itself, and releases the
    /// writer. Takes `&self` so it can run from `Drop` and from synchronous
    /// close handlers; nothing here waits.
    ///
    /// The holder ends the child's whole tree once the child has gone and
    /// reports the exit after the child's last output, which is when
    /// [`PtySession::exit_notified`] turns `true`;
    /// [`PtySession::terminate_tree`] ends the tree without waiting for the
    /// child. A holder whose child already exited stays to answer reads of
    /// the window until it is told this. A writer stuck inside a write is not
    /// waited for: its write fails once the child or the holder is gone.
    ///
    /// A window not connected to its holder (still being taken back, its
    /// take-back failed, or its connection lost) has its holder ended by its
    /// link instead.
    pub fn terminate(&self) {
        self.link.terminate(false);
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

    /// Waits, at most `limit`, until the window's link has let go of its
    /// holder (see [`PtySession::terminate`]); returns whether it got there.
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
            .field("child_process_id", &self.child_process_id())
            .field("exit", &self.exit.get())
            .finish_non_exhaustive()
    }
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
