//! The operating-system abstraction boundary of yagura.
//!
//! Every other crate in the workspace reaches the operating system through
//! the operating-system layer rather than the platform APIs directly: this
//! crate, and `yagura-locale` for the locale queries only the daemon makes.
//!
//! The platform implementations live in
//! `platform_windows` / `platform_unix` (each compiled only under its own
//! `cfg`, so neither name resolves as an intra-doc link from here) and are
//! selected statically at compile time; callers name them through the
//! [`PlatformIpc`] and [`ProcessGroup`] aliases and never through a `cfg` of
//! their own.

use std::ffi::OsStr;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

pub mod bundled_conpty;
mod bundled_conpty_pins;
pub mod capture;
pub mod file_replace;
// Only Unix escalates; Windows keeps the module for its tests alone.
#[cfg(any(not(windows), test))]
mod force_kill;
pub mod hook_path;
pub mod instance;
pub mod login_shell;
pub mod mcp_bridge;
#[cfg(not(windows))]
pub mod platform_unix;
#[cfg(windows)]
pub mod platform_windows;
mod process_identity;
pub mod program;
pub mod pty;
mod reap_gate;
#[cfg(windows)]
pub mod webview;
pub mod window_lineage;

pub use capture::{
    CaptureEnv, CaptureLimits, CaptureOutcome, CapturedChild, MarkerOutcome, spawn_captured,
};
pub use file_replace::replace_existing_file;
pub use hook_path::{SpacelessPathError, spaceless_path, spaceless_path_from};
pub use instance::{
    APP_IDENTIFIER, InstanceIdError, InstanceNamespace, StartupEnvError, guest_instance_dir,
    instance_dir, resolve_instance_from, resolve_startup_env, store_override_conflicts,
    validate_instance_id,
};
pub use mcp_bridge::{merge_wslenv, resolve_sibling_executable, to_plain_path};
pub use process_identity::{
    ExitWatch, Liveness, ProcessHandle, ProcessStart, process_liveness, process_start,
};
pub use reap_gate::ReapGate;

/// Failure of an operating-system operation behind this boundary.
#[derive(Debug, thiserror::Error)]
pub enum OsError {
    /// The platform in use does not implement `feature`. Callers are expected
    /// to fail closed rather than fall back to a weaker mechanism.
    #[error("{feature} is not supported on this platform")]
    Unsupported {
        /// Name of the operation that is not available.
        feature: &'static str,
    },
    /// An I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A security operation (descriptor, token, identifier) failed.
    #[error("security operation failed: {0}")]
    Security(String),
    /// A live process holds the endpoint's lock, which only a bound listener
    /// takes: the endpoint is served by a daemon, however long it takes to answer.
    #[error("{0}")]
    EndpointHeld(String),
}

/// Identity of the endpoint owner and of a connected peer.
///
/// The endpoint is locked down at creation time (`endpoint_security`) *and*
/// every accepted peer is checked afterwards ([`PeerAuth::peer_id`] +
/// [`is_authorized_peer`]), so a platform whose creation-time protection is
/// weaker than expected still cannot serve an unauthorized peer.
pub trait PeerAuth: Send + Sync + 'static {
    /// Platform representation of the protection applied to an endpoint when
    /// it is created.
    type Security: Send + Sync + 'static;
    /// An accepted server-side connection.
    type Connection: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Builds the protection that limits an endpoint to the current user.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when the platform refuses to report the
    /// current user, and [`OsError::Unsupported`] where the platform branch is
    /// not implemented.
    fn endpoint_security(&self) -> Result<Self::Security, OsError>;

    /// Returns the identifier of the user this process runs as.
    ///
    /// # Errors
    ///
    /// As for [`PeerAuth::endpoint_security`].
    fn current_user_id(&self) -> Result<String, OsError>;

    /// Returns the identifier of the user owning the peer of `conn`, or `None`
    /// when it cannot be determined.
    fn peer_id(&self, conn: &Self::Connection) -> Option<String>;

    /// Returns the process id the operating system recorded for the peer of
    /// `conn` when it connected, or `None` when it cannot be determined.
    fn peer_process_id(&self, conn: &Self::Connection) -> Option<u32>;

    /// Returns the path of the executable image backing the peer of `conn`,
    /// or `None` when it cannot be determined.
    ///
    /// The path is reported as the platform gives it and is *not*
    /// canonicalized here: the caller compares it against a configured path
    /// and owns the normalization of both sides, so that normalization
    /// happens once per value rather than once per comparison.
    fn peer_executable(&self, conn: &Self::Connection) -> Option<PathBuf>;

    /// Reports whether the *existing* endpoint named `endpoint` grants access
    /// to `user_id` and to nobody else.
    ///
    /// The answer is read back from the operating system rather than from
    /// in-process bookkeeping, so it also detects an endpoint that some other
    /// code path created without protection.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Security`] when the endpoint cannot be opened or its
    /// protection cannot be read.
    fn endpoint_allows_only(&self, endpoint: &str, user_id: &str) -> Result<bool, OsError>;
}

/// Reports whether a peer whose user identifier is `peer` may be served by
/// an endpoint owned by `owner`. Fail-closed: an absent peer identifier is
/// never authorized.
///
/// A free function rather than a [`PeerAuth`] method, so no platform
/// implementation can override the rule.
#[must_use]
pub fn is_authorized_peer(peer: Option<&str>, owner: &str) -> bool {
    matches!(peer, Some(id) if id == owner)
}

