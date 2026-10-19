//! Unix implementation of the OS abstraction boundary.
//!
//! A Unix domain socket carries the IPC (`UnixIpc`), and a process group +
//! `killpg` owns each child's process tree (`UnixProcessGroup`), led by the
//! child from the moment it is started. The peer-credential path is
//! implemented for Linux and macOS, each through its own platform call, and
//! fails closed on every other Unix target rather than failing to compile.
//!
//! **Known residual risk (TOCTOU)**: `is_secure_dir`'s verification and the
//! privileged operation that follows it (`bind`, `DirBuilder::create`) are
//! separate syscalls, so a parent directory swapped for a symlink in the gap
//! between them is not caught. Only an attacker who already owns that
//! directory can do so, and such a same-uid attacker gains no new privilege
//! (the peer-credential check would already authorize them), so this is
//! accepted rather than moved to fd-relative (`openat`-style) calls.

// This module *is* the transport: `IpcTransport` for Unix is implemented in
// terms of the raw socket types that `clippy.toml` bans everywhere else.
#![allow(clippy::disallowed_types)]

mod process_group;
pub(crate) mod pty;

pub(crate) use process_group::DEFAULT_TERM_GRACE;
pub use process_group::UnixProcessGroup;

use std::ffi::OsStr;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UnixListener;
use tokio::net::UnixStream;

use crate::DetachedSpawn;
use crate::IpcTransport;
use crate::OsError;
use crate::PeerAuth;
use crate::instance::InstanceNamespace;

// =========================================================================
// Endpoint resolution
// =========================================================================

/// Largest usable length of `sockaddr_un.sun_path` on Linux: 108 bytes total,
/// minus 1 for the mandatory NUL terminator.
#[cfg(target_os = "linux")]
const SUN_PATH_MAX_LEN: usize = 107;

/// Largest usable length of `sockaddr_un.sun_path` on macOS: 104 bytes total,
/// minus 1 for the mandatory NUL terminator. Every Unix
/// target other than Linux takes this smaller value, since the only failure
/// mode of an under-estimate is rejecting a path the kernel would have
/// accepted, while an over-estimate hands `bind` a path it truncates.
#[cfg(not(target_os = "linux"))]
const SUN_PATH_MAX_LEN: usize = 103;

/// Whether [`resolve_endpoint_with`]'s stage-3 fallback directory may be
/// created when it is missing.
///
/// Defined only on this (non-Windows) side: the Windows endpoint has no
/// equivalent stage-3 fallback to gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointCreation {
    /// The daemon's bind path: create the stage-3 fallback directory if it
    /// is missing.
    CreateIfMissing,
    /// A client's connect/resolve path: never mutate the filesystem while
    /// resolving.
    ReadOnly,
}

/// Resolves the Unix-domain-socket endpoint for `user_id` through a 3-stage
/// fallback, each stage gated by the same `is_secure_dir` check
/// [`UnixIpc::bind`] applies to its parent directory:
///
/// 1. `xdg_runtime_dir`, when given and secure:
///    `{xdg_runtime_dir}/{namespace}.sock`.
/// 2. `/run/user/{user_id}`, when secure: same shape.
/// 3. `{tmp_root}/{namespace}-{user_id}/ipc.sock`: the directory is only created when `create_missing` is
///    [`EndpointCreation::CreateIfMissing`] (the daemon's bind path); a
///    client ([`EndpointCreation::ReadOnly`]) never mutates the filesystem
///    while resolving, so an absent directory here simply fails this stage
///    (the daemon starting up later creates it, making the same call
///    succeed afterwards).
///
/// Every stage that resolves is logged once at `info` so the runtime can be
/// diagnosed without reproducing the fallback by hand.
///
/// # Errors
///
/// Returns [`OsError::Security`] when no stage yields a usable,
/// appropriately-sized directory, and when the resolved path would exceed
/// `SUN_PATH_MAX_LEN` (not an intra-doc link: that constant is private).
pub fn resolve_endpoint_with(
    user_id: &str,
    xdg_runtime_dir: Option<&OsStr>,
    tmp_root: &Path,
    create_missing: EndpointCreation,
    namespace: &InstanceNamespace,
) -> Result<String, OsError> {
    // SAFETY: getuid takes no arguments and only reads this process's real
    // user id.
    let owner_uid = unsafe { libc::getuid() };

    if let Some(dir) = xdg_runtime_dir {
        let dir = Path::new(dir);
        if is_secure_dir(dir, owner_uid) {
            let endpoint = build_endpoint(dir, &shared_dir_socket_name(namespace))?;
            tracing::info!(stage = 1, endpoint = %endpoint, "resolved IPC endpoint via XDG_RUNTIME_DIR");
            return Ok(endpoint);
        }
    }

    let stage2_dir = PathBuf::from(format!("/run/user/{user_id}"));
    if is_secure_dir(&stage2_dir, owner_uid) {
        let endpoint = build_endpoint(&stage2_dir, &shared_dir_socket_name(namespace))?;
        tracing::info!(stage = 2, endpoint = %endpoint, "resolved IPC endpoint via /run/user/{{uid}}");
        return Ok(endpoint);
    }

    let dir = stage3_dir(tmp_root, user_id, namespace);
    if create_missing == EndpointCreation::CreateIfMissing
        && std::fs::symlink_metadata(&dir).is_err()
    {
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    }
    if is_secure_dir(&dir, owner_uid) {
        let endpoint = build_endpoint(&dir, STAGE3_SOCKET_NAME)?;
        tracing::info!(stage = 3, endpoint = %endpoint, "resolved IPC endpoint via the fallback runtime dir");
        return Ok(endpoint);
    }

    Err(OsError::Security(format!(
        "no usable IPC runtime directory for user {user_id} (checked XDG_RUNTIME_DIR, \
         /run/user/{user_id}, and {})",
        dir.display()
    )))
}

