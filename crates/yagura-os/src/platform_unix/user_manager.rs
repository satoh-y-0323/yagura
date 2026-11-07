//! The user's systemd user manager, reached over the user bus: the scopes a
//! window's holder and child are moved into, by pidfd, and the kill of a
//! child's scope by name once its holder is gone.
//!
//! Only unit names this crate makes, fixed properties and pidfds are sent: no
//! command, path or string a user chose reaches the user manager. The bus
//! client starts no process (no `autolaunch:` or `unixexec:` transport is built
//! in), so a machine without a reachable user bus only gets an error back.
//! Nothing here waits on a clock: a request ends with its job's own end.

use std::io;
use std::os::fd::{BorrowedFd, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, Variant};

use crate::OsError;
use crate::ReapGate;
use crate::instance::InstanceNamespace;

const SYSTEMD: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const SCOPE_SUFFIX: &str = ".scope";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// A scope is garbage-collected once it is empty, whether it stopped cleanly
/// or not, so a window's names never linger in the user manager.
const COLLECT_MODE: &str = "inactive-or-failed";
/// Bytes of randomness in each pair of names: new for every holder start.
const NAME_RANDOM_BYTES: usize = 8;

/// The scope names of one start of a window's holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeNames {
    /// The scope holding the holder alone.
    pub holder: String,
    /// The scope holding the window's child and its tree, bound to the
    /// holder's.
    pub child: String,
}

/// The prefix every scope name of `namespace` starts with.
fn scope_prefix(namespace: &InstanceNamespace) -> String {
    let mut prefix: String = namespace
        .name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    prefix.push('_');
    prefix
}

/// Makes the scope names of a new start of `window_id`'s holder: letters,
/// digits and `_` followed by `.scope`, from the instance's namespace, the
/// window id and randomness drawn here, so no start ever reuses a name.
///
/// # Errors
///
/// [`OsError::Security`] when `window_id` holds anything but ASCII letters,
/// digits, `-` and `_`, or when no randomness can be drawn.
pub fn scope_names(namespace: &InstanceNamespace, window_id: &str) -> Result<ScopeNames, OsError> {
    let plain = !window_id.is_empty()
        && window_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !plain {
        return Err(OsError::Security(
            "a window id is not a plain identifier".to_owned(),
        ));
    }
    let mut random = [0u8; NAME_RANDOM_BYTES];
    crate::fill_random(&mut random)?;
    let random: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let window: String = window_id.replace('-', "_");
    let stem = format!("{}{window}_{random}", scope_prefix(namespace));
    Ok(ScopeNames {
        holder: format!("{stem}_holder{SCOPE_SUFFIX}"),
        child: format!("{stem}_child{SCOPE_SUFFIX}"),
    })
}

