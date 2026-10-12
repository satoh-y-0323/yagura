//! A window's terminal on Unix: portable-pty opens the pseudoterminal and
//! starts the child as its own session leader, and the child's process group
//! is taken from the child's id the moment it exists.

use std::ffi::{OsStr, OsString};
use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty};

use super::{DEFAULT_TERM_GRACE, UnixProcessGroup};
use crate::pty::{EnvOp, PtyCommand, PtySize, PtySpawn, PtySpawnError};

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

/// Resizes a window's terminal and lets go of it.
///
/// Closing only releases the handle kept for resizing; the output side stays
/// open for as long as its reader does.
pub struct PtyControl {
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
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
        master
            .resize(to_portable(PtySize { cols, rows }))
            .map_err(pty_error)
    }

    /// Releases the handle kept for resizing. Does nothing when already
    /// closed.
    pub fn close(&self) {
        drop(lock(&self.master).take());
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
}

/// Kills the child. Clones act on the same process and work while another
/// thread waits for it.
#[derive(Debug)]
pub struct PtyKiller(Mutex<Box<dyn ChildKiller + Send + Sync>>);

impl PtyKiller {
    /// Asks the child to end. A child that has already exited is not an error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the signal cannot be sent.
    pub fn kill(&self) -> io::Result<()> {
        lock(&self.0).kill()
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
    for op in &command.env {
        match op {
            EnvOp::Set(name, value) => builder.env(name, value),
            EnvOp::Remove(name) => builder.env_remove(name),
        }
    }

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
    let group = match UnixProcessGroup::led_by(pid, DEFAULT_TERM_GRACE) {
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
            master: Mutex::new(Some(pair.master)),
        },
        reader,
        writer,
    })
}

/// The value `name` has in a window child's base environment.
pub(crate) fn default_env_var(name: &OsStr) -> Option<OsString> {
    CommandBuilder::new_default_prog()
        .get_env(name)
        .map(OsStr::to_os_string)
}