/// The stage-3 fallback directory `resolve_endpoint_with` resolves or
/// creates: `{tmp_root}/{namespace}-{user_id}`. Factored out so
/// [`stage3_candidate_path`] (the unverified fallback `default_endpoint`
/// falls back to) always agrees with the seam on what that directory is.
fn stage3_dir(tmp_root: &Path, user_id: &str, namespace: &InstanceNamespace) -> PathBuf {
    tmp_root.join(format!("{}-{user_id}", namespace.name()))
}

/// The stage-3 endpoint path, without verifying (or creating) that it
/// actually resolves -- used by `default_endpoint`/`default_endpoint_owning`
/// (`crate::lib`) as a deterministic fallback when
/// [`resolve_endpoint_with`] itself fails, so a subsequent `connect`/`bind`
/// attempt against it fails the same way an absent daemon would.
pub(crate) fn stage3_candidate_path(
    tmp_root: &Path,
    user_id: &str,
    namespace: &InstanceNamespace,
) -> String {
    format!(
        "{}/{STAGE3_SOCKET_NAME}",
        stage3_dir(tmp_root, user_id, namespace).to_string_lossy()
    )
}

/// The socket's name in the stage-3 directory. That directory already belongs to one
/// instance, so the name does not repeat it: macOS's long `TMPDIR` leaves no room for the
/// identifier twice within `sockaddr_un.sun_path`.
const STAGE3_SOCKET_NAME: &str = "ipc.sock";

/// The socket's name in a runtime directory every instance shares (stages 1 and 2).
fn shared_dir_socket_name(namespace: &InstanceNamespace) -> String {
    format!("{}.sock", namespace.name())
}

/// Joins `dir` with `file_name` and rejects the result if it would not fit in
/// `sockaddr_un.sun_path`.
fn build_endpoint(dir: &Path, file_name: &str) -> Result<String, OsError> {
    let dir_str = dir
        .to_str()
        .ok_or_else(|| OsError::Security(format!("{} is not valid UTF-8", dir.display())))?;
    let endpoint = format!("{dir_str}/{file_name}");
    if endpoint.len() > SUN_PATH_MAX_LEN {
        return Err(OsError::Security(format!(
            "resolved endpoint path exceeds the {SUN_PATH_MAX_LEN}-byte sockaddr_un.sun_path \
             limit: {endpoint}"
        )));
    }
    Ok(endpoint)
}

/// The directory check shared by every stage of
/// [`resolve_endpoint_with`] and by [`UnixIpc::bind`]'s parent-directory
/// check: `path` must exist, not be a symlink, be a directory, be owned by
/// `owner_uid`, and grant no group/other access (mode & 0o077 == 0).
fn is_secure_dir(path: &Path, owner_uid: u32) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.is_symlink() || !meta.is_dir() {
        return false;
    }
    meta.uid() == owner_uid && meta.mode() & 0o077 == 0
}

