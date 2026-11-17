//! Naming a process by its id and its start, so an id the operating system
//! hands to another process later is never taken for the one it named.
//!
//! A process that has exited counts as dead before anyone reaps it: its id
//! still names it then, but it will run no more.

use std::fmt;
use std::io;

/// When a process started, as the operating system reports it: an opaque
/// value, equal for one process at every reading and different for a
/// process that later takes the same id. Shown as a string, which is the
/// form records keep.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessStart(String);

impl fmt::Display for ProcessStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reads a start back from the string a record keeps; an empty string is
/// no start.
impl std::str::FromStr for ProcessStart {
    type Err = io::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an empty process start",
            ));
        }
        Ok(Self(text.to_owned()))
    }
}

/// Whether a process named by its id and start runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// It runs.
    Alive,
    /// It has exited (reaped or not), or the id now names another process.
    Dead,
    /// The operating system did not answer. Never to be taken for dead.
    Unknown,
}

/// The start of the process `pid`; `None` when no process has that id.
///
/// # Errors
///
/// Returns the operating system's error when the process is there but its
/// start cannot be read (for instance, it belongs to another user).
pub fn process_start(pid: u32) -> io::Result<Option<ProcessStart>> {
    platform::start_of(pid).map(|start| start.map(ProcessStart))
}

/// Whether the process `pid` that started at `start` runs, from one look:
/// for a one-off check. Ending a process goes through [`ProcessHandle`],
/// which checks and acts on the same handle.
#[must_use]
pub fn process_liveness(pid: u32, start: &ProcessStart) -> Liveness {
    match ProcessHandle::open(pid, start) {
        Ok(Some(handle)) => handle.liveness(),
        Ok(None) => Liveness::Dead,
        Err(_) => Liveness::Unknown,
    }
}

/// What one reading of a process says about its place among the others.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessFacts {
    pub(crate) ppid: u32,
    pub(crate) pgid: u32,
    pub(crate) sid: u32,
    /// A zombie or a process being torn down.
    pub(crate) exited: bool,
    pub(crate) start: ProcessStart,
}

/// One reading of the process `pid`; `None` when no process has that id.
///
/// # Errors
///
/// Returns the operating system's error when the process is there but
/// cannot be read.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn process_facts(pid: u32) -> io::Result<Option<ProcessFacts>> {
    platform::facts_of(pid)
}

/// A handle on one running process, opened by its id and checked against
/// its start: its liveness, the wait for its end and its kill all go
/// through the same handle, so none of them reaches a process that took
/// over the id. On macOS, which has no such handle, a gap is left between
/// the check and a kill.
#[derive(Debug)]
pub struct ProcessHandle(platform::Handle);

impl ProcessHandle {
    /// Opens the process `pid` that started at `start`; `None` when the id
    /// names no process, names one with another start, or the process has
    /// already exited.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the process cannot be
    /// opened or looked at (for instance, it belongs to another user).
    pub fn open(pid: u32, start: &ProcessStart) -> io::Result<Option<Self>> {
        platform::Handle::open(pid, &start.0).map(|handle| handle.map(Self))
    }

    /// Whether the process runs: [`Liveness::Dead`] once it has exited,
    /// reaped or not.
    #[must_use]
    pub fn liveness(&self) -> Liveness {
        self.0.liveness()
    }

    /// Blocks until the process has exited. Reaps nothing.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait fails.
    pub fn wait(&self) -> io::Result<()> {
        self.0.wait()
    }

    /// Ends the process at once. A process that has already exited is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a running process cannot be
    /// ended.
    pub fn kill(&self) -> io::Result<()> {
        self.0.kill()
    }

    /// Sends `signal` to the process. A process that has already exited is
    /// not an error.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn signal(&self, signal: libc::c_int) -> io::Result<()> {
        self.0.signal(signal)
    }

    /// Starts watching for the process's end, to be made before whatever ends
    /// it: [`ExitWatch::wait`] then returns once the exit is complete, the
    /// process's descriptors closed. On macOS a [`ProcessHandle::wait`] begun
    /// once the exit is under way returns at once, while the process may
    /// still hold its descriptors (and the locks taken on them).
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the watch cannot be made.
    pub fn watch_exit(&self) -> io::Result<ExitWatch> {
        self.0.watch().map(ExitWatch)
    }
}

