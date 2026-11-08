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

mod exit_watch;
mod input_drain;
mod process_group;
pub(crate) mod pty;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tree_end;
mod tty_mark;
#[cfg(target_os = "linux")]
mod user_manager;

pub use exit_watch::{
    ChildChange, WatcherEvents, WatcherWait, block_child_signal, block_watcher_signals,
    continue_child, hang_up_child, kill_child, reap_exited_child, take_child_change, unreaped_exit,
    wait_child_signal, wait_for_exit_without_reaping,
};
#[cfg(target_os = "linux")]
pub use exit_watch::{child_pids, reap_orphans, set_child_subreaper};
pub(crate) use process_group::DEFAULT_TERM_GRACE;
pub use process_group::UnixProcessGroup;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use tree_end::{ProcessRow, SignalTarget, TreeEnd, TreeEndHooks, TreeEndRest, window_members};
#[cfg(target_os = "linux")]
pub use user_manager::{
    ScopeNames, UserManager, UserManagerError, cgroup_dir_of, descendants_outside,
    is_window_scope_name, kill_cgroup, orphan_pidfds, pidfd_open, scope_names,
};

use std::ffi::OsStr;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::net::UnixStream;

use crate::DetachedSpawn;
use crate::IpcTransport;
use crate::OsError;
use crate::PeerAuth;
use crate::ReapGate;
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
    if create_missing == EndpointCreation::CreateIfMissing {
        create_private_dir(&dir)?;
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

/// Unix branch of [`crate::holder_endpoint`]: `{holders_dir}/{window_id}.sock`,
/// or, when that does not fit in `sockaddr_un.sun_path`, `{hash}.sock` in an
/// owner-only [`holder_fallback_dir`] created on demand. `hash` is 16 hex
/// digits of the instance, `holders_dir` and `window_id`, which that shared
/// directory does not separate, so the fallback's length depends on `tmp_root`
/// and `user_id` only (91 bytes under macOS's 49-byte `TMPDIR`). Two windows
/// whose hashes collide are not served on one socket: the second bind is
/// refused as held.
///
/// # Errors
///
/// Returns [`OsError::Security`] when neither path fits or the fallback
/// directory is not private to this user, and [`OsError::Io`] when it cannot
/// be created.
pub fn holder_socket_path(
    user_id: &str,
    namespace: &InstanceNamespace,
    holders_dir: &Path,
    tmp_root: &Path,
    window_id: &str,
) -> Result<String, OsError> {
    if let Ok(endpoint) = build_endpoint(holders_dir, &format!("{window_id}.sock")) {
        return Ok(endpoint);
    }
    let dir = holder_fallback_dir(tmp_root, user_id);
    create_private_dir(&dir)?;
    if !is_secure_dir(&dir, current_uid()) {
        return Err(OsError::Security(format!(
            "{} is not a private, owned, 0700 directory",
            dir.display()
        )));
    }
    let hash = crate::short_hash(&[
        namespace.name().as_bytes(),
        holders_dir.as_os_str().as_encoded_bytes(),
        window_id.as_bytes(),
    ]);
    build_endpoint(&dir, &format!("{hash}.sock"))
}

/// The directory every instance's holder sockets fall back to:
/// `{tmp_root}/yagura-{user_id}-h`, short because `tmp_root` may be long.
fn holder_fallback_dir(tmp_root: &Path, user_id: &str) -> PathBuf {
    tmp_root.join(format!("yagura-{user_id}-h"))
}

/// Unix branch of [`crate::ensure_private_dir`].
///
/// # Errors
///
/// As for [`crate::ensure_private_dir`].
pub fn ensure_private_dir(dir: &Path) -> Result<(), OsError> {
    create_private_dir(dir)?;
    if is_secure_dir(dir, current_uid()) {
        Ok(())
    } else {
        Err(OsError::Security(format!(
            "{} is not a private, owned, 0700 directory",
            dir.display()
        )))
    }
}

/// Unix branch of [`crate::release_standard_output`].
///
/// # Errors
///
/// As for [`crate::release_standard_output`].
pub fn release_standard_output() -> Result<(), OsError> {
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: both descriptors are valid for the call; dup2 closes the old
    // standard output and puts a copy of `null` in its place atomically.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        return Err(OsError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

/// Creates `dir` owner-only (`0700`) unless something is already there, which
/// the caller then inspects. Create first: checking first lets a concurrent
/// caller create it in between, failing this create.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

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
/// missing component of `leaf`, inspecting or restricting an already-existing one, or a
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
        // Create first and inspect only what already exists: checking before creating lets a
        // concurrent caller create the component in between, and `mkdir` refuses a symlink too.
        match std::fs::DirBuilder::new().mode(0o700).create(&cursor) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(&cursor)
                    .map_err(|err| format!("failed to inspect {}: {err}", cursor.display()))?;
                if meta.is_symlink() {
                    return Err(format!(
                        "{} is a symlink; refusing to restrict or descend into it",
                        cursor.display()
                    ));
                }
            }
            Err(err) => {
                return Err(format!(
                    "failed to create {} with mode 0700: {err}",
                    cursor.display()
                ));
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

    fn peer_process_id(&self, conn: &Self::Connection) -> Option<u32> {
        peer_pid(conn)
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

/// A bound socket together with the endpoint lock that marks its listener as
/// alive.
///
/// The lock file (`<endpoint>.lock`) is held for as long as this value lives
/// and is released by the kernel when the process dies, even by `SIGKILL`.
/// Dropping the value removes the socket and then the lock file while the lock
/// is still held, so neither can be another listener's; a process that opened
/// the lock file before it was removed finds, once it locks, that the path no
/// longer names that file and opens it again, so two processes never hold
/// locks on one endpoint. A process that dies leaves both files, which the
/// next bind takes over.
#[derive(Debug)]
pub struct SocketListener {
    // Declared before `lock` so the socket closes before the lock is
    // released: a free lock never leaves a still-listening socket behind.
    listener: UnixListener,
    socket_path: PathBuf,
    lock: EndpointLock,
}

impl SocketListener {
    /// A copy of the descriptor the endpoint's lock is held on, close-on-exec:
    /// the lock stays held while any copy is open, so a copy is handed only
    /// to the one process meant to hold the lock on (the holder's watcher).
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the descriptor cannot be
    /// copied.
    pub fn lock_descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::AsFd as _;
        self.lock._file.as_fd().try_clone_to_owned()
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.lock.path);
    }
}

/// Removes the socket at `endpoint` and its lock file, as a [`SocketListener`]
/// does as it ends, for a listener that died before it could (killed). Both
/// are removed under the endpoint lock, so a live listener's files are never
/// removed and no listener binds the endpoint meanwhile. A path that is not a
/// socket is left in place.
///
/// # Errors
///
/// Returns [`OsError::EndpointHeld`] when a live listener holds the lock,
/// [`OsError::Security`] when `endpoint` names something other than a socket
/// (its lock file is still removed), and [`OsError::Io`] when a file cannot be
/// read or removed.
pub fn remove_abandoned_endpoint(endpoint: &Path) -> Result<(), OsError> {
    let lock = lock_endpoint(endpoint)?;
    let socket = match std::fs::symlink_metadata(endpoint) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(endpoint).map_err(OsError::Io)
        }
        Ok(_) => Err(OsError::Security(format!(
            "{} is not a socket; refusing to remove it",
            endpoint.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OsError::Io(error)),
    };
    let lock_file = std::fs::remove_file(&lock.path).map_err(OsError::Io);
    drop(lock);
    socket.and(lock_file)
}

/// Takes the lock on `endpoint` (`<endpoint>.lock`, created owner-only if
/// missing), blocking until every process holding it has let it go.
///
/// The wait has no bound: the kernel wakes it when the last descriptor
/// holding the lock closes. A signal that interrupts the wait does not end it.
/// `Ok(None)` means the lock was taken on a file its path no longer names: its
/// last holder removed it on the way out, so its clean-up is done.
///
/// # Errors
///
/// Returns [`OsError::Security`] when the lock path names something other
/// than a regular file, and [`OsError::Io`] when the lock file cannot be
/// opened, locked or looked up.
pub fn lock_endpoint_blocking(endpoint: &Path) -> Result<Option<EndpointLock>, OsError> {
    let lock_path = endpoint_lock_path(endpoint);
    let file = open_lock_file(&lock_path)?;
    loop {
        // SAFETY: `file` owns this open file descriptor for the whole call, and
        // flock only reads the descriptor and its flags.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            break;
        }
        let lock_error = io::Error::last_os_error();
        if lock_error.raw_os_error() != Some(libc::EINTR) {
            return Err(OsError::Io(lock_error));
        }
    }
    lock_if_current(file, &lock_path)
}