// =========================================================================
// Owner-restricted persistence permissions
// =========================================================================

/// Unix implementation of [`crate::secure_owned_dir`].
///
/// `base` is created plain (`create_dir_all`, mode left to the OS/umask)
/// and never chmod'd: it may be a directory shared with other applications
/// (e.g. `XDG_DATA_HOME`). Every path component from `base` (exclusive)
/// through `leaf` (inclusive) is created or re-chmod'd to owner-only
/// (`0700`) on every call, so a component left at a looser mode is brought
/// back into line on the next start.
///
/// Any of those components (not just the last) is rejected instead, without
/// being `chmod`'d or descended into, when it already exists as a symlink.
/// `std::fs::set_permissions` follows symlinks (`chmod(2)` semantics; Rust's
/// std has no `lchmod` equivalent), so a pre-planted symlink would otherwise
/// redirect this restriction into whatever the link points at.
///
/// A `0700` database directory also contains SQLite's rollback-journal / WAL
/// temp files, which have no permissions of their own and inherit their
/// containment from this directory's mode.
///
/// **Residual risk (db path)**: `store::open_store` only warns and continues
/// when this returns `Err`, so a symlinked component still lets the on-disk
/// store land inside the link's target, redirected rather than blocked. Only
/// an attacker who already owns `base` can plant that symlink, and such a
/// same-uid attacker gains no new privilege from the redirection.
///
/// # Errors
///
/// Returns a description of the first failure (creating `base`, creating a
/// missing component of `leaf`, restricting an already-existing one, or a
/// component that already exists as a symlink) rather than a typed error,
/// matching [`crate::secure_owned_dir`]'s contract of never being treated as
/// fatal by its callers.
pub fn secure_owned_dir(base: &Path, leaf: &Path) -> Result<(), String> {
    std::fs::create_dir_all(base)
        .map_err(|err| format!("failed to create {}: {err}", base.display()))?;
    let relative = leaf.strip_prefix(base).map_err(|_| {
        format!(
            "{} is not a descendant of {}",
            leaf.display(),
            base.display()
        )
    })?;

    let mut cursor = base.to_path_buf();
    for component in relative.components() {
        cursor.push(component);
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.is_symlink() => {
                return Err(format!(
                    "{} is a symlink; refusing to restrict or descend into it",
                    cursor.display()
                ));
            }
            Ok(_) => {}
            Err(_) => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&cursor)
                    .map_err(|err| {
                        format!(
                            "failed to create {} with mode 0700: {err}",
                            cursor.display()
                        )
                    })?;
            }
        }
        std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o700)).map_err(
            |err| {
                format!(
                    "failed to restrict {} to mode 0700: {err}",
                    cursor.display()
                )
            },
        )?;
    }
    Ok(())
}

/// Unix implementation of [`crate::secure_owned_file`].
///
/// Unlike [`secure_owned_dir`], `path` is `chmod`'d even when it is a
/// symlink. The residual risk that leaves differs by the caller: replacing
/// `path` with a symlink requires write access to its parent directory. For
/// the default db/log layout, that parent is the one [`secure_owned_dir`]
/// already narrows to owner-only. For an explicitly configured path
/// (`config` or `YAGURA_STORE_DB`), the parent is never narrowed by design,
/// so a same-uid attacker with write access there can still swap `path` for
/// a symlink before this call -- again without gaining a new privilege.
///
/// # Errors
///
/// Returns a description of the failure rather than a typed error, matching
/// [`crate::secure_owned_file`]'s contract.
pub fn secure_owned_file(path: &Path) -> Result<(), String> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("failed to restrict {} to mode 0600: {err}", path.display()))
}

// =========================================================================
// UnixIpc: PeerAuth + IpcTransport
// =========================================================================

/// Protection applied to an endpoint at creation time: the current user's
/// uid, checked again -- read back from the bound socket file itself, not
/// from this value -- by [`PeerAuth::endpoint_allows_only`].
///
/// Not `Clone`: [`IpcTransport::bind`] consumes it, mirroring
/// `platform_windows::PipeSecurity`.
#[derive(Debug)]
pub struct SocketSecurity {
    owner_uid: u32,
}

/// The Unix-domain-socket transport and peer check.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnixIpc;

