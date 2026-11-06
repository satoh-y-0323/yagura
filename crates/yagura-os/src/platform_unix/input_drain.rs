//! Reading a terminal's input on the far side, so a write to the terminal
//! that nobody reads any more can finish.
//!
//! A write to a pseudoterminal's master blocks while the terminal's input
//! queue is full. Once the child's tree has ended, a process outside it that
//! keeps the terminal open and never reads it would hold such a write up for
//! as long as it lives; reading the queue here takes its place. On Linux and
//! macOS the read itself wakes the blocked writer (`n_tty_check_unthrottle`,
//! `ptsd_read`).

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::thread::JoinHandle;

/// Bytes taken from the terminal's input at a time.
const DRAIN_CHUNK: usize = 4096;

/// Reads and drops the input of a terminal, opened by its name, until
/// dropped. Dropping stops the reading and waits for it to have stopped.
#[derive(Debug)]
pub struct InputDrain {
    stop: Option<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for InputDrain {
    fn drop(&mut self) {
        // Closing the pipe's writing end makes its reading end readable.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts reading the input of the terminal `slave`.
pub(crate) fn drain(slave: &CStr) -> io::Result<InputDrain> {
    // Its own open file, so the non-blocking mode touches nobody else's,
    // and a close that does not wait for output to be read (macOS).
    let terminal = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(Path::new(std::ffi::OsStr::from_bytes(slave.to_bytes())))?;
    let (stopped, stop) = pipe()?;
    let limit = RawFd::try_from(libc::FD_SETSIZE).unwrap_or(RawFd::MAX);
    if terminal.as_raw_fd() >= limit || stopped.as_raw_fd() >= limit {
        return Err(io::Error::other(
            "the descriptors are past what select can watch",
        ));
    }
    let thread = std::thread::Builder::new()
        .name("yagura-input-drain".to_owned())
        .spawn(move || drain_until_stopped(&terminal, &stopped))?;
    Ok(InputDrain {
        stop: Some(stop),
        thread: Some(thread),
    })
}

/// A pipe as (reading end, writing end), both closed on exec.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` is a live array of the two descriptors pipe() fills.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe() succeeded, so both are open descriptors owned by nobody
    // else.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for end in [&ends.0, &ends.1] {
        // SAFETY: the descriptor is open; F_SETFD only sets its flags.
        if unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ends)
}

/// Waits with `select`, which, unlike `poll`, watches a terminal on macOS.
fn drain_until_stopped(terminal: &std::fs::File, stopped: &OwnedFd) {
    let terminal = terminal.as_raw_fd();
    let stopped = stopped.as_raw_fd();
    let mut chunk = [0u8; DRAIN_CHUNK];
    loop {
        // SAFETY: an all-zero fd_set is valid; FD_ZERO, FD_SET and FD_ISSET
        // are given descriptors below FD_SETSIZE, checked by `drain`, and
        // borrowed open for as long as this runs.
        let (terminal_ready, stop_asked) = unsafe {
            let mut readable: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut readable);
            libc::FD_SET(terminal, &mut readable);
            libc::FD_SET(stopped, &mut readable);
            let ready = libc::select(
                terminal.max(stopped) + 1,
                &mut readable,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            (
                libc::FD_ISSET(terminal, &readable),
                libc::FD_ISSET(stopped, &readable),
            )
        };
        if stop_asked {
            return;
        }
        if terminal_ready {
            // SAFETY: `chunk` is a live buffer of the length passed.
            let read = unsafe { libc::read(terminal, chunk.as_mut_ptr().cast(), chunk.len()) };
            // A canonical terminal reads its end-of-file character as 0
            // bytes; the next select tells whether more is there.
            if read < 0 {
                let kind = io::Error::last_os_error().kind();
                if !matches!(kind, io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) {
                    return;
                }
            }
        }
    }
}
