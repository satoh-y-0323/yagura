//! A window's terminal on Unix.
//!
//! Two ways in. [`spawn`] has portable-pty open the pseudoterminal and start
//! the child as its own session leader, with the child's process group taken
//! from the child's id the moment it exists. [`open`] opens the terminal alone
//! and hands its slave out, and [`start_leader`] starts a child on such a
//! slave: a holder keeps the terminal while its watcher starts the child.

use std::ffi::{CString, OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty};

pub use super::input_drain::InputDrain;
pub use super::tty_mark::EndMarkWriter;
use super::{UnixProcessGroup, input_drain, tty_mark};
use crate::pty::{EnvOp, PtyCommand, PtySize, PtySpawn, PtySpawnError};
use crate::{PROCESS_GROUP_TERM_GRACE, ReapGate};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pty_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn to_portable(size: PtySize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The master end a [`PtyControl`] resizes and names the terminal by.
enum Master {
    Portable(Box<dyn MasterPty + Send>),
    Own(OwnedFd),
}

impl Master {
    fn resize(&self, size: PtySize) -> io::Result<()> {
        match self {
            Self::Portable(master) => master.resize(to_portable(size)).map_err(pty_error),
            Self::Own(fd) => set_size(fd.as_raw_fd(), size),
        }
    }

    fn raw_fd(&self) -> Option<RawFd> {
        match self {
            Self::Portable(master) => master.as_raw_fd(),
            Self::Own(fd) => Some(fd.as_raw_fd()),
        }
    }
}

fn set_size(master: RawFd, size: PtySize) -> io::Result<()> {
    let size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads one `winsize`, a live local, from the pointer.
    if unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &raw const size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Resizes a window's terminal and lets go of it.
///
/// Closing only releases the handle kept for resizing; the output side stays
/// open for as long as its reader does.
pub struct PtyControl {
    master: Mutex<Option<Master>>,
}

impl PtyControl {
    /// Changes the terminal's size to `cols` x `rows`.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal has been closed or refuses the size.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let master = lock(&self.master);
        let Some(master) = master.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the pseudoterminal is closed",
            ));
        };
        master.resize(PtySize { cols, rows })
    }

    /// Releases the handle kept for resizing. Does nothing when already
    /// closed.
    pub fn close(&self) {
        drop(lock(&self.master).take());
    }

    /// A new end mark: a run of ASCII digits nobody could know before, made
    /// before it is written so the reader can look for it from the start.
    ///
    /// # Errors
    ///
    /// Returns an error when no randomness is available.
    pub fn new_end_mark() -> io::Result<Vec<u8>> {
        tty_mark::new_mark()
    }

    /// Writes `mark` to the terminal by opening the terminal by its name
    /// (never as a controlling terminal) after letting stopped output go on,
    /// and returns the still open terminal.
    ///
    /// Written once every process that wrote to the terminal has died, the
    /// mark is the last thing the terminal's output holds, even while another
    /// process keeps the terminal open so the output never ends. Blocks while
    /// the terminal's buffer is full: read the output on another thread. Keep
    /// the returned writer until the reader has read the mark back or reached
    /// the end of file (see [`EndMarkWriter`]).
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal has been closed, or cannot be named,
    /// opened or written.
    pub fn write_mark(&self, mark: &[u8]) -> io::Result<EndMarkWriter> {
        let slave = self.slave_name()?;
        // The write may block on a full buffer, so a resize must not wait for it.
        tty_mark::write_mark(&slave, mark)
    }

    /// Starts reading and dropping what is written to the terminal, on
    /// another thread, until the returned [`InputDrain`] is dropped.
    ///
    /// Once nothing is to read the terminal any more, this lets a write to
    /// it finish that a full input queue holds up, even while another
    /// process keeps the terminal open and never reads it.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal has been closed or cannot be named
    /// or opened, or the thread cannot be started.
    pub fn drain_input(&self) -> io::Result<InputDrain> {
        input_drain::drain(&self.slave_name()?)
    }

    fn slave_name(&self) -> io::Result<CString> {
        let master = lock(&self.master);
        let fd = master
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "the pseudoterminal is closed")
            })?
            .raw_fd()
            .ok_or_else(|| io::Error::other("the pseudoterminal has no descriptor"))?;
        tty_mark::slave_name(fd)
    }
}