impl PeerAuth for UnixIpc {
    type Security = SocketSecurity;
    type Connection = UnixStream;

    fn endpoint_security(&self) -> Result<Self::Security, OsError> {
        Ok(SocketSecurity {
            owner_uid: current_uid(),
        })
    }

    fn current_user_id(&self) -> Result<String, OsError> {
        Ok(current_uid().to_string())
    }

    fn peer_id(&self, conn: &Self::Connection) -> Option<String> {
        peer_credential_uid(conn).map(|uid| uid.to_string())
    }

    fn peer_executable(&self, conn: &Self::Connection) -> Option<PathBuf> {
        peer_executable_path(conn)
    }

    fn endpoint_allows_only(&self, endpoint: &str, user_id: &str) -> Result<bool, OsError> {
        let owner_uid: u32 = user_id
            .parse()
            .map_err(|_| OsError::Security(format!("{user_id} is not a valid numeric uid")))?;

        let path = Path::new(endpoint);
        let meta = std::fs::symlink_metadata(path).map_err(OsError::Io)?;

        if meta.is_symlink() || !meta.file_type().is_socket() {
            return Ok(false);
        }
        if meta.uid() != owner_uid || meta.mode() & 0o077 != 0 {
            return Ok(false);
        }
        Ok(match path.parent() {
            Some(parent) => is_secure_dir(parent, owner_uid),
            None => false,
        })
    }
}

/// A bound socket together with the endpoint lock that marks its daemon as
/// alive.
///
/// The lock file (`<endpoint>.lock`) is held for as long as this value lives
/// and is released by the kernel when the process dies, even by `SIGKILL`; it
/// is never deleted, since unlinking it would let two processes lock two
/// different files under the same name.
#[derive(Debug)]
pub struct SocketListener {
    // Declared before `_lock` so the socket closes before the lock is
    // released: a free lock never leaves a still-listening socket behind.
    listener: UnixListener,
    _lock: std::fs::File,
}

impl IpcTransport for UnixIpc {
    type Listener = SocketListener;
    type ClientConnection = UnixStream;

    async fn bind(
        &self,
        endpoint: &str,
        security: Self::Security,
    ) -> Result<Self::Listener, OsError> {
        let path = Path::new(endpoint);
        let parent = path.parent().ok_or_else(|| {
            OsError::Security(format!("endpoint {endpoint} has no parent directory"))
        })?;
        if !is_secure_dir(parent, security.owner_uid) {
            return Err(OsError::Security(format!(
                "refusing to bind {endpoint}: its parent directory is not a private, owned, \
                 0700 directory"
            )));
        }

        let lock = lock_endpoint(path)?;
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(bind_error) if bind_error.kind() == std::io::ErrorKind::AddrInUse => {
                recover_from_addr_in_use(path, bind_error).await?
            }
            Err(other) => return Err(OsError::Io(other)),
        };
        finalize_socket_permissions(path)?;
        Ok(SocketListener {
            listener,
            _lock: lock,
        })
    }

    async fn accept(&self, listener: &mut Self::Listener) -> Result<Self::Connection, OsError> {
        listener
            .listener
            .accept()
            .await
            .map(|(stream, _peer_addr)| stream)
            .map_err(OsError::Io)
    }

    async fn connect(&self, endpoint: &str) -> Result<Self::ClientConnection, OsError> {
        let client = UnixStream::connect(endpoint).await.map_err(OsError::Io)?;
        let server = peer_credential_uid(&client).map(|uid| uid.to_string());
        crate::require_own_server(endpoint, server.as_deref(), &self.current_user_id()?)?;
        Ok(client)
    }
}

/// Takes the lock that marks `endpoint` as owned by a live daemon, opening
/// (or creating, owner-only) `<endpoint>.lock` beside it.
///
/// A lock already held by another process is reported as
/// [`OsError::EndpointHeld`].
fn lock_endpoint(endpoint: &Path) -> Result<std::fs::File, OsError> {
    let mut lock_path = endpoint.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);

    // O_NONBLOCK keeps a FIFO planted at the path from blocking the open;
    // O_CLOEXEC keeps a spawned child from holding the lock past our death.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&lock_path)
        .map_err(OsError::Io)?;
    if !lock.metadata().map_err(OsError::Io)?.is_file() {
        return Err(OsError::Security(format!(
            "{} is not a regular file; refusing to use it as the endpoint lock",
            lock_path.display()
        )));
    }

    // SAFETY: `lock` owns this open file descriptor for the whole call, and
    // flock only reads the descriptor and its flags.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(lock);
    }
    let lock_error = io::Error::last_os_error();
    if lock_error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Err(OsError::EndpointHeld(format!(
            "{} is held by a live listener (its lock {} is taken)",
            endpoint.display(),
            lock_path.display()
        )));
    }
    Err(OsError::Io(lock_error))
}