/// Fails unless the process serving `endpoint` runs as `own`, the user of
/// this process. Fail-closed like [`is_authorized_peer`]: a server whose user
/// cannot be determined is refused.
///
/// Endpoint names are predictable and, for Windows named pipes, live in a
/// namespace every local user can create in, so another user can claim the
/// name while no daemon holds it. Checking the server from the client side is
/// what keeps a client from talking to such a claimant.
///
/// # Errors
///
/// Returns [`OsError::Security`] when `server` is absent or differs from
/// `own`.
fn require_own_server(endpoint: &str, server: Option<&str>, own: &str) -> Result<(), OsError> {
    if is_authorized_peer(server, own) {
        Ok(())
    } else {
        Err(OsError::Security(format!(
            "the IPC endpoint {endpoint} is not served by this user's daemon; refusing to connect"
        )))
    }
}

/// The local IPC transport between the daemon and its clients.
pub trait IpcTransport: PeerAuth + Default {
    /// A bound endpoint that owns its name.
    type Listener: Send + 'static;
    /// A client-side connection.
    type ClientConnection: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Claims the endpoint name and returns a listener that owns it.
    /// A failure means the name is already taken (create-first).
    ///
    /// `security` is consumed, so an endpoint can never be created without
    /// deciding who may reach it.
    fn bind(
        &self,
        endpoint: &str,
        security: Self::Security,
    ) -> impl Future<Output = Result<Self::Listener, OsError>> + Send;

    /// Waits for the next peer and returns the connection serving it.
    ///
    /// Cancel-safe: usable as a `select!` branch without losing the pending
    /// instance -- dropping the returned future leaves `listener` able to
    /// accept the same peer on the next call.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Io`] when the platform cannot provide an instance to
    /// wait on.
    fn accept(
        &self,
        listener: &mut Self::Listener,
    ) -> impl Future<Output = Result<Self::Connection, OsError>> + Send;

    /// Connects to an endpoint claimed by a daemon running as the current
    /// user.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Io`] when no endpoint of that name is currently
    /// listening, and [`OsError::Security`] when the process serving it runs
    /// as another user or its user cannot be determined.
    fn connect(
        &self,
        endpoint: &str,
    ) -> impl Future<Output = Result<Self::ClientConnection, OsError>> + Send;

    /// Returns the process id of the process serving the endpoint `conn` is
    /// connected to, as the operating system reports it, or `None` when it
    /// cannot be determined.
    fn server_process_id(&self, conn: &Self::ClientConnection) -> Option<u32>;
}

/// Outcome of a detached daemon spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachedSpawn {
    /// `true` when the platform refused the strongest detachment mode and a
    /// weaker fallback was used. On Windows this means
    /// `CREATE_BREAKAWAY_FROM_JOB` was refused, so the daemon may be
    /// terminated together with the caller.
    ///
    /// Callers that have no log of their own (the GUI) are expected to surface
    /// this to the user.
    pub used_fallback: bool,
}

/// The longest a daemon spends reading its login shell's environment at
/// start-up, before its endpoint is bound.
///
/// This is part of the start-up contract between the daemon and the GUI that
/// starts it: right after spawning a daemon, the GUI keeps trying to connect
/// for at least this much longer than a later reconnection does, since the
/// endpoint cannot be reached until the read has finished or timed out.
pub const LOGIN_ENV_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawns the daemon detached from the caller, falling back to a weaker mode
/// when the platform refuses the strongest one.
///
/// `args` is passed to the daemon process as an argument vector, never
/// through a shell command line (CWE-78).
///
/// The known limitation of that fallback is described on
/// [`DetachedSpawn::used_fallback`]: the daemon then shares the caller's
/// lifetime instead of surviving it.
///
/// # Errors
///
/// Returns [`OsError::Io`] when even the weaker mode fails to start `path`,
/// and [`OsError::Unsupported`] where the platform branch is not implemented.
pub fn spawn_daemon_detached(path: &Path, args: &[&OsStr]) -> Result<DetachedSpawn, OsError> {
    #[cfg(windows)]
    {
        platform_windows::spawn_daemon_detached(path, args)
    }
    #[cfg(not(windows))]
    {
        platform_unix::spawn_daemon_detached(path, args)
    }
}

/// Where the standard error of a process started by
/// [`spawn_detached_process`] goes.
#[derive(Debug)]
pub enum DetachedStderr {
    /// This process's own standard error (the null device when it has none).
    Inherit,
    /// The null device.
    Null,
    /// An open file, such as a log opened for appending.
    File(std::fs::File),
}

/// A process started by [`spawn_detached_process`]: its process id, the
/// caller's ends of its standard input and output, and the means to wait for
/// and to end it.
#[derive(Debug)]
pub struct DetachedProcess {
    #[cfg(windows)]
    child: platform_windows::ChildProcess,
    #[cfg(not(windows))]
    child: std::process::Child,
    pid: u32,
    used_fallback: bool,
    stdin: Option<std::fs::File>,
    stdout: Option<std::fs::File>,
}

