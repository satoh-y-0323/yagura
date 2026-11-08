//! The exits a window's end is ordered by: the leader's, known without
//! reaping it, and those of the rest of its tree.

use std::io;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

/// Blocks until this process's child `pid` has exited, leaving it unreaped:
/// it stays a zombie, so its id still names it, until it is reaped.
///
/// # Errors
///
/// Returns the operating system's error when `pid` is not a child of this
/// process (`ECHILD`) or the wait fails otherwise.
pub fn wait_for_exit_without_reaping(pid: u32) -> io::Result<()> {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: zeroed is a valid `siginfo_t`, which waitid only writes.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live stack local for the call; `WNOWAIT` leaves
        // the child waitable.
        let result =
            unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Makes this process a child subreaper: a descendant whose parent ends is
/// handed to this process instead of to init, so [`reap_orphans`] reaps it.
///
/// The attribute is the whole process's, and is inherited by none of its
/// children.
///
/// # Errors
///
/// Returns the operating system's error when `prctl` refuses it.
#[cfg(target_os = "linux")]
pub fn set_child_subreaper() -> io::Result<()> {
    let on: libc::c_ulong = 1;
    // SAFETY: PR_SET_CHILD_SUBREAPER takes one integer argument and changes
    // only this process's own attribute.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, on) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The ids of this process's children, exited and unreaped ones included.
///
/// Read from every thread's `children` list, since a child is listed under
/// the thread that started (or was handed) it; from the parent field of
/// every process's `stat` when the kernel has no such lists.
///
/// # Errors
///
/// Returns the error reading `/proc` fails with.
#[cfg(target_os = "linux")]
pub fn child_pids() -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for task in std::fs::read_dir("/proc/self/task")? {
        let list = match std::fs::read_to_string(task?.path().join("children")) {
            Ok(list) => list,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return child_pids_from_stat();
            }
            Err(error) => return Err(error),
        };
        pids.extend(
            list.split_whitespace()
                .filter_map(|pid| pid.parse::<u32>().ok()),
        );
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn child_pids_from_stat() -> io::Result<Vec<u32>> {
    let own = std::process::id();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that ended since the listing has no stat any more.
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if parent_in_stat(&stat) == Some(own) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

/// The parent id in a `/proc/<pid>/stat` line. The command name before it is
/// in parentheses and may hold spaces and parentheses itself, so the fields
/// are counted from the last `)`.
#[cfg(target_os = "linux")]
fn parent_in_stat(stat: &str) -> Option<u32> {
    let (_, after_name) = stat.rsplit_once(')')?;
    let mut fields = after_name.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// Reaps every child of this process that has exited, except `leader` while
/// `gate` has not seen it reaped, and returns the ids reaped.
///
/// The children are listed first, then reaped under `gate`'s lock: a listed
/// child stays this process's until reaped, so its id is not reused in
/// between, and the leader is told apart by whether `gate` has seen it reaped
/// for as long as the reaping runs. Nothing here blocks on a child.
///
/// # Errors
///
/// Returns the error listing the children or reaping one fails with; a child
/// already gone (`ECHILD`) is not an error.
#[cfg(target_os = "linux")]
pub fn reap_orphans(leader: u32, gate: &crate::ReapGate) -> io::Result<Vec<u32>> {
    let children = child_pids()?;
    gate.hold(|leader_reaped| {
        let mut reaped = Vec::new();
        for pid in children {
            if pid == leader && !leader_reaped {
                continue;
            }
            if reap_if_exited(pid)? {
                reaped.push(pid);
            }
        }
        Ok(reaped)
    })
}

/// Sends `SIGKILL` to every child of this process except `leader` while
/// `gate` has not seen it reaped, and returns how many children were
/// signalled: `0` once the leader is the only child left.
///
/// The children are listed and signalled under `gate`'s lock, the lock
/// [`reap_orphans`] reaps under, so no listed id is reaped and reused before
/// its signal. Exited, unreaped children are counted too: they are still
/// children until reaped.
///
/// # Errors
///
/// Returns the error listing the children fails with, or the error of a
/// signal that fails other than for a child already gone.
#[cfg(target_os = "linux")]
pub fn kill_orphans(leader: u32, gate: &crate::ReapGate) -> io::Result<usize> {
    with_orphans(leader, gate, |orphans, _| {
        for &pid in orphans {
            signal_child(pid, libc::SIGKILL)?;
        }
        Ok(orphans.len())
    })
}

/// Runs `body` under `gate`'s lock with this process's children other than
/// `leader` while `gate` has not seen it reaped, and with whether it has. No
/// listed id is reaped, so none is reused, until `body` returns.
///
/// # Errors
///
/// Returns the error listing the children fails with, or `body`'s.
#[cfg(target_os = "linux")]
pub(crate) fn with_orphans<R>(
    leader: u32,
    gate: &crate::ReapGate,
    body: impl FnOnce(&[u32], bool) -> io::Result<R>,
) -> io::Result<R> {
    gate.hold(|leader_reaped| {
        let mut orphans = child_pids()?;
        orphans.retain(|&pid| pid != leader || leader_reaped);
        body(&orphans, leader_reaped)
    })
}

/// Sends `signal` to this process's unreaped child `pid`; a child already
/// gone is not an error.
///
/// # Errors
///
/// Returns the error the signal fails with otherwise.
pub(crate) fn signal_child(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let Ok(target) = libc::pid_t::try_from(pid) else {
        return Ok(());
    };
    // SAFETY: the caller holds the child unreaped, so the id still names it;
    // kill takes plain values.
    if unsafe { libc::kill(target, signal) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

/// Whether this process's unreaped child `pid` has exited, looked at without
/// reaping it or waiting.
///
/// # Errors
///
/// Returns the operating system's error, such as `ECHILD` for a process
/// that is not an unreaped child.
#[cfg(target_os = "linux")]
pub(crate) fn child_has_exited(pid: u32) -> io::Result<bool> {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: zeroed is a valid `siginfo_t`, which waitid only writes.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live stack local for the call; `WNOWAIT`
        // leaves the child waitable and `WNOHANG` returns at once.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == 0 {
            // SAFETY: waitid filled `info`; with WNOHANG and no child ready
            // it leaves the pid zero.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The `SIGCHLD`s this process receives, awaited one after another.
///
/// Signals that arrive while nobody awaits are merged into one, so a caller
/// that reacts to each one looks at every child again rather than counting.
#[derive(Debug)]
pub struct ChildSignals(tokio::signal::unix::Signal);

impl ChildSignals {
    /// Starts receiving `SIGCHLD`. Every signal from here on wakes a later
    /// [`Self::next`]. Call from within a Tokio runtime that has its I/O
    /// driver enabled.
    ///
    /// # Errors
    ///
    /// Returns the error installing the handler fails with, or when called
    /// outside such a runtime.
    pub fn new() -> io::Result<Self> {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).map(Self)
    }

    /// Waits for the next `SIGCHLD` since the last call returned; `false`
    /// once no more can arrive.
    pub async fn next(&mut self) -> bool {
        self.0.recv().await.is_some()
    }
}

/// Reaps the child `pid` if it has exited; `false` while it runs or once it
/// is gone.
#[cfg(target_os = "linux")]
fn reap_if_exited(pid: u32) -> io::Result<bool> {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return Ok(false);
    };
    loop {
        let mut status = 0;
        // SAFETY: `status` is a live stack local; `WNOHANG` returns at once.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid {
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ECHILD) => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// Blocks until every process in `members` has exited (a zombie counts as
/// exited), or until `limit` has passed, and returns the members still
/// running then: empty once every one has exited.
///
/// Each member's exit is awaited through kqueue, so a member that ends while
/// others still run does not end the wait. `limit` only bounds the wait; a
/// limit too far off to be a point in time is no limit.
///
/// # Errors
///
/// Returns the operating system's error when the exits cannot be watched.
/// The caller then has no event for the members' end.
#[cfg(target_os = "macos")]
pub fn wait_for_members_exit(members: &[u32], limit: Duration) -> io::Result<Vec<u32>> {
    let deadline = Instant::now().checked_add(limit);
    let queue = ExitQueue::new()?;
    let mut remaining = Vec::with_capacity(members.len());
    for &member in members {
        if !remaining.contains(&member) && queue.watch(member)? {
            remaining.push(member);
        }
    }
    while !remaining.is_empty() {
        let Some(pid) = queue.next_exit(deadline)? else {
            break;
        };
        remaining.retain(|&member| member != pid);
    }
    Ok(remaining)
}

/// One kqueue that processes' exits are watched on and reported from, one
/// at a time.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct ExitQueue(std::os::fd::OwnedFd);

#[cfg(target_os = "macos")]
impl ExitQueue {
    pub(crate) fn new() -> io::Result<Self> {
        use std::os::fd::FromRawFd as _;

        // SAFETY: kqueue takes no arguments and returns a new descriptor or -1.
        let queue = unsafe { libc::kqueue() };
        if queue == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `queue` is a descriptor just returned to this function alone.
        Ok(Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(queue) }))
    }

    /// Watches `pid`'s exit; `false` when `pid` has already exited or is
    /// gone, so no exit of it will be reported.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the watch cannot be made.
    pub(crate) fn watch(&self, pid: u32) -> io::Result<bool> {
        use std::os::fd::AsRawFd as _;

        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Ok(false);
        };
        // A process that became a zombie before it was watched may send no
        // event.
        Ok(watch_exit(self.0.as_raw_fd(), pid)? && !has_exited(pid))
    }

    /// Blocks until a watched process's exit is reported and returns its id,
    /// or returns `None` once `deadline` has passed. No deadline is no limit.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the queue cannot be read.
    pub(crate) fn next_exit(&self, deadline: Option<Instant>) -> io::Result<Option<u32>> {
        loop {
            let timeout = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(None);
                    }
                    Some(timespec_of(left))
                }
                None => None,
            };
            if let Some(pid) = self.take(timeout.as_ref())? {
                return Ok(Some(pid));
            }
        }
    }

    /// The id of a watched process whose exit has been reported and not yet
    /// taken, without waiting.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the queue cannot be read.
    pub(crate) fn reported_exit(&self) -> io::Result<Option<u32>> {
        self.take(Some(&timespec_of(Duration::ZERO)))
    }

    /// Takes one event, waiting at most `timeout` (no limit when `None`):
    /// the exited process's id, or `None` when nothing came.
    fn take(&self, timeout: Option<&libc::timespec>) -> io::Result<Option<u32>> {
        use std::os::fd::AsRawFd as _;

        loop {
            // SAFETY: zeroed is a valid `kevent`, which kevent only writes.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: no changes are passed; `event` is a live stack local of
            // the one entry asked for; the timeout is a live local or null,
            // which blocks.
            let count = unsafe {
                libc::kevent(
                    self.0.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    timeout.map_or(std::ptr::null(), std::ptr::from_ref),
                )
            };
            if count == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 1
                && event.filter == libc::EVFILT_PROC
                && event.fflags & libc::NOTE_EXIT != 0
                && let Ok(pid) = u32::try_from(event.ident)
            {
                return Ok(Some(pid));
            }
            return Ok(None);
        }
    }
}

