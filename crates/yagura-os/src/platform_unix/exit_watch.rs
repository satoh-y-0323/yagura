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

// =========================================================================
// A holder's watcher and the holder that is its parent
// =========================================================================

/// Blocks `signals` in the calling thread, and so in every thread it starts
/// afterwards.
fn block_signals(signals: &[libc::c_int]) -> io::Result<()> {
    // SAFETY: zeroed is a valid `sigset_t`, set below through the libc calls.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a live local the calls write.
    let blocked = unsafe {
        libc::sigemptyset(&mut set);
        for &signal in signals {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut())
    };
    if blocked == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(blocked))
    }
}

/// Blocks `SIGCHLD` in the calling thread and every thread it starts
/// afterwards. Called before a process starts any thread, it leaves the
/// signal to [`wait_child_signal`] and to the `signalfd` of an owner's
/// [`crate::platform_unix::TreeEnd`] (Linux), which no handler in another
/// thread then takes first.
///
/// # Errors
///
/// Returns the operating system's error when the handler or the mask cannot
/// be set.
pub fn block_child_signal() -> io::Result<()> {
    // macOS discards a signal whose action is to be ignored, the default
    // `SIGCHLD`'s, even while it is blocked: a handler keeps it pending. It
    // never runs, the signal being blocked in every thread.
    extern "C" fn keep_pending(_signal: libc::c_int) {}
    // SAFETY: zeroed is a valid `sigaction`, filled in below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = keep_pending as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: `action` is a live local with an empty mask; the handler only
    // returns, which is async-signal-safe.
    let set = unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut())
    };
    if set != 0 {
        return Err(io::Error::last_os_error());
    }
    block_signals(&[libc::SIGCHLD])
}

/// Waits for a `SIGCHLD` blocked by [`block_child_signal`] and takes it. A
/// signal that came before the call is taken at once.
///
/// # Errors
///
/// Returns the operating system's error when the wait fails.
pub fn wait_child_signal() -> io::Result<()> {
    // SAFETY: zeroed is a valid `sigset_t`, set below through the libc calls.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a live local the calls write.
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
    }
    loop {
        let mut taken: libc::c_int = 0;
        // SAFETY: `set` and `taken` are live locals for the call.
        let result = unsafe { libc::sigwait(&set, &mut taken) };
        if result == 0 {
            return Ok(());
        }
        if result != libc::EINTR {
            return Err(io::Error::from_raw_os_error(result));
        }
    }
}

/// Blocks the signals a holder's watcher takes as events, `SIGTERM` and
/// `SIGCHLD`, before it starts its child (which [`crate::pty::start_leader`]
/// gives every signal unblocked) and before [`WatcherWait`] reads them.
///
/// # Errors
///
/// Returns the operating system's error when the mask cannot be changed.
pub fn block_watcher_signals() -> io::Result<()> {
    block_signals(&[libc::SIGTERM, libc::SIGCHLD])
}

/// A change in the state of a child of this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildChange {
    /// It was stopped.
    Stopped,
    /// It was continued.
    Continued,
    /// It exited, and has been reaped.
    Exited,
}

/// Takes the next change of this process's child `pid` without waiting: a
/// stop or a continue as it came, or the exit, reaping the child under
/// `gate`'s lock. `None` when nothing has changed.
///
/// # Errors
///
/// Returns the operating system's error, `ECHILD` for a process that is not
/// an unreaped child.
pub fn take_child_change(pid: u32, gate: &crate::ReapGate) -> io::Result<Option<ChildChange>> {
    let Some(code) = child_event(
        pid,
        libc::WEXITED | libc::WSTOPPED | libc::WCONTINUED | libc::WNOWAIT,
    )?
    else {
        return Ok(None);
    };
    if code == libc::CLD_CONTINUED {
        child_event(pid, libc::WCONTINUED)?;
        return Ok(Some(ChildChange::Continued));
    }
    if code == libc::CLD_STOPPED || code == libc::CLD_TRAPPED {
        child_event(pid, libc::WSTOPPED)?;
        return Ok(Some(ChildChange::Stopped));
    }
    gate.reap_with(|| reap_exited(pid))?;
    Ok(Some(ChildChange::Exited))
}