impl DetachedProcess {
    #[cfg(windows)]
    pub(crate) fn new(
        child: platform_windows::ChildProcess,
        used_fallback: bool,
        stdin: std::fs::File,
        stdout: std::fs::File,
    ) -> Self {
        let pid = child.id();
        Self {
            child,
            pid,
            used_fallback,
            stdin: Some(stdin),
            stdout: Some(stdout),
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn new(
        child: std::process::Child,
        stdin: Option<std::fs::File>,
        stdout: Option<std::fs::File>,
    ) -> Self {
        let pid = child.id();
        Self {
            child,
            pid,
            used_fallback: false,
            stdin,
            stdout,
        }
    }

    /// The process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Whether the strongest detachment was refused, as for
    /// [`DetachedSpawn::used_fallback`]: on Windows the process then stays in
    /// the caller's job and ends with it.
    #[must_use]
    pub fn used_fallback(&self) -> bool {
        self.used_fallback
    }

    /// The writing end of the process's standard input; `None` once taken.
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.stdin.take()
    }

    /// The reading end of the process's standard output; `None` once taken.
    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.stdout.take()
    }

    /// Blocks until the process has ended and returns its exit code; `None`
    /// on Unix when a signal ended it.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait fails.
    pub fn wait(&mut self) -> std::io::Result<Option<i32>> {
        #[cfg(windows)]
        {
            self.child
                .wait()
                .map(|code| Some(i32::from_ne_bytes(code.to_ne_bytes())))
        }
        #[cfg(not(windows))]
        {
            self.child.wait().map(|status| status.code())
        }
    }