impl std::fmt::Debug for PtyControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyControl")
            .field("open", &lock(&self.master).is_some())
            .finish_non_exhaustive()
    }
}

/// The started child, to wait on.
#[derive(Debug)]
pub struct PtyChild {
    child: Box<dyn Child + Send + Sync>,
    pid: u32,
}

impl PtyChild {
    /// The child's process id.
    #[must_use]
    pub fn process_id(&self) -> u32 {
        self.pid
    }

    /// Blocks until the child exits and returns its exit code.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when waiting fails.
    pub fn wait(mut self) -> io::Result<i32> {
        self.child.wait().map(|status| status.exit_code() as i32)
    }

    /// Blocks until the child exits, leaving it unreaped for [`Self::wait`],
    /// so its id keeps naming it until then.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when waiting fails.
    pub fn wait_for_exit(&self) -> io::Result<()> {
        super::wait_for_exit_without_reaping(self.pid)
    }
}

/// Kills the child. Clones act on the same process and work while another
/// thread waits for it.
#[derive(Debug)]
pub struct PtyKiller(Mutex<Box<dyn ChildKiller + Send + Sync>>);

impl PtyKiller {
    /// Asks the child to end, under `gate`'s lock, unless the child has been
    /// reaped through `gate`: its id may name another process by then. A
    /// child that has exited is not an error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the signal cannot be sent.
    pub fn kill(&self, gate: &ReapGate) -> io::Result<()> {
        gate.signal_unless_reaped(|| lock(&self.0).kill())
            .unwrap_or(Ok(()))
    }
}

impl Clone for PtyKiller {
    fn clone(&self) -> Self {
        Self(Mutex::new(lock(&self.0).clone_killer()))
    }
}

/// Ends a child that was started but cannot be handed over.
fn reap(mut child: Box<dyn Child + Send + Sync>) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Starts `command` on a new pseudoterminal as a session leader whose process
/// group is the child's id.
pub(crate) fn spawn(command: &PtyCommand, size: PtySize) -> Result<PtySpawn, PtySpawnError> {
    let spawn_error = PtySpawnError::Spawn;
    let (program, args) = command.argv.split_first().ok_or_else(|| {
        spawn_error(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argv must not be empty",
        ))
    })?;

    let pair = portable_pty::native_pty_system()
        .openpty(to_portable(size))
        .map_err(|error| spawn_error(pty_error(error)))?;
    let mut builder = CommandBuilder::new(program);
    builder.args(args);
    if let Some(cwd) = &command.cwd {
        builder.cwd(cwd);
    }
    apply_env(&mut builder, &command.env);

    // Taken before the child starts: a failure here (out of descriptors) then
    // leaves no child to kill, which a failure after the start would.
    let (writer, reader) = pair
        .master
        .take_writer()
        .and_then(|writer| Ok((writer, pair.master.try_clone_reader()?)))
        .map_err(|error| spawn_error(pty_error(error)))?;
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|error| spawn_error(pty_error(error)))?;
    // The parent's copy of the slave must go so the reader sees the end of the
    // output once every process on the terminal is gone.
    drop(pair.slave);

    // The group is made from the id of the child just started, not read back
    // later: a child that exits and is reaped first would no longer have one.
    let Some(pid) = child.process_id().filter(|&pid| pid != 0) else {
        reap(child);
        return Err(spawn_error(io::Error::other(
            "the started child has no process id",
        )));
    };
    let group = match UnixProcessGroup::led_by(pid, PROCESS_GROUP_TERM_GRACE) {
        Ok(group) => group,
        Err(error) => {
            reap(child);
            return Err(spawn_error(crate::capture::os_error_to_io(error)));
        }
    };
    let killer = child.clone_killer();

    Ok(PtySpawn {
        child: PtyChild { child, pid },
        killer: PtyKiller(Mutex::new(killer)),
        group,
        control: PtyControl {
            master: Mutex::new(Some(Master::Portable(pair.master))),
        },
        reader,
        writer,
    })
}

fn apply_env(builder: &mut CommandBuilder, env: &[EnvOp]) {
    for op in env {
        match op {
            EnvOp::Set(name, value) => builder.env(name, value),
            EnvOp::Remove(name) => builder.env_remove(name),
        }
    }
}