/// Removes an endpoint's files while the caller holds its lock: the record,
/// then the socket, then the lock file. The lock is not taken again (a new
/// open of the file could not take it), and stays with the caller.
///
/// The record goes first, so a removal that stops part-way leaves no record
/// to act on; the lock file goes last, so nobody locks the path meanwhile. A
/// file already gone counts as removed.
///
/// # Errors
///
/// Returns [`OsError::Io`] for the first file that cannot be removed; the
/// files after it are left in place.
pub fn remove_endpoint_files_under_lock(
    record: &Path,
    socket: &Path,
    lock_path: &Path,
) -> Result<(), OsError> {
    for path in [record, socket, lock_path] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(OsError::Io(error)),
        }
    }
    Ok(())
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
            socket_path: path.to_path_buf(),
            lock,
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

    fn server_process_id(&self, conn: &Self::ClientConnection) -> Option<u32> {
        peer_pid(conn)
    }
}

/// The lock on an endpoint, taken on the file its lock path named once the
/// lock was held. Held until dropped.
#[derive(Debug)]
pub struct EndpointLock {
    // Holds the lock until dropped.
    _file: std::fs::File,
    path: PathBuf,
}

/// The lock file of the endpoint `endpoint`: `<endpoint>.lock`.
#[must_use]
pub fn endpoint_lock_path(endpoint: &Path) -> PathBuf {
    let mut lock_path = endpoint.as_os_str().to_owned();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

/// Takes the lock that marks `endpoint` as owned by a live daemon, opening
/// (or creating, owner-only) `<endpoint>.lock` beside it.
///
/// A lock already held by another process is reported as
/// [`OsError::EndpointHeld`].
fn lock_endpoint(endpoint: &Path) -> Result<EndpointLock, OsError> {
    let lock_path = endpoint_lock_path(endpoint);
    for _ in 0..LOCK_ATTEMPTS {
        let file = open_lock_file(&lock_path)?;
        if let Some(lock) = take_lock_if_current(file, endpoint, &lock_path)? {
            return Ok(lock);
        }
    }
    Err(OsError::EndpointHeld(format!(
        "{} kept changing hands while its lock {} was being taken",
        endpoint.display(),
        lock_path.display()
    )))
}

/// Each retry needs a listener to have let the endpoint go in between, so a
/// few are plenty; the bound only keeps a churning endpoint from spinning.
const LOCK_ATTEMPTS: usize = 8;

fn open_lock_file(lock_path: &Path) -> Result<std::fs::File, OsError> {
    // O_NONBLOCK keeps a FIFO planted at the path from blocking the open.
    // O_CLOEXEC: the lock is handed on only on purpose, to the holder's
    // watcher; the window's command and its descendants never inherit it.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(lock_path)
        .map_err(OsError::Io)?;
    if !file.metadata().map_err(OsError::Io)?.is_file() {
        return Err(OsError::Security(format!(
            "{} is not a regular file; refusing to use it as the endpoint lock",
            lock_path.display()
        )));
    }
    Ok(file)
}

/// Locks `file`, opened from `lock_path`; `None` when the lock was taken on a
/// file that is no longer the one `lock_path` names (its holder removed it on
/// the way out), which excludes nobody.
fn take_lock_if_current(
    file: std::fs::File,
    endpoint: &Path,
    lock_path: &Path,
) -> Result<Option<EndpointLock>, OsError> {
    // SAFETY: `file` owns this open file descriptor for the whole call, and
    // flock only reads the descriptor and its flags.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let lock_error = io::Error::last_os_error();
        if lock_error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(OsError::EndpointHeld(format!(
                "{} is held by a live listener (its lock {} is taken)",
                endpoint.display(),
                lock_path.display()
            )));
        }
        return Err(OsError::Io(lock_error));
    }
    lock_if_current(file, lock_path)
}