    /// Ends the process at once. A process that has already ended is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a running process cannot be
    /// ended.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

/// Starts `path` with `args`, detached from the caller as
/// [`spawn_daemon_detached`] starts the daemon (Windows: no console and, when
/// allowed, outside the caller's job; Unix: a new session), with pipes as its
/// standard input and output, `stderr` as its standard error, and no other
/// handle inherited. It runs in `cwd` and the caller's environment.
///
/// `args` is passed as an argument vector, never through a shell (CWE-78).
///
/// # Errors
///
/// Returns [`OsError::Io`] when the pipes cannot be made or `path` cannot be
/// started even in the weaker mode.
pub fn spawn_detached_process(
    path: &Path,
    args: &[&OsStr],
    cwd: Option<&Path>,
    stderr: DetachedStderr,
) -> Result<DetachedProcess, OsError> {
    #[cfg(windows)]
    {
        platform_windows::spawn_detached_process(path, args, cwd, &stderr)
    }
    #[cfg(not(windows))]
    {
        platform_unix::spawn_detached_process(path, args, cwd, stderr)
    }
}

/// Asks for `pid` to be force-terminated once `grace` has elapsed, unless
/// `gate` has seen the child reaped by then.
///
/// Returns immediately; any waiting happens on a thread of this function's
/// own, so a caller that must not block (a `Drop` implementation, a
/// synchronous close handler) can still escalate a kill it just sent.
///
/// The grace is waited out outside `gate`'s lock, and the force-terminate is
/// sent under it only if the child has not been reaped, so a recycled process
/// id is never hit. `gate` must be the one the child is reaped through.
///
/// # Platform-specific behavior
///
/// **Windows: this does nothing, and callers must never expect it to force a
/// process to terminate there.** The kill it would escalate from is
/// `TerminateProcess`, which is immediate and cannot be refused, so nothing is
/// left to escalate to.
///
/// **Unix**: sends `SIGKILL` after `grace`. This escalates from the `SIGHUP`
/// that a caller's own kill sends, which a child that installed a handler for
/// it (or ignores it) can survive.
///
/// There is no error path: the outcome of the signal is not reported. Callers
/// confirm termination through their own exit observation.
pub fn schedule_force_kill(pid: u32, grace: Duration, gate: Arc<ReapGate>) {
    #[cfg(windows)]
    {
        platform_windows::schedule_force_kill(pid, grace, gate);
    }
    #[cfg(not(windows))]
    {
        platform_unix::schedule_force_kill(pid, grace, gate);
    }
}

/// Puts the terminal on this process's standard input into raw mode: the
/// input a full-screen terminal program needs, where every byte the other end
/// writes arrives unmodified rather than being echoed back, buffered until a
/// line is complete, or turned into a signal.
///
/// Windows clears `ENABLE_LINE_INPUT` / `ENABLE_ECHO_INPUT` /
/// `ENABLE_PROCESSED_INPUT` and sets `ENABLE_VIRTUAL_TERMINAL_INPUT`; Unix
/// applies `cfmakeraw`'s settings except for `OPOST`, which stays on so that
/// a newline written by this process still reaches the terminal as CR LF.
/// Both branches leave the output side otherwise untouched, and both report
/// success without changing anything when standard input is not a terminal
/// (a pipe, a file), since there is no line discipline in front of those.
///
/// The mode is not restored on exit: a process that raw-modes its own
/// standard input is expected to own that terminal for its whole lifetime.
///
/// # Errors
///
/// Returns [`OsError::Security`] on Windows when the standard input handle
/// cannot be obtained or the new console mode is rejected, and
/// [`OsError::Io`] on Unix when the terminal attributes cannot be read or the
/// new ones cannot be applied.
pub fn enter_raw_console_input() -> Result<(), OsError> {
    #[cfg(windows)]
    {
        platform_windows::enter_raw_console_input()
    }
    #[cfg(not(windows))]
    {
        platform_unix::enter_raw_console_input()
    }
}

/// Fills `buf` with cryptographically secure random bytes.
///
/// Cross-platform: this is **not** part of the OS abstraction boundary and has
/// no per-platform stub, because `getrandom` already absorbs
/// the platform difference.
///
/// # Errors
///
/// Returns [`OsError::Security`] when the platform RNG is unavailable. `buf`
/// is then unspecified and must not be used: callers are expected to treat
/// that as "no randomness available" and fail closed rather than fall back to
/// a weaker source.
pub fn fill_random(buf: &mut [u8]) -> Result<(), OsError> {
    getrandom::fill(buf).map_err(|error| OsError::Security(error.to_string()))
}

// Env input seams + wrappers: the `*_from` seams below take every
// environment-derived value as an argument instead of reading it, so they can
// be tested without mutating the process environment; only the `default_*`
// wrappers read the environment.

/// Builds the default IPC endpoint name for `user_id` and `namespace`
/// without creating anything: the gui / mcp client path.
///
/// `\\.\pipe\{namespace}-{user_id}` on Windows, which is
/// `\\.\pipe\yagura-{user_id}` for the default instance. Pure string
/// operation; the identifier itself must be obtained by the caller through
/// [`PeerAuth::current_user_id`].
#[cfg(windows)]
#[must_use]
pub fn default_endpoint(user_id: &str, namespace: &InstanceNamespace) -> String {
    format!(r"\\.\pipe\{}-{user_id}", namespace.name())
}

/// Unix branch of [`default_endpoint`]: resolves through
/// [`platform_unix::resolve_endpoint_with`] with `XDG_RUNTIME_DIR` and the OS
/// temp directory read from the environment and
/// `create_missing = `[`platform_unix::EndpointCreation::ReadOnly`]. A
/// resolution failure (most commonly: the daemon has not started yet, so its
/// runtime directory does not exist) falls back to the same unverified
/// stage-3 candidate path the seam itself would use, so the connection
/// attempt that follows fails the same way it would against an absent
/// daemon, rather than this function needing its own error type.
#[cfg(not(windows))]
#[must_use]
pub fn default_endpoint(user_id: &str, namespace: &InstanceNamespace) -> String {
    resolve_endpoint_env(
        user_id,
        platform_unix::EndpointCreation::ReadOnly,
        namespace,
    )
}

/// Owning counterpart of [`default_endpoint`], used only by the daemon's
/// bind path (`create_missing = platform_unix::EndpointCreation::CreateIfMissing`,
/// not an intra-doc link here: that type is compiled only under
/// `#[cfg(not(windows))]`, and this doc comment is attached to the
/// `#[cfg(windows)]` branch): on Unix this may create the stage-3 fallback
/// runtime directory. On Windows it is identical to
/// [`default_endpoint`] (pipe names need no directory to be created).
#[cfg(windows)]
#[must_use]
pub fn default_endpoint_owning(user_id: &str, namespace: &InstanceNamespace) -> String {
    default_endpoint(user_id, namespace)
}

/// Unix branch of [`default_endpoint_owning`].
#[cfg(not(windows))]
#[must_use]
pub fn default_endpoint_owning(user_id: &str, namespace: &InstanceNamespace) -> String {
    resolve_endpoint_env(
        user_id,
        platform_unix::EndpointCreation::CreateIfMissing,
        namespace,
    )
}

/// The endpoint the holder of window `window_id` listens on, for `user_id` in
/// `namespace`, with its holders in `holders_dir`:
/// `\\.\pipe\{namespace}-{user_id}-holder-{dir_id}-{window_id}` on Windows, and
/// `{holders_dir}/{window_id}.sock` on Unix, moved to a fixed-length name in a
/// short owner-only directory under the OS temp directory when it would not
/// fit in `sockaddr_un.sun_path`.
///
/// `dir_id` and the fixed-length name are derived from `holders_dir` as
/// spelled (no file system access), so holders of the same window in two
/// directories never share an endpoint, and one spelling of a directory
/// always gives the same endpoint.
///
/// # Errors
///
/// Returns [`OsError::Security`] when `window_id` is not exactly one path
/// component (empty, `.`, `..`, or holding a separator or NUL), and on Unix
/// the errors of the fallback directory. Which ids are valid is the caller's
/// rule; this only keeps the endpoint inside its directory or pipe namespace.
pub fn holder_endpoint(
    user_id: &str,
    namespace: &InstanceNamespace,
    holders_dir: &Path,
    window_id: &str,
) -> Result<String, OsError> {
    // A NUL would cut the name short where the OS reads it, aliasing another one.
    if !is_single_component(window_id) || window_id.contains('\0') {
        return Err(OsError::Security(
            "a window id is not a single path component".to_owned(),
        ));
    }
    #[cfg(windows)]
    {
        Ok(format!(
            r"\\.\pipe\{}-{user_id}-holder-{}-{window_id}",
            namespace.name(),
            short_hash(&[holders_dir.as_os_str().as_encoded_bytes()])
        ))
    }
    #[cfg(not(windows))]
    {
        platform_unix::holder_socket_path(
            user_id,
            namespace,
            holders_dir,
            &std::env::temp_dir(),
            window_id,
        )
    }
}

/// FNV-1a 64 of `parts` joined by NUL, as 16 hex digits: stable across builds,
/// since an endpoint is found again through the name a holder recorded. None
/// of the parts holds a NUL, so different parts never join to the same bytes.
pub(crate) fn short_hash(parts: &[&[u8]]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (index, part) in parts.iter().enumerate() {
        let separator: &[u8] = if index == 0 { &[] } else { &[0] };
        for byte in separator.iter().chain(part.iter()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// Makes sure `dir` is a directory private to this user: creates it
/// owner-only when it is missing (its parent must exist), and otherwise
/// accepts it only when it is a directory rather than a link. On Unix it must
/// also be owned by this user and grant nobody else access. On Windows its
/// access is the ACL it inherits, as for [`secure_owned_dir`].
///
/// # Errors
///
/// Returns [`OsError::Io`] when `dir` cannot be created or inspected, and
/// [`OsError::Security`] when it is a link, not a directory, or (Unix) not
/// private to this user.
pub fn ensure_private_dir(dir: &Path) -> Result<(), OsError> {
    #[cfg(windows)]
    {
        match std::fs::create_dir(dir) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => other?,
        }
        let meta = std::fs::symlink_metadata(dir)?;
        if meta.is_symlink() || !meta.is_dir() {
            return Err(OsError::Security(format!(
                "{} is not a plain directory",
                dir.display()
            )));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        platform_unix::ensure_private_dir(dir)
    }
}

/// Writes nothing more to this process's standard output: points it at the
/// null device and closes what it was, so a reader of the old one sees its
/// end while this process runs on, and a later write cannot reach a handle
/// reused for something else.
///
/// # Errors
///
/// Returns [`OsError::Io`] when the null device cannot be opened or put in
/// place; standard output is then left as it was.
pub fn release_standard_output() -> Result<(), OsError> {
    #[cfg(windows)]
    {
        platform_windows::release_standard_output()
    }
    #[cfg(not(windows))]
    {
        platform_unix::release_standard_output()
    }
}

#[cfg(not(windows))]
fn resolve_endpoint_env(
    user_id: &str,
    create_missing: platform_unix::EndpointCreation,
    namespace: &InstanceNamespace,
) -> String {
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
    let tmp_root = std::env::temp_dir();
    match platform_unix::resolve_endpoint_with(
        user_id,
        xdg_runtime_dir.as_deref(),
        &tmp_root,
        create_missing,
        namespace,
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to resolve the IPC runtime directory; returning the unverified \
                 stage-3 candidate path, which the next bind/connect will fail on the same \
                 way an absent daemon would"
            );
            platform_unix::stage3_candidate_path(&tmp_root, user_id, namespace)
        }
    }
}

/// Base directory for persistent application data: the seam
/// behind [`default_data_base_dir`].
///
/// The platform branches differ in which argument they use. Windows:
/// `primary` (read from `LOCALAPPDATA`) is returned unchanged and `home` is
/// unused. macOS: `primary` is ignored and `home` is mapped by
/// [`macos_base_dir_from`] to `{home}/Library/Application Support`.
/// Other Unix: `primary` (read from `XDG_DATA_HOME`) is returned
/// unchanged when present, otherwise `home` (`HOME`) with `.local/share`
/// appended. `None` when the inputs the branch uses are all absent.
#[cfg(windows)]
#[must_use]
pub fn data_base_dir_from(primary: Option<&OsStr>, _home: Option<&OsStr>) -> Option<PathBuf> {
    windows_base_dir_from(primary)
}

/// macOS branch of [`data_base_dir_from`]: `primary` (`XDG_DATA_HOME`) is
/// ignored, since data, state and configuration share one base directory
/// here.
#[cfg(target_os = "macos")]
#[must_use]
pub fn data_base_dir_from(_primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    macos_base_dir_from(home)
}

/// XDG-family Unix branch of [`data_base_dir_from`].
#[cfg(all(not(windows), not(target_os = "macos")))]
#[must_use]
pub fn data_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    unix_base_dir_from(primary, home, ".local/share")
}

/// State-directory counterpart of [`data_base_dir_from`]: Windows
/// `LOCALAPPDATA` with `home` unused; macOS `{home}/Library/Application
/// Support` with `primary` ignored; other Unix `XDG_STATE_HOME`
/// falling back to `HOME/.local/state`.
#[cfg(windows)]
#[must_use]
pub fn state_base_dir_from(primary: Option<&OsStr>, _home: Option<&OsStr>) -> Option<PathBuf> {
    windows_base_dir_from(primary)
}

/// macOS branch of [`state_base_dir_from`]: the same base directory as the
/// data and configuration seams.
#[cfg(target_os = "macos")]
#[must_use]
pub fn state_base_dir_from(_primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    macos_base_dir_from(home)
}

/// XDG-family Unix branch of [`state_base_dir_from`].
#[cfg(all(not(windows), not(target_os = "macos")))]
#[must_use]
pub fn state_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    xdg_state_base_dir_from(primary, home)
}

