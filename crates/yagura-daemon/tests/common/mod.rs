#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Compiled once per test binary, and each binary exercises only a subset of these
// fixtures, so the unused half is structurally dead code in that binary.
#![allow(dead_code)]

//! Fixtures shared across `yagura-daemon` integration tests.
//!
//! Kept here rather than per-binary because Rust integration-test binaries cannot import
//! each other's items, and forking safety-relevant helpers would let the copies drift.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use yagura_daemon::config::OwnedEmptyDir;
use yagura_daemon::control::{ApprovalPolicy, ApprovalSection};
use yagura_daemon::detector::definition::{
    ComposerSection, Definition, load_definition, parse_definition,
};
use yagura_daemon::detector::engine::{DetectionEngine, DetectorHandle};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::detector::{SharedScreen, VtScreen};
use yagura_daemon::profile::ProfileOs;
use yagura_daemon::{
    DaemonConfig, IpcServer, OpenWindowRequest, PtySession, RequestedCwd, WindowRegistry,
};
use yagura_os::program::ProgramLookup;
use yagura_protocol::{
    ApprovalRequest, ApprovalSettingsInfo, ClientMessage, DaemonMessage, DetectedStatus,
    HandoffDelivery, ToolAutoApprove, WindowInfo, WindowStatus,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// The literal behind [`SUBMIT_CHILD_PROBE`], so [`CONTROL_INPUT_TABLE`] can
/// name the same probe through `concat!`.
macro_rules! composer_probe {
    () => {
        "x"
    };
}

// --- detection-engine test helpers ------------------------------------------
//
// Shared by `detector_engine.rs`, `detector_failsafe.rs`, `detector_definition.rs`
// and `detector_codex_cli.rs`, moved here so the four binaries drive the same
// engine-construction and clock-advance code rather than four independent copies.

/// Screen width used by most `detector_engine` tests; irrelevant to the assertions
/// there. Callers with a fixture of their own geometry use [`new_engine_sized`].
pub const SCREEN_COLS: u16 = 80;

/// Screen height used by most `detector_engine` tests. Kept at the default
/// `bottom_line_count` so that fed text stays inside the matched region
/// (`VtScreen::bottom_lines` ends the window at the last line holding a
/// visible character, so fed text fits when `SCREEN_ROWS` equals the
/// default `bottom_line_count`).
pub const SCREEN_ROWS: u16 = 5;

/// Parses `definition_toml` and builds a `DetectionEngine` at the fixed
/// `SCREEN_COLS` x `SCREEN_ROWS` geometry.
pub fn new_engine(definition_toml: &str, config: &DaemonConfig) -> DetectionEngine {
    new_engine_sized(definition_toml, config, SCREEN_COLS, SCREEN_ROWS)
}

/// Parses `definition_toml` and builds a `DetectionEngine` at the given `(cols, rows)`
/// geometry, for callers whose fixture carries its own recorded dimensions.
pub fn new_engine_sized(
    definition_toml: &str,
    config: &DaemonConfig,
    cols: u16,
    rows: u16,
) -> DetectionEngine {
    let definition = parse_definition(definition_toml).unwrap();
    DetectionEngine::new(Some(definition), config, screen_of(cols, rows))
}

/// A fresh model of `(cols, rows)` for an engine to read.
pub fn screen_of(cols: u16, rows: u16) -> SharedScreen {
    VtScreen::new(cols, rows).into_shared()
}

/// Drives an engine the way a window's PTY reader does: output goes into the
/// model the engine reads, and the engine is told it arrived.
pub trait ScreenFeed {
    /// Feeds `bytes` to the model, then reports the arrival.
    fn feed(&mut self, bytes: &[u8]) -> Option<DetectedStatus>;
    /// Makes the model panic on its next feed, so it is rebuilt blank, and
    /// reports that arrival: the one way a screen is lost.
    fn lose_screen(&mut self) -> Option<DetectedStatus>;
}

impl ScreenFeed for DetectionEngine {
    fn feed(&mut self, bytes: &[u8]) -> Option<DetectedStatus> {
        self.screen().lock().feed(bytes);
        self.on_output()
    }

    fn lose_screen(&mut self) -> Option<DetectedStatus> {
        self.poison_screen_after_calls(0);
        self.feed(b"")
    }
}

/// [`ScreenFeed::feed`] for an engine running behind a [`DetectorHandle`]:
/// `screen` is the model the engine was built on.
pub fn feed_handle(screen: &SharedScreen, handle: &DetectorHandle, bytes: &[u8]) {
    screen.lock().feed(bytes);
    handle.notify_output();
}

/// Bytes that clear the screen and write `text` from the home position, the way a
/// full-screen agent repaints. Makes previously matched text genuinely disappear.
pub fn repaint(text: &str) -> Vec<u8> {
    format!("\x1b[2J\x1b[H{text}").into_bytes()
}

/// Same clear-and-home repaint as [`repaint`], for a screen captured line by line (a
/// fixture's `lines()`): the lines are rejoined with `\r\n`, the way a real terminal
/// writer emits them.
pub fn repaint_lines(lines: &[&str]) -> Vec<u8> {
    repaint(&lines.join("\r\n"))
}

pub async fn advance_ms(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
}

/// Advances the paused clock and re-evaluates, mirroring what the driver task does
/// when a deadline expires.
pub async fn tick_after_ms(engine: &mut DetectionEngine, ms: u64) -> Option<DetectedStatus> {
    advance_ms(ms).await;
    engine.on_tick()
}

/// Path to this crate's `detection/` directory, where real detection definition
/// files (`claude-code.toml`, `codex-cli.toml`, ...) are loaded from.
pub fn repo_detection_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("detection")
}

/// Shared by repo_definition and repo_definition_text so the two panic messages never diverge.
fn definition_load_failure(path: &Path, err: impl std::fmt::Display) -> String {
    format!(
        "failed to load detection definition at {}: {err} (if this path looks wrong, the bug \
         is in repo_detection_dir(); otherwise the file is missing or invalid, see the error)",
        path.display()
    )
}

/// Parses the real detection definition TOML at
/// `repo_detection_dir().join(file_name)`.
///
/// # Panics
/// Panics if the file cannot be read or parsed as a detection definition. The
/// message carries both the resolved path and the underlying error, so a
/// path-resolution bug is distinguishable from a definition file that is
/// missing or invalid.
pub fn repo_definition(file_name: &str) -> Definition {
    let path = repo_detection_dir().join(file_name);
    load_definition(&path).unwrap_or_else(|err| panic!("{}", definition_load_failure(&path, err)))
}

/// Reads the real detection definition TOML source at
/// `repo_detection_dir().join(file_name)`, without parsing it.
///
/// # Panics
/// Panics if the file cannot be read. The message carries both the resolved
/// path and the underlying error, so a path-resolution bug is distinguishable
/// from a definition file that is missing or invalid.
pub fn repo_definition_text(file_name: &str) -> String {
    let path = repo_detection_dir().join(file_name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}", definition_load_failure(&path, err)))
}

/// Creates a unique temporary directory for this test, under this crate's own
/// `yagura-test-ctrl` prefix. Delegates to
/// [`yagura_test_support::paths::unique_temp_dir`], which documents the whitespace rule
/// and the panic conditions.
pub fn unique_temp_dir(tag: &str) -> PathBuf {
    yagura_test_support::paths::unique_temp_dir("yagura-test-ctrl", tag)
}

/// A second, distinct workspace for a chat room case: a fresh
/// [`unique_temp_dir`] with a bare `.git` directory marker inside it (never
/// the real `git` binary -- `resolve_key`'s walk only needs a directory
/// named `.git` to exist), so a window opened with this as its `cwd`
/// resolves to a `WorkspaceKind::Git` key distinct from the daemon
/// process's own.
pub fn second_workspace_dir(tag: &str) -> PathBuf {
    let dir = unique_temp_dir(tag);
    std::fs::create_dir(dir.join(".git")).expect("create a bare .git marker directory");
    dir
}

/// Fixture for the natural-exit teardown route: a detached grandchild appending to
/// `tick_path`, behind a direct child that exits on its own a few seconds later. See
/// [`yagura_test_support::process::grandchild_command`] for the argv shape.
pub fn grandchild_command(tick_path: &str) -> Vec<String> {
    yagura_test_support::process::grandchild_command(tick_path)
}

/// Fixture for the `close_window` teardown route: the same grandchild shape with a far
/// longer-lived direct child. See
/// [`yagura_test_support::process::surviving_grandchild_command`] for why the two
/// lifetimes stay two fixtures.
pub fn surviving_grandchild_command(tick_path: &str) -> Vec<String> {
    yagura_test_support::process::surviving_grandchild_command(tick_path)
}

/// A fixed command (not a real vendor CLI) whose direct child stays alive for roughly
/// `ping_count - 1` seconds and then exits on its own, which is enough to reproduce a
/// pseudoconsole host-process leak. See [`yagura_test_support::process::sleepy_command`].
pub fn sleepy_command(ping_count: &str) -> Vec<String> {
    yagura_test_support::process::sleepy_command(ping_count)
}

pub fn file_len(path: &Path) -> u64 {
    yagura_test_support::process::file_len(path)
}

/// Asserts the descendant is alive: the file it appends to keeps growing.
pub async fn wait_until_file_grows(path: &Path, timeout: Duration) {
    yagura_test_support::process::wait_until_file_grows(path, timeout).await;
}

/// Asserts that after teardown the file size stays put. The failure text names the
/// teardown route `"after teardown"`.
pub async fn assert_file_len_stable(path: &Path, observation: Duration) {
    yagura_test_support::process::assert_file_len_stable(path, observation, "after teardown").await;
}

/// Installs a permanent, always-enabled, discard-everything `tracing` global default
/// once per test binary process (idempotent). Called from `store_fixtures::count_warns`
/// / `capture_warns` so every WARN capture through those helpers is covered.
///
/// Why: `tracing`'s per-callsite interest cache is process-global and adopts whichever
/// thread first touches a call site's ambient dispatcher, which can permanently cache
/// "never interested" before a scoped `with_default` capture on another thread ever runs.
pub fn ensure_tracing_default_installed() {
    static INIT: std::sync::Once = std::sync::Once::new();

    struct DiscardEverything;

    impl tracing::Subscriber for DiscardEverything {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    INIT.call_once(|| {
        let _ = tracing::subscriber::set_global_default(DiscardEverything);
    });
}

/// One row of a Windows process-table snapshot.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub image: String,
}

/// Snapshots the whole Windows process table.
///
/// Read straight from the operating system rather than through `yagura-os`, so these
/// regression tests observe the processes the production teardown leaves behind instead
/// of trusting that teardown's own bookkeeping.
#[cfg(windows)]
pub fn process_table() -> Vec<ProcessRow> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
    use windows::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
    use windows::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
    use windows::Win32::System::Diagnostics::ToolHelp::Process32NextW;
    use windows::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;

    let mut rows = Vec::new();

    // SAFETY: the snapshot handle is closed on every path below. `entry` is
    // a stack-local buffer with `dwSize` set before the first enumeration
    // call, as `Process32FirstW`/`Process32NextW` require, and is only read
    // after being filled by one of them.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return rows;
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };

        let mut has_entry = Process32FirstW(snapshot, &mut entry).is_ok();
        while has_entry {
            let name_len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            rows.push(ProcessRow {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                image: String::from_utf16_lossy(&entry.szExeFile[..name_len]),
            });
            has_entry = Process32NextW(snapshot, &mut entry).is_ok();
        }

        let _ = CloseHandle(snapshot);
    }

    rows
}

/// The creation time of `pid` in 100 ns `FILETIME` units, or `None` when the
/// process cannot be opened.
#[cfg(windows)]
pub fn creation_time(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the handle is closed right after the one call that reads it, and the
    // four out-parameters are stack locals that outlive that call.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let read = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        read.ok()?;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// The rows of `table` that are children of `parent`.
///
/// A row's parent PID is the value at its birth and is never updated, so once its
/// real parent exits the PID can name an unrelated, younger process. A row born
/// before `parent` is therefore not its child.
#[cfg(windows)]
pub fn children_of(table: &[ProcessRow], parent: u32) -> Vec<ProcessRow> {
    let parent_created = creation_time(parent);
    table
        .iter()
        .filter(|row| row.parent_pid == parent && row.pid != parent)
        .filter(|row| match (parent_created, creation_time(row.pid)) {
            (Some(parent_created), Some(created)) => created >= parent_created,
            _ => true,
        })
        .cloned()
        .collect()
}

/// Image names a pseudoconsole host process can run under: the bundled
/// `OpenConsole.exe`, or the OS's own `conhost.exe`.
#[cfg(windows)]
pub const PTY_HOST_IMAGES: [&str; 2] = ["OpenConsole.exe", "conhost.exe"];

/// Lists the process IDs of every direct child of the current process that is a
/// pseudoconsole host (any of [`PTY_HOST_IMAGES`]).
///
/// `cfg(windows)`: a pseudoconsole host has no Unix analogue, so this stays Windows-only.
#[cfg(windows)]
pub fn pty_host_child_pids() -> Vec<u32> {
    children_of(&process_table(), std::process::id())
        .into_iter()
        .filter(|row| {
            PTY_HOST_IMAGES
                .iter()
                .any(|image| row.image.eq_ignore_ascii_case(image))
        })
        .map(|row| row.pid)
        .collect()
}

/// Returns the subset of `after` not present in `before`: the PIDs that newly appeared
/// between the two snapshots. Scoping an observed PID set to what appeared around a
/// test's own window open keeps it correct while other tests in the same binary spawn
/// and exit their own pseudoconsole host processes -- those PIDs cancel out of the diff.
///
/// `cfg(windows)`: only meaningful paired with [`pty_host_child_pids`].
#[cfg(windows)]
pub fn new_pids(before: &[u32], after: &[u32]) -> Vec<u32> {
    after
        .iter()
        .copied()
        .filter(|pid| !before.contains(pid))
        .collect()
}

/// Waits until a pseudoconsole host not in `before` is a direct child of this
/// process and returns the new hosts' PIDs, failing once `timeout` passes.
///
/// `cfg(windows)`: only meaningful paired with [`pty_host_child_pids`].
#[cfg(windows)]
pub async fn wait_for_new_pty_host_pids(before: &[u32], timeout: Duration) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let new_hosts = new_pids(before, &pty_host_child_pids());
        if !new_hosts.is_empty() {
            return new_hosts;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no pseudoconsole host appeared as a direct child after opening a window");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Polls until every PID in `pids` is no longer a running process, bounded by `timeout`.
///
/// "Running" is decided by `GetExitCodeProcess` reporting `STILL_ACTIVE` rather than by
/// `OpenProcess` merely succeeding: a PID can outlive the process's actual exit as a
/// stale handle elsewhere, and that must not be mistaken for the process still running.
///
/// `cfg(windows)`: probes PIDs through a Windows-only API surface.
#[cfg(windows)]
pub async fn wait_processes_gone(pids: &[u32], timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::GetExitCodeProcess;
    use windows::Win32::System::Threading::OpenProcess;
    use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

    const STILL_ACTIVE: u32 = 259;

    fn is_running(pid: u32) -> bool {
        // SAFETY: OpenProcess either returns an owned handle for an existing
        // process or an error; the handle is closed on the success path
        // immediately after GetExitCodeProcess reads it, and never used
        // afterwards.
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut exit_code = 0u32;
            let running =
                GetExitCodeProcess(handle, &mut exit_code).is_ok() && exit_code == STILL_ACTIVE;
            let _ = CloseHandle(handle);
            running
        }
    }

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let still_alive: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|&pid| is_running(pid))
            .collect();
        if still_alive.is_empty() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            let table = process_table();
            let describe = |pid: u32| {
                let row = table.iter().find(|row| row.pid == pid);
                format!(
                    "{pid} {} created {:?}",
                    row.map_or("?", |row| row.image.as_str()),
                    creation_time(pid)
                )
            };
            let rows: Vec<String> = still_alive
                .iter()
                .map(|&pid| {
                    let parent = table
                        .iter()
                        .find(|row| row.pid == pid)
                        .map(|row| row.parent_pid);
                    format!(
                        "[{}; parent {}]",
                        describe(pid),
                        parent.map_or("?".to_string(), describe)
                    )
                })
                .collect();
            panic!("processes still alive after teardown: {}", rows.join(", "));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Returns a platform-specific path that is guaranteed to be nonexistent,
/// used in playbook and detection-dir resolution tests to verify fail-closed behavior.
pub fn guaranteed_nonexistent_exe_path() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"C:\yagura-nonexistent-test-path-should-never-exist-12345\yagura-daemon.exe"
    } else {
        "/yagura-nonexistent-test-path-should-never-exist-12345/yagura-daemon"
    })
}