/// Keeps the lock held on `file`, opened from `lock_path`, when `lock_path`
/// still names that file; `None` (dropping the lock) when it does not.
fn lock_if_current(file: std::fs::File, lock_path: &Path) -> Result<Option<EndpointLock>, OsError> {
    let locked = file.metadata().map_err(OsError::Io)?;
    let named = match std::fs::symlink_metadata(lock_path) {
        Ok(named) => named,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(OsError::Io(error)),
    };
    if (locked.dev(), locked.ino()) != (named.dev(), named.ino()) {
        return Ok(None);
    }
    Ok(Some(EndpointLock {
        _file: file,
        path: lock_path.to_path_buf(),
    }))
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

/// The pid the kernel recorded for the peer of `conn`, so an unrelated
/// process cannot answer for the peer by reusing a pid the peer merely named.
#[cfg(target_os = "linux")]
fn peer_pid(conn: &UnixStream) -> Option<u32> {
    // SO_PEERCRED reports 0 for a peer outside this pid namespace: unknown.
    u32::try_from(peer_credentials(conn)?.pid)
        .ok()
        .filter(|&pid| pid != 0)
}

#[cfg(target_os = "macos")]
fn peer_pid(conn: &UnixStream) -> Option<u32> {
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
    u32::try_from(pid).ok().filter(|&pid| pid != 0)
}

/// Neither peer-pid source is portable across Unix: `SO_PEERCRED` is
/// Linux-specific and `LOCAL_PEERPID` is macOS-specific, so every other Unix
/// target fails closed here rather than failing to compile.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_pid(_conn: &UnixStream) -> Option<u32> {
    None
}

/// Reads the peer's executable through [`peer_pid`].
#[cfg(target_os = "linux")]
fn peer_executable_path(conn: &UnixStream) -> Option<PathBuf> {
    let pid = peer_pid(conn)?;
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn peer_executable_path(conn: &UnixStream) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let pid = libc::pid_t::try_from(peer_pid(conn)?).ok()?;
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_executable_path(_conn: &UnixStream) -> Option<PathBuf> {
    None
}

// =========================================================================
// A holder and its watcher
// =========================================================================

/// The most descriptors one received message may carry; more are an error.
const MOST_RECEIVED_FDS: usize = 256;

/// A connected pair of stream sockets, both close-on-exec.
///
/// # Errors
///
/// Returns the operating system's error when the pair cannot be made.
pub fn socket_pair() -> io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let (one, other) = std::os::unix::net::UnixStream::pair()?;
    Ok((one.into(), other.into()))
}

/// The size of a control buffer for `fds` descriptors.
fn control_space(fds: usize) -> usize {
    let bytes = libc::c_uint::try_from(fds * std::mem::size_of::<libc::c_int>())
        .unwrap_or(libc::c_uint::MAX);
    // SAFETY: CMSG_SPACE only computes a size from its argument.
    unsafe { libc::CMSG_SPACE(bytes) as usize }
}

#[cfg(target_os = "linux")]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
// A peer gone raises SIGPIPE here, which a Rust program ignores from its start.
#[cfg(not(target_os = "linux"))]
const SEND_FLAGS: libc::c_int = 0;