/// XDG state base directory: `primary` (`XDG_STATE_HOME`) unchanged when
/// present, otherwise `{home}/.local/state`. Pure mapping compiled on every OS
/// so that it stays unit-testable off the platforms that use it. `None` when
/// both are absent. Never reads the environment.
#[must_use]
pub fn xdg_state_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    unix_base_dir_from(primary, home, ".local/state")
}

/// Base directory the daemon places the executable a CLI hook runs under (its
/// report directory): the seam behind [`default_report_base_dir`].
///
/// Windows and XDG-family Unix: the same value as [`state_base_dir_from`].
/// macOS: [`xdg_state_base_dir_from`] instead of `Application Support`,
/// because some CLIs cannot run a hook command whose path holds a space.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn report_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    state_base_dir_from(primary, home)
}

/// macOS branch of [`report_base_dir_from`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn report_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    xdg_state_base_dir_from(primary, home)
}

fn unix_base_dir_from(
    primary: Option<&OsStr>,
    home: Option<&OsStr>,
    home_suffix: &str,
) -> Option<PathBuf> {
    // The XDG spec has a relative value ignored, as it would make the base
    // depend on the working directory.
    if let Some(primary) = primary.map(PathBuf::from).filter(|path| path.has_root()) {
        return Some(primary);
    }
    home.map(|home| PathBuf::from(home).join(home_suffix))
}