/// Drives a real `WindowRegistry` + `IpcServer` pair over a real IPC endpoint
/// (a named pipe on Windows, a Unix domain socket elsewhere).
pub struct Harness {
    pub registry: Arc<WindowRegistry>,
    pub endpoint: String,
    // How the daemon was configured to answer at a window's doors, which the windows
    // opened by `open_delivering` declare as their profile's own.
    delivery: yagura_daemon::inbox::DeliverySpec,
    _server: tokio::task::JoinHandle<()>,
}

impl Harness {
    pub async fn start(tag: &str, config: DaemonConfig) -> Self {
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let endpoint = unique_pipe_name(tag);
        let server = Arc::new(IpcServer::new(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
        ));
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        Self {
            registry,
            endpoint,
            delivery: config.delivery.clone(),
            _server: handle,
        }
    }

    /// Same as [`Harness::start`], but with the chat runtime wired in: a
    /// `WindowRegistry` gets its `set_window_opened_hook`
    /// before any window is opened by the caller, and the `IpcServer` serves
    /// `send_chat` / `broadcast_chat` / `read_chat` instead of `unsupported`.
    /// The startup order mirrors `main.rs`'s,
    /// minus `store::wire_into` (the session-snapshot writer): no chat test
    /// in this suite exercises window-layout persistence, and `Harness::start`
    /// does not wire it either.
    pub async fn start_with_chat(tag: &str, config: DaemonConfig) -> Self {
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let chat = {
            let from_config_config = config.clone();
            Arc::new(
                tokio::task::spawn_blocking(move || {
                    yagura_daemon::ipc::chat::ChatRuntime::from_config(&from_config_config)
                })
                .await
                .expect("ChatRuntime::from_config must not panic"),
            )
        };
        yagura_daemon::ipc::chat::install_intro_hook(&registry, chat.clone());
        let endpoint = unique_pipe_name(tag);
        let server = Arc::new(IpcServer::new_with_chat(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
            chat,
        ));
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        Self {
            registry,
            endpoint,
            delivery: config.delivery.clone(),
            _server: handle,
        }
    }