/// Writes all of `bytes` to the stream socket `socket`, with `fds` passed
/// beside the first byte (`SCM_RIGHTS`).
///
/// # Errors
///
/// Returns the operating system's error, such as `EPIPE` once the peer is
/// gone and `EINVAL` for more descriptors than one message carries.
pub fn send_with_descriptors(
    socket: std::os::fd::BorrowedFd<'_>,
    bytes: &[u8],
    fds: &[std::os::fd::BorrowedFd<'_>],
) -> io::Result<()> {
    let raw: Vec<libc::c_int> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    let mut control = vec![0u64; control_space(raw.len()).div_ceil(8)];
    let mut rest = bytes;
    let mut first = true;
    while !rest.is_empty() {
        let mut iov = libc::iovec {
            iov_base: rest.as_ptr().cast_mut().cast(),
            iov_len: rest.len(),
        };
        // SAFETY: an all-zero msghdr is an empty header, filled in below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        if first && !raw.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = control_space(raw.len()) as _;
            let data_len = libc::c_uint::try_from(raw.len() * std::mem::size_of::<libc::c_int>())
                .unwrap_or(libc::c_uint::MAX);
            // SAFETY: the control buffer is sized for one header followed by
            // `raw`, so CMSG_FIRSTHDR of it is not null and the copy fits.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&raw const msg);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(data_len) as _;
                std::ptr::copy_nonoverlapping(
                    raw.as_ptr(),
                    libc::CMSG_DATA(header).cast::<libc::c_int>(),
                    raw.len(),
                );
            }
        }
        // SAFETY: `msg` points at buffers that outlive the call.
        let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &raw const msg, SEND_FLAGS) };
        let Ok(sent) = usize::try_from(sent) else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        };
        first = false;
        rest = rest.get(sent..).unwrap_or_default();
    }
    Ok(())
}

/// Reads what the stream socket `socket` has into `buf`, waiting for some,
/// and adds the descriptors passed beside it to `fds`, each close-on-exec.
/// Returns how many bytes were read: `0` at the end of the stream.
///
/// # Errors
///
/// Returns the operating system's error, and an error when the message
/// carried more descriptors than this side takes at once (256).
pub fn receive_with_descriptors(
    socket: std::os::fd::BorrowedFd<'_>,
    buf: &mut [u8],
    fds: &mut Vec<std::os::fd::OwnedFd>,
) -> io::Result<usize> {
    use std::os::fd::FromRawFd as _;

    let mut control = vec![0u64; control_space(MOST_RECEIVED_FDS).div_ceil(8)];
    loop {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        // SAFETY: an all-zero msghdr is an empty header, filled in below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = (control.len() * 8) as _;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        // SAFETY: `msg` points at buffers that outlive the call.
        let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut msg, flags) };
        let Ok(read) = usize::try_from(read) else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        };
        let mut received = Vec::new();
        // SAFETY: recvmsg filled `msg`; the walk stays within its control
        // buffer, and each descriptor read is owned from here on.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&raw const msg);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
                    let len = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    for index in 0..len / std::mem::size_of::<libc::c_int>() {
                        received.push(std::os::fd::OwnedFd::from_raw_fd(
                            data.add(index).read_unaligned(),
                        ));
                    }
                }
                header = libc::CMSG_NXTHDR(&raw const msg, header);
            }
        }
        #[cfg(not(target_os = "linux"))]
        for fd in &received {
            set_cloexec(fd.as_raw_fd())?;
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a message carried more descriptors than can be taken",
            ));
        }
        fds.extend(received);
        return Ok(read);
    }
}

fn set_cloexec(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: F_GETFD and F_SETFD only read and set this process's fd flag.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Starts `program`, a holder's watcher, with `socket` as its one descriptor
/// beside its standard three, which are on `/dev/null`, and the descriptor's
/// number as its only argument. It starts with an empty environment, in this
/// process's session and process group.
///
/// # Errors
///
/// Returns an error when the program cannot be started, and off Linux when
/// no bound on the descriptors to keep from it can be read.
// The OS boundary owns this spawn: the holder that calls it is the watcher's
// parent and reaper, and the ban in `clippy.toml` keeps every other call site
// from creating a child of its own.
#[allow(clippy::disallowed_methods)]
pub fn spawn_watcher(
    program: &Path,
    socket: std::os::fd::BorrowedFd<'_>,
) -> io::Result<std::process::Child> {
    let fd = socket.as_raw_fd();
    let mut marker = UnnamedFdMarker::prepare()?;
    let mut command = Command::new(program);
    command
        .arg(fd.to_string())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child between fork and exec. It
    // calls only fcntl(2) and what `UnnamedFdMarker::mark` calls, each a bare
    // system call there; it allocates nothing, takes no lock, and touches only
    // `fd` and `marker`, moved in from the parent.
    unsafe {
        command.pre_exec(move || {
            marker.mark()?;
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Takes the descriptor `fd` this process was started with (by
/// [`spawn_watcher`]) as its own, close-on-exec, and closes every other
/// descriptor from 3 up.
///
/// # Errors
///
/// Returns an error when `fd` is not an open socket or the descriptors
/// cannot be listed.
pub fn take_inherited_socket(fd: libc::c_int) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd as _;

    if fd < FIRST_UNNAMED_FD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a standard stream is not the watcher's socket",
        ));
    }
    // SAFETY: zeroed is a valid `stat`, which fstat only writes.
    let mut meta: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `meta` is a live local of the type fstat writes.
    if unsafe { libc::fstat(fd, &mut meta) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if meta.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the watcher's descriptor is not a socket",
        ));
    }
    set_cloexec(fd)?;
    let open: Vec<libc::c_int> = std::fs::read_dir("/dev/fd")?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    for other in open {
        if other >= FIRST_UNNAMED_FD && other != fd {
            // SAFETY: closing a number nothing in this process owns yet; the
            // listing's own number is closed already and fails harmlessly.
            unsafe { libc::close(other) };
        }
    }
    // SAFETY: `fd` is an open socket that nothing else in this process owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// The soft `RLIMIT_NOFILE` this process had before [`raise_descriptor_limit`]
/// first raised it: what the children it starts get back.
static SOFT_LIMIT_BEFORE_RAISE: std::sync::OnceLock<libc::rlim_t> = std::sync::OnceLock::new();

/// Raises this process's soft `RLIMIT_NOFILE` as far as it goes (its hard
/// limit, on macOS no further than `kern.maxfilesperproc`) and returns the
/// soft limit then in force.
///
/// The children this process starts afterwards through this module start
/// with the soft limit from before the first raise, as a program written for
/// the default limit (one that sizes a table by it, or calls `select`)
/// expects.
///
/// # Errors
///
/// Returns the operating system's error when the limit cannot be read.
pub fn raise_descriptor_limit() -> io::Result<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live out-parameter of the type getrlimit(2) writes.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut wanted = limit.rlim_max;
    if let Some(most) = max_files_per_proc().and_then(|most| libc::rlim_t::try_from(most).ok()) {
        wanted = wanted.min(most);
    }
    if wanted > limit.rlim_cur {
        let raised = libc::rlimit {
            rlim_cur: wanted,
            rlim_max: limit.rlim_max,
        };
        // SAFETY: `raised` is a live local of the type setrlimit(2) reads.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            let _ = SOFT_LIMIT_BEFORE_RAISE.set(limit.rlim_cur);
            limit.rlim_cur = wanted;
        }
    }
    Ok(limit.rlim_cur)
}