/// Windows counterpart of [`unix_base_dir_from`]: `LOCALAPPDATA` is the only
/// input, so there is no fallback branch to share.
#[cfg(windows)]
fn windows_base_dir_from(primary: Option<&OsStr>) -> Option<PathBuf> {
    primary.map(PathBuf::from)
}

/// macOS base directory for persistent data, state, and configuration:
/// `{home}/Library/Application Support`. Pure mapping compiled on every OS so
/// that it stays unit-testable off-macOS. Returns `None` when `home` is
/// absent. Never reads the environment.
#[must_use]
pub fn macos_base_dir_from(home: Option<&OsStr>) -> Option<PathBuf> {
    home.map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
    })
}

/// Configuration-directory counterpart of [`data_base_dir_from`] /
/// [`state_base_dir_from`]: Windows `LOCALAPPDATA` with
/// `home` unused; macOS `{home}/Library/Application Support` with `primary`
/// ignored; other Unix `XDG_CONFIG_HOME` falling back to
/// `HOME/.config`. This is the seam behind [`default_config_base_dir`] and
/// the base the GUI's configuration root (keymap, themes, playbook) is
/// resolved under.
#[cfg(windows)]
#[must_use]
pub fn config_base_dir_from(primary: Option<&OsStr>, _home: Option<&OsStr>) -> Option<PathBuf> {
    windows_base_dir_from(primary)
}

/// macOS branch of [`config_base_dir_from`]: the same base directory as the
/// data and state seams, so the GUI's configuration root lives
/// under `Application Support` rather than a `.config` of its own.
#[cfg(target_os = "macos")]
#[must_use]
pub fn config_base_dir_from(_primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    macos_base_dir_from(home)
}

/// XDG-family Unix branch of [`config_base_dir_from`].
#[cfg(all(not(windows), not(target_os = "macos")))]
#[must_use]
pub fn config_base_dir_from(primary: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    unix_base_dir_from(primary, home, ".config")
}

/// Reads the `{XDG,LOCALAPPDATA}`-family environment variables and resolves
/// the default persistent-data base directory (env is read
/// nowhere else in this crate): `LOCALAPPDATA` on Windows, `XDG_DATA_HOME`
/// falling back to `HOME` on XDG-family Unix, and `HOME` alone on macOS,
/// whose seam ignores the `XDG_DATA_HOME` value this wrapper still reads and
/// passes. `None` when the variables the platform's seam uses are
/// all unset.
#[must_use]
pub fn default_data_base_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        data_base_dir_from(std::env::var_os("LOCALAPPDATA").as_deref(), None)
    }
    #[cfg(not(windows))]
    {
        data_base_dir_from(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

/// State-directory counterpart of [`default_data_base_dir`]: `LOCALAPPDATA`
/// on Windows, `XDG_STATE_HOME` falling back to `HOME` on XDG-family Unix,
/// and `HOME` alone on macOS.
#[must_use]
pub fn default_state_base_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        state_base_dir_from(std::env::var_os("LOCALAPPDATA").as_deref(), None)
    }
    #[cfg(not(windows))]
    {
        state_base_dir_from(
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

/// Report-directory counterpart of [`default_state_base_dir`] (see
/// [`report_base_dir_from`]): `LOCALAPPDATA` on Windows, and `XDG_STATE_HOME`
/// falling back to `HOME` on every Unix, macOS included.
#[must_use]
pub fn default_report_base_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        report_base_dir_from(std::env::var_os("LOCALAPPDATA").as_deref(), None)
    }
    #[cfg(not(windows))]
    {
        report_base_dir_from(
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

/// Configuration-directory counterpart of [`default_data_base_dir`] /
/// [`default_state_base_dir`]: `LOCALAPPDATA` on
/// Windows, `XDG_CONFIG_HOME` falling back to `HOME` on XDG-family Unix, and
/// `HOME` alone on macOS. `None` when the variables the
/// platform's seam uses are all unset.
#[must_use]
pub fn default_config_base_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        config_base_dir_from(std::env::var_os("LOCALAPPDATA").as_deref(), None)
    }
    #[cfg(not(windows))]
    {
        config_base_dir_from(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }
}

/// Restricts every path component from `base` (exclusive) through `leaf`
/// (inclusive) to owner-only access, creating any that are missing. `base`
/// itself is created plain and never chmod'd, since it may be shared with
/// other applications (e.g. `XDG_DATA_HOME`); re-applied on every call, so a
/// directory left at a looser mode is brought into line on the next start.
///
/// On Unix, any of those components that already exists as a symlink is
/// rejected with an `Err` instead of being chmod'd or descended into. That
/// rejection is a Unix implementation contract: the Windows no-op never
/// inspects either argument, so Windows callers cannot rely on it.
///
/// Windows: no-op, always `Ok(())` -- NTFS ACL inheritance from
/// `%LOCALAPPDATA%` already scopes the directory to its owner,
/// and the mode of neither argument is inspected or changed.
///
/// # Errors
///
/// Returns a description of the failure rather than a typed error, so a
/// caller can log it and keep running with weaker protection instead of
/// treating it as fatal. On Unix, this includes a path component that
/// already exists as a symlink.
pub fn secure_owned_dir(base: &Path, leaf: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = (base, leaf);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        platform_unix::secure_owned_dir(base, leaf)
    }
}

/// Restricts the already-existing file at `path` to owner-only access: the
/// companion to [`secure_owned_dir`] for a file whose creation API offers no
/// mode argument (e.g. rusqlite's `Connection::open`), or whose mode should
/// be re-applied idempotently.
///
/// Unlike [`secure_owned_dir`], `path` is chmod'd even when it is a symlink.
/// The residual risk that leaves differs by call site, since it depends on
/// whether `path`'s parent directory is one [`secure_owned_dir`] narrows: see
/// the Unix implementation's doc for the per-call-site breakdown.
///
/// Windows: no-op, always `Ok(())`, for the same reason as
/// [`secure_owned_dir`].
///
/// # Errors
///
/// Returns a description of the failure rather than a typed error, for the
/// same reason as [`secure_owned_dir`].
pub fn secure_owned_file(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        platform_unix::secure_owned_file(path)
    }
}

/// Outcome of [`seed_config_file`]: whether it created the file or found one
/// already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The file did not exist yet, and `content` was written to it.
    Created,
    /// The file already existed and was left untouched.
    AlreadyPresent,
}