/// The value `name` has in a window child's base environment.
pub(crate) fn default_env_var(name: &OsStr) -> Option<OsString> {
    CommandBuilder::new_default_prog()
        .get_env(name)
        .map(OsStr::to_os_string)
}

/// A terminal opened with no child on it: the side its keeper holds, and the
/// slave for whoever starts the child on it ([`start_leader`]).
///
/// Every descriptor here is close-on-exec, and none of them makes the
/// terminal anyone's controlling terminal.
pub struct PtyPair {
    /// Resizes and closes the terminal.
    pub control: PtyControl,
    /// What the child writes. Read it until it ends.
    pub reader: Box<dyn Read + Send>,
    /// What the child reads.
    pub writer: Box<dyn Write + Send>,
    /// The terminal's slave, to start the child on. Close every copy of it
    /// once the child has started, so the reader sees the end of the output
    /// once every process on the terminal is gone.
    pub slave: OwnedFd,
}

impl std::fmt::Debug for PtyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyPair")
            .field("control", &self.control)
            .finish_non_exhaustive()
    }
}

/// Opens a new pseudoterminal of `size` with no child on it.
pub(crate) fn open(size: PtySize) -> io::Result<PtyPair> {
    // O_NOCTTY on both ends: a session leader without a controlling terminal
    // (a holder) would otherwise take the terminal as its own.
    let master = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open("/dev/ptmx")?;
    let master = OwnedFd::from(master);
    // SAFETY: `master` is an open pseudoterminal master for both calls, which
    // only change the state of its slave.
    unsafe {
        if libc::grantpt(master.as_raw_fd()) != 0 || libc::unlockpt(master.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let name = tty_mark::slave_name(master.as_raw_fd())?;
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(Path::new(OsStr::from_bytes(name.to_bytes())))?;
    set_size(master.as_raw_fd(), size)?;
    let reader = MasterReader(std::fs::File::from(master.try_clone()?));
    let writer = MasterWriter(std::fs::File::from(master.try_clone()?));
    Ok(PtyPair {
        control: PtyControl {
            master: Mutex::new(Some(Master::Own(master))),
        },
        reader: Box::new(reader),
        writer: Box::new(writer),
        slave: OwnedFd::from(slave),
    })
}

/// The output of a terminal opened by [`open`]. The master reads `EIO` once no
/// process has the slave open, which is the output's end.
struct MasterReader(std::fs::File);

impl Read for MasterReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

/// The input of a terminal opened by [`open`]. Dropped, it ends the input as
/// portable-pty's writer does: a newline and the terminal's end-of-file
/// character, so a child reading it sees its input end.
struct MasterWriter(std::fs::File);

impl Write for MasterWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Drop for MasterWriter {
    fn drop(&mut self) {
        // SAFETY: zeroed is a valid `termios`, which tcgetattr only writes.
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is open for as long as `self.0` lives, and
        // `settings` is a live local of the type tcgetattr writes.
        if unsafe { libc::tcgetattr(self.0.as_raw_fd(), &mut settings) } == 0 {
            let end_of_file = settings.c_cc[libc::VEOF];
            if end_of_file != 0 {
                let _ = self.0.write_all(&[b'\n', end_of_file]);
            }
        }
    }
}

/// A window's child as it is started on a terminal opened by [`open`]: its
/// command line, and the directory and whole environment it starts with.
#[derive(Clone, PartialEq, Eq)]
pub struct LeaderCommand {
    /// The program followed by its arguments; a program without a `/` is
    /// looked up in the working directory, then in `env`'s `PATH`.
    pub argv: Vec<String>,
    /// The working directory, an existing one.
    pub cwd: String,
    /// The child's whole environment.
    pub env: Vec<(String, String)>,
}

impl std::fmt::Debug for LeaderCommand {
    // An environment routinely carries tokens, so only the names are shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderCommand")
            .field("argv", &self.argv)
            .field("cwd", &self.cwd)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The child `command` describes as [`spawn`] would start it: from this
/// process's environment with `SHELL` set to the user's shell, changed by
/// `command.env`, in `command.cwd` when it is a directory and the home
/// directory otherwise. A variable whose name or value is not UTF-8 is left
/// out.
pub(crate) fn leader_command(command: &PtyCommand) -> io::Result<LeaderCommand> {
    let program = command
        .argv
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "argv must not be empty"))?;
    let mut builder = CommandBuilder::new(program);
    apply_env(&mut builder, &command.env);
    let mut env: Vec<(String, String)> = builder
        .iter_full_env_as_str()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    // portable-pty sets SHELL before the environment, which a SHELL there
    // replaces.
    if !env.iter().any(|(name, _)| name == "SHELL") {
        env.push(("SHELL".to_owned(), builder.get_shell()));
    }
    let cwd = match command.cwd.as_deref().filter(|dir| dir.is_dir()) {
        Some(dir) => dir.to_path_buf(),
        None => builder
            .get_env("HOME")
            .map(PathBuf::from)
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/")),
    };
    let cwd = cwd.into_os_string().into_string().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the working directory is not UTF-8",
        )
    })?;
    Ok(LeaderCommand {
        argv: command.argv.clone(),
        cwd,
        env,
    })
}