/// How many more descriptors this process may open before its soft
/// `RLIMIT_NOFILE`, keeping `margin` of them aside.
///
/// # Errors
///
/// Returns the operating system's error when the limit or the open
/// descriptors cannot be read.
pub fn spare_descriptors(margin: usize) -> io::Result<usize> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live out-parameter of the type getrlimit(2) writes.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let open = std::fs::read_dir("/dev/fd")?.count();
    let soft = usize::try_from(limit.rlim_cur).unwrap_or(usize::MAX);
    Ok(soft.saturating_sub(open).saturating_sub(margin))
}

/// Whether the peer of the stream socket `socket` has closed it, read without
/// waiting: what is still unread on it is dropped. An error reading counts
/// as closed.
#[must_use]
pub fn peer_has_closed(socket: std::os::fd::BorrowedFd<'_>) -> bool {
    let mut scratch = [0u8; 4096];
    loop {
        // SAFETY: `scratch` is a live buffer of the length passed.
        let read = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                scratch.as_mut_ptr().cast(),
                scratch.len(),
                libc::MSG_DONTWAIT,
            )
        };
        match read {
            0 => return true,
            1.. => {}
            _ => {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => {}
                    io::ErrorKind::WouldBlock => return false,
                    _ => return true,
                }
            }
        }
    }
}

// =========================================================================
// schedule_force_kill
// =========================================================================