/// Seeds `file_name` with `content` under the directory `namespace` owns
/// below `base_dir` (plus `nested`, when given) the first time it is called,
/// and leaves an existing file untouched on every later call: the mechanism
/// behind a GUI's first-run config defaults (e.g. the default playbook).
///
/// The target directory is [`instance_dir`]`(base_dir, namespace)` joined
/// with `nested`, so the caller never has to compose the namespace name
/// itself.
///
/// Hand-off order: [`secure_owned_dir`] first, so an unsafe target directory
/// (a Unix symlink) is rejected before anything is written -> then
/// `create_dir_all` (the directory creation Windows still needs, and a
/// no-op on Unix once `secure_owned_dir` already created it) -> an
/// exclusive `create_new` open, so a file created concurrently by another
/// process is never overwritten -> [`secure_owned_file`] on success only.
///
/// # Errors
///
/// Returns the [`secure_owned_dir`] failure, converted to a generic
/// [`std::io::Error`], when the directory cannot be secured; this
/// short-circuits before anything is written. Otherwise returns any
/// [`std::io::Error`] from creating the directory or the file -- except
/// that `file_name` already existing is reported as
/// [`SeedOutcome::AlreadyPresent`] rather than an error. A
/// [`secure_owned_file`] failure *after* a successful write is not
/// surfaced as an error: the file has already landed, and re-applying the
/// owner-only mode is best-effort, the same degrade-and-continue contract
/// [`secure_owned_dir`] documents.
pub fn seed_config_file(
    base_dir: &Path,
    namespace: &InstanceNamespace,
    nested: Option<&str>,
    file_name: &str,
    content: &str,
) -> std::io::Result<SeedOutcome> {
    let instance_root = instance_dir(base_dir, namespace);
    let dir = match nested {
        Some(nested) => instance_root.join(nested),
        None => instance_root,
    };
    secure_owned_dir(base_dir, &dir).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(file_name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(content.as_bytes())?;
            let _ = secure_owned_file(&path);
            Ok(SeedOutcome::Created)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(SeedOutcome::AlreadyPresent)
        }
        Err(error) => Err(error),
    }
}

