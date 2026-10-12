//! A started child process, shared between whoever waits on it and whoever
//! kills it.

use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::Arc;

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject,
};

use super::command_line::win32_error_to_io;

/// A child process started by this crate's spawn function.
///
/// Waiting and killing both take `&self` and may run at the same time from
/// different threads (or clones): they act on one kernel process object, and
/// holding its handle keeps the process id from being reused.
#[derive(Debug, Clone)]
pub struct ChildProcess {
    handle: Arc<OwnedHandle>,
    pid: u32,
}

impl ChildProcess {
    pub(crate) fn new(handle: OwnedHandle, pid: u32) -> Self {
        Self {
            handle: Arc::new(handle),
            pid,
        }
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle() as isize)
    }

    /// The child's process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Blocks until the child exits and returns its exit code.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait or the exit-code
    /// query fails.
    pub fn wait(&self) -> io::Result<u32> {
        // SAFETY: the handle is valid for as long as `self` holds it.
        let event = unsafe { WaitForSingleObject(self.raw(), INFINITE) };
        if event != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_code()
    }

    /// Returns the child's exit code when it has exited, `None` while it runs.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the wait or the exit-code
    /// query fails.
    pub fn try_wait(&self) -> io::Result<Option<u32>> {
        // SAFETY: the handle is valid for as long as `self` holds it.
        let event = unsafe { WaitForSingleObject(self.raw(), 0) };
        if event == WAIT_TIMEOUT {
            return Ok(None);
        }
        if event != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_code().map(Some)
    }

    /// Terminates the child with exit code `1`. A child that has already
    /// exited is not an error.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the child is still running
    /// and cannot be terminated.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: the handle is valid for as long as `self` holds it.
        match unsafe { TerminateProcess(self.raw(), 1) } {
            Ok(()) => Ok(()),
            Err(error) => match self.try_wait() {
                Ok(Some(_)) => Ok(()),
                _ => Err(win32_error_to_io(&error)),
            },
        }
    }

    fn exit_code(&self) -> io::Result<u32> {
        let mut code = 0u32;
        // SAFETY: the handle is valid for as long as `self` holds it, and
        // `code` is a live out-parameter for the call.
        unsafe { GetExitCodeProcess(self.raw(), &mut code) }
            .map_err(|error| win32_error_to_io(&error))?;
        Ok(code)
    }
}