/// Unix branch of [`crate::schedule_force_kill`].
///
/// The wait happens on a detached thread so the caller never blocks for the
/// grace period; the decision itself is `force_kill::escalate_after_grace`,
/// which sends under `gate`'s lock only while the child is unreaped, so the
/// pid still names the child when the signal goes out.
///
/// `kill` rather than `killpg`, because this escalation is about the single
/// child the caller spawned; its descendants are the process group's business.
/// The result is discarded: the caller learns whether the child is gone from
/// its own exit observation, not from here.
pub fn schedule_force_kill(pid: u32, grace: Duration, gate: Arc<ReapGate>) {
    std::thread::spawn(move || {
        crate::force_kill::escalate_after_grace(grace, &gate, std::thread::sleep, || {
            // SAFETY: `kill` signals the single process `pid`, which the gate
            // has not seen reaped, so it is still the caller's child;
            // `SIGKILL` is a constant.
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        });
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

// =========================================================================
// What a spawned child inherits
// =========================================================================

/// The lowest fd a child never names: 0-2 are its stdio.
const FIRST_UNNAMED_FD: libc::c_int = 3;

/// Makes `command`'s child lead a new session and inherit no fd beyond its
/// stdio. Without `FD_CLOEXEC` at fork time, an fd another thread is still
/// setting up (a pipe or a PTY pair) would otherwise survive the exec.
///
/// # Errors
///
/// Off Linux, returns an I/O error when no finite bound on the fd numbers can
/// be read (see [`unnamed_fd_bound`]), since the child could not cover them,
/// and everywhere when the limit to give back to the child cannot be read.
fn start_in_new_session_with_only_stdio(command: &mut Command) -> io::Result<()> {
    let mut marker = UnnamedFdMarker::prepare()?;
    let restored = limit_to_restore()?;
    // SAFETY: the closure runs in the forked child between fork and exec. It
    // calls only setsid(2), setrlimit(2) and what `UnnamedFdMarker::mark`
    // calls, each a bare system call there; it allocates nothing, takes no
    // lock, and touches only `marker` and `restored`, moved in from the parent.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            marker.mark()?;
            // A child left with the raised limit would size its tables by it or
            // pass fds past FD_SETSIZE to select, so it is not started at all.
            if let Some(restored) = restored
                && libc::setrlimit(libc::RLIMIT_NOFILE, &restored) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

/// The `RLIMIT_NOFILE` a child gets back when [`raise_descriptor_limit`] has
/// raised this process's soft limit: the soft value from before, under the
/// hard value now in force. Read in the parent because the child must not
/// allocate or lock.
fn limit_to_restore() -> io::Result<Option<libc::rlimit>> {
    let Some(&before) = SOFT_LIMIT_BEFORE_RAISE.get() else {
        return Ok(None);
    };
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live out-parameter of the type getrlimit(2) writes.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let before = before.min(limit.rlim_max);
    Ok((before < limit.rlim_cur).then_some(libc::rlimit {
        rlim_cur: before,
        rlim_max: limit.rlim_max,
    }))
}

/// One past the highest fd number this process can hold: the `RLIMIT_NOFILE`
/// soft value, read in the parent because the child must not allocate.
fn unnamed_fd_bound() -> Option<libc::c_int> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live out-parameter of the type getrlimit(2) writes.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
    if read && limit.rlim_cur != libc::RLIM_INFINITY {
        // Every fd number is a c_int, so a larger soft value covers them all.
        return Some(libc::c_int::try_from(limit.rlim_cur).unwrap_or(libc::c_int::MAX));
    }
    max_files_per_proc()
}

/// macOS caps the fds of a process at `kern.maxfilesperproc` whatever the
/// soft limit says, so it bounds the scan when the soft value does not.
#[cfg(target_os = "macos")]
fn max_files_per_proc() -> Option<libc::c_int> {
    let mut mib = [libc::CTL_KERN, libc::KERN_MAXFILESPERPROC];
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `mib` names a c_int-valued sysctl, `value` and `len` are live
    // out-parameters sized for it, and nothing is written to the kernel.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut value).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    (read && value > 0).then_some(value)
}

#[cfg(not(target_os = "macos"))]
fn max_files_per_proc() -> Option<libc::c_int> {
    None
}

/// What a child needs to mark every fd it was not named close-on-exec,
/// prepared in the parent because the child must not allocate.
struct UnnamedFdMarker {
    /// One past the highest fd number at the fork (see [`unnamed_fd_bound`]).
    /// It stays the raised soft value: it must cover every fd this process
    /// may hold, not only those the child may open. Linux needs it only if
    /// close_range fails, which only the child sees.
    #[cfg(target_os = "linux")]
    bound: Option<libc::c_int>,
    #[cfg(not(target_os = "linux"))]
    bound: libc::c_int,
    /// Room for the child to list its open fds, so marking costs what is open
    /// rather than what the soft limit allows: macOS has no close_range.
    #[cfg(target_os = "macos")]
    listing: Vec<libc::proc_fdinfo>,
}

impl UnnamedFdMarker {
    /// Parent side.
    ///
    /// # Errors
    ///
    /// Off Linux, returns an I/O error when no finite bound on the fd numbers
    /// can be read, since the child could not cover them.
    fn prepare() -> io::Result<Self> {
        let bound = unnamed_fd_bound();
        #[cfg(not(target_os = "linux"))]
        let bound = bound.ok_or_else(|| {
            io::Error::other(
                "no finite RLIMIT_NOFILE bound to mark inherited fds close-on-exec up to",
            )
        })?;
        Ok(Self {
            bound,
            #[cfg(target_os = "macos")]
            listing: fd_listing_room(),
        })
    }

    /// Child side: marks every fd from [`FIRST_UNNAMED_FD`] up close-on-exec
    /// in one close_range(2). std's own fds there are close-on-exec already,
    /// so only the ones that would have leaked change.
    #[cfg(target_os = "linux")]
    fn mark(&mut self) -> io::Result<()> {
        // The raw syscall, not libc's close_range: that wrapper needs glibc 2.34.
        // SAFETY: close_range(2) with CLOSE_RANGE_CLOEXEC only sets fd flags of
        // this process; its arguments are plain integers.
        let marked = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                FIRST_UNNAMED_FD as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0;
        if marked {
            return Ok(());
        }
        // ENOSYS before 5.9, EINVAL before 5.11 (no CLOSE_RANGE_CLOEXEC), EPERM
        // under seccomp: the loop reaches the same state, and marking is idempotent.
        let error = io::Error::last_os_error();
        match self.bound {
            Some(bound) => {
                mark_cloexec_from(FIRST_UNNAMED_FD, bound);
                Ok(())
            }
            None => Err(error),
        }
    }

    /// Child side: marks the open fds this process lists close-on-exec. A
    /// listing that fails is replaced by trying every number below the bound,
    /// and one that fills its room is completed that way above its last fd:
    /// the kernel lists fds in ascending order.
    #[cfg(target_os = "macos")]
    fn mark(&mut self) -> io::Result<()> {
        let Some(count) = list_own_fds(&mut self.listing) else {
            mark_cloexec_from(FIRST_UNNAMED_FD, self.bound);
            return Ok(());
        };
        let mut next = FIRST_UNNAMED_FD;
        for info in &self.listing[..count] {
            mark_cloexec(info.proc_fd);
            next = next.max(info.proc_fd.saturating_add(1));
        }
        if count == self.listing.len() {
            mark_cloexec_from(next, self.bound);
        }
        Ok(())
    }

    /// Child side: tries every number below the bound.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn mark(&mut self) -> io::Result<()> {
        mark_cloexec_from(FIRST_UNNAMED_FD, self.bound);
        Ok(())
    }
}

/// Entries kept beyond the kernel's estimate for fds other threads open
/// between the estimate and the fork.
#[cfg(target_os = "macos")]
const FD_LISTING_MARGIN: usize = 32;

/// Parent side: room for a listing of this process's fds. The kernel's
/// estimate covers its whole fd table; empty when it cannot be read, which
/// makes the child try every number instead.
#[cfg(target_os = "macos")]
fn fd_listing_room() -> Vec<libc::proc_fdinfo> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: a NULL, zero-length query only asks for the size a listing
    // would need; it writes through no pointer.
    let estimate = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    let estimate = usize::try_from(estimate).unwrap_or(0);
    if estimate == 0 {
        return Vec::new();
    }
    let most = usize::try_from(libc::c_int::MAX).unwrap_or(0) / entry;
    let entries = (estimate / entry)
        .saturating_add(FD_LISTING_MARGIN)
        .min(most);
    vec![
        libc::proc_fdinfo {
            proc_fd: -1,
            proc_fdtype: 0,
        };
        entries
    ]
}

/// Child side: lists this process's open fds into `listing` in ascending
/// order and returns how many it wrote, or `None` when they cannot be listed.
#[cfg(target_os = "macos")]
fn list_own_fds(listing: &mut [libc::proc_fdinfo]) -> Option<usize> {
    let bytes = libc::c_int::try_from(std::mem::size_of_val(listing)).ok()?;
    if bytes == 0 {
        return None;
    }
    // SAFETY: `listing` is a live buffer of `bytes` bytes of the kernel's own
    // listing type, and at most that is written. Both calls are bare system
    // calls: libproc's proc_pidinfo only calls __proc_info (xnu
    // `libproc.c`), and its kernel side, proc_pidfdlist (`proc_info.c`),
    // allocates and locks only inside the kernel.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            listing.as_mut_ptr().cast(),
            bytes,
        )
    };
    let written = usize::try_from(written)
        .ok()
        .filter(|&written| written > 0)?;
    Some((written / std::mem::size_of::<libc::proc_fdinfo>()).min(listing.len()))
}