    /// [`Self::start_with_chat`]'s counterpart for the plan ledger: opens a
    /// `yagura_daemon::ipc::plan::PlanRuntime` against
    /// `config.store_db_path_resolved` (as a new boot, on a blocking thread,
    /// the way `ChatRuntime::from_config` is opened above) and wires it into
    /// a fresh `IpcServer`, the shape `main.rs` is expected to build. Returns
    /// the runtime's own lock table alongside the harness, so a case can
    /// hold and inspect a plan's guard directly -- the same `Arc` the
    /// server's own handlers acquire theirs from.
    pub async fn start_with_ledger(
        tag: &str,
        config: DaemonConfig,
    ) -> (Self, Arc<yagura_daemon::plan_permit::LockTable>) {
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let ledger = Arc::new(
            tokio::task::spawn_blocking({
                let from_config_config = config.clone();
                move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&from_config_config)
            })
            .await
            .expect("PlanRuntime::from_config must not panic"),
        );
        let locks = ledger.locks();
        let endpoint = unique_pipe_name(tag);
        let server = Arc::new(IpcServer::new_with_ledger(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
            ledger,
        ));
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        (
            Self {
                registry,
                endpoint,
                delivery: config.delivery.clone(),
                _server: handle,
            },
            locks,
        )
    }

    /// [`Self::start_with_ledger`] with the ticket board wired in as well:
    /// the plan ledger and the ticket store are opened against the same
    /// `config.store_db_path_resolved`, in the order `main.rs` opens them, and
    /// `config.tickets_path` is read on every ticket request.
    pub async fn start_with_ledger_and_tickets(
        tag: &str,
        config: DaemonConfig,
    ) -> (Self, Arc<yagura_daemon::plan_permit::LockTable>) {
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let ledger = Arc::new(
            tokio::task::spawn_blocking({
                let from_config_config = config.clone();
                move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&from_config_config)
            })
            .await
            .expect("PlanRuntime::from_config must not panic"),
        );
        let tickets = Arc::new(
            tokio::task::spawn_blocking({
                let from_config_config = config.clone();
                move || yagura_daemon::ipc::ticket::TicketRuntime::from_config(&from_config_config)
            })
            .await
            .expect("TicketRuntime::from_config must not panic"),
        );
        let locks = ledger.locks();
        let endpoint = unique_pipe_name(tag);
        let server = Arc::new(
            IpcServer::new_with_ledger(registry.clone(), endpoint.clone(), config.clone(), ledger)
                .with_tickets(tickets),
        );
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        (
            Self {
                registry,
                endpoint,
                delivery: config.delivery.clone(),
                _server: handle,
            },
            locks,
        )
    }

    /// [`Self::start_with_chat`] and [`Self::start_with_ledger`] combined, in
    /// the same order `main.rs` builds them (chat, then the ledger, both
    /// against the same `config.store_db_path_resolved`): both `send_chat` /
    /// `broadcast_chat` / `read_chat` and the plan tools are served together.
    /// Needed only by a case that must resolve `@parent` for a task-bound
    /// window -- there the parent is the plan's live owner rather than the
    /// window's opener, so exercising it over chat
    /// needs both runtimes wired into the one daemon a real boot would build.
    pub async fn start_with_chat_and_ledger(
        tag: &str,
        config: DaemonConfig,
    ) -> (Self, Arc<yagura_daemon::plan_permit::LockTable>) {
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let chat = {
            let from_config_config = config.clone();
            Arc::new(
                tokio::task::spawn_blocking(move || {
                    yagura_daemon::ipc::chat::ChatRuntime::from_config(&from_config_config)
                })
                .await
                .expect("ChatRuntime::from_config must not panic"),
            )
        };
        yagura_daemon::ipc::chat::install_intro_hook(&registry, chat.clone());
        let ledger = Arc::new(
            tokio::task::spawn_blocking({
                let from_config_config = config.clone();
                move || yagura_daemon::ipc::plan::PlanRuntime::from_config(&from_config_config)
            })
            .await
            .expect("PlanRuntime::from_config must not panic"),
        );
        let locks = ledger.locks();
        let endpoint = unique_pipe_name(tag);
        let server = Arc::new(
            IpcServer::new_with_chat(registry.clone(), endpoint.clone(), config.clone(), chat)
                .with_ledger(ledger),
        );
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        (
            Self {
                registry,
                endpoint,
                delivery: config.delivery.clone(),
                _server: handle,
            },
            locks,
        )
    }

    pub async fn connect(&self) -> Connection {
        yagura_test_support::ipc::connect_with_retry(&self.endpoint, Duration::from_secs(5)).await
    }

    pub async fn open(&self, command: Vec<String>, detection: Option<&str>) -> String {
        self.registry
            .open(OpenWindowRequest {
                command,
                cwd: None,
                name: None,
                vendor: None,
                detection: detection.map(str::to_string),
                on_name_taken: Default::default(),
            })
            .await
            .expect("registry.open should spawn the child")
            .window_id
    }

    /// [`Self::open`], with an explicit `cwd`: unlike `open`,
    /// which leaves `OpenWindowRequest.cwd` at `None` (the daemon process's
    /// own cwd), this opens the window against `cwd` so a case can put it in
    /// a second workspace (a second git-keyed directory, most commonly) than
    /// the daemon's own.
    pub async fn open_in_cwd(
        &self,
        command: Vec<String>,
        detection: Option<&str>,
        cwd: &Path,
    ) -> String {
        self.registry
            .open(OpenWindowRequest {
                command,
                cwd: Some(RequestedCwd::from_caller(cwd.to_path_buf())),
                name: None,
                vendor: None,
                detection: detection.map(str::to_string),
                on_name_taken: Default::default(),
            })
            .await
            .expect("registry.open should spawn the child")
            .window_id
    }

    /// [`Self::open`], for a window whose profile declares delivery with no way to
    /// wake its CLI ([`wake_none_delivery`]): it takes part in chat and notices
    /// whatever its command line, and is typed into while it waits. Its doors answer
    /// as the daemon's own `delivery` configuration says.
    pub async fn open_delivering(&self, command: Vec<String>, detection: Option<&str>) -> String {
        self.open_delivering_in(command, detection, None).await
    }

    /// [`Self::open_delivering`], with an explicit `cwd` when one is given.
    pub async fn open_delivering_in(
        &self,
        command: Vec<String>,
        detection: Option<&str>,
        cwd: Option<&Path>,
    ) -> String {
        self.registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command,
                    cwd: cwd.map(|cwd| RequestedCwd::from_caller(cwd.to_path_buf())),
                    name: None,
                    vendor: None,
                    detection: detection.map(str::to_string),
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                yagura_daemon::window::WindowProvenance {
                    delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                        max_chars: self.delivery.max_chars,
                        output: self.delivery.output.clone(),
                        wake_stream: self.delivery.wake_stream,
                        wake_exit: self.delivery.wake_exit,
                        ..wake_none_delivery()
                    }),
                    ..yagura_daemon::window::WindowProvenance::default()
                },
            )
            .await
            .expect("registry.open_with_extra_env should spawn the child")
            .window_id
    }

    pub async fn write_input(&self, window_id: &str, bytes: &[u8]) {
        self.registry
            .get(window_id)
            .expect("window must exist in registry")
            .session()
            .write_input(bytes)
            .await
            .expect("write_input");
    }

    /// All the raw bytes `window_id`'s child wrote that the session keeps
    /// (its last 4 MiB), for a reader that must not lose an early marker to
    /// what followed it.
    pub fn captured_output(&self, window_id: &str) -> Vec<u8> {
        self.registry
            .get(window_id)
            .expect("window must exist in registry")
            .session()
            .captured_output()
    }

    /// The last 256 KiB of the raw bytes `window_id`'s child wrote.
    pub fn scrollback(&self, window_id: &str) -> Vec<u8> {
        let captured = self
            .registry
            .get(window_id)
            .expect("window must exist in registry")
            .session()
            .captured_output();
        captured[captured.len().saturating_sub(256 * 1024)..].to_vec()
    }

    /// `window_id`'s scrollback with CSI and OSC sequences and line breaks
    /// removed, so a line the terminal wrapped or redrew still reads as one
    /// run of text.
    pub fn flattened_scrollback(&self, window_id: &str) -> String {
        flatten_terminal_text(&self.scrollback(window_id), OscPayload::Remove)
    }

    /// Like [`Harness::flattened_scrollback`], but an OSC sequence's payload
    /// (a window title, for one) stays in the text; only its introducer goes.
    pub fn flattened_scrollback_keeping_osc_payload(&self, window_id: &str) -> String {
        flatten_terminal_text(&self.scrollback(window_id), OscPayload::Keep)
    }

    /// Times `window_id`'s `submit_child` echoed a submitted line starting
    /// with `text`; an empty `text` counts every submitted line.
    pub fn submitted_count(&self, window_id: &str, text: &str) -> usize {
        self.flattened_scrollback(window_id)
            .matches(&format!("SUBMITTED {text}"))
            .count()
    }

    /// Re-reads `window_id`'s scrollback every 50ms, handing each read and
    /// the instant it was taken to `judge`, until `judge` returns a value.
    async fn poll_scrollback<T>(
        &self,
        window_id: &str,
        mut judge: impl FnMut(&[u8], tokio::time::Instant) -> Option<T>,
    ) -> T {
        loop {
            let scrollback = self.scrollback(window_id);
            if let Some(verdict) = judge(&scrollback, tokio::time::Instant::now()) {
                return verdict;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn wait_for_scrollback(&self, window_id: &str, needle: &[u8], timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        let appeared = self
            .poll_scrollback(window_id, |scrollback, now| {
                if contains_subsequence(scrollback, needle) {
                    Some(true)
                } else {
                    (now >= deadline).then_some(false)
                }
            })
            .await;
        if !appeared {
            panic!(
                "timed out waiting for {:?} to appear in window {window_id} scrollback",
                String::from_utf8_lossy(needle)
            );
        }
    }

    /// Waits until `needle` appears at least `min_count` times in `window_id`'s
    /// scrollback. A handoff envelope's marker line is written once by the
    /// daemon but rendered twice: once as the terminal's own local echo of the
    /// input, once as the child's own echo of what it actually read from
    /// stdin. Only the second copy proves the daemon's write was delivered as
    /// a complete line.
    pub async fn wait_for_scrollback_occurrences(
        &self,
        window_id: &str,
        needle: &str,
        min_count: usize,
        timeout: Duration,
    ) {
        let deadline = tokio::time::Instant::now() + timeout;
        let (found, reached) = self
            .poll_scrollback(window_id, |scrollback, now| {
                let found = count_in_scrollback(scrollback, needle);
                let reached = found >= min_count;
                (reached || now >= deadline).then_some((found, reached))
            })
            .await;
        if !reached {
            panic!(
                "timed out waiting for {min_count} occurrences of {needle:?} in window \
                 {window_id} scrollback (found {found})"
            );
        }
    }

    /// Waits until `window_id`'s child has exited and the window's confirmed
    /// status says so: the daemon's own gates go by that status, which only
    /// follows the exit itself.
    pub async fn wait_until_child_exited(&self, window_id: &str, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let entry = self
                .registry
                .get(window_id)
                .expect("window must exist in registry");
            let exited = entry.session().exit_status().is_some()
                && entry.current_status() == WindowStatus::Exited;
            if exited {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for window {window_id} child to exit");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// File stem this file's control fixtures resolve under a detection directory: the
/// value a caller must write [`control_definition`] to (as `{CONTROL_DETECTION_NAME}.toml`)
/// for [`Harness::open_idle_window`] to be able to leave `Unknown`.
pub const CONTROL_DETECTION_NAME: &str = yagura_test_support::detection::CONTROL_DETECTION_NAME;

/// The `[input]` table of the daemon's control definition: a short submit delay and no
/// delivery confirmation, so a delivery test is not held up waiting for a reaction the
/// `echo_child` window never gives, and the probe a judged delivery needs.
pub const CONTROL_INPUT_TABLE: &str = concat!(
    "\n[input]\nsubmit_delay_ms = 40\ndelivery_confirm_ms = 0\nprobe = \"",
    composer_probe!(),
    "\"\n"
);

/// The detection definition (rule `CTRL-READY` -> `idle`) shared by every daemon
/// integration test that needs a window able to leave `Unknown` on demand, with an
/// explicit `bottom_line_count` when given. `rules = []` is unusable for that purpose: it
/// stays `Unknown` forever.
///
/// It ends with [`composer_section_toml`]: a window whose definition names no composer
/// region is unobservable to the delivery path.
pub fn control_definition(bottom_line_count: Option<u16>) -> String {
    let head = match bottom_line_count {
        Some(n) => format!("bottom_line_count = {n}\n"),
        None => String::new(),
    };
    yagura_test_support::detection::idle_definition_toml(
        CONTROL_DETECTION_NAME,
        &head,
        &format!("{CONTROL_INPUT_TABLE}{}", composer_section_toml()),
    )
}

/// File stem of [`leading_keys_definition`] under a detection directory.
pub const LEADING_KEYS_DETECTION_NAME: &str = "leading-keys-control";

/// [`control_definition`] whose CLI consumes a leading `!` or `?` in an empty
/// composer as a key rather than typing it.
pub fn leading_keys_definition() -> String {
    yagura_test_support::detection::idle_definition_toml(
        LEADING_KEYS_DETECTION_NAME,
        "",
        &format!(
            "{CONTROL_INPUT_TABLE}leading_keys = [\"!\", \"?\"]\n{}",
            composer_section_toml()
        ),
    )
}

/// Writes [`leading_keys_definition`] under `dir` as `{LEADING_KEYS_DETECTION_NAME}.toml`.
pub fn write_leading_keys_definition(dir: &Path) {
    std::fs::write(
        dir.join(format!("{LEADING_KEYS_DETECTION_NAME}.toml")),
        leading_keys_definition(),
    )
    .expect("write leading-keys detection definition file");
}

/// Written into a window's PTY input to trigger the `idle` rule of [`control_definition`].
pub const READY_MARKER: &[u8] = yagura_test_support::detection::CONTROL_READY_LINE.as_bytes();

/// The `[composer]` table naming the region a fixture in composer-draw mode paints.
///
/// Appended to every synthetic definition here whose windows take a delivery: a
/// definition without this table can never be observed, so the delivery path would
/// refuse to write to such a window at all.
pub fn composer_section_toml() -> String {
    yagura_test_support::detection::composer_section_toml()
}

/// Writes [`control_definition`] under `dir` as
/// `{CONTROL_DETECTION_NAME}.toml`, the file layout [`detection_config`] and
/// [`Harness::open_idle_window`] both depend on.
pub fn write_control_definition(dir: &Path) {
    write_control_definition_with_bottom_line_count(dir, None);
}

/// [`write_control_definition`], optionally writing an explicit
/// `bottom_line_count` into the definition. `None` reproduces
/// [`write_control_definition`]'s own behavior exactly (the key stays out of
/// the file, so the engine falls back to [`DaemonConfig::bottom_line_count`]).
///
/// [`control_definition`] is paired with composer-draw-mode fixtures through
/// `handoff_profile_config` / `detection_config` (e.g.
/// `ready_child_profile_source_with_composer`), so the same
/// marker-scrolled-out-of-the-window risk
/// [`write_submit_definition_with_bottom_line_count`]'s doc comment
/// describes applies here too, once a caller's composer content grows past
/// the fallback value.
pub fn write_control_definition_with_bottom_line_count(dir: &Path, bottom_line_count: Option<u16>) {
    std::fs::write(
        dir.join(format!("{CONTROL_DETECTION_NAME}.toml")),
        control_definition(bottom_line_count),
    )
    .expect("write detection definition file");
}

/// `base` with its detection directory pointed at `dir` (holding
/// [`control_definition`]) and its detection name set to [`CONTROL_DETECTION_NAME`], so
/// a window opened with `detection = Some(CONTROL_DETECTION_NAME)` -- or, once `base`
/// also carries that as its default `detection_name`, a window opened with no
/// `detection` argument at all -- can leave `Unknown`.
pub fn detection_config(dir: &Path, base: DaemonConfig) -> DaemonConfig {
    write_control_definition(dir);
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: CONTROL_DETECTION_NAME.to_string(),
        ..base
    }
}

/// What a profile declares to hand a first request over at launch, as
/// `--prompt <request>`, with no way to wake its CLI later.
pub const LAUNCH_PROMPT_TABLE: &str = "
[delivery]
wake = \"none\"
launch_prompt = [\"--prompt\", \"{prompt}\"]
";

/// A `hook_child` profile that is handed a first request at launch
/// ([`LAUNCH_PROMPT_TABLE`]), logging what it was started with under `logs` when
/// given. `extra_tables` is appended after the `[delivery]` table.
pub fn launch_prompt_profile_source(
    display_name: &str,
    logs: Option<&Path>,
    extra_tables: &str,
) -> String {
    let mut command = vec![
        PathBuf::from(env!("CARGO_BIN_EXE_hook_child"))
            .display()
            .to_string(),
        process::COMPOSER_MARKER.to_string(),
        process::COMPOSER_SEPARATOR.to_string(),
        process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
    ];
    if let Some(logs) = logs {
        command.push("--log".to_string());
        command.push(logs.display().to_string());
    }
    let command = command
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1
display_name = \"{display_name}\"

         [command]
default = [{command}]
{LAUNCH_PROMPT_TABLE}{extra_tables}"
    )
}

/// The `args` the `hook_child` logging under `logs` was started with, once it has
/// logged them.
pub async fn launch_args_logged_in(logs: &Path) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = std::fs::read_to_string(logs.join("events.jsonl")).unwrap_or_default();
        let argv = events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|line| line["event"] == "argv");
        if let Some(argv) = argv {
            return argv["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| arg.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hang guard: the window never logged its argv under {}",
            logs.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The `[delivery]` table of a profile with no way to wake its CLI while it waits,
/// with the daemon's own defaults otherwise.
pub fn wake_none_delivery() -> yagura_daemon::profile_delivery::ProfileDelivery {
    yagura_daemon::profile_delivery::ProfileDelivery {
        output: yagura_daemon::inbox::DoorOutputs::all(&yagura_daemon::inbox::OutputForm::Plain),
        max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
        wake: yagura_daemon::profile_delivery::WakeMode::None,
        wake_stream: yagura_protocol::DeliveryStream::Stderr,
        wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
        launch_prompt: None,
        launch_context: None,
    }
}

impl Harness {
    /// Opens an `echo_child` window with no detection definition: its status
    /// is permanently `Unknown` (fail-safe rule).
    pub async fn open_unknown_window(&self) -> String {
        let path = echo_child_path();
        let path = path.to_str().expect("echo_child path must be valid UTF-8");
        self.open_delivering(vec![path.to_string()], None).await
    }

    /// Opens an `echo_child` window bound to [`CONTROL_DETECTION_NAME`], drives it to
    /// `idle` with [`READY_MARKER`] and observes the transition before returning: never
    /// assert against a window whose transition has not been observed.
    ///
    /// The caller's `DaemonConfig` must have written [`control_definition`] as
    /// `{CONTROL_DETECTION_NAME}.toml` under its resolved detection directory (see the
    /// `detection_config` helper each test file that calls this keeps for itself).
    pub async fn open_idle_window(&self, client: &mut Connection) -> String {
        self.open_idle_window_running(client, yagura_test_support::process::echo_child_command())
            .await
    }

    /// [`Harness::open_idle_window`] with `echo_child` in composer-draw mode,
    /// reflecting each write as soon as it lands. A window that takes a
    /// delivery needs this: the delivery path observes the composer before it
    /// writes, and a fixture that paints nothing can never be observed.
    pub async fn open_idle_window_with_composer(&self, client: &mut Connection) -> String {
        self.open_idle_window_running(
            client,
            yagura_test_support::process::echo_child_command_with_composer(0),
        )
        .await
    }

    /// [`Harness::open_idle_window_with_composer`] bound to `detection`, which must
    /// share [`control_definition`]'s ready rule (e.g. [`leading_keys_definition`]).
    pub async fn open_idle_window_with_composer_detected_by(
        &self,
        client: &mut Connection,
        detection: &str,
    ) -> String {
        self.open_idle_window_detected_by(
            client,
            yagura_test_support::process::echo_child_command_with_composer(0),
            detection,
        )
        .await
    }

    async fn open_idle_window_running(
        &self,
        client: &mut Connection,
        command: Vec<String>,
    ) -> String {
        self.open_idle_window_detected_by(client, command, CONTROL_DETECTION_NAME)
            .await
    }

    async fn open_idle_window_detected_by(
        &self,
        client: &mut Connection,
        command: Vec<String>,
        detection: &str,
    ) -> String {
        let window_id = self.open_delivering(command, Some(detection)).await;
        self.write_input(&window_id, READY_MARKER).await;
        wait_until_status(
            client,
            &window_id,
            WindowStatus::Idle,
            Duration::from_secs(10),
        )
        .await;
        window_id
    }
}

/// Opens a window running the `echo_child` fixture binary under `registry`, optionally
/// selecting `detection` as its per-window detection definition. The whole `OpenedWindow`
/// is returned, so a caller needing `detection_warning` and one needing only `window_id`
/// share this single implementation.
pub async fn open_echo_window(
    registry: &WindowRegistry,
    detection: Option<&str>,
) -> yagura_daemon::window::OpenedWindow {
    registry
        .open(OpenWindowRequest {
            command: yagura_test_support::process::echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: detection.map(str::to_string),
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open should spawn echo_child (a detection warning must not fail it)")
}

/// Writes `bytes` directly to a window's PTY input, bypassing the IPC `input`
/// path. Tests use this to stimulate a PTY out-of-band so that only the
/// behaviour under test -- not the `input` IPC round trip -- is exercised.
pub async fn write_window_input(registry: &WindowRegistry, window_id: &str, bytes: &[u8]) {
    registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .write_input(bytes)
        .await
        .expect("write_input");
}

/// Path to the `echo_child` test-fixture binary next to this test binary.
pub fn echo_child_path() -> PathBuf {
    yagura_test_support::process::echo_child_path()
}

/// Path to the `hex_echo` test-fixture binary next to this test binary.
pub fn hex_echo_path() -> PathBuf {
    yagura_test_support::process::hex_echo_path()
}

/// Path to the `env_probe` test-fixture binary next to this test binary.
pub fn env_probe_path() -> PathBuf {
    yagura_test_support::process::env_probe_path()
}

/// Polls `session`'s scrollback until it contains `needle` and returns the decoded text.
/// Polling rather than a fixed sleep keeps the test deterministic on a loaded machine.
pub async fn wait_for_scrollback_containing(
    session: &PtySession,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let captured = session.captured_output();
        let text = String::from_utf8_lossy(&captured[captured.len().saturating_sub(64 * 1024)..])
            .to_string();
        if text.contains(needle) {
            return text;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {needle:?} in child output; scrollback was: {text:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What [`flatten_terminal_text`] does with an OSC sequence's payload.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OscPayload {
    Remove,
    Keep,
}

/// `raw` decoded lossily, with CSI sequences, line breaks and (per `osc`)
/// OSC payloads removed. Any other escape drops itself and the char after it.
pub fn flatten_terminal_text(raw: &[u8], osc: OscPayload) -> String {
    let raw = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') if osc == OscPayload::Remove => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' | '\n' => {}
            other => out.push(other),
        }
    }
    out
}

/// The last 600 characters of `text`, for a failure message.
pub fn text_tail(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(600);
    chars[start..].iter().collect()
}

pub fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    yagura_test_support::scan::contains_subsequence(haystack, needle)
}

pub fn unique_pipe_name(tag: &str) -> String {
    yagura_test_support::ipc::unique_endpoint_name("yagura-test", tag)
}

/// Runs the future `test` builds on a single-threaded runtime of its own,
/// failing instead of hanging when it has not finished after `guard`. Anything
/// that blocks that runtime's one thread stops the whole test with it, so a
/// case that needs the runtime to keep running cannot pass by luck.
pub fn run_on_own_runtime<F>(guard: Duration, test: impl FnOnce() -> F + Send + 'static)
where
    F: std::future::Future<Output = ()> + 'static,
{
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build the test's runtime");
        runtime.block_on(test());
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(guard) {
        Ok(()) => thread.join().expect("the test's thread"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = thread.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the test had not finished after {guard:?}: its runtime is blocked")
        }
    }
}

pub async fn send(client: &mut Connection, msg: &ClientMessage) {
    let body = yagura_protocol::encode_client_message(msg);
    let frame = yagura_protocol::frame_encode(&body);
    client
        .write_all(&frame)
        .await
        .expect("write_all frame to the endpoint");
}

/// Reads exactly one frame, whatever it is. Ordering-sensitive tests use this
/// directly; everything else goes through [`read_response`].
pub async fn read_frame(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let mut len_buf = [0u8; 4];
    match tokio::time::timeout(timeout, client.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("connection closed while reading a frame length prefix: {e}"),
        Err(_) => panic!("timed out waiting for a frame length prefix"),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    match tokio::time::timeout(timeout, client.read_exact(&mut body)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("connection closed while reading a frame body: {e}"),
        Err(_) => panic!("timed out waiting for a frame body"),
    }
    yagura_protocol::decode_daemon_message(&body)
        .expect("daemon must only ever emit well-formed frames")
}

/// Reads the next *response* frame, skipping the asynchronous notification
/// kinds (view `Frame` parts / `StatusChanged` / `ChatParticipantsChanged`)
/// that a connection may interleave at any moment.
pub async fn read_response(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match read_message(client, remaining).await {
            DaemonMessage::ChatParticipantsChanged => continue,
            other => return other,
        }
    }
}

/// [`read_response`] without skipping `ChatParticipantsChanged`, for a test
/// that waits for that push itself.
pub async fn read_message(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a response");
        match read_frame(client, remaining).await {
            DaemonMessage::Frame(_) | DaemonMessage::StatusChanged { .. } => continue,
            other => return other,
        }
    }
}

pub fn expect_error(msg: DaemonMessage, expected_code: &str) -> String {
    match msg {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(
                code, expected_code,
                "unexpected error code (message: {message})"
            );
            message
        }
        other => panic!("expected error({expected_code}), got {other:?}"),
    }
}

/// Reports `status` for `window_id` the way a structured source does. Returns
/// once the daemon has handed the report to the window's detector, so it is
/// ordered before any output the caller causes afterwards.
pub async fn report_status(client: &mut Connection, window_id: &str, status: WindowStatus) {
    send(
        client,
        &ClientMessage::StatusEvent {
            source: "test-report".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    query_status(client, window_id).await;
}

pub async fn query_status(client: &mut Connection, window_id: &str) -> WindowStatus {
    send(
        client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.to_string(),
        },
    )
    .await;
    // An approver connection is also sent room and plan changes as they settle.
    loop {
        match read_response(client, Duration::from_secs(5)).await {
            DaemonMessage::WindowStatus { status, .. } => return status,
            DaemonMessage::ChatChanged { .. } | DaemonMessage::PlanChanged { .. } => continue,
            other => panic!("expected window_status, got {other:?}"),
        }
    }
}

pub async fn wait_until_status(
    client: &mut Connection,
    window_id: &str,
    expected: WindowStatus,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = query_status(client, window_id).await;
        if status == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {window_id} to reach {expected:?} (last seen {status:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Time-based judgement configuration with a prompt-shaped rule that actually matches,
/// mirroring the `idle` rule in `detection/generic-shell.toml`: a shell prompt matched by
/// that rule must not flip to `Running`/`Unknown` just because the next output is the
/// echo of a non-submitting interactive keystroke.
pub const TIME_BOTH_PROMPT_DEF: &str = r#"
schema_version = 1
name = "test-time-both-prompt"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '[$#>]\s*$'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000

[activity]
status = "running"
window_ms = 500
"#;

// Brace-matched textual extraction of a named function's body, so a wiring check can be
// scoped to that function's own source rather than a whole-file `contains`, which stays
// vacuously true for a needle occurring anywhere else in the file. Braces inside string,
// raw-string and char literals are recognized and skipped. A deliberately smaller,
// independent implementation of the brace matching `tests/wiring_enforcement.rs` has.

/// Returns the index of the first `{` at or after byte offset `from` in
/// `source` that is not inside a double-quoted string literal (plain,
/// `"..."` with `\"` escapes, or raw, `r"..."` / `r#".."#` / ...) or a char
/// literal (`'{'`, `'}'`, `'\{'`, `'\u{7FFF}'`, ...; see
/// [`char_literal_end`] for how these are told apart from a lifetime such as
/// `'a` / `'static`). `None` if no such `{` exists.
fn first_open_brace(source: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < source.len() {
        if let Some(end) = string_literal_end(source, i) {
            i = end;
            continue;
        }
        if let Some(end) = char_literal_end(source, i) {
            i = end;
            continue;
        }
        if source[i] == b'{' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Returns the index of the `}` that matches the `{` at `source[open]`, by
/// counting nested brace depth while skipping over string/raw-string
/// contents via [`string_literal_end`] and char literal contents via
/// [`char_literal_end`] (so a brace inside e.g. a `"{status:?}"` tracing
/// format string, or inside a `'{'` / `'}'` char literal, is never
/// miscounted as structural, and can neither end the scan early nor extend
/// it past the function's own closing brace). `None` if depth never returns
/// to `0`. A char-level, open/close-parameterized twin named `matching_close`
/// lives in `wiring_enforcement.rs`; kept separate because this one walks a
/// byte slice that has no shared `literal_end` step to reuse.
fn matching_close_brace(source: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < source.len() {
        if let Some(end) = string_literal_end(source, i) {
            i = end;
            continue;
        }
        if let Some(end) = char_literal_end(source, i) {
            i = end;
            continue;
        }
        match source[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// If `source[i..]` opens a char literal (`'x'`, `'"'`, `'\n'`, `'\''`,
/// `'\u{7FFF}'`, ...), returns the index just past its closing `'`. `None`
/// for anything that is not a char literal, including a lifetime annotation
/// (`'a`, `'static`, `'_`): the distinguishing rule is that a char literal's
/// content is a single (possibly escaped) character immediately followed by
/// a closing `'`, while a lifetime is followed by an identifier with no
/// closing `'` right after it.
fn char_literal_end(source: &[u8], i: usize) -> Option<usize> {
    if source.get(i) != Some(&b'\'') {
        return None;
    }
    let mut j = i + 1;
    if source.get(j) == Some(&b'\\') {
        j += 1;
        if source.get(j) == Some(&b'u') && source.get(j + 1) == Some(&b'{') {
            j += 2;
            while source.get(j).is_some_and(|c| *c != b'}') {
                j += 1;
            }
            if source.get(j) != Some(&b'}') {
                return None;
            }
            j += 1;
        } else if source.get(j).is_some() {
            j += 1;
        } else {
            return None;
        }
    } else if source.get(j).is_some() {
        j += 1;
    } else {
        return None;
    }
    (source.get(j) == Some(&b'\'')).then_some(j + 1)
}

/// If `source[i..]` opens a string literal -- plain (`"..."`) or raw
/// (`r"..."`, `r#".."#`, ...) -- returns the index just past its closing
/// delimiter. `None` for everything else.
fn string_literal_end(source: &[u8], i: usize) -> Option<usize> {
    if source.get(i) == Some(&b'r') {
        let mut j = i + 1;
        let mut hashes = 0usize;
        while source.get(j) == Some(&b'#') {
            hashes += 1;
            j += 1;
        }
        if source.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
        loop {
            match source.get(j) {
                None => return Some(source.len()),
                Some(b'"') => {
                    let mut k = j + 1;
                    let mut matched = 0usize;
                    while matched < hashes && source.get(k) == Some(&b'#') {
                        matched += 1;
                        k += 1;
                    }
                    if matched == hashes {
                        return Some(k);
                    }
                    j += 1;
                }
                Some(_) => j += 1,
            }
        }
    }
    if source.get(i) == Some(&b'"') {
        let mut j = i + 1;
        while j < source.len() {
            if source[j] == b'\\' && j + 1 < source.len() {
                j += 2;
                continue;
            }
            if source[j] == b'"' {
                return Some(j + 1);
            }
            j += 1;
        }
        return Some(source.len());
    }
    None
}

/// Extracts the source text of the first function named `fn_name` -- keyed by the literal
/// `fn {fn_name}(`, whose trailing `(` requires an exact break after the name, so a
/// same-prefixed name never matches -- from its signature line through its own closing
/// `}`, located by brace-matching from the first `{` after the signature. Braces inside
/// string/raw-string and char literals are skipped (see [`first_open_brace`] /
/// [`matching_close_brace`]), so the scan neither stops early nor bleeds into a
/// following, unrelated function. `None` if `fn_name` is not found, or its body is
/// unterminated.
pub fn extract_fn_body(source: &str, fn_name: &str) -> Option<String> {
    let needle = format!("fn {fn_name}(");
    let bytes = source.as_bytes();
    let sig_start = source.find(&needle)?;
    let open = first_open_brace(bytes, sig_start)?;
    let close = matching_close_brace(bytes, open)?;
    Some(source[sig_start..=close].to_string())
}
// --- approval-broker fixtures shared across ipc_control.rs, close_status_settle.rs,
// ipc_v2.rs, store_snapshot.rs and ipc_profile.rs ---------------------------

/// Known-good token injected through `DaemonConfig.status_token_override`
/// (hex, 32 characters -- the only shape `StatusToken::from_override`
/// accepts).
pub const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Base config: the status token is always injected so that `status_event`
/// tests can produce a valid token, `approver_exe` defaults to this test
/// binary's own path -- the same value a real GUI passes via
/// `--approver-exe` -- so `register_approver` succeeds unless
/// a test overrides it to prove the fail-closed / mismatch cases, and
/// detection stays disabled unless a test opts in via [`detection_config`].
/// `open_window` is auto-approved, so the many cases that open windows over
/// IPC to set something else up are not held at the open gate; a case that
/// measures that gate replaces `approval_policy` itself.
pub fn approver_base_config() -> DaemonConfig {
    DaemonConfig {
        status_token_override: Some(VALID_TOKEN.into()),
        approver_exe: Some(yagura_test_support::process::own_canonical_exe()),
        approval_policy: Some(policy(&["open_window"])),
        ..DaemonConfig::default()
    }
}

/// Config for the approval-broker tests: no playbook (every destructive call
/// needs a human decision), on top of [`approver_base_config`] and
/// [`detection_config`] so [`Harness::open_idle_window`] stays available.
/// `approval_timeout_secs` is given explicitly by every caller: short for
/// the tests that must observe a timeout, long for those that must not
/// settle on their own.
pub fn approval_config(dir: &Path, approval_timeout_secs: u64) -> DaemonConfig {
    DaemonConfig {
        playbook: None,
        approval_policy: None,
        approval_timeout_secs,
        ..detection_config(dir, approver_base_config())
    }
}

/// Sends `register_approver` and returns the pending requests the reply
/// carried.
pub async fn register_approver(client: &mut Connection) -> Vec<ApprovalRequest> {
    send(client, &ClientMessage::RegisterApprover).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ApproverRegistered { pending } => pending,
        other => panic!("expected approver_registered, got {other:?}"),
    }
}

/// Reads the next frame and asserts it is `approval_requested`, returning the
/// request it carried.
pub async fn expect_approval_requested(client: &mut Connection) -> ApprovalRequest {
    expect_approval_requested_within(client, Duration::from_secs(5)).await
}

/// Same contract as `expect_approval_requested`, but with a caller-chosen
/// timeout. Use this instead of the 5s default at call sites where the
/// request being awaited just launched a profile-addressed window: that
/// launch spawns a fresh OS process and runs it through the status-detection
/// engine's poll/debounce cycle before the gate ever executes, so it
/// competes for OS process-spawn capacity with every other test binary
/// running concurrently in the same `cargo test` invocation. This suite
/// already budgets 10s for that category of wait elsewhere
/// (`Harness::wait_until_child_exited`, the `wait_for_scrollback` right
/// after a profile launch); a bare IPC round trip on an already-open window
/// does not need it and should keep using `expect_approval_requested`.
pub async fn expect_approval_requested_within(
    client: &mut Connection,
    timeout: Duration,
) -> ApprovalRequest {
    match read_response(client, timeout).await {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    }
}

/// Sends `approval_decision` and asserts it resolves `request_id`.
pub async fn decide(client: &mut Connection, request_id: &str, approved: bool) {
    send(
        client,
        &ClientMessage::ApprovalDecision {
            request_id: request_id.to_string(),
            approved,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::ApprovalResolved {
            request_id: resolved,
        } => assert_eq!(resolved, request_id),
        other => panic!("expected approval_resolved, got {other:?}"),
    }
}

/// Policy that auto-approves exactly `tools` (values are the external tool
/// names). Built through the public fields rather than TOML: the playbook
/// file path is exercised by `tests/control_gate.rs`.
pub fn policy(tools: &[&str]) -> ApprovalPolicy {
    ApprovalPolicy {
        approval: Some(ApprovalSection {
            auto_approve: tools.iter().map(|t| (*t).to_string()).collect(),
        }),
        limits: None,
        chat: None,
        plans: None,
    }
}

/// [`policy`] that also auto-approves `open_window`, for the cases that open
/// windows over IPC to set something else up and measure nothing about the
/// open gate itself.
pub fn policy_opening(tools: &[&str]) -> ApprovalPolicy {
    let mut tools: Vec<&str> = tools.to_vec();
    if !tools.contains(&"open_window") {
        tools.push("open_window");
    }
    policy(&tools)
}

/// A default config whose policy auto-approves `open_window` (see
/// [`policy_opening`]).
pub fn opening_config() -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&["open_window"])),
        ..DaemonConfig::default()
    }
}

/// `base` booted from a real playbook file: `source` is written as
/// `{dir}/playbook.toml`, read back through the same `load_policy` /
/// `apply_limits` / `apply_chat_section` / `apply_plans_section` path the
/// daemon's own startup takes, and `DaemonConfig.playbook` points at that
/// file, so a settings change the daemon writes through lands in a file that
/// actually exists.
pub fn live_playbook_config(dir: &Path, base: DaemonConfig, source: &str) -> DaemonConfig {
    let path = dir.join("playbook.toml");
    std::fs::write(&path, source).expect("write the live playbook file");
    let loaded: ApprovalPolicy = yagura_daemon::control::load_policy(&path)
        .unwrap_or_else(|err| panic!("the live playbook must load: {err}\n---\n{source}\n---"));
    let mut config = base;
    yagura_daemon::control::apply_limits(&mut config, loaded.limits());
    yagura_daemon::control::apply_chat_section(&mut config, loaded.chat.as_ref());
    yagura_daemon::control::apply_plans_section(&mut config, loaded.plans.as_ref());
    config.approval_policy = Some(loaded);
    config.playbook = Some(path);
    config
}

/// Reads the next response frame on an approver connection, skipping the
/// `chat_changed` / `plan_changed` pushes an approver is sent while its own
/// requests are in flight.
pub async fn read_approver_reply(client: &mut Connection, timeout: Duration) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for an approver reply"
        );
        let frame: DaemonMessage = read_response(client, remaining).await;
        match frame {
            DaemonMessage::ChatChanged { .. } | DaemonMessage::PlanChanged { .. } => continue,
            other => return other,
        }
    }
}

/// [`expect_approval_requested_within`] on an approver connection that may
/// also be pushed `chat_changed` / `plan_changed` (see
/// [`read_approver_reply`]).
pub async fn expect_approval_requested_skipping_pushes(
    client: &mut Connection,
    timeout: Duration,
) -> ApprovalRequest {
    let reply: DaemonMessage = read_approver_reply(client, timeout).await;
    match reply {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    }
}

/// Sends `set_auto_approve` over an approver connection and returns the
/// settings its `auto_approve_set` answer carried.
pub async fn set_auto_approve(
    client: &mut Connection,
    tool: &str,
    auto_approve: bool,
) -> ApprovalSettingsInfo {
    send(
        client,
        &ClientMessage::SetAutoApprove {
            tool: tool.to_string(),
            auto_approve,
        },
    )
    .await;
    let reply: DaemonMessage = read_approver_reply(client, Duration::from_secs(10)).await;
    match reply {
        DaemonMessage::AutoApproveSet { settings } => settings,
        other => panic!("expected auto_approve_set for {tool}={auto_approve}, got {other:?}"),
    }
}

/// Sends `set_plan_approval_required` over an approver connection and
/// returns the settings its `plan_approval_required_set` answer carried.
pub async fn set_plan_approval_required(
    client: &mut Connection,
    required: bool,
) -> ApprovalSettingsInfo {
    send(client, &ClientMessage::SetPlanApprovalRequired { required }).await;
    let reply: DaemonMessage = read_approver_reply(client, Duration::from_secs(10)).await;
    match reply {
        DaemonMessage::PlanApprovalRequiredSet { settings } => settings,
        other => panic!("expected plan_approval_required_set for {required}, got {other:?}"),
    }
}

/// The `auto_approve` value `settings` reports for `tool`, or `None` when
/// the tool is not listed at all.
pub fn reported_auto_approve(settings: &ApprovalSettingsInfo, tool: &str) -> Option<bool> {
    settings
        .tools
        .iter()
        .find(|entry: &&ToolAutoApprove| entry.tool == tool)
        .map(|entry: &ToolAutoApprove| entry.auto_approve)
}

// --- chat fixtures --------------------------------------------------
//
// Every chat integration test builds its `DaemonConfig` from a synthetic
// playbook TOML source run through the real `parse_policy` -> `apply_limits`
// / `apply_chat_section` path, never by poking
// `DaemonConfig.chat_limits` / `chat_intro` / `chat_exclude_detections`
// directly: a chat test then also exercises the same parsing and merging code
// production startup does.

/// The knobs a chat integration test may want to vary in its synthetic
/// playbook. Every field defaults to "omitted from the TOML", which leaves
/// the built-in default in effect (`room::ChatLimits::default()` /
/// `DaemonConfig::default()`), with two exceptions: `intro`, whose builder
/// default is `Some("")` (intro disabled), not `None` (built-in default
/// text), because intro is a delivery that shares the chat delivery lock and
/// budget with every other test in this suite -- only the
/// intro-observing cases opt in with a non-empty value; and `auto_approve`,
/// whose builder default is the bundled `playbook.toml`'s own default
/// (`["send_chat", "open_window"]`) rather than an empty list, so a test that
/// never overrides it exercises the same auto-approval boundary production
/// ships with.
#[derive(Debug, Clone, Copy)]
pub struct ChatPlaybook<'a> {
    pub auto_approve: &'a [&'a str],
    pub repetition_count: Option<u64>,
    pub repetition_window: Option<u64>,
    pub repetition_distance: Option<u64>,
    pub monologue_max: Option<u64>,
    pub turn_message_max: Option<u64>,
    pub silence_timeout_secs: Option<u64>,
    pub history_max: Option<u64>,
    /// `None` means "use this builder's own default" (`""`, intro disabled),
    /// not "omit `intro` from the TOML and take the built-in default text".
    /// Nothing in this suite needs that built-in-text case.
    pub intro: Option<&'a str>,
    pub exclude_detections: Option<&'a [&'a str]>,
    pub chat_delivery_max: Option<u64>,
    pub approval_timeout_secs: Option<u64>,
}

impl<'a> Default for ChatPlaybook<'a> {
    fn default() -> Self {
        Self {
            auto_approve: &["send_chat", "open_window"],
            repetition_count: None,
            repetition_window: None,
            repetition_distance: None,
            monologue_max: None,
            turn_message_max: None,
            silence_timeout_secs: None,
            history_max: None,
            intro: None,
            exclude_detections: None,
            chat_delivery_max: None,
            approval_timeout_secs: None,
        }
    }
}

/// Escapes `value` as a TOML basic-string body: only `"` and `\` need
/// escaping for the plain ASCII text every caller in this suite passes.
fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn chat_playbook_toml(spec: &ChatPlaybook<'_>) -> String {
    let mut out = String::new();
    out.push_str("[approval]\nauto_approve = [");
    out.push_str(
        &spec
            .auto_approve
            .iter()
            .map(|tool| format!("\"{}\"", toml_escape(tool)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("]\n\n[chat]\n");
    out.push_str(&format!(
        "intro = \"{}\"\n",
        toml_escape(spec.intro.unwrap_or(""))
    ));
    if let Some(v) = spec.repetition_count {
        out.push_str(&format!("repetition_count = {v}\n"));
    }
    if let Some(v) = spec.repetition_window {
        out.push_str(&format!("repetition_window = {v}\n"));
    }
    if let Some(v) = spec.repetition_distance {
        out.push_str(&format!("repetition_distance = {v}\n"));
    }
    if let Some(v) = spec.monologue_max {
        out.push_str(&format!("monologue_max = {v}\n"));
    }
    if let Some(v) = spec.turn_message_max {
        out.push_str(&format!("turn_message_max = {v}\n"));
    }
    if let Some(v) = spec.silence_timeout_secs {
        out.push_str(&format!("silence_timeout_secs = {v}\n"));
    }
    if let Some(v) = spec.history_max {
        out.push_str(&format!("history_max = {v}\n"));
    }
    if let Some(names) = spec.exclude_detections {
        out.push_str("exclude_detections = [");
        out.push_str(
            &names
                .iter()
                .map(|name| format!("\"{}\"", toml_escape(name)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push_str("]\n");
    }
    out.push_str("\n[limits]\n");
    if let Some(v) = spec.chat_delivery_max {
        out.push_str(&format!("chat_delivery_max = {v}\n"));
    }
    if let Some(v) = spec.approval_timeout_secs {
        out.push_str(&format!("approval_timeout_secs = {v}\n"));
    }
    out
}

/// Builds a chat-ready `DaemonConfig`: [`detection_config`] (so
/// [`Harness::open_idle_window`] stays available) layered under a synthetic
/// playbook run through `parse_policy` -> `apply_limits` /
/// `apply_chat_section`, plus a resolved store path under `dir` so
/// `ChatRuntime::from_config` actually opens a `ChatStore` (a fresh path per
/// call; two calls given the *same* `dir` therefore share one chat history,
/// which is what the daemon-restart case relies on).
pub fn chat_config(dir: &Path, spec: &ChatPlaybook<'_>) -> DaemonConfig {
    let source = chat_playbook_toml(spec);
    let chat_policy = yagura_daemon::control::parse_policy(&source).unwrap_or_else(|err| {
        panic!("the synthetic chat playbook must parse: {err}\n---\n{source}\n---")
    });
    let mut config = detection_config(dir, approver_base_config());
    yagura_daemon::control::apply_limits(&mut config, chat_policy.limits());
    yagura_daemon::control::apply_chat_section(&mut config, chat_policy.chat.as_ref());
    config.approval_policy = Some(chat_policy);
    config.playbook = Some(dir.join("synthetic-chat-playbook.toml"));
    config.store_db_path_resolved = Some(dir.join("chat.db"));
    config
}

/// [`chat_config`] whose synthetic playbook is also written to the path
/// `DaemonConfig.playbook` names, for a case in which the daemon writes a
/// settings change through to that file.
pub fn chat_config_with_playbook_file(dir: &Path, spec: &ChatPlaybook<'_>) -> DaemonConfig {
    let config = chat_config(dir, spec);
    let path: &Path = config
        .playbook
        .as_deref()
        .expect("chat_config always names a playbook path");
    std::fs::write(path, chat_playbook_toml(spec)).expect("write the synthetic chat playbook");
    config
}

/// Probe used as [`DaemonConfig::program_is_executable`] by
/// [`handoff_profile_config`]: a plain existence check, the same discipline
/// `ipc_profile.rs`'s own `file_exists` uses.
fn handoff_program_is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Config for `ipc_profile.rs`'s `send_handoff` tests: a launch-profile
/// resolution environment ([`ProgramLookup`] injected, never the real PATH)
/// layered on [`approver_base_config`] and [`detection_config`] (so a window
/// opened from a profile can reach `WaitingInput` / `Idle`), with the
/// approval side ([`DaemonConfig::playbook`] / `approval_policy` /
/// `approval_timeout_secs`) left to the caller: `playbook` alone (with no
/// `approval_policy`) produces `PolicySource::Failed`, and `approval_policy`
/// injects an `auto_approve` list.
pub fn handoff_profile_config(
    profiles_dir: Option<PathBuf>,
    lookup: ProgramLookup,
    detection_dir: &Path,
    playbook: Option<PathBuf>,
    approval_policy: Option<ApprovalPolicy>,
    approval_timeout_secs: u64,
) -> DaemonConfig {
    // Opening the window is not what these cases measure: `open_window` is
    // auto-approved on top of whatever policy is given, except for a
    // playbook that failed to load, which stays failed.
    let approval_policy = match (approval_policy, playbook.is_some()) {
        (Some(mut given), _) => {
            let section = given.approval.get_or_insert_with(|| ApprovalSection {
                auto_approve: Vec::new(),
            });
            if !section
                .auto_approve
                .iter()
                .any(|name| name == "open_window")
            {
                section.auto_approve.push("open_window".to_string());
            }
            Some(given)
        }
        (None, false) => Some(policy(&["open_window"])),
        (None, true) => None,
    };
    DaemonConfig {
        profiles_dir,
        host_os: ProfileOs::host(),
        program_lookup: lookup,
        program_is_executable: handoff_program_is_executable,
        default_shell: None,
        playbook,
        approval_policy,
        approval_timeout_secs,
        ..detection_config(detection_dir, approver_base_config())
    }
}

// --- submit_child delivery-test detection definitions -----------

/// The probe every test definition that takes a judged delivery declares:
/// typed as-is into a composer-draw fixture's composer and erased by DEL.
pub const SUBMIT_CHILD_PROBE: &str = composer_probe!();

/// Values for the `[input]` table of the one detection-definition shape every
/// `send_handoff` delivery test against the `submit_child` fixture uses (see
/// [`write_submit_definition`]).
///
/// `submit` is left at the definition-loader default (`"\r"`, matching the
/// carriage return `submit_child` itself treats as a submit key), so it is
/// not a field here.
pub struct SubmitDefinitionInput {
    pub bracketed_paste: bool,
    pub ready_hold_ms: u64,
    pub submit_delay_ms: u64,
    /// The `clear` value's TOML basic-string *body*, already escaped: `""`
    /// for no clear sequence, or `"\\u0015"` for Ctrl-U. Kept as the escaped
    /// source text (rather than a raw control byte) so the definition file
    /// stays literally readable -- an embedded `0x15` byte would be
    /// invisible in a diff or a failed test's file dump.
    pub clear: &'static str,
    pub delivery_confirm_ms: u64,
    pub resend_max: u8,
}

impl SubmitDefinitionInput {
    /// The body written under the definition's `[input]` heading, naming
    /// [`SUBMIT_CHILD_PROBE`] whether or not a clear sequence is given: a
    /// judged delivery needs a probe either way.
    pub fn input_toml(&self) -> String {
        let Self {
            bracketed_paste,
            ready_hold_ms,
            submit_delay_ms,
            clear,
            delivery_confirm_ms,
            resend_max,
        } = self;
        format!(
            "bracketed_paste = {bracketed_paste}\nready_hold_ms = {ready_hold_ms}\nsubmit_delay_ms = {submit_delay_ms}\nclear = \"{clear}\"\ndelivery_confirm_ms = {delivery_confirm_ms}\nresend_max = {resend_max}\nprobe = \"{SUBMIT_CHILD_PROBE}\""
        )
    }
}

/// [`write_submit_definition`]'s own `bottom_line_count`: enough for a
/// short, unwrapped composer, but not for the multi-line region a long
/// composer-draw-mode body wraps into (see
/// [`write_submit_definition_with_bottom_line_count`]).
const SUBMIT_DEFINITION_BOTTOM_LINE_COUNT: u16 = 5;

/// Writes the one `submit_child` delivery-test detection definition shape as
/// `{dir}/{name}.toml`: a `waiting_input` rule
/// on `marker` (`submit_child`'s startup line and its re-print on a
/// discarded submit), a `running` rule on `submit_child`'s own `SUBMITTED` /
/// `REACT` output, the `[input]` table from `input`, and
/// [`composer_section_toml`].
///
/// `marker` is inserted into the definition's regex `pattern` verbatim, so a
/// caller's marker must not contain regex metacharacters.
pub fn write_submit_definition(dir: &Path, name: &str, marker: &str, input: SubmitDefinitionInput) {
    write_submit_definition_with_bottom_line_count(
        dir,
        name,
        marker,
        input,
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
    );
}

/// The `waiting_input` pattern of a `submit_child` window that can be returned
/// to the ready status after it has echoed a line: `marker` alone matches only
/// the line printed at startup, which a typed line's `SUBMITTED` echo (a
/// `running` rule of the same definition) pushes out of the detection window
/// for good. The dots [`redraw_screen`] prints match the second alternative.
///
/// Pass the result wherever a marker is taken; the fixture prints it as text,
/// and the pattern matches that text through its first alternative.
pub fn ready_pattern_with_redraw(marker: &str) -> String {
    format!(r"{marker}|^\.$")
}

/// Makes a `submit_child` window repaint its screen with filler dots (the byte
/// `0x03`), the way a CLI redraws its prompt after a turn. The window is ready
/// again only if its definition's ready pattern accepts the dots
/// ([`ready_pattern_with_redraw`]); callers wait for the status they expect.
///
/// `marker` is the text the window's `submit_child` was given. The fixture
/// prints it once it reads raw input, and a byte written before that is lost,
/// so the byte waits for the marker to appear.
pub async fn redraw_screen(h: &Harness, window_id: &str, marker: &str) {
    h.wait_for_scrollback(window_id, marker.as_bytes(), Duration::from_secs(30))
        .await;
    h.write_input(window_id, &[0x03]).await;
}

/// [`write_submit_definition`], with `bottom_line_count` taken from the
/// caller instead of fixed at [`SUBMIT_DEFINITION_BOTTOM_LINE_COUNT`].
///
/// A window's own `waiting_input` marker line is printed once, at startup,
/// and never repainted; a composer-draw-mode fixture repaints its region on
/// every reflected write, below that line. A body long enough to wrap past
/// several rows pushes the marker line up along with it, and once it falls
/// outside the last `bottom_line_count` rows of the screen, the
/// `waiting_input` rule can no longer match it -- the window's confirmed
/// status regresses out from under a test that never touched it on purpose,
/// so once the composer grows past the window's own `bottom_line_count` the
/// caller needs a wider one (see [`write_control_definition_with_bottom_line_count`]).
pub fn write_submit_definition_with_bottom_line_count(
    dir: &Path,
    name: &str,
    marker: &str,
    input: SubmitDefinitionInput,
    bottom_line_count: u16,
) {
    let toml = yagura_test_support::detection::submit_definition_toml(
        name,
        marker,
        bottom_line_count,
        &input.input_toml(),
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write submit_child delivery-test definition file");
}

/// The same `submit_child` delivery-test definition shape as
/// [`write_submit_definition`], with its `[input]` table taken verbatim from
/// `input_toml` instead of built from [`SubmitDefinitionInput`], for a case
/// whose `[input]` table does not fit that struct's fixed field set.
/// `input_toml` is the body the caller writes under `[input]` (e.g.
/// `"submit_delay_ms = 120\ndelivery_confirm_ms = 0\n"`).
pub fn write_submit_definition_with_input_toml(
    dir: &Path,
    name: &str,
    marker: &str,
    input_toml: &str,
) {
    let toml = yagura_test_support::detection::submit_definition_toml(
        name,
        marker,
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        input_toml,
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write submit_child delivery-test definition file");
}

/// Opens a `submit_child` window whose detection definition's `[input]`
/// table is `input_toml` (see [`write_submit_definition_with_input_toml`]),
/// launched in composer-draw mode with `clear_scope` selecting the fixture's
/// take-back behavior (see
/// [`yagura_test_support::process::submit_child_command_with_clear_scope`]),
/// and observes it reach `waiting_input` before returning.
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_clear_scope(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    input_toml: &str,
    reflect_delay_ms: u64,
    clear_scope: &str,
) -> String {
    write_submit_definition_with_input_toml(dir, name, marker, input_toml);
    let command = yagura_test_support::process::submit_child_command_with_clear_scope(
        marker,
        0,
        0,
        0,
        reflect_delay_ms,
        clear_scope,
    );
    let window_id = h.open_delivering(command, Some(name)).await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

// --- window/session interaction helpers shared across ipc_control.rs and
// ipc_profile.rs, and by any future test binary that needs the same
// constructions (e.g. `composer_gate.rs`) -----------------------

/// Sends `Identify` and asserts the daemon acked the given window. Every
/// caller that needs a window-owning connection (rather than an external
/// one) relies on [`VALID_TOKEN`] being injected via [`approver_base_config`]
/// or [`handoff_profile_config`].
pub async fn identify(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::Identify {
            window_id: window_id.to_string(),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected identified, got {other:?}"),
    }
}

/// Sends `open_window` over `client`'s own connection (so the daemon sees
/// this request's real origin, unlike [`Harness::open`], which spawns
/// straight through the registry with no connection at all) and returns the
/// newly opened window's id.
pub async fn call_open_window(client: &mut Connection, command: Vec<String>) -> String {
    send(
        client,
        &ClientMessage::OpenWindow {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// Opens a window over `client` whose child exits at once with code 5, and
/// returns only once the daemon has confirmed `Exited`: the `child_exited`
/// gates read that confirmed status, which follows the raw exit observation.
pub async fn open_exited_window_via_ipc(client: &mut Connection) -> String {
    let window_id = call_open_window(
        client,
        yagura_test_support::process::exit_with_code_command("5"),
    )
    .await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::Exited,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// Sends `ListWindows` and returns the windows it reports.
pub async fn call_list_windows(client: &mut Connection) -> Vec<WindowInfo> {
    send(client, &ClientMessage::ListWindows).await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::WindowList { windows } => windows,
        other => panic!("expected window_list, got {other:?}"),
    }
}

/// Counts non-overlapping occurrences of `needle` in `window_id`'s current
/// scrollback, decoded lossily (the fixtures this suite uses only ever write
/// plain ASCII, so lossy decoding never hides a real occurrence).
///
/// This is the vessel for "how many times did the receiver accept a
/// send" -- pass a needle the fixture writes only on acceptance (its
/// `SUBMITTED ` / `REACT` lines, never a composer-echo line). A caller
/// counting a composer's own on-screen text instead would double-count once
/// a fixture starts repainting an unsent draft on every write, because that
/// text then appears both on each repaint and once more on acceptance; this
/// helper's contract is to count acceptance only, matching
/// [`Harness::wait_for_scrollback_occurrences`]'s note that a marker line is
/// rendered by two independent writers (the terminal's local echo and the
/// child's own echo of what it read).
pub fn count_marker(h: &Harness, window_id: &str, needle: &str) -> usize {
    count_in_scrollback(&h.scrollback(window_id), needle)
}

fn count_in_scrollback(scrollback: &[u8], needle: &str) -> usize {
    yagura_test_support::scan::count_occurrences(&String::from_utf8_lossy(scrollback), needle)
}

/// How long [`settled_marker_count`] keeps watching once `expected` copies
/// have shown, so an extra copy still in flight is counted too. No event
/// proves a copy will never come; on a slow machine this window can only
/// miss a late extra copy (a false pass), never fail a correct run.
const MARKER_SETTLE_QUIET: Duration = Duration::from_millis(500);

/// `needle`'s count in `window_id`'s scrollback, read only once `expected`
/// copies have appeared (or 10s passed) and no further copy has shown for
/// [`MARKER_SETTLE_QUIET`]. A call returning `Placed` has written its bytes,
/// but the child's echo reaches the scrollback through its own asynchronous
/// path, so an immediate read can still see nothing.
pub async fn settled_marker_count(
    h: &Harness,
    window_id: &str,
    needle: &str,
    expected: usize,
) -> usize {
    settled_count(h, window_id, expected, |scrollback| {
        count_in_scrollback(scrollback, needle)
    })
    .await
}

/// [`settled_marker_count`] with the counting left to `count`, for a caller
/// that must tell some copies apart from others (a composer draft from an
/// accepted line).
pub async fn settled_count(
    h: &Harness,
    window_id: &str,
    expected: usize,
    count: impl Fn(&[u8]) -> usize,
) -> usize {
    let appear_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut quiet_deadline = None;
    h.poll_scrollback(window_id, |scrollback, now| {
        let found = count(scrollback);
        let settled = if found > expected {
            true
        } else if found == expected {
            now >= *quiet_deadline.get_or_insert(now + MARKER_SETTLE_QUIET)
        } else {
            now >= appear_deadline
        };
        settled.then_some(found)
    })
    .await
}

/// [`settled_marker_count`] for the line `submit_child` echoes once `body` is
/// submitted, with or without the space the daemon ends its text with: a
/// repaint may drop a trailing space, and whether it was written is measured
/// on the child's input instead.
pub async fn settled_submitted_count(
    h: &Harness,
    window_id: &str,
    body: &str,
    expected: usize,
) -> usize {
    let lines = [
        format!("SUBMITTED {body}\r"),
        format!("SUBMITTED {body} \r"),
    ];
    settled_count(h, window_id, expected, |scrollback| {
        lines
            .iter()
            .map(|line| count_in_scrollback(scrollback, line))
            .sum()
    })
    .await
}

/// Opens a `submit_child` window bound to a fresh detection definition built
/// by [`write_submit_definition`], and observes it reach `waiting_input`
/// (the fixture's startup marker line) before returning: never assert
/// against a delivery outcome for a window whose readiness was not itself
/// observed first.
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
) -> String {
    open_submit_child_window_in_mode(
        h,
        client,
        dir,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        None,
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        None,
    )
    .await
}

/// [`open_submit_child_window`] with the fixture in composer-draw mode: it
/// paints what is written to it as a composer region and reflects each write
/// `reflect_delay_ms` after it lands, so a caller can put a delivery's own
/// steps on either side of that wait.
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_composer(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: u64,
) -> String {
    open_submit_child_window_in_mode(
        h,
        client,
        dir,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        Some(reflect_delay_ms),
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        None,
    )
    .await
}

/// [`open_submit_child_window_with_composer`], with an explicit window
/// `cwd`: a second entry point for a case that
/// must open a participant in a second workspace.
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_composer_in_cwd(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    cwd: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: u64,
) -> String {
    open_submit_child_window_in_mode_with_cwd(
        h,
        client,
        dir,
        Some(cwd),
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        Some(reflect_delay_ms),
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        None,
        None,
    )
    .await
}

/// [`open_submit_child_window_with_composer`] whose fixture appends every
/// byte it reads to `input_log`, so a case can wait on what the child has
/// read (see [`wait_until_input_log_contains`]).
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_composer_logging_input(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: u64,
    input_log: &Path,
) -> String {
    open_submit_child_window_in_mode_with_cwd(
        h,
        client,
        dir,
        None,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        Some(reflect_delay_ms),
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        None,
        Some(input_log),
    )
    .await
}

/// [`open_submit_child_window_with_composer`] with an explicit
/// `bottom_line_count`, for a body long enough that composer-draw mode wraps
/// it past [`SUBMIT_DEFINITION_BOTTOM_LINE_COUNT`] and scrolls the fixture's
/// once-only startup marker line out of the window (see
/// [`write_submit_definition_with_bottom_line_count`]).
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_composer_and_bottom_line_count(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: u64,
    bottom_line_count: u16,
) -> String {
    open_submit_child_window_in_mode(
        h,
        client,
        dir,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        Some(reflect_delay_ms),
        bottom_line_count,
        None,
    )
    .await
}

/// [`open_submit_child_window_with_composer`] with the fixture's composer
/// folded: whatever is written to it paints as `placeholder` instead of the
/// body, so the composer stays observable while
/// its content cannot be read back off the screen, so only the write record
/// can vouch for a resend or a take-back.
#[allow(clippy::too_many_arguments)]
pub async fn open_submit_child_window_with_folded_composer(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: u64,
    placeholder: &str,
) -> String {
    open_submit_child_window_in_mode(
        h,
        client,
        dir,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        Some(reflect_delay_ms),
        SUBMIT_DEFINITION_BOTTOM_LINE_COUNT,
        Some(placeholder),
    )
    .await
}

/// The one implementation behind all `submit_child` window openers: they
/// differ only in whether the fixture is launched in composer-draw mode, in
/// whether that composer folds, and in `bottom_line_count`, and the
/// definition, the readiness wait and the failure text must not drift
/// between them.
#[allow(clippy::too_many_arguments)]
async fn open_submit_child_window_in_mode(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: Option<u64>,
    bottom_line_count: u16,
    fold_placeholder: Option<&str>,
) -> String {
    open_submit_child_window_in_mode_with_cwd(
        h,
        client,
        dir,
        None,
        name,
        marker,
        discard_reads,
        paste_guard_keys,
        react_on_body,
        input,
        reflect_delay_ms,
        bottom_line_count,
        fold_placeholder,
        None,
    )
    .await
}

/// [`open_submit_child_window_in_mode`], with an explicit window `cwd`:
/// `dir` is where the detection
/// definition is written, `cwd` is where the opened window's own process
/// runs, `None` reproducing the original's `h.open` (the daemon's own cwd).
#[allow(clippy::too_many_arguments)]
async fn open_submit_child_window_in_mode_with_cwd(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    cwd: Option<&Path>,
    name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    input: SubmitDefinitionInput,
    reflect_delay_ms: Option<u64>,
    bottom_line_count: u16,
    fold_placeholder: Option<&str>,
    input_log: Option<&Path>,
) -> String {
    write_submit_definition_with_bottom_line_count(dir, name, marker, input, bottom_line_count);
    let mut command = match (reflect_delay_ms, fold_placeholder) {
        (Some(reflect_delay_ms), Some(placeholder)) => {
            yagura_test_support::process::submit_child_command_with_folded_composer(
                marker,
                discard_reads,
                paste_guard_keys,
                react_on_body,
                reflect_delay_ms,
                placeholder,
            )
        }
        (Some(reflect_delay_ms), None) => {
            yagura_test_support::process::submit_child_command_with_composer(
                marker,
                discard_reads,
                paste_guard_keys,
                react_on_body,
                reflect_delay_ms,
            )
        }
        (None, _) => yagura_test_support::process::submit_child_command(
            marker,
            discard_reads,
            paste_guard_keys,
            react_on_body,
        ),
    };
    if let Some(input_log) = input_log {
        // The log is argv index 14; `-` leaves every slot before it at its default.
        command.resize(14, "-".to_string());
        command.push(
            input_log
                .to_str()
                .expect("the input log path must be valid UTF-8")
                .to_string(),
        );
    }
    let window_id = h.open_delivering_in(command, Some(name), cwd).await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// Attaches `client` to `window_id` (the interactive PTY-streaming path:
/// [`ClientMessage::AttachView`] with no size), and asserts the daemon acknowledged it.
/// The frames that follow the answer are left unread (and unacknowledged);
/// `read_response` skips them.
/// Unlike [`identify`], attaching does not require a token: it is the
/// path a GUI window uses to stream its own PTY, not the path a window
/// authenticates its own `status_event` calls through.
pub async fn attach(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::AttachView {
            window_id: window_id.to_string(),
            size: None,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(5)).await {
        DaemonMessage::Attached { .. } => {}
        other => panic!("expected attached, got {other:?}"),
    }
}

/// Writes `text` to an already-[`attach`]ed connection's window through the
/// `text` IPC message ([`InputOrigin::Interactive`] on the daemon side) --
/// scene (d) of this file's shared-helper contract: the human-keystroke
/// path, distinct from [`write_window_input`]'s direct registry write
/// (bypasses the daemon entirely) and from a daemon-originated send
/// (`SendText` / `SendHandoff`, `InputOrigin::Message`). No reply is read:
/// the daemon does not acknowledge a successful `text` frame.
pub async fn write_interactive_input(client: &mut Connection, text: &str) {
    send(
        client,
        &ClientMessage::Text {
            text: text.to_string(),
        },
    )
    .await;
}

/// Sends `first` on `client_a`, awaits `between`, then sends `second` on
/// `client_b`. `send` only writes the request frame and does not wait for a
/// response, so `between` alone separates the two requests' arrival at the
/// daemon.
///
/// This is scenes (f) and (h) of this file's shared-helper contract: two
/// connections addressing the same window. A case whose assertions depend on
/// `first` arriving before `second` passes an observation of `first`'s effect
/// (for instance its body reaching the child); a case that only tries for an
/// overlap passes a short sleep.
pub async fn send_from_two_connections(
    client_a: &mut Connection,
    first: &ClientMessage,
    client_b: &mut Connection,
    second: &ClientMessage,
    between: impl std::future::Future<Output = ()>,
) {
    send(client_a, first).await;
    between.await;
    send(client_b, second).await;
}

// --- profile-addressed send_handoff helpers shared across ipc_profile.rs and
// any future binary needing scene (g) (opening a new window through a
// launch profile's send_handoff) ------------------------

pub fn write_profile(dir: &Path, id: &str, source: &str) {
    std::fs::write(dir.join(format!("{id}.toml")), source).expect("profile should be writable");
}

/// A [`ProgramLookup`] resolving nothing: every fixture profile launches its
/// child by absolute path, so no lookup is ever needed to spawn it, but
/// [`handoff_profile_config`] requires one.
pub fn empty_lookup() -> ProgramLookup {
    ProgramLookup {
        path_dirs: Vec::new(),
        extra_bin_dirs: Vec::new(),
        windows_comspec: None,
    }
}

/// A `submit_child` profile source under its own `detection` definition
/// ([`write_submit_definition`]'s output), whose `[command]` launches
/// the fixture in composer-draw mode: the window a profile-addressed
/// delivery opens paints its composer just as a directly opened one does.
pub fn submit_child_profile_source_with_detection_and_composer(
    display_name: &str,
    marker: &str,
    discard_reads: u64,
    paste_guard_keys: u64,
    react_on_body: u8,
    detection: &str,
    reflect_delay_ms: u64,
) -> String {
    with_detection_name(
        &yagura_test_support::process::submit_child_profile_source_with_composer(
            display_name,
            marker,
            discard_reads,
            paste_guard_keys,
            react_on_body,
            reflect_delay_ms,
        ),
        detection,
    )
}

/// A profile source launching `submit_child` in composer-draw mode with
/// `clear_scope` (see the fixture's module doc), under the detection
/// definition `detection`.
pub fn submit_child_profile_source_with_detection_and_clear_scope(
    display_name: &str,
    marker: &str,
    detection: &str,
    clear_scope: &str,
) -> String {
    let mut command =
        yagura_test_support::process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    // A profile's argv admits no empty element, so the "no folding" slot is `-`.
    command.push("-".to_string());
    command.push(clear_scope.to_string());
    with_detection_name(
        &yagura_test_support::process::profile_source(display_name, &command),
        detection,
    )
}

/// `source` with an explicit `detection` name inserted ahead of its
/// `[command]` table.
pub fn with_detection_name(source: &str, detection: &str) -> String {
    let with_detection = source.replacen(
        "\n\n[command]",
        &format!("\ndetection = \"{detection}\"\n\n[command]"),
        1,
    );
    assert_ne!(
        with_detection, source,
        "a profile source's shape must still contain \"\\n\\n[command]\" exactly once"
    );
    with_detection
}

/// Writes `profile_source` as `{profiles_dir}/{profile_id}.toml`, then sends
/// a profile-addressed `send_handoff` and returns whatever frame comes back
/// first on `client` (never assumed to be the reply to *this* request:
/// callers with more than one request in flight on the same connection must
/// match by message kind, not by position).
pub async fn send_handoff_from_profile(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
) {
    send_handoff_from_profile_with_options(client, profile, cwd, name, reference, note, None, None)
        .await;
}

/// [`send_handoff_from_profile`] carrying the two launch options only the
/// profile form of the request accepts.
#[allow(clippy::too_many_arguments)]
pub async fn send_handoff_from_profile_with_options(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
    model: Option<&str>,
    effort: Option<&str>,
) {
    send(
        client,
        &ClientMessage::SendHandoff {
            window_id: None,
            profile: Some(profile.to_string()),
            cwd: cwd.map(str::to_string),
            name: name.map(str::to_string),
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            reference: reference.to_string(),
            note: note.to_string(),
        },
    )
    .await;
}

/// [`send_handoff_from_profile`] followed by reading the reply.
///
/// Use this for cases rejected before a window is ever opened
/// (`invalid_argument` / `approval_required` on a pre-launch policy
/// judgement): the reply is a plain IPC round trip, so the default 5s is
/// enough. A case that actually launches a profile window and waits for it
/// to become ready before replying `handoff_sent` needs
/// [`call_send_handoff_from_profile_within`] instead (see its doc comment).
pub async fn call_send_handoff_from_profile(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
) -> DaemonMessage {
    call_send_handoff_from_profile_with_options(
        client, profile, cwd, name, reference, note, None, None,
    )
    .await
}

/// [`call_send_handoff_from_profile`] carrying the two launch options only the
/// profile form of the request accepts.
#[allow(clippy::too_many_arguments)]
pub async fn call_send_handoff_from_profile_with_options(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> DaemonMessage {
    call_send_handoff_from_profile_within_with_options(
        client,
        profile,
        cwd,
        name,
        reference,
        note,
        model,
        effort,
        Duration::from_secs(5),
    )
    .await
}

/// Same contract as [`call_send_handoff_from_profile`], but with a
/// caller-chosen timeout. A request that launches a profile-addressed
/// window and reports `opened: true` only replies `handoff_sent` once that
/// window has spawned and passed the status-detection engine's
/// poll/debounce cycle, the same category of wait this suite already
/// budgets 10s for elsewhere (`Harness::wait_until_child_exited`,
/// `Harness::wait_for_scrollback_occurrences` right after a profile
/// launch); a flat 5s races with that spawn under the process-spawn
/// contention of a full multi-binary `cargo test` run.
pub async fn call_send_handoff_from_profile_within(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
    timeout: Duration,
) -> DaemonMessage {
    call_send_handoff_from_profile_within_with_options(
        client, profile, cwd, name, reference, note, None, None, timeout,
    )
    .await
}

/// [`call_send_handoff_from_profile_within`] carrying the two launch options
/// only the profile form of the request accepts.
#[allow(clippy::too_many_arguments)]
pub async fn call_send_handoff_from_profile_within_with_options(
    client: &mut Connection,
    profile: &str,
    cwd: Option<&str>,
    name: Option<&str>,
    reference: &str,
    note: &str,
    model: Option<&str>,
    effort: Option<&str>,
    timeout: Duration,
) -> DaemonMessage {
    send_handoff_from_profile_with_options(
        client, profile, cwd, name, reference, note, model, effort,
    )
    .await;
    read_response(client, timeout).await
}

/// Asserts `msg` is `handoff_sent` and returns `(window_id, opened, delivery)`.
pub fn expect_handoff_sent(msg: DaemonMessage) -> (String, bool, HandoffDelivery) {
    match msg {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
            ..
        } => (window_id, opened, delivery),
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

/// Every `PASTE ` line's `buffered=` value in `window_id`'s scrollback, in
/// the order the `submit_child` fixture wrote them: how many bytes were
/// already sitting in the receiver's buffer when each body landed, which is
/// the only direct evidence of one send having been written on top of
/// another's.
pub fn paste_buffered_values(h: &Harness, window_id: &str) -> Vec<usize> {
    let text = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
    text.lines()
        .filter(|line| line.contains("PASTE "))
        .map(|line| {
            let (_, after) = line.split_once("buffered=").unwrap_or_else(|| {
                panic!(
                    "fixture must write PASTE <n> buffered=<bytes>, got a PASTE line \
                     without buffered=: {line:?}"
                )
            });
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() {
                panic!(
                    "fixture must write PASTE <n> buffered=<bytes>, got a buffered= value \
                     with no digits: {line:?}"
                );
            }
            digits
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("buffered= value is not a valid usize: {line:?}"))
        })
        .collect()
}

/// The marker every handoff envelope line starts with.
const HANDOFF_ENVELOPE_MARKER: &str = "[yagura handoff] ";

/// The JSON body of the handoff envelope `window_id`'s child accepted as a
/// submitted line, waited for rather than read once.
///
/// A window in composer-draw mode paints the same envelope text as an
/// unsubmitted draft -- wrapped mid-line, so it neither parses as JSON nor
/// proves anything was delivered -- and repaints it any number of times
/// before and after the accepted line lands. Occurrences of the marker are
/// therefore not countable evidence; only one the fixture did not paint as a
/// draft is.
pub async fn wait_for_accepted_handoff_envelope_json(
    h: &Harness,
    window_id: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let text = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
        if let Some(json) = accepted_handoff_envelope_json(&text) {
            return json;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for an accepted handoff envelope line in window \
                 {window_id} scrollback: {text:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The first envelope occurrence in `text` that composer-draw mode did not
/// paint as a draft, parsed as JSON, or `None` while no such occurrence has
/// landed yet. A draft always carries the marker glued directly onto
/// `{COMPOSER_MARKER}{COMPOSER_SEPARATOR}`; an accepted line never does.
fn accepted_handoff_envelope_json(text: &str) -> Option<serde_json::Value> {
    let draft_prefix = format!(
        "{}{}",
        yagura_test_support::process::COMPOSER_MARKER,
        yagura_test_support::process::COMPOSER_SEPARATOR,
    );
    let start = text
        .match_indices(HANDOFF_ENVELOPE_MARKER)
        .map(|(index, _)| index)
        .find(|index| !text[..*index].ends_with(&draft_prefix))?;
    let after = text.get(start + HANDOFF_ENVELOPE_MARKER.len()..)?;
    let end = after.find('\r').unwrap_or(after.len());
    let json_part = after.get(..end)?.trim_end_matches(|c: char| c != '}');
    serde_json::from_str(json_part).ok()
}

// --- mcp_cli_stub fixture path and profile document --------------------------

/// Path to the `mcp_cli_stub` fixture binary that belongs to the test binary
/// currently running (see `src/bin/mcp_cli_stub.rs`'s module doc for its
/// argv and environment-variable contract).
///
/// Resolved the same way [`echo_child_path`] is (`cargo test` places a test
/// binary in `target/{profile}/deps/`, a plain bin target one level up), and
/// kept local to this file rather than added to `yagura-test-support`: only
/// `yagura-daemon`'s own tests spawn a registration CLI.
///
/// # Panics
///
/// Panics if the running executable's own path cannot be resolved, or if the
/// fixture binary is not present next to it -- build it with `cargo build -p
/// yagura-daemon --bin mcp_cli_stub` first.
pub fn mcp_cli_stub_path() -> PathBuf {
    let mut dir =
        std::env::current_exe().expect("current_exe should resolve for a running test binary");
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    let name = if cfg!(windows) {
        "mcp_cli_stub.exe"
    } else {
        "mcp_cli_stub"
    };
    dir.push(name);
    assert!(
        dir.exists(),
        "the mcp_cli_stub fixture binary is missing at {}: run `cargo build -p yagura-daemon \
         --bin mcp_cli_stub` first",
        dir.display()
    );
    dir
}

/// The environment [`mcp_cli_stub_profile_source`] carries through the
/// profile's `[env]` table into `mcp_cli_stub`'s own `MCP_CLI_STUB_*`
/// variables (`src/bin/mcp_cli_stub.rs`'s module doc) -- the same path a
/// real registration child's environment is built through (base env +
/// profile `[env]`).
pub struct McpCliStubFixture<'a> {
    /// `MCP_CLI_STUB_STATE_DIR`: where the fixture keeps its own
    /// registration record, independent of the settings file below.
    pub state_dir: &'a Path,
    /// `MCP_CLI_STUB_TOML_TABLE_PATH`, when the row also edits a TOML
    /// settings file (the `[mcp_registration.toml_set]` table this source
    /// carries points at the same path).
    pub toml_table_path: Option<&'a str>,
    /// `MCP_CLI_STUB_ON_DUPLICATE`: `mcp add`'s duplicate-name behavior.
    pub on_duplicate: Option<&'a str>,
    /// `MCP_CLI_STUB_LOG`: where each invocation appends its argv and
    /// timing, for a case that must observe call order or concurrency.
    pub log_path: Option<&'a Path>,
    /// The home the fixture resolves a relative settings-file path against
    /// ([`stub_home_env`]), when it should not be the test process's own.
    pub home_dir: Option<&'a Path>,
}

/// A [`yagura_daemon::DaemonConfig::registration_cwd`] below `root`, for a
/// case whose registration commands must run.
pub fn registration_cwd_in(root: &Path) -> Option<OwnedEmptyDir> {
    Some(OwnedEmptyDir::new(
        root.to_path_buf(),
        root.join("registration-cwd"),
    ))
}

/// A home directory and [`registration_cwd_in`] below a fresh temporary
/// root: what the daemon needs to run `[mcp_registration]` and
/// `[host_paths]` commands at all.
pub fn registration_dirs(tag: &str) -> (PathBuf, Option<OwnedEmptyDir>) {
    let root = unique_temp_dir(tag);
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create home dir");
    (home, registration_cwd_in(&root))
}

/// The variables that make `home` the home directory a `mcp_cli_stub`
/// child resolves a relative settings-file path against, on every OS.
pub fn stub_home_env(home: &Path) -> [(&'static str, String); 2] {
    let home = home.to_str().expect("home is UTF-8").to_string();
    [("HOME", home.clone()), ("USERPROFILE", home)]
}

/// A TOML literal string (`'...'`), the same way
/// `yagura_test_support::process::profile_source` writes `[command]`
/// elements: content is taken verbatim, so a Windows path's backslashes
/// never need escaping.
fn toml_literal(value: &str) -> String {
    format!("'{value}'")
}

/// A `[env]` line naming `key` as a TOML basic string holding `value`
/// exactly: a literal string cannot carry control characters, which some
/// injected outputs contain.
fn env_line(key: &str, value: &str) -> String {
    format!("{key} = {}\n", toml_basic(value))
}

fn toml_basic(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The TOML source of a launch profile whose `[command]` runs the
/// `mcp_cli_stub` fixture and whose `[mcp_registration]` table exercises the
/// "host, JSON status, with a settings-file edit" shape --
/// `profile_mcp_registration.rs`'s `JSON_TABLE`, the shape most rows in the
/// shipped profiles take (a Codex-like CLI). `fixture` supplies the
/// environment the spawned `mcp_cli_stub` process reads its behavior from.
///
/// A case needing a different registration shape (lines status, a file
/// status, or a foreign/WSL row) builds its own source: this one covers only
/// the shape most callers of `mcp_cli_stub` need, not every table
/// `profile_mcp_registration.rs` exercises.
///
/// # Panics
///
/// Panics under the same conditions as [`mcp_cli_stub_path`].
pub fn mcp_cli_stub_profile_source(display_name: &str, fixture: &McpCliStubFixture<'_>) -> String {
    let stub_path = mcp_cli_stub_path();
    let stub_path = stub_path
        .to_str()
        .expect("mcp_cli_stub path must be valid UTF-8");
    let toml_set_file = fixture
        .toml_table_path
        .unwrap_or(".mcp-cli-stub/config.toml");

    let mut source = format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{}]\n\n",
        toml_literal(stub_path)
    );
    source.push_str(
        "[mcp_registration]\n\
         add = [\"mcp\", \"add\", \"{server}\", \"{env_args}\", \"--\", \"{bridge}\"]\n\
         remove = [\"mcp\", \"remove\", \"{server}\"]\n\
         env_arg = [\"--env\", \"{env}\"]\n\n\
         [mcp_registration.status]\n\
         args = [\"mcp\", \"get\", \"{server}\", \"--json\"]\n\
         format = \"json\"\n\
         command = \"/transport/command\"\n\
         args_pointer = \"/transport/args\"\n\
         absent = \"No MCP server named\"\n\n\
         [mcp_registration.toml_set]\n",
    );
    source.push_str(&format!("file = {}\n", toml_literal(toml_set_file)));
    source.push_str(
        "table = [\"mcp_servers\", \"{server}\"]\n\
         values = { tool_timeout_sec = 86400 }\n\
         confirm = { tool_timeout_sec = \"/tool_timeout_sec\" }\n",
    );

    let mut env_table = String::from("\n[env]\n");
    env_table.push_str(&env_line(
        "MCP_CLI_STUB_STATE_DIR",
        &fixture.state_dir.to_string_lossy(),
    ));
    if let Some(path) = fixture.toml_table_path {
        env_table.push_str(&env_line("MCP_CLI_STUB_TOML_TABLE_PATH", path));
    }
    if let Some(mode) = fixture.on_duplicate {
        env_table.push_str(&env_line("MCP_CLI_STUB_ON_DUPLICATE", mode));
    }
    if let Some(path) = fixture.log_path {
        env_table.push_str(&env_line("MCP_CLI_STUB_LOG", &path.to_string_lossy()));
    }
    if let Some(home) = fixture.home_dir {
        for (key, value) in stub_home_env(home) {
            env_table.push_str(&env_line(key, &value));
        }
    }
    source.push_str(&env_table);
    source
}

/// The `mcp_cli_stub` fixture's path as a TOML literal string, for a
/// profile whose `[host_paths]` or `files` argv runs the fixture directly
/// (those argv are standalone, not appended to the launch argv).
///
/// # Panics
///
/// Panics under the same conditions as [`mcp_cli_stub_path`].
pub fn mcp_cli_stub_toml_literal() -> String {
    let stub_path = mcp_cli_stub_path();
    toml_literal(
        stub_path
            .to_str()
            .expect("mcp_cli_stub path must be valid UTF-8"),
    )
}

/// The TOML source of a launch profile whose `[command]` runs the
/// `mcp_cli_stub` fixture, carrying `registration_table` verbatim (any
/// registration shape: lines, a file status, a probed foreign row) and an
/// `[env]` table built from `env` (omitted when `env` is empty).
/// `preamble` is inserted among the top-level keys, before `[command]`
/// (for example `"enabled = false\n"`).
///
/// # Panics
///
/// Panics under the same conditions as [`mcp_cli_stub_path`].
pub fn mcp_cli_stub_profile_with_table(
    display_name: &str,
    preamble: &str,
    registration_table: &str,
    env: &[(&str, &str)],
) -> String {
    let mut source = format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n{preamble}\n[command]\ndefault = \
         [{}]\n\n",
        mcp_cli_stub_toml_literal()
    );
    source.push_str(registration_table);
    if !env.is_empty() {
        source.push_str("\n[env]\n");
        for (key, value) in env {
            source.push_str(&env_line(key, value));
        }
    }
    source
}

pub mod detection_rules;
pub mod docs;
pub mod real_daemon;
pub mod store_fixtures;
pub mod views;

// --- reading submit_child's composer ------------------------------

/// The composer region `submit_child` paints in composer-draw mode.
pub fn submit_child_composer_section() -> ComposerSection {
    ComposerSection {
        marker: regex::Regex::new(yagura_test_support::process::COMPOSER_MARKER)
            .expect("the shared composer marker must compile as a regex"),
        separator: yagura_test_support::process::COMPOSER_SEPARATOR.to_string(),
        continuation_prefix: yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        blank_lines_in_body: false,
        left_panel: None,
        right_border: None,
        placeholder: None,
    }
}

/// The composer `submit_child` shows in `window_id` right now, read off the
/// window's model: a direct read of what reached the child, independent of
/// any judgment under test.
pub async fn read_composer_now(h: &Harness, window_id: &str) -> ComposerReadout {
    let session = h
        .registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    session
        .read_screen(|screen| screen.read_composer(&submit_child_composer_section()))
        .await
        .unwrap()
}

/// Reads [`read_composer_now`] until it equals `expected`, failing once
/// `timeout` passes: a case waits for what the child shows, not for a delay.
pub async fn wait_until_composer_is(
    h: &Harness,
    window_id: &str,
    expected: &ComposerReadout,
    timeout: Duration,
) {
    wait_until_composer_satisfies(h, window_id, &format!("{expected:?}"), timeout, |readout| {
        readout == expected
    })
    .await;
}

/// Reads [`read_composer_now`] until `accept` holds and returns that readout,
/// failing with `what` once `timeout` passes. For a case that knows only part
/// of what the child will show: any other readout on the way is not the end.
pub async fn wait_until_composer_satisfies(
    h: &Harness,
    window_id: &str,
    what: &str,
    timeout: Duration,
    accept: impl Fn(&ComposerReadout) -> bool,
) -> ComposerReadout {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let readout = read_composer_now(h, window_id).await;
        if accept(&readout) {
            return readout;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("composer for {window_id} never reached {what}, last saw {readout:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until `window_id`'s model holds a frame `accept` takes, given the
/// composer `submit_child` shows in it and whether the program is still
/// painting it (the cursor hidden or a synchronized update open, which no
/// write is decided on). Woken by the model's own output arrivals, never by a
/// delay; `timeout` only calls it hung.
pub async fn wait_until_frame_satisfies(
    h: &Harness,
    window_id: &str,
    what: &str,
    timeout: Duration,
    accept: impl Fn(&ComposerReadout, bool) -> bool,
) -> ComposerReadout {
    let session = h
        .registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    let mut arrivals = session.output_arrivals();
    let mut last = None;
    let found = tokio::time::timeout(timeout, async {
        loop {
            // Marked seen before the read, so output fed after it wakes the
            // wait below.
            arrivals.borrow_and_update();
            let (readout, unfinished) = session
                .read_screen(|screen| {
                    (
                        screen.read_composer(&submit_child_composer_section()),
                        screen.cursor_hidden() || screen.sync_open(),
                    )
                })
                .await
                .unwrap();
            if accept(&readout, unfinished) {
                return readout;
            }
            last = Some((readout, unfinished));
            arrivals
                .changed()
                .await
                .expect("the window's output arrivals stay open");
        }
    })
    .await;
    found.unwrap_or_else(|_| {
        panic!(
            "the model of {window_id} never held {what}; last saw (composer, unfinished) {last:?}"
        )
    })
}

// --- what a hex_echo child received -------------------------------------------

/// `bytes` as lowercase hex, the form `hex_echo` prints what it reads.
pub fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Waits until `window_id`'s model, not just its raw output, holds `marker`: the
/// modes the child set before printing it are then the model's modes.
pub async fn wait_until_model_shows(
    h: &Harness,
    window_id: &str,
    marker: &[u8],
    timeout: Duration,
) {
    let marker = String::from_utf8_lossy(marker).into_owned();
    let session = h
        .registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some((text, _)) = session.text_tail(200, 64 * 1024).await
            && text.contains(&marker)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's model never showed {marker:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What `window_id`'s child wrote after the first `marker`.
pub fn output_after_marker(h: &Harness, window_id: &str, marker: &[u8]) -> Vec<u8> {
    let output = h.captured_output(window_id);
    let start = output
        .windows(marker.len())
        .position(|window| window == marker)
        .map_or(0, |at| at + marker.len());
    output[start..].to_vec()
}

/// Waits until a `hex_echo` child prints `expected` (as hex) after `marker`, failing
/// with what it did print.
pub async fn assert_hex_echo_receives(
    h: &Harness,
    window_id: &str,
    marker: &[u8],
    expected: &[u8],
    timeout: Duration,
) {
    let needle = hex_of(expected);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let received = output_after_marker(h, window_id, marker);
        if contains_subsequence(&received, needle.as_bytes()) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never received {:?} (hex {needle}); it printed after the marker: {:?}",
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(&received)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- clock bounds --------------------------------------------------------------

/// Lets an hour of the test's clock pass while nothing else moves, then goes
/// back to the system clock. A wait that ends because a clock ran out does not
/// survive this; a wait that ends on an event does. Call it only once everything
/// the child will do has been seen, so no event is still on its way: the paused
/// clock does not wait for one. The clock moves by itself whenever the runtime
/// is idle, which a thread the runtime waits on (a held model) prevents: use
/// [`let_every_clock_bound_run_out_while_a_thread_is_awaited`] there.
pub async fn let_every_clock_bound_run_out() {
    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    tokio::time::resume();
}

/// [`let_every_clock_bound_run_out`] for a wait that is blocked on a thread the
/// test itself holds, where the clock cannot move by itself: it is moved by
/// hand, after the waiting call has started and registered what it waits on.
/// A bound set once an earlier one has run out is a new timer, so the clock goes
/// on for as many rounds as a chain of such bounds could need.
pub async fn let_every_clock_bound_run_out_while_a_thread_is_awaited() {
    tokio::time::pause();
    for _ in 0..16 {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3600)).await;
    }
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

// --- requester-departure helpers: a profile-addressed handoff whose
// requester leaves gets no answer, so these stand in for one -------------

/// The id of the one window a fresh harness ends up holding: the one a
/// profile-addressed handoff opens.
pub async fn wait_for_only_window(h: &Harness, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(window) = h.registry.list().into_iter().next() {
            return window.window_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the handoff never opened its window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A `submit_child` profile in composer-draw mode that appends every byte it
/// reads to `input_log`. A profile's argv admits no empty element, so `-`
/// stands for "none" in the folding, overlay and heading slots.
pub fn submit_child_profile_source_logging_input(
    display_name: &str,
    marker: &str,
    detection: &str,
    clear_scope: &str,
    input_log: &Path,
) -> String {
    let mut command =
        yagura_test_support::process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    command.push("-".to_string());
    command.push(clear_scope.to_string());
    command.push("-".to_string());
    command.push("-".to_string());
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    with_detection_name(
        &yagura_test_support::process::profile_source(display_name, &command),
        detection,
    )
}

/// Returns once a profile-addressed `send_handoff` of `probe_profile`, sent on
/// a fresh connection, is no longer refused for the daemon-wide handoff budget
/// and has been answered. Under `max_open_handoffs = 1` that is the moment the
/// handoff holding the budget gave it back, which it does only once it has
/// decided whether to close the window it opened -- the answer its departed
/// requester can no longer read. The hang guard only bounds a failure.
pub async fn wait_until_the_handoff_budget_is_given_back(
    h: &Harness,
    probe_profile: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the handoff never gave its budget back after its requester left"
        );
        let mut probe = h.connect().await;
        send_handoff_from_profile(
            &mut probe,
            probe_profile,
            None,
            None,
            "src/probe.rs",
            "probe",
        )
        .await;
        match read_response(&mut probe, remaining).await {
            DaemonMessage::Error { code, .. } if code == "resource_exhausted" => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            DaemonMessage::HandoffSent { .. } => return,
            other => panic!("expected the budget probe to be refused or delivered, got {other:?}"),
        }
    }
}

/// Waits until the child's input log holds `needle`, and returns the log.
/// Written into the window after everything a case wants to inspect, a
/// sentinel read back this way proves the child has read all of it.
pub async fn wait_until_input_log_contains(
    log: &Path,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let read = std::fs::read(log).unwrap_or_default();
        if read.windows(needle.len()).any(|window| window == needle) {
            return read;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never read {needle:?}; it read {read:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Everything the child read up to now, from its input log. The log is filled
/// as the child reads, behind the daemon's writes, so it is read only once a
/// sentinel written after them has come back through it.
pub async fn input_log_read_so_far(
    entry: &yagura_daemon::window::WindowEntry,
    log: &Path,
    timeout: Duration,
) -> Vec<u8> {
    const SENTINEL: &[u8] = b"SENTINEL-AFTER";
    entry
        .write_pty_input(SENTINEL, yagura_daemon::window::InputOrigin::Interactive)
        .await
        .expect("the sentinel must not fail");
    let mut read = wait_until_input_log_contains(log, SENTINEL, timeout).await;
    let at = read
        .windows(SENTINEL.len())
        .position(|window| window == SENTINEL)
        .expect("the log was returned holding the sentinel");
    read.truncate(at);
    read
}

// --- a `submit_child` window whose screen can show an approval dialog ---------

/// The first line of the fixture's approval dialog, matched by
/// [`write_gate_definition`]'s approval rule.
pub const APPROVAL_HEADING: &str = "GATE-APPROVAL";

/// The status line of the fixture's `working` overlay, matched by
/// [`write_gate_definition`]'s running rule.
pub const WORKING_HEADING: &str = "GATE-WORKING";

/// Ctrl-U, as a TOML basic-string body.
pub const CLEAR_CTRL_U: &str = "\\u0015";

/// A rules-only definition for `submit_child`: approval over working over
/// input, so a drawn dialog wins even while the startup marker line is still
/// on screen, plus the shared composer section.
pub fn write_gate_definition(
    dir: &Path,
    name: &str,
    marker: &str,
    clear: &str,
    submit_delay_ms: u64,
) {
    let toml = format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = 5

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_approval"
pattern = '{APPROVAL_HEADING}'
priority = 30
region = "bottom"

[[rules]]
status = "running"
pattern = '(SUBMITTED)|({WORKING_HEADING})'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{marker}'
priority = 10
region = "bottom"

[input]
bracketed_paste = true
ready_hold_ms = 0
submit_delay_ms = {submit_delay_ms}
clear = "{clear}"
delivery_confirm_ms = 2000
resend_max = 2
probe = "{SUBMIT_CHILD_PROBE}"
"#
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write detection definition file");
}

/// How one gate window is launched.
pub struct GateWindow<'a> {
    pub name: &'a str,
    pub marker: &'a str,
    /// `submit_child`'s overlay argument; empty for none.
    pub overlay: &'a str,
    pub heading: &'a str,
    pub clear: &'a str,
    /// `submit_child`'s `clear_scope` argument; empty for the ordinary one.
    pub clear_scope: &'a str,
    pub reflect_delay_ms: u64,
    /// A file the child appends every byte it reads to; none when unset.
    pub input_log: Option<&'a Path>,
    /// The definition's `submit_delay_ms`: how long a delivery sleeps after
    /// the body, before it goes on to the probe.
    pub submit_delay_ms: u64,
    /// `submit_child`'s `fold_placeholder` argument; empty for no folding.
    pub fold_placeholder: &'a str,
}

impl<'a> GateWindow<'a> {
    pub fn new(name: &'a str, marker: &'a str, overlay: &'a str, heading: &'a str) -> Self {
        GateWindow {
            name,
            marker,
            overlay,
            heading,
            clear: CLEAR_CTRL_U,
            clear_scope: "",
            reflect_delay_ms: 0,
            input_log: None,
            submit_delay_ms: 120,
            fold_placeholder: "",
        }
    }
}

/// Opens `submit_child` in composer-draw mode with the overlay argument and
/// observes the window reach `waiting_input` before returning.
pub async fn open_gate_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    window: GateWindow<'_>,
) -> String {
    write_gate_definition(
        dir,
        window.name,
        window.marker,
        window.clear,
        window.submit_delay_ms,
    );
    let mut command = process::submit_child_command(window.marker, 0, 0, 0);
    command.push(process::COMPOSER_MARKER.to_string());
    command.push(process::COMPOSER_SEPARATOR.to_string());
    command.push(process::COMPOSER_CONTINUATION_PREFIX.to_string());
    command.push(window.reflect_delay_ms.to_string());
    command.push(window.fold_placeholder.to_string());
    command.push(window.clear_scope.to_string());
    command.push(window.overlay.to_string());
    command.push(window.heading.to_string());
    if let Some(log) = window.input_log {
        command.push(
            log.to_str()
                .expect("the input log path must be valid UTF-8")
                .to_string(),
        );
    }
    let window_id = h.open_delivering(command, Some(window.name)).await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

// --- shared by the foreign-write gate suites ---------------------------------

/// The longest any single step may take before the case is called hung.
pub const HANG_GUARD: Duration = Duration::from_secs(60);

pub const BODY: &str = "BODY";
pub const CLEAR: u8 = 0x15;
pub const CTRL_C: u8 = 0x03;
pub const ERASE: u8 = 0x7f;
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Written last; read back, it proves the child has read everything before
/// it. `M` then an erase leaves the composer as it stood.
pub const MARKER: &[u8] = b"M\x7f";

pub fn gate_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        ..approver_base_config()
    }
}

pub fn window_entry(h: &Harness, window_id: &str) -> Arc<yagura_daemon::window::WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// What the child read after the body's paste, up to the marker.
pub fn after_the_body(read: &[u8]) -> &[u8] {
    let end = read
        .windows(PASTE_END.len())
        .position(|w| w == PASTE_END)
        .unwrap_or_else(|| panic!("the body's paste must have reached the child: {read:?}"));
    &read[end + PASTE_END.len()..]
}

/// Splits `after_body`, which must end with `foreign`, into what came before
/// the run of clear bytes right in front of `foreign`, and that run's length.
pub fn take_back_in_front_of<'a>(after_body: &'a [u8], foreign: &[u8]) -> (&'a [u8], usize) {
    assert!(
        after_body.ends_with(foreign),
        "the other writer's bytes must be the last the child read before the marker -- the \
         delivery writes nothing after them; after the body it read {after_body:?}"
    );
    let before = &after_body[..after_body.len() - foreign.len()];
    let run = before.iter().rev().take_while(|&&b| b == CLEAR).count();
    (&before[..before.len() - run], run)
}

// --- inbox / delivery-door fixtures -------------------------------------------

impl Harness {
    /// [`Harness::start`] bound to the endpoint a `yagura-daemon` child resolves from
    /// `YAGURA_INSTANCE=instance`, so a hook command run as a real process reaches this
    /// in-process daemon.
    pub async fn start_on_instance(instance: &str, config: DaemonConfig) -> Self {
        let user = yagura_os::PeerAuth::current_user_id(&yagura_os::PlatformIpc::default())
            .expect("the current user id must resolve");
        let namespace = yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(instance)))
            .expect("the instance identifier must be valid");
        let endpoint = yagura_os::default_endpoint_owning(&user, &namespace);
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let server = Arc::new(IpcServer::new(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
        ));
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
        });
        Self {
            registry,
            endpoint,
            delivery: config.delivery.clone(),
            _server: handle,
        }
    }
}

/// Sends one inbox door message and reads its single reply.
pub async fn call_door(client: &mut Connection, msg: &ClientMessage) -> DaemonMessage {
    send(client, msg).await;
    read_response(client, Duration::from_secs(10)).await
}

/// Unwraps a `delivery` reply into (output, stream, exit code).
pub fn expect_delivery(msg: DaemonMessage) -> (String, yagura_protocol::DeliveryStream, u8) {
    match msg {
        DaemonMessage::Delivery {
            output,
            stream,
            exit_code,
        } => (output, stream, exit_code),
        other => panic!("expected delivery, got {other:?}"),
    }
}