/// Handles `EADDRINUSE` from `UnixListener::bind` once the endpoint lock is
/// held: the path may hold a non-socket file (reject, never unlinked), a
/// listener of a daemon that predates the lock (connect succeeds; surface the
/// original error), or a dead socket (any connect failure; unlink and rebind
/// once). The lock already proves no current daemon owns the socket, so the
/// connect error is not interpreted.
async fn recover_from_addr_in_use(
    path: &Path,
    original: std::io::Error,
) -> Result<UnixListener, OsError> {
    let meta = std::fs::symlink_metadata(path).map_err(OsError::Io)?;
    if !meta.file_type().is_socket() {
        return Err(OsError::Security(format!(
            "{} already exists and is not a socket; refusing to bind over it",
            path.display()
        )));
    }

    if UnixStream::connect(path).await.is_ok() {
        return Err(OsError::Io(std::io::Error::new(
            original.kind(),
            format!("{original}: {} is held by a live listener", path.display()),
        )));
    }
    std::fs::remove_file(path).map_err(OsError::Io)?;
    UnixListener::bind(path).map_err(OsError::Io)
}

/// Narrows the just-bound socket file to owner-only. The brief
/// window between `bind` creating it (at whatever mode the umask leaves) and
/// this call is not a real exposure: the 0700 parent directory checked just
/// before binding already keeps every other user from reaching the path.
fn finalize_socket_permissions(path: &Path) -> Result<(), OsError> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(OsError::Io)
}

fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments and only reads this process's real
    // user id.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "linux")]
fn peer_credential_uid(conn: &UnixStream) -> Option<u32> {
    peer_credentials(conn).map(|cred| cred.uid)
}

/// Credentials the kernel recorded for the peer when it connected, so the uid
/// and the pid come from one call and describe the same instant.
#[cfg(target_os = "linux")]
fn peer_credentials(conn: &UnixStream) -> Option<libc::ucred> {
    let fd = conn.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `fd` is a valid, currently-open socket fd owned by `conn` for
    // the whole call; `cred`/`len` are stack locals sized exactly for
    // `SO_PEERCRED`, matching what getsockopt(2) documents for this option.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    };
    (result == 0).then_some(cred)
}

#[cfg(target_os = "macos")]
fn peer_credential_uid(conn: &UnixStream) -> Option<u32> {
    let fd = conn.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `fd` is a valid, currently-open socket fd owned by `conn` for
    // the whole call; `uid`/`gid` are stack locals of exactly the types
    // getpeereid(3) writes its two out-parameters as, and both are written
    // only when it returns 0.
    let result = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    (result == 0).then_some(uid)
}

/// Neither peer-credential call is portable across Unix: `SO_PEERCRED` is
/// Linux-specific and `getpeereid` is macOS/BSD-specific, so every other
/// Unix target fails closed here rather than failing to compile.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_credential_uid(_conn: &UnixStream) -> Option<u32> {
    None
}