/// Child side: marks each open fd in `first..bound` close-on-exec.
fn mark_cloexec_from(first: libc::c_int, bound: libc::c_int) {
    for fd in first..bound {
        mark_cloexec(fd);
    }
}

/// Child side: sets `FD_CLOEXEC` on `fd` if it is open, unnamed and lacks it,
/// so exec closes it.
fn mark_cloexec(fd: libc::c_int) {
    if fd < FIRST_UNNAMED_FD {
        return;
    }
    // SAFETY: F_GETFD and F_SETFD only read and set this process's fd flag; a
    // number that is not open makes F_GETFD return -1.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Unix branch of [`crate::spawn_daemon_detached`]: `setsid` detaches the
/// daemon into its own session (so it survives the caller's controlling
/// terminal going away), it inherits no fd beyond its null stdio, and a
/// reaper thread waits on it so it never lingers as a zombie. Always uses
/// this one mode -- there is no weaker Unix
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
    start_in_new_session_with_only_stdio(&mut command).map_err(OsError::Io)?;

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

/// Unix branch of [`crate::spawn_detached_process`]: a new session, as for
/// [`spawn_daemon_detached`], with pipes as its standard input and output and
/// no fd beyond its three standard ones. The caller waits on it.
///
/// # Errors
///
/// As for [`crate::spawn_detached_process`].
// The OS boundary owns this detached spawn; the ban in `clippy.toml` keeps
// every other call site from creating a child of its own.
#[allow(clippy::disallowed_methods)]
pub fn spawn_detached_process(
    path: &Path,
    args: &[&OsStr],
    cwd: Option<&Path>,
    stderr: crate::DetachedStderr,
) -> Result<crate::DetachedProcess, OsError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(match stderr {
            crate::DetachedStderr::Inherit => Stdio::inherit(),
            crate::DetachedStderr::Null => Stdio::null(),
            crate::DetachedStderr::File(file) => Stdio::from(file),
        });
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    start_in_new_session_with_only_stdio(&mut command).map_err(OsError::Io)?;
    let mut child = command.spawn().map_err(OsError::Io)?;
    let stdin = child
        .stdin
        .take()
        .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe)));
    let stdout = child
        .stdout
        .take()
        .map(|pipe| std::fs::File::from(std::os::fd::OwnedFd::from(pipe)));
    Ok(crate::DetachedProcess::new(child, stdin, stdout))
}

// =========================================================================
// spawn_captured: one-shot, non-PTY process capture
// =========================================================================