/// Whether `name` has the form of a scope name [`scope_names`] makes for
/// `namespace`. A name read back from a file any process of the user can
/// write is checked with this before it is sent.
#[must_use]
pub fn is_window_scope_name(namespace: &InstanceNamespace, name: &str) -> bool {
    name.strip_suffix(SCOPE_SUFFIX).is_some_and(|stem| {
        stem.starts_with(&scope_prefix(namespace))
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

/// Opens a pidfd for the process `pid`.
///
/// # Errors
///
/// The operating system's error: `ESRCH` when no process has the id,
/// `ENOSYS` on a kernel without pidfds.
pub fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let target = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a process id out of range"))?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor
    // or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, target, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = libc::c_int::try_from(fd)
        .map_err(|_| io::Error::other("pidfd_open returned an out-of-range descriptor"))?;
    // SAFETY: the descriptor was just returned to this function alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Pidfds of this process's children other than `leader`, opened under
/// `gate`'s lock, the lock the orphans are reaped under, so no listed id is
/// reaped and reused before its pidfd names it. A child gone meanwhile is
/// left out.
///
/// # Errors
///
/// The error listing the children or opening a pidfd fails with, other than
/// for a child already gone.
pub fn orphan_pidfds(leader: u32, gate: &ReapGate) -> io::Result<Vec<OwnedFd>> {
    gate.hold(|_| {
        let mut fds = Vec::new();
        for pid in super::exit_watch::child_pids()? {
            if pid == leader {
                continue;
            }
            match pidfd_open(pid) {
                Ok(fd) => fds.push(fd),
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(fds)
    })
}

/// The unified cgroup directory of the process `pid`, from its
/// `/proc/<pid>/cgroup`; `None` when the process is gone or has none. The
/// caller keeps `pid` from being reused meanwhile (an unreaped child).
///
/// # Errors
///
/// The error reading the file fails with, other than for a process gone.
pub fn cgroup_dir_of(pid: u32) -> io::Result<Option<PathBuf>> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/cgroup")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .filter(|path| path.starts_with('/'))
        .map(|path| Path::new(CGROUP_ROOT).join(path.trim_start_matches('/'))))
}

/// Kills every process of the cgroup `dir` through its `cgroup.kill`, which
/// closes the race with a member forking. Returns `false`, doing nothing, on a
/// kernel without the file.
///
/// # Errors
///
/// The error writing the file fails with otherwise.
pub fn kill_cgroup(dir: &Path) -> io::Result<bool> {
    match std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("cgroup.kill"))
    {
        Ok(mut file) => {
            io::Write::write_all(&mut file, b"1")?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// How many of this process's descendants are outside the cgroup `dir`.
///
/// # Errors
///
/// The error listing this process's children fails with.
pub fn descendants_outside(dir: &Path) -> io::Result<usize> {
    let mut outside = 0;
    let mut pending = super::exit_watch::child_pids()?;
    while let Some(pid) = pending.pop() {
        // A process gone meanwhile is no longer a descendant.
        let Ok(Some(cgroup)) = cgroup_dir_of(pid) else {
            continue;
        };
        if cgroup != dir {
            outside += 1;
        }
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            pending.extend(
                children
                    .split_whitespace()
                    .filter_map(|id| id.parse::<u32>().ok()),
            );
        }
    }
    Ok(outside)
}

/// Why a request to the user manager came to nothing.
#[derive(Debug, thiserror::Error)]
pub enum UserManagerError {
    /// The bus could not be reached, or refused or failed the call.
    #[error("the user bus: {0}")]
    Bus(#[from] zbus::Error),
    /// The unit's job ended other than `done`.
    #[error("the unit's job ended {0}")]
    Job(String),
    /// The bus closed before the unit's job ended.
    #[error("the user bus closed before the unit's job ended")]
    Closed,
}

/// A connection to the user's systemd user manager.
pub struct UserManager {
    conn: Connection,
}

impl UserManager {
    /// Connects to the user bus the process's environment leads to, as
    /// `systemd-run --user` would (`DBUS_SESSION_BUS_ADDRESS`, else
    /// `$XDG_RUNTIME_DIR/bus`, else `/run/user/<uid>/bus`), and asks for the
    /// manager's job signals.
    ///
    /// # Errors
    ///
    /// [`UserManagerError::Bus`] when no bus is reached or the manager does
    /// not answer.
    pub async fn connect() -> Result<Self, UserManagerError> {
        let conn = Connection::session().await?;
        conn.call_method(Some(SYSTEMD), MANAGER_PATH, Some(MANAGER), "Subscribe", &())
            .await?;
        Ok(Self { conn })
    }

    /// Moves the process `holder` names into a new scope `name` holding it
    /// alone, and returns once the job is done.
    ///
    /// # Errors
    ///
    /// [`UserManagerError`] when the request is refused (a process already
    /// gone, a name in use, a systemd without `PIDFDs`) or its job ends other
    /// than `done`.
    pub async fn start_holder_scope(
        &self,
        name: &str,
        holder: BorrowedFd<'_>,
    ) -> Result<(), UserManagerError> {
        let properties = vec![
            ("PIDFDs", Variant::from(vec![zbus::Fd::from(holder)])),
            ("CollectMode", Variant::from(COLLECT_MODE)),
        ];
        self.start_scope(name, properties).await
    }

    /// Moves the processes `members` names into a new scope `name` bound to
    /// `holder_scope`: once that scope stops, every process in this one is
    /// killed with `SIGKILL`. Returns once the job is done.
    ///
    /// # Errors
    ///
    /// As [`UserManager::start_holder_scope`].
    pub async fn start_child_scope(
        &self,
        name: &str,
        holder_scope: &str,
        members: &[BorrowedFd<'_>],
    ) -> Result<(), UserManagerError> {
        let fds: Vec<zbus::Fd<'_>> = members.iter().map(|fd| zbus::Fd::from(*fd)).collect();
        let properties = vec![
            ("PIDFDs", Variant::from(fds)),
            ("BindsTo", Variant::from(vec![holder_scope])),
            ("After", Variant::from(vec![holder_scope])),
            ("KillSignal", Variant::from(libc::SIGKILL)),
            ("CollectMode", Variant::from(COLLECT_MODE)),
        ];
        self.start_scope(name, properties).await
    }

    /// Asks the user manager to send `SIGKILL` to every process of the unit
    /// `name`; returns its answer, not the processes' end.
    ///
    /// # Errors
    ///
    /// [`UserManagerError::Bus`] when the call fails, as it does for a unit
    /// that is not loaded.
    pub async fn kill_unit(&self, name: &str) -> Result<(), UserManagerError> {
        self.conn
            .call_method(
                Some(SYSTEMD),
                MANAGER_PATH,
                Some(MANAGER),
                "KillUnit",
                &(name, "all", libc::SIGKILL),
            )
            .await?;
        Ok(())
    }

    async fn start_scope(
        &self,
        name: &str,
        properties: Vec<(&str, Variant<'_>)>,
    ) -> Result<(), UserManagerError> {
        // Listened for before the call, so a job that ends at once is not missed.
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(SYSTEMD)
            .path(MANAGER_PATH)
            .interface(MANAGER)
            .member("JobRemoved")
            .build()?;
        let mut removed = MessageStream::for_match_rule(rule, &self.conn, None).await?;
        let auxiliary: Vec<(&str, Vec<(&str, Variant<'_>)>)> = Vec::new();
        let reply = self
            .conn
            .call_method(
                Some(SYSTEMD),
                MANAGER_PATH,
                Some(MANAGER),
                "StartTransientUnit",
                &(name, "fail", properties, auxiliary),
            )
            .await?;
        let job: OwnedObjectPath = reply.body().deserialize()?;
        loop {
            let next = std::future::poll_fn(|cx| Pin::new(&mut removed).poll_next(cx)).await;
            let message = next.ok_or(UserManagerError::Closed)??;
            let (_id, ended, _unit, result): (u32, OwnedObjectPath, String, String) =
                message.body().deserialize()?;
            if ended == job {
                return if result == "done" {
                    Ok(())
                } else {
                    Err(UserManagerError::Job(result))
                };
            }
        }
    }
}