/// The user's home directory from the user database.
fn home_dir() -> Option<PathBuf> {
    let mut buffer = vec![0u8; 16 * 1024];
    // SAFETY: zeroed is a valid `passwd`, which getpwuid_r only writes.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `entry`, `buffer` and `found` are live locals; `buffer`'s length
    // is the one passed, and the strings `entry` points at live in it.
    let result = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so `pw_dir` is a NUL-terminated string in
    // `buffer`, which is still alive.
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes())))
}

/// The program `program` names, looked up as portable-pty does: a relative
/// name first in `cwd`, then in each directory of `path`.
fn resolve_program(program: &str, cwd: &str, path: Option<&str>) -> io::Result<PathBuf> {
    let exe = Path::new(program);
    if exe.is_absolute() {
        if is_runnable(exe) {
            return Ok(exe.to_path_buf());
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{program} does not exist or is not executable"),
        ));
    }
    let in_cwd = Path::new(cwd).join(exe);
    if in_cwd.exists() {
        return Ok(in_cwd);
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(exe);
            if is_runnable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{program} is neither in the working directory nor in PATH"),
    ))
}

/// Whether this process may execute `path`.
fn is_runnable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a NUL-terminated string that outlives the call.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

/// One past the highest signal number.
#[cfg(target_os = "linux")]
const SIGNAL_END: libc::c_int = 65;
#[cfg(not(target_os = "linux"))]
const SIGNAL_END: libc::c_int = 32;

/// Starts `command` on `slave`, a terminal's slave from [`open`], as the
/// leader of a new session whose controlling terminal it is, and returns its
/// process id. The child starts with every signal's default action and none
/// blocked, and with no descriptor but its standard three, all on `slave`.
/// It is left unreaped for the caller.
///
/// # Errors
///
/// Returns an error when the program cannot be found or started, or when
/// `slave` cannot be copied.
// The OS boundary owns this spawn: the watcher that calls it owns the child's
// session and ends its tree, and the ban in `clippy.toml` keeps every other
// call site from creating a child of its own.
#[allow(clippy::disallowed_methods)]
pub(crate) fn start_leader(command: &LeaderCommand, slave: OwnedFd) -> io::Result<u32> {
    let (program, args) = command
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "argv must not be empty"))?;
    let path = command
        .env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.as_str());
    let resolved = resolve_program(program, &command.cwd, path)?;
    let mut child = Command::new(&resolved);
    child
        .arg0(program)
        .args(args)
        .current_dir(&command.cwd)
        .env_clear()
        .envs(command.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    super::start_in_new_session_with_only_stdio(&mut child)?;
    // SAFETY: the closure runs in the forked child between fork and exec,
    // after the session is made. It calls only signal(2), sigemptyset(3),
    // pthread_sigmask(3) and ioctl(2), takes no lock and allocates nothing.
    unsafe {
        child.pre_exec(|| {
            for signal in 1..SIGNAL_END {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut none);
            libc::pthread_sigmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Dropping the handle neither waits nor kills: the child stays unreaped.
    let started = child.spawn()?;
    Ok(started.id())
}