/// Reads the peer's executable through the pid the kernel recorded for the
/// connection, so an unrelated process cannot answer for it by reusing a pid
/// the peer merely named.
#[cfg(target_os = "linux")]
fn peer_executable_path(conn: &UnixStream) -> Option<PathBuf> {
    let pid = peer_credentials(conn)?.pid;
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn peer_executable_path(conn: &UnixStream) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let fd = conn.as_raw_fd();
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: `fd` is a valid, currently-open socket fd owned by `conn` for
    // the whole call; `pid`/`len` are stack locals sized exactly for
    // `LOCAL_PEERPID`, matching what getsockopt(2) documents for this option.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::addr_of_mut!(pid).cast(),
            &mut len,
        )
    };
    if result != 0 {
        return None;
    }

    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buffer` is a live allocation of the length passed as
    // `buffersize`, which is the contract proc_pidpath(3) documents; it
    // returns the number of bytes written, or a non-positive value on
    // failure.
    let written =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if written <= 0 {
        return None;
    }

    buffer.truncate(written as usize);
    Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

/// Neither peer-pid source is portable across Unix: `SO_PEERCRED` is
/// Linux-specific and `LOCAL_PEERPID` is macOS-specific, so every other Unix
/// target fails closed here rather than failing to compile.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_executable_path(_conn: &UnixStream) -> Option<PathBuf> {
    None
}

// =========================================================================
// schedule_force_kill
// =========================================================================

/// How often the escalation thread re-reads `exited` while the grace period
/// runs, so a child that dies from the caller's first signal stops the
/// escalation long before the deadline instead of at it.
const FORCE_KILL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Unix branch of [`crate::schedule_force_kill`].
///
/// The wait happens on a detached thread so the caller never blocks for the
/// grace period, and `exited` is read once more immediately before the signal
/// rather than only at the deadline: in between, the child may have been
/// reaped and its pid handed to an unrelated same-uid process. That leaves the
/// same narrow window `UnixProcessGroup` documents for a reused `pgid`, and is
/// accepted for the same reason.
///
/// `kill` rather than `killpg`, because this escalation is about the single
/// child the caller spawned; its descendants are the process group's business.
/// The result is discarded and no error is reported: `ESRCH` (already reaped)
/// is the expected outcome of the race above, and the caller learns whether
/// the child is gone from its own exit observation, not from here.
pub fn schedule_force_kill(pid: u32, grace: Duration, exited: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + grace;
        loop {
            if exited.load(Ordering::SeqCst) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(FORCE_KILL_POLL_INTERVAL));
        }
        if exited.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: `kill` signals the single process `pid` supplied by the
        // caller, which spawned it and is still tracking its exit; `SIGKILL`
        // is a constant. A pid that no longer exists fails with `ESRCH`
        // instead of affecting anything.
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    });
}

// =========================================================================
// enter_raw_console_input
// =========================================================================

/// Unix branch of [`crate::enter_raw_console_input`].
///
/// Applies `cfmakeraw`'s settings to the terminal on standard input except
/// for the output flag `OPOST`, which is left as it was so that a newline
/// written by this process still reaches the terminal as CR LF. That means
/// clearing the input translations (`ICRNL` and its neighbours, so a CR
/// arrives as a CR), the line discipline and signal generation (`ICANON`,
/// `ECHO`, `ISIG`, `IEXTEN`), and the 8-bit-clean character settings.
/// `VMIN` = 1 with `VTIME` = 0 keeps a read blocking until at least one byte
/// arrives rather than returning zero, which a caller would read as end of
/// input. A standard input that is not a terminal is left untouched and
/// reported as success.
///
/// # Errors
///
/// Returns [`OsError::Io`] when the terminal attributes cannot be read or the
/// new ones cannot be applied.
pub fn enter_raw_console_input() -> Result<(), OsError> {
    let fd = libc::STDIN_FILENO;
    // SAFETY: isatty only inspects the descriptor it is given, and standard
    // input is valid for the lifetime of the process.
    if unsafe { libc::isatty(fd) } != 1 {
        return Ok(());
    }

    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `fd` is a valid descriptor and the pointer is to stack storage
    // sized exactly for the struct tcgetattr(3) fills.
    if unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) } != 0 {
        return Err(OsError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: tcgetattr returned success, so it initialized the struct.
    let mut attributes = unsafe { attributes.assume_init() };

    attributes.c_iflag &= !(libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON);
    attributes.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
    attributes.c_cflag &= !(libc::CSIZE | libc::PARENB);
    attributes.c_cflag |= libc::CS8;
    attributes.c_cc[libc::VMIN] = 1;
    attributes.c_cc[libc::VTIME] = 0;

    // SAFETY: `fd` is a valid descriptor and `attributes` is a fully
    // initialized struct borrowed for the duration of the call.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &attributes) } != 0 {
        return Err(OsError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

// =========================================================================
// spawn_daemon_detached
// =========================================================================

/// Unix branch of [`crate::program::default_is_executable`]: a regular file
/// carrying an execute bit. Follows symlinks and reads the mode of the file
/// they point at, which is what an `exec` would run.
///
/// Any execute bit counts rather than only the caller's, because the
/// permission that applies depends on the process's user and groups at
/// `exec` time; the narrower check would report a program the caller can in
/// fact start as unavailable.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Unix branch of [`crate::spawn_daemon_detached`]: `setsid` detaches the
/// daemon into its own session (so it survives the caller's controlling
/// terminal going away) and a reaper thread waits on it so it never lingers
/// as a zombie. Always uses this one mode -- there is no weaker Unix
/// fallback to fall back to -- so `used_fallback` is always `false`.
// The OS boundary owns the one detached spawn outside the PTY path; the ban in
// `clippy.toml` keeps every other call site from creating a child of its own.
#[allow(clippy::disallowed_methods)]
pub fn spawn_daemon_detached(path: &Path, args: &[&OsStr]) -> Result<DetachedSpawn, OsError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: the closure passed to pre_exec runs in the forked child,
    // between fork and exec, and calls only setsid(2) -- one of the
    // operations POSIX guarantees is async-signal-safe in that window. It
    // takes no arguments and its failure is reported to the child alone.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(OsError::Io)?;

    // Reaps the detached daemon so it never lingers as a zombie once it
    // exits; nothing in this process waits on the result.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(DetachedSpawn {
        used_fallback: false,
    })
}

// =========================================================================
// spawn_captured: one-shot, non-PTY process capture
// =========================================================================

/// Unix branch of [`crate::capture::spawn_captured`]: starts `argv[0]` in a
/// new session with no controlling terminal (`setsid` before `exec` runs in
/// the child, which also makes it the leader of a fresh process group that a
/// descendant it forks before this function even returns still inherits),
/// and takes that group as [`UnixProcessGroup::led_by`] the child's own pid --
/// so tearing the tree down is just dropping [`UnixProcessGroup`].
///
/// # Errors
///
/// Returns whatever [`std::process::Command::spawn`] returns, and an I/O
/// error when [`UnixProcessGroup::led_by`] rejects the child's pid; the child
/// is then killed and reaped before returning.
// The OS boundary owns the one one-shot, non-PTY capture spawn outside the
// PTY path; the ban in `clippy.toml` keeps every other call site from
// creating a child of its own.
#[allow(clippy::disallowed_methods)]
pub(crate) fn spawn_captured(
    argv: &[String],
    cwd: Option<&Path>,
    env: crate::capture::CaptureEnv<'_>,
    stdin: Option<Vec<u8>>,
) -> io::Result<crate::capture::CapturedChild> {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let vars = match env {
        crate::capture::CaptureEnv::Inherit(vars) => vars,
        crate::capture::CaptureEnv::Replace(vars) => {
            command.env_clear();
            vars
        }
    };
    for (key, value) in vars {
        command.env(key, value);
    }
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A new session, not just a group: a group keeps the daemon's controlling
    // terminal, where an interactive shell's job control stops it (SIGTTIN).
    // No `process_group` either: std applies it first, so `setsid` would EPERM.
    // SAFETY: the closure runs in the forked child between fork and exec and
    // calls only setsid(2), which POSIX lists as async-signal-safe there.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn()?;

    let group = match UnixProcessGroup::led_by(child.id(), DEFAULT_TERM_GRACE) {
        Ok(group) => group,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(crate::capture::os_error_to_io(error));
        }
    };

    let pipes = crate::capture::CapturePipes {
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        stdin: child.stdin.take(),
    };
    Ok(crate::capture::CapturedChild::new(
        child, group, pipes, stdin,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // A client resolves before the daemon has created the stage-3 directory
    // and keeps the fallback it got, so that fallback must be where the
    // daemon binds once it starts.
    #[test]
    fn the_unverified_stage3_fallback_is_the_endpoint_the_daemon_later_resolves() {
        let root = yagura_test_support::paths::short_tmp_dir("os-unix", "stage3-fallback");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let namespace = crate::resolve_instance_from(None).unwrap();
        let user_id = format!("999999999{}", std::process::id());
        let resolve =
            |creation| resolve_endpoint_with(&user_id, None, root.path(), creation, &namespace);
        assert!(
            resolve(EndpointCreation::ReadOnly).is_err(),
            "precondition: before the daemon starts, the client cannot resolve"
        );

        let client_endpoint = stage3_candidate_path(root.path(), &user_id, &namespace);
        let daemon_endpoint = resolve(EndpointCreation::CreateIfMissing).unwrap();

        assert_eq!(client_endpoint, daemon_endpoint);
    }
}