/// `duration` as a kevent timeout, saturating at the largest one.
#[cfg(target_os = "macos")]
fn timespec_of(duration: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: libc::time_t::try_from(duration.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::from(duration.subsec_nanos()),
    }
}

/// Asks `queue` for `pid`'s exit; `false` when `pid` is already gone.
#[cfg(target_os = "macos")]
fn watch_exit(queue: libc::c_int, pid: libc::pid_t) -> io::Result<bool> {
    let Ok(ident) = libc::uintptr_t::try_from(pid) else {
        return Ok(false);
    };
    // SAFETY: zeroed is a valid `kevent`; the fields that matter are set below.
    let mut change: libc::kevent = unsafe { std::mem::zeroed() };
    change.ident = ident;
    change.filter = libc::EVFILT_PROC;
    change.flags = libc::EV_ADD | libc::EV_ONESHOT;
    change.fflags = libc::NOTE_EXIT;
    // SAFETY: one change from a live stack local, no events asked for.
    let result =
        unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(true)
}

/// Whether `pid` is gone or a zombie. A process that cannot be looked at for
/// another reason counts as running, so its exit event is waited for.
#[cfg(target_os = "macos")]
pub(crate) fn has_exited(pid: libc::pid_t) -> bool {
    // SAFETY: zeroed is a valid `proc_bsdinfo`, which proc_pidinfo only writes.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let Ok(size) = libc::c_int::try_from(size) else {
        return false;
    };
    // SAFETY: `info` is a live stack local of `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            size,
        )
    };
    if written != size {
        return io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    info.pbi_status == libc::SZOMB
}