/// Unix branch of [`crate::capture::spawn_captured`]: starts `argv[0]` in a
/// new session with no controlling terminal (`setsid` before `exec` runs in
/// the child, which also makes it the leader of a fresh process group that a
/// descendant it forks before this function even returns still inherits),
/// and takes that group as [`UnixProcessGroup::led_by`] the child's own pid --
/// so tearing the tree down is just dropping [`UnixProcessGroup`]. The child
/// inherits no fd beyond its three standard ones.
///
/// # Errors
///
/// Returns whatever [`std::process::Command::spawn`] returns, an I/O error
/// when no bound on the fds to keep from the child can be read, and an I/O
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
    start_in_new_session_with_only_stdio(&mut command)?;

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

    // A listener removes its lock file as it ends, so a file opened just before
    // that is no longer the endpoint's lock; locking it must not count, or two
    // processes would each hold a lock on the same endpoint.
    #[test]
    fn a_lock_on_a_file_the_lock_path_no_longer_names_is_not_the_endpoint_lock() {
        let dir = yagura_test_support::paths::short_tmp_dir("os-unix", "lock-inode");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = dir.path().join("e.sock");
        let lock_path = endpoint_lock_path(&endpoint);

        let removed = open_lock_file(&lock_path).unwrap();
        std::fs::remove_file(&lock_path).unwrap();
        assert!(
            take_lock_if_current(removed, &endpoint, &lock_path)
                .unwrap()
                .is_none(),
            "a lock on a removed lock file was taken as the endpoint's"
        );

        let replaced = open_lock_file(&lock_path).unwrap();
        std::fs::remove_file(&lock_path).unwrap();
        let current = lock_endpoint(&endpoint).unwrap();
        assert!(
            take_lock_if_current(replaced, &endpoint, &lock_path)
                .unwrap()
                .is_none(),
            "a lock on a replaced lock file was taken while the current one is held"
        );
        drop(current);
    }

    // A killed listener leaves its socket and lock behind; they are another
    // process's to remove only once no live listener holds the lock.
    #[test]
    fn an_abandoned_endpoint_is_removed_only_once_its_lock_is_free() {
        let dir = yagura_test_support::paths::short_tmp_dir("os-unix", "abandoned");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = dir.path().join("e.sock");
        let lock_path = endpoint_lock_path(&endpoint);
        drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
        let live = lock_endpoint(&endpoint).unwrap();

        assert!(matches!(
            remove_abandoned_endpoint(&endpoint),
            Err(OsError::EndpointHeld(_))
        ));
        assert!(
            endpoint.exists() && lock_path.exists(),
            "a live listener's files are kept"
        );

        drop(live);
        remove_abandoned_endpoint(&endpoint).unwrap();
        assert!(
            !endpoint.exists() && !lock_path.exists(),
            "the files of a listener that is gone are removed"
        );
    }

    // The fallback directory may already exist (a concurrent creator won the
    // race); only an owner-only directory of ours may then be used.
    #[test]
    fn an_existing_fallback_directory_is_used_only_when_it_is_ours_and_owner_only() {
        let root = yagura_test_support::paths::short_tmp_dir("os-unix", "holders-fallback");
        let namespace = crate::resolve_instance_from(None).unwrap();
        let too_long = PathBuf::from(format!("/{}", "a".repeat(200)));
        let fallback = holder_fallback_dir(root.path(), "1000");
        let resolve = || holder_socket_path("1000", &namespace, &too_long, root.path(), "w1");

        std::fs::create_dir(&fallback).unwrap();
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(resolve().is_ok(), "an existing 0700 directory of ours");

        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(resolve().is_err(), "a directory open to group and others");

        let target = root.path().join("target");
        std::fs::remove_dir(&fallback).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, &fallback).unwrap();
        assert!(resolve().is_err(), "a symlink");

        std::fs::remove_file(&fallback).unwrap();
        std::fs::write(&fallback, b"x").unwrap();
        assert!(resolve().is_err(), "a regular file");
    }

    // The listing can fail or fill its room (fds opened by other threads after
    // the parent sized it); either way the fds it did not list still must not
    // reach the child.
    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn a_child_inherits_no_unnamed_fd_when_its_fd_listing_fails_or_fills_its_room() {
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;

        // High enough that the shell has nothing of its own there, under the
        // 256 a default soft limit allows.
        let leaked: Vec<OwnedFd> = [200, 210, 220]
            .into_iter()
            .map(|at| {
                let null = std::fs::File::open("/dev/null").unwrap();
                // SAFETY: F_DUPFD only creates a new fd in this process.
                let fd = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, at) };
                assert!(fd >= at, "{}", io::Error::last_os_error());
                // SAFETY: `fd` was just created and nothing else owns it.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                // SAFETY: F_SETFD only clears this process's flag on its own fd.
                assert_eq!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, 0) }, 0);
                fd
            })
            .collect();
        let numbers: Vec<String> = leaked.iter().map(|fd| fd.as_raw_fd().to_string()).collect();
        let script = format!(
            "for n in {}; do [ -e /dev/fd/$n ] && echo $n; done; exit 0",
            numbers.join(" ")
        );
        let unlisted = Vec::new();
        let room_for_one = vec![
            libc::proc_fdinfo {
                proc_fd: -1,
                proc_fdtype: 0,
            };
            1
        ];
        for (case, listing) in [("failed", unlisted), ("full", room_for_one)] {
            let mut marker = UnnamedFdMarker {
                bound: unnamed_fd_bound().unwrap(),
                listing,
            };
            let mut command = Command::new("/bin/sh");
            command.args(["-c", &script]).stdin(Stdio::null());
            // SAFETY: the closure runs in the forked child and only marks fds,
            // as the spawns of this module do.
            unsafe {
                command.pre_exec(move || marker.mark());
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{case}: {output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "",
                "{case} listing: the child inherited fds it was not named"
            );
        }
        drop(leaked);
    }
}