/// The `si_code` of the child `pid`'s next event among `flags`, taken
/// without waiting (`WNOHANG`); `None` when there is none.
fn child_event(pid: u32, flags: libc::c_int) -> io::Result<Option<libc::c_int>> {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: zeroed is a valid `siginfo_t`, which waitid only writes.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live stack local; WNOHANG returns at once.
        let result = unsafe { libc::waitid(libc::P_PID, id, &mut info, flags | libc::WNOHANG) };
        if result == 0 {
            // SAFETY: waitid filled `info`, leaving the pid zero for no event.
            return Ok((unsafe { info.si_pid() } != 0).then_some(info.si_code));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Sends `SIGCONT` to this process's child `pid` under `gate`'s lock, unless
/// it has been reaped.
///
/// # Errors
///
/// Returns the operating system's error when the signal cannot be sent.
pub fn continue_child(pid: u32, gate: &crate::ReapGate) -> io::Result<()> {
    gate.signal_unless_reaped(|| signal_child(pid, libc::SIGCONT))
        .unwrap_or(Ok(()))
}

/// Sends `SIGHUP` to this process's child `pid`, as closing its terminal
/// would, under `gate`'s lock unless it has been reaped; a child already gone
/// is not an error.
///
/// # Errors
///
/// Returns the operating system's error when the signal cannot be sent.
pub fn hang_up_child(pid: u32, gate: &crate::ReapGate) -> io::Result<()> {
    gate.signal_unless_reaped(|| signal_child(pid, libc::SIGHUP))
        .unwrap_or(Ok(()))
}

/// Sends `SIGKILL` to this process's child `pid` under `gate`'s lock unless
/// it has been reaped; a child already gone is not an error.
///
/// # Errors
///
/// Returns the operating system's error when the signal cannot be sent.
pub fn kill_child(pid: u32, gate: &crate::ReapGate) -> io::Result<()> {
    gate.signal_unless_reaped(|| signal_child(pid, libc::SIGKILL))
        .unwrap_or(Ok(()))
}

/// The exit of this process's unreaped child `pid`, looked at without
/// reaping it or waiting: `None` while it runs, else its exit code, `None`
/// within for an end by a signal.
///
/// # Errors
///
/// Returns the operating system's error, `ECHILD` for a process that is not
/// an unreaped child.
pub fn unreaped_exit(pid: u32) -> io::Result<Option<Option<i32>>> {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: zeroed is a valid `siginfo_t`, which waitid only writes.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live stack local; WNOWAIT leaves the child
        // waitable and WNOHANG returns at once.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == 0 {
            // SAFETY: waitid filled `info`, leaving the pid zero while the
            // child runs; the status is the exit code for CLD_EXITED.
            let (pid, status) = unsafe { (info.si_pid(), info.si_status()) };
            if pid == 0 {
                return Ok(None);
            }
            return Ok(Some((info.si_code == libc::CLD_EXITED).then_some(status)));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Reaps this process's child `pid` under `gate`'s lock, once
/// [`unreaped_exit`] has seen it exit. A child already gone is not an error.
///
/// # Errors
///
/// Returns the operating system's error otherwise.
pub fn reap_exited_child(pid: u32, gate: &crate::ReapGate) -> io::Result<()> {
    gate.reap_with(|| reap_exited(pid))
}

/// Reaps the exited child `pid`, which must not block.
fn reap_exited(pid: u32) -> io::Result<()> {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return Ok(());
    };
    loop {
        let mut status = 0;
        // SAFETY: `status` is a live stack local.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ECHILD) => return Ok(()),
            _ => return Err(error),
        }
    }
}

/// What woke a [`WatcherWait`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WatcherEvents {
    /// The leader exited (told once).
    pub leader_exited: bool,
    /// The holder's socket has something to read, or has ended.
    pub socket: bool,
    /// A `SIGTERM` came.
    pub terminated: bool,
    /// A `SIGCHLD` came (Linux): a child, perhaps an orphan, changed.
    pub children: bool,
}

impl WatcherEvents {
    #[cfg(target_os = "macos")]
    fn any(self) -> bool {
        self.leader_exited || self.socket || self.terminated || self.children
    }
}