/// Atomically replaces (or creates) `file_name` inside `leaf_dir`: the
/// write-back counterpart to [`seed_config_file`]'s create-if-absent
/// contract, behind a daemon writing a GUI-edited config file (e.g.
/// `roles.toml`) back to disk.
///
/// Unlike [`seed_config_file`], which composes the target directory itself
/// from `base_dir` / `namespace` / `nested`, this takes the same two-path
/// split [`secure_owned_dir`] does: `base_dir` (exclusive, never hardened)
/// and `leaf_dir` (inclusive, hardened). Hand-off order mirrors
/// [`seed_config_file`]: [`secure_owned_dir`]`(base_dir, leaf_dir)` first,
/// so an unsafe target directory is rejected before anything is written,
/// then `create_dir_all(leaf_dir)`, then `content` is written to a
/// same-directory temporary file and renamed onto `leaf_dir.join(file_name)`
/// so a reader never observes a partially written file.
/// [`secure_owned_file`] is applied to the target after a successful
/// rename, the same owner-only narrowing [`seed_config_file`] applies.
///
/// # Errors
///
/// Returns the [`secure_owned_dir`] failure, converted to a generic
/// [`std::io::Error`], when the directory cannot be secured; this
/// short-circuits before anything is written. Otherwise returns any
/// [`std::io::Error`] from creating the directory, writing the temporary
/// file, or renaming it onto the target -- the last of which includes the
/// target existing as something a rename cannot replace (e.g. a
/// directory). The temporary file is removed on any failure that occurs
/// after it was created, so a failed call never leaves one behind.
pub fn replace_config_file(
    base_dir: &Path,
    leaf_dir: &Path,
    file_name: &str,
    content: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    secure_owned_dir(base_dir, leaf_dir).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(leaf_dir)?;
    let target = leaf_dir.join(file_name);
    let temp_path = leaf_dir.join(format!(".{file_name}.tmp-{}", std::process::id()));

    if let Err(err) = std::fs::write(&temp_path, content.as_ref()) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    match std::fs::rename(&temp_path, &target) {
        Ok(()) => {
            let _ = secure_owned_file(&target);
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

/// Publishes the directory `dir_name` inside `parent_dir`, holding exactly
/// `files` (file name, content): the directory counterpart to
/// [`replace_config_file`], for a caller that names the directory after its
/// content, so a directory already present under that name already holds
/// these files.
///
/// [`secure_owned_dir`]`(base_dir, parent_dir)` comes first, as in
/// [`replace_config_file`]. If `parent_dir.join(dir_name)` already exists as
/// a directory, nothing is written. Otherwise every file is written into a
/// temporary directory beside it, each narrowed by [`secure_owned_file`]
/// and the directory itself by [`secure_owned_dir`], and the temporary
/// directory is renamed onto the final name, so a reader never observes a
/// partially written directory. A rename that fails because another writer
/// published the same name first counts as success.
///
/// # Errors
///
/// Returns [`std::io::ErrorKind::InvalidInput`] when `dir_name` or a file
/// name is not a single plain path component, or a file name repeats, before
/// anything is touched. Returns the [`secure_owned_dir`] failure, converted to
/// a generic [`std::io::Error`], when `parent_dir` or the temporary directory
/// cannot be secured. Otherwise returns any [`std::io::Error`] from creating
/// the temporary directory, writing a file into it, or renaming it when the
/// final name still does not exist afterwards. The temporary directory is
/// removed on any failure that occurs after it was created.
pub fn publish_config_dir(
    base_dir: &Path,
    parent_dir: &Path,
    dir_name: &str,
    files: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    let names_ok = is_single_component(dir_name)
        && files
            .iter()
            .all(|(name, _)| is_single_component(name) && seen.insert(*name));
    if !names_ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a settings directory or file name is not a single plain path component",
        ));
    }
    secure_owned_dir(base_dir, parent_dir).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(parent_dir)?;
    let target = parent_dir.join(dir_name);
    if target.is_dir() {
        return Ok(());
    }

    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = parent_dir.join(format!(
        ".{dir_name}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // Only a crashed earlier process with the same id can have left it.
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir(&temp)?;
    let filled = fill_temp_config_dir(base_dir, &temp, files);
    let published = filled.and_then(|()| std::fs::rename(&temp, &target));
    match published {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&temp);
            if target.is_dir() { Ok(()) } else { Err(err) }
        }
    }
}

/// Narrows the freshly created `temp` to its owner and writes `files` into
/// it, each narrowed the same way.
fn fill_temp_config_dir(
    base_dir: &Path,
    temp: &Path,
    files: &[(&str, &str)],
) -> std::io::Result<()> {
    secure_owned_dir(base_dir, temp).map_err(std::io::Error::other)?;
    for (name, content) in files {
        let path = temp.join(name);
        std::fs::write(&path, content.as_bytes())?;
        let _ = secure_owned_file(&path);
    }
    Ok(())
}

/// Whether `name` is exactly one normal path component (not empty, `.`,
/// `..`, a root or a prefix, and no separator).
fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(only)), None) if only == OsStr::new(name)
    )
}

/// The [`IpcTransport`] implementation of the platform being compiled for.
#[cfg(windows)]
pub type PlatformIpc = platform_windows::WindowsIpc;
/// The [`IpcTransport`] implementation of the platform being compiled for.
#[cfg(not(windows))]
pub type PlatformIpc = platform_unix::UnixIpc;

#[cfg(not(windows))]
pub use platform_unix::{ClientReadHalf, split_client, write_error_shows_peer_closed};
#[cfg(windows)]
pub use platform_windows::{ClientReadHalf, split_client, write_error_shows_peer_closed};

/// The unit that owns a child's whole process tree on the platform being
/// compiled for: a job object on Windows, a process group on Unix. Ending it
/// ends every process in it, and dropping it does too.
#[cfg(windows)]
pub type ProcessGroup = platform_windows::JobObject;
/// The unit that owns a child's whole process tree on the platform being
/// compiled for: a job object on Windows, a process group on Unix. Ending it
/// ends every process in it, and dropping it does too.
#[cfg(not(windows))]
pub type ProcessGroup = platform_unix::UnixProcessGroup;

/// How long a window's process tree is given between `SIGTERM` and
/// `SIGKILL`, by a [`ProcessGroup`] ended with `terminate` and by
/// [`platform_unix::TreeEnd`].
#[cfg(not(windows))]
pub const PROCESS_GROUP_TERM_GRACE: Duration = Duration::from_millis(250);

/// How long the owner of a window's tree waits, from its first `SIGKILL`,
/// for the processes it signalled to end. Only a process that a `SIGKILL`
/// does not end (one stuck in the kernel) keeps it waiting that long; it is
/// then left and reported instead.
#[cfg(not(windows))]
pub const TREE_END_LIMIT: Duration = Duration::from_secs(5);