/// A watch of one process's end, made by [`ProcessHandle::watch_exit`].
#[derive(Debug)]
pub struct ExitWatch(platform::Watch);

impl ExitWatch {
    /// Blocks until the watched process has exited; at once when it had
    /// already ended when the watch was made. Reaps nothing.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait fails.
    pub fn wait(self) -> io::Result<()> {
        self.0.wait()
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    use windows::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, INFINITE, OpenProcess, PROCESS_ACCESS_RIGHTS,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    };

    use super::Liveness;
    use crate::platform_windows::win32_error_to_io;

    #[derive(Debug)]
    pub(super) struct Handle(OwnedHandle);

    /// Opens `pid` with `access`; `None` when no process has that id.
    fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Option<OwnedHandle>> {
        // SAFETY: OpenProcess takes plain values and returns a new handle or
        // an error.
        match unsafe { OpenProcess(access, false, pid) } {
            // SAFETY: the handle was just returned to this function alone.
            Ok(handle) => Ok(Some(unsafe { OwnedHandle::from_raw_handle(handle.0 as _) })),
            Err(error) => {
                let error = win32_error_to_io(&error);
                // OpenProcess's answer for an id no process has.
                if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER.0 as i32) {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    fn raw(handle: &OwnedHandle) -> HANDLE {
        HANDLE(handle.as_raw_handle() as isize)
    }

    /// The process's start: its creation time, in 100 ns ticks.
    fn created(handle: &OwnedHandle) -> io::Result<String> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: the handle is open for the call; the four times are live
        // out-parameters.
        unsafe {
            GetProcessTimes(
                raw(handle),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .map_err(|error| win32_error_to_io(&error))?;
        let ticks = u64::from(creation.dwHighDateTime) << 32 | u64::from(creation.dwLowDateTime);
        Ok(ticks.to_string())
    }

    fn wait_for(handle: &OwnedHandle) -> io::Result<()> {
        // SAFETY: the handle is open for the call.
        if unsafe { WaitForSingleObject(raw(handle), INFINITE) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[derive(Debug)]
    pub(super) struct Watch(OwnedHandle);

    impl Watch {
        pub(super) fn wait(self) -> io::Result<()> {
            wait_for(&self.0)
        }
    }

    /// A process object is signalled once its process has exited, whoever
    /// still holds it open.
    fn liveness_of(handle: &OwnedHandle) -> Liveness {
        // SAFETY: the handle is open for the call.
        match unsafe { WaitForSingleObject(raw(handle), 0) } {
            WAIT_OBJECT_0 => Liveness::Dead,
            WAIT_TIMEOUT => Liveness::Alive,
            _ => Liveness::Unknown,
        }
    }

    pub(super) fn start_of(pid: u32) -> io::Result<Option<String>> {
        match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)? {
            Some(handle) => created(&handle).map(Some),
            None => Ok(None),
        }
    }

    impl Handle {
        pub(super) fn open(pid: u32, start: &str) -> io::Result<Option<Self>> {
            let access =
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
            let Some(handle) = open_process(pid, access)? else {
                return Ok(None);
            };
            if created(&handle)? != start || liveness_of(&handle) == Liveness::Dead {
                return Ok(None);
            }
            Ok(Some(Self(handle)))
        }

        pub(super) fn liveness(&self) -> Liveness {
            liveness_of(&self.0)
        }

        pub(super) fn wait(&self) -> io::Result<()> {
            wait_for(&self.0)
        }

        pub(super) fn watch(&self) -> io::Result<Watch> {
            self.0.try_clone().map(Watch)
        }

        pub(super) fn kill(&self) -> io::Result<()> {
            // SAFETY: the handle is open for the call.
            match unsafe { TerminateProcess(raw(&self.0), 1) } {
                Ok(()) => Ok(()),
                Err(_) if liveness_of(&self.0) == Liveness::Dead => Ok(()),
                Err(error) => Err(win32_error_to_io(&error)),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    use super::Liveness;

    #[derive(Debug)]
    pub(super) struct Handle(OwnedFd);

    /// The state and the start time (in clock ticks since boot) in a
    /// `/proc/<pid>/stat` line. The command name before them is in
    /// parentheses and may hold spaces and parentheses itself, so the fields
    /// are counted from the last `)`.
    fn state_and_start(stat: &str) -> Option<(char, &str)> {
        let (_, after_name) = stat.rsplit_once(')')?;
        let mut fields = after_name.split_whitespace();
        let state = fields.next()?.chars().next()?;
        // The start time is the 22nd field; the state is the 3rd.
        let start = fields.nth(18)?;
        Some((state, start))
    }

    /// The parent, group and session ids in a `/proc/<pid>/stat` line: the
    /// 4th to 6th fields.
    fn family(stat: &str) -> Option<(u32, u32, u32)> {
        let (_, after_name) = stat.rsplit_once(')')?;
        let mut fields = after_name.split_whitespace().skip(1);
        let ppid = fields.next()?.parse().ok()?;
        let pgid = fields.next()?.parse().ok()?;
        let sid = fields.next()?.parse().ok()?;
        Some((ppid, pgid, sid))
    }

    /// The process's state and start, the start tied to this boot, since
    /// clock ticks count from it; `None` when no process has that id.
    fn read_stat(pid: u32) -> io::Result<Option<(char, String)>> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
            Err(error) => return Err(error),
        };
        let (state, ticks) = state_and_start(&stat)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a malformed stat line"))?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        Ok(Some((state, format!("{}:{ticks}", boot.trim()))))
    }

    /// A zombie (`Z`) or a process being torn down (`X`) has exited.
    fn has_exited(state: char) -> bool {
        matches!(state, 'Z' | 'X')
    }

    pub(super) fn start_of(pid: u32) -> io::Result<Option<String>> {
        Ok(read_stat(pid)?.map(|(_, start)| start))
    }

    pub(super) fn facts_of(pid: u32) -> io::Result<Option<super::ProcessFacts>> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
            Err(error) => return Err(error),
        };
        let malformed = || io::Error::new(io::ErrorKind::InvalidData, "a malformed stat line");
        let (state, ticks) = state_and_start(&stat).ok_or_else(malformed)?;
        let (ppid, pgid, sid) = family(&stat).ok_or_else(malformed)?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        Ok(Some(super::ProcessFacts {
            ppid,
            pgid,
            sid,
            exited: has_exited(state),
            start: super::ProcessStart(format!("{}:{ticks}", boot.trim())),
        }))
    }

    #[derive(Debug)]
    pub(super) struct Watch(OwnedFd);

    impl Watch {
        pub(super) fn wait(self) -> io::Result<()> {
            poll_exit(&self.0, -1).map(drop)
        }
    }

    /// Polls the process's descriptor, which turns readable once it has
    /// exited, waiting at most `timeout_ms` (`-1` for no limit).
    fn poll_exit(fd: &OwnedFd, timeout_ms: libc::c_int) -> io::Result<bool> {
        loop {
            let mut entry = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `entry` is a live stack local, the one entry passed.
            let ready = unsafe { libc::poll(&mut entry, 1, timeout_ms) };
            if ready >= 0 {
                return Ok(ready > 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    impl Handle {
        /// Opens the process first and compares its start after: a start
        /// read first could belong to a process that exits before the open,
        /// leaving the open to reach whoever took over the id.
        pub(super) fn open(pid: u32, start: &str) -> io::Result<Option<Self>> {
            let Ok(target) = libc::pid_t::try_from(pid) else {
                return Ok(None);
            };
            // SAFETY: pidfd_open takes a pid and flags and returns a new
            // descriptor or -1.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, target, 0) };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(None);
                }
                return Err(error);
            }
            let fd = libc::c_int::try_from(fd)
                .map_err(|_| io::Error::other("pidfd_open returned an out-of-range descriptor"))?;
            // SAFETY: the descriptor was just returned to this function alone.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            match read_stat(pid)? {
                Some((state, read)) if read == start && !has_exited(state) => Ok(Some(Self(fd))),
                _ => Ok(None),
            }
        }

        pub(super) fn liveness(&self) -> Liveness {
            match poll_exit(&self.0, 0) {
                Ok(true) => Liveness::Dead,
                Ok(false) => Liveness::Alive,
                Err(_) => Liveness::Unknown,
            }
        }

        pub(super) fn wait(&self) -> io::Result<()> {
            poll_exit(&self.0, -1).map(drop)
        }

        /// The descriptor turns readable only once the exit is complete.
        pub(super) fn watch(&self) -> io::Result<Watch> {
            self.0.try_clone().map(Watch)
        }

        pub(super) fn kill(&self) -> io::Result<()> {
            self.signal(libc::SIGKILL)
        }

        pub(super) fn signal(&self, signal: libc::c_int) -> io::Result<()> {
            // SAFETY: the descriptor is open for the call; a null info sends
            // the signal as kill(2) would.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    use super::Liveness;

    #[derive(Debug)]
    pub(super) struct Handle {
        pid: libc::pid_t,
        start: String,
    }

    /// The process's status and start; `None` when no process has that id.
    fn read_info(pid: libc::pid_t) -> io::Result<Option<(u32, String)>> {
        Ok(read_bsdinfo(pid)?.map(|info| (info.pbi_status, start_in(&info))))
    }

    fn start_in(info: &libc::proc_bsdinfo) -> String {
        format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec)
    }

    pub(super) fn facts_of(pid: u32) -> io::Result<Option<super::ProcessFacts>> {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Ok(None);
        };
        let Some(info) = read_bsdinfo(pid)? else {
            return Ok(None);
        };
        let exited = info.pbi_status == libc::SZOMB;
        // SAFETY: getsid takes a plain value.
        let sid = unsafe { libc::getsid(pid) };
        // A zombie has no session to report; its session counts for nothing.
        let sid = if sid == -1 && exited {
            0
        } else if sid == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        } else {
            u32::try_from(sid).unwrap_or(0)
        };
        Ok(Some(super::ProcessFacts {
            ppid: info.pbi_ppid,
            pgid: info.pbi_pgid,
            sid,
            exited,
            start: super::ProcessStart(start_in(&info)),
        }))
    }

    fn read_bsdinfo(pid: libc::pid_t) -> io::Result<Option<libc::proc_bsdinfo>> {
        // SAFETY: zeroed is a valid `proc_bsdinfo`, which proc_pidinfo only
        // writes.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
            .map_err(|_| io::Error::other("proc_bsdinfo does not fit a c_int"))?;
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
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(info))
    }

    pub(super) fn start_of(pid: u32) -> io::Result<Option<String>> {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Ok(None);
        };
        Ok(read_info(pid)?.map(|(_, start)| start))
    }

    /// Whether `pid` runs with `start`: a zombie (`SZOMB`) has exited, and an
    /// id with another start names another process.
    fn runs(pid: libc::pid_t, start: &str) -> io::Result<bool> {
        Ok(read_info(pid)?.is_some_and(|(status, read)| read == start && status != libc::SZOMB))
    }

    /// `None` when the process had already ended, or begun to, as the watch
    /// was made.
    #[derive(Debug)]
    pub(super) struct Watch(Option<OwnedFd>);

    impl Watch {
        pub(super) fn wait(self) -> io::Result<()> {
            let Some(queue) = self.0 else {
                return Ok(());
            };
            exit_reported(&queue)
        }
    }

    /// Blocks until `queue` reports its one watch's event: the exit, the only
    /// one asked for.
    fn exit_reported(queue: &OwnedFd) -> io::Result<()> {
        loop {
            // SAFETY: zeroed is a valid `kevent`, which kevent only writes.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: no changes are passed; `event` is a live stack local of
            // the one entry asked for.
            let count = unsafe {
                libc::kevent(
                    queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    std::ptr::null(),
                )
            };
            if count >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// A new watch of `pid`'s exit (`EVFILT_PROC`, `NOTE_EXIT`); `None` when
    /// no process has that id. Each waiter needs its own: a queue reports the
    /// exit to one of the threads waiting on it.
    fn watch_exit(pid: libc::pid_t) -> io::Result<Option<OwnedFd>> {
        let Ok(ident) = libc::uintptr_t::try_from(pid) else {
            return Ok(None);
        };
        // SAFETY: kqueue takes no arguments and returns a new descriptor
        // or -1.
        let queue = unsafe { libc::kqueue() };
        if queue == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor was just returned to this function alone.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        // SAFETY: zeroed is a valid `kevent`; the fields that matter are
        // set below.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = ident;
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_EXIT;
        // SAFETY: one change from a live stack local, no events asked for.
        let added = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if added == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(queue))
    }

    impl Handle {
        pub(super) fn open(pid: u32, start: &str) -> io::Result<Option<Self>> {
            let Ok(pid) = libc::pid_t::try_from(pid) else {
                return Ok(None);
            };
            if !runs(pid, start)? {
                return Ok(None);
            }
            Ok(Some(Self {
                pid,
                start: start.to_owned(),
            }))
        }

        pub(super) fn liveness(&self) -> Liveness {
            match runs(self.pid, &self.start) {
                Ok(true) => Liveness::Alive,
                Ok(false) => Liveness::Dead,
                Err(_) => Liveness::Unknown,
            }
        }

        pub(super) fn wait(&self) -> io::Result<()> {
            self.watch()?.wait()
        }

        /// The watch is made before the start is checked, so it is known to
        /// be on the process the start names. A watch attaches only to a
        /// process that has not begun to exit (an exiting or zombie process
        /// answers ESRCH), so an attached one is sent the exit, once the
        /// process has closed its descriptors.
        pub(super) fn watch(&self) -> io::Result<Watch> {
            let Some(queue) = watch_exit(self.pid)? else {
                return Ok(Watch(None));
            };
            match read_info(self.pid)? {
                Some((_, start)) if start != self.start => Ok(Watch(None)),
                _ => Ok(Watch(Some(queue))),
            }
        }

        /// The start is checked again just before the signal; the gap
        /// between the two is left, macOS having no handle to signal through.
        pub(super) fn kill(&self) -> io::Result<()> {
            self.signal(libc::SIGKILL)
        }

        pub(super) fn signal(&self, signal: libc::c_int) -> io::Result<()> {
            if !runs(self.pid, &self.start)? {
                return Ok(());
            }
            // SAFETY: kill takes plain values.
            if unsafe { libc::kill(self.pid, signal) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod platform {
    use std::io;

    use super::Liveness;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "process identity is not supported on this platform",
        )
    }

    #[derive(Debug)]
    pub(super) struct Handle;

    pub(super) fn start_of(_pid: u32) -> io::Result<Option<String>> {
        Err(unsupported())
    }

    impl Handle {
        pub(super) fn open(_pid: u32, _start: &str) -> io::Result<Option<Self>> {
            Err(unsupported())
        }

        pub(super) fn liveness(&self) -> Liveness {
            Liveness::Unknown
        }

        pub(super) fn wait(&self) -> io::Result<()> {
            Err(unsupported())
        }

        pub(super) fn watch(&self) -> io::Result<Watch> {
            Err(unsupported())
        }

        pub(super) fn kill(&self) -> io::Result<()> {
            Err(unsupported())
        }
    }

    #[derive(Debug)]
    pub(super) enum Watch {}

    impl Watch {
        pub(super) fn wait(self) -> io::Result<()> {
            match self {}
        }
    }
}