/// The one wait of a holder's watcher: its leader's exit, its holder's
/// socket and the signals [`block_watcher_signals`] blocked. Linux waits on
/// the leader's pidfd, the socket and a `signalfd` with `poll`; macOS on one
/// kqueue.
#[derive(Debug)]
pub struct WatcherWait {
    leader_told: bool,
    #[cfg(target_os = "linux")]
    socket: libc::c_int,
    #[cfg(target_os = "linux")]
    pidfd: std::os::fd::OwnedFd,
    #[cfg(target_os = "linux")]
    signals: std::os::fd::OwnedFd,
    #[cfg(target_os = "macos")]
    leader: u32,
    #[cfg(target_os = "macos")]
    socket: Option<libc::c_int>,
    #[cfg(target_os = "macos")]
    queue: std::os::fd::OwnedFd,
    #[cfg(target_os = "macos")]
    early: WatcherEvents,
}

#[cfg(target_os = "linux")]
impl WatcherWait {
    /// Watches the unreaped child `leader` and `socket`, which must outlive
    /// the wait.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a descriptor cannot be made.
    pub fn new(leader: u32, socket: std::os::fd::BorrowedFd<'_>) -> io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let pidfd = super::user_manager::pidfd_open(leader)?;
        // SAFETY: zeroed is a valid `sigset_t`, set below through the libc calls.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `set` is a live local the calls write; -1 asks for a new
        // descriptor.
        let fd = unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGCHLD);
            libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC)
        };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            leader_told: false,
            socket: socket.as_raw_fd(),
            pidfd,
            // SAFETY: the descriptor was just returned to this function alone.
            signals: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Stops watching the socket, so a socket at its end no longer wakes the
    /// wait: what is left to wait for is the leader and the signals.
    ///
    /// # Errors
    ///
    /// None on Linux.
    pub fn ignore_socket(&mut self) -> io::Result<()> {
        // poll skips an entry with a negative descriptor.
        self.socket = -1;
        Ok(())
    }

    /// Waits until something happens, or until `limit` has passed (no limit
    /// when `None`), and says what did: nothing at the limit.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait fails.
    pub fn wait(&mut self, limit: Option<std::time::Duration>) -> io::Result<WatcherEvents> {
        use std::os::fd::AsRawFd as _;

        let timeout = limit.map_or(-1, |limit| {
            libc::c_int::try_from(limit.as_millis()).unwrap_or(libc::c_int::MAX)
        });
        let mut fds = vec![
            libc::pollfd {
                fd: self.socket,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.signals.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if !self.leader_told {
            fds.push(libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let count = libc::nfds_t::try_from(fds.len())
            .map_err(|_| io::Error::other("too many descriptors to poll"))?;
        // SAFETY: `fds` is a live vector of `count` entries.
        if unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) } == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
            return Ok(WatcherEvents::default());
        }
        let mut events = WatcherEvents {
            socket: fds[0].revents != 0,
            ..WatcherEvents::default()
        };
        if fds[1].revents != 0 {
            self.read_signals(&mut events)?;
        }
        if fds.get(2).is_some_and(|fd| fd.revents != 0) {
            self.leader_told = true;
            events.leader_exited = true;
        }
        Ok(events)
    }

    fn read_signals(&self, events: &mut WatcherEvents) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;

        loop {
            // SAFETY: zeroed is a valid `signalfd_siginfo`, which read writes.
            let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::signalfd_siginfo>();
            // SAFETY: `info` is a live local of `size` bytes.
            let read = unsafe {
                libc::read(
                    self.signals.as_raw_fd(),
                    std::ptr::addr_of_mut!(info).cast(),
                    size,
                )
            };
            if usize::try_from(read).ok() != Some(size) {
                let error = io::Error::last_os_error();
                return match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => Ok(()),
                    _ if read >= 0 => Ok(()),
                    _ => Err(error),
                };
            }
            match libc::c_int::try_from(info.ssi_signo) {
                Ok(libc::SIGTERM) => events.terminated = true,
                Ok(libc::SIGCHLD) => events.children = true,
                _ => {}
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl WatcherWait {
    /// Watches the unreaped child `leader` and `socket`, which must outlive
    /// the wait.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the queue cannot be made.
    pub fn new(leader: u32, socket: std::os::fd::BorrowedFd<'_>) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;

        let queue = ExitQueue::new()?;
        let mut early = WatcherEvents::default();
        let watched = queue.watch(leader)?;
        early.leader_exited = !watched;
        let socket = socket.as_raw_fd();
        for (ident, filter) in [
            (
                libc::uintptr_t::try_from(socket).unwrap_or(0),
                libc::EVFILT_READ,
            ),
            (
                libc::uintptr_t::try_from(libc::SIGTERM).unwrap_or(0),
                libc::EVFILT_SIGNAL,
            ),
        ] {
            // SAFETY: zeroed is a valid `kevent`; the fields that matter are
            // set below.
            let mut change: libc::kevent = unsafe { std::mem::zeroed() };
            change.ident = ident;
            change.filter = filter;
            change.flags = libc::EV_ADD;
            // SAFETY: one change from a live stack local, no events asked for.
            let added = unsafe {
                libc::kevent(
                    queue.0.as_raw_fd(),
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if added == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        // A SIGTERM blocked before the queue watched for it is pending only.
        // SAFETY: zeroed is a valid `sigset_t`, which sigpending writes.
        let mut pending: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `pending` is a live local of the type the calls take.
        early.terminated = unsafe {
            libc::sigpending(&mut pending) == 0 && libc::sigismember(&pending, libc::SIGTERM) == 1
        };
        Ok(Self {
            leader_told: false,
            leader,
            socket: Some(socket),
            queue: queue.0,
            early,
        })
    }

    /// Stops watching the socket, so a socket at its end no longer wakes the
    /// wait: what is left to wait for is the leader and the signals.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the queue refuses to drop
    /// the socket's filter.
    pub fn ignore_socket(&mut self) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;

        let Some(socket) = self.socket else {
            return Ok(());
        };
        // SAFETY: zeroed is a valid `kevent`; the fields that matter are set
        // below.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = libc::uintptr_t::try_from(socket).unwrap_or(0);
        change.filter = libc::EVFILT_READ;
        change.flags = libc::EV_DELETE;
        // SAFETY: one change from a live stack local, no events asked for.
        let deleted = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if deleted == -1 {
            return Err(io::Error::last_os_error());
        }
        self.socket = None;
        self.early.socket = false;
        Ok(())
    }

    /// Waits until something happens, or until `limit` has passed (no limit
    /// when `None`), and says what did: nothing at the limit.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait fails.
    pub fn wait(&mut self, limit: Option<std::time::Duration>) -> io::Result<WatcherEvents> {
        use std::os::fd::AsRawFd as _;

        let mut events = std::mem::take(&mut self.early);
        let timeout = if events.any() {
            Some(std::time::Duration::ZERO)
        } else {
            limit
        };
        let timeout = timeout.map(timespec_of);
        let mut fired: [libc::kevent; 4] =
            // SAFETY: zeroed is a valid `kevent`, which kevent only writes.
            unsafe { std::mem::zeroed() };
        // SAFETY: no changes are passed; `fired` is a live array of the
        // length passed; the timeout is a live local or null, which blocks.
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                fired.as_mut_ptr(),
                4,
                timeout
                    .as_ref()
                    .map_or(std::ptr::null(), std::ptr::from_ref),
            )
        };
        if count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let leader = libc::uintptr_t::try_from(self.leader).unwrap_or(0);
        for event in fired.iter().take(usize::try_from(count).unwrap_or(0)) {
            match event.filter {
                libc::EVFILT_PROC if event.ident == leader => events.leader_exited = true,
                libc::EVFILT_READ => events.socket = true,
                libc::EVFILT_SIGNAL => events.terminated = true,
                _ => {}
            }
        }
        if events.leader_exited {
            if self.leader_told {
                events.leader_exited = false;
            }
            self.leader_told = true;
        }
        Ok(events)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl WatcherWait {
    /// Not on this platform.
    ///
    /// # Errors
    ///
    /// Always.
    pub fn new(_leader: u32, _socket: std::os::fd::BorrowedFd<'_>) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a watcher's wait is not known on this platform",
        ))
    }

    /// Not on this platform.
    ///
    /// # Errors
    ///
    /// Always.
    pub fn wait(&mut self, _limit: Option<std::time::Duration>) -> io::Result<WatcherEvents> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a watcher's wait is not known on this platform",
        ))
    }

    /// Not on this platform.
    ///
    /// # Errors
    ///
    /// Always.
    pub fn ignore_socket(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a watcher's wait is not known on this platform",
        ))
    }
}
