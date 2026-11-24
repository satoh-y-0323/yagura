//! Capturing the process at the other end of an accepted connection, at the
//! moment it is accepted: its id, its start and its executable, or nothing
//! when the operating system cannot name that process for certain.
//!
//! The capture itself is [`PeerAuth::capture_peer`](crate::PeerAuth::capture_peer).
//! This module holds what it returns, the order macOS reads a peer in (kept
//! free of the operating system so it compiles and is measured everywhere),
//! and the startup check that the capture is available at all.

use std::io;
use std::path::PathBuf;

use crate::ProcessStart;

/// The process captured at the other end of a connection: its id and start
/// name one process for good, and `executable` is the image it ran when it
/// was captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPeer {
    /// The process's id.
    pub pid: u32,
    /// The process's start, which tells it from a later process with its id.
    pub start: ProcessStart,
    /// The executable the process runs, as the operating system reports it
    /// (not canonicalized).
    pub executable: PathBuf,
}

/// The reads [`capture_in_order`] makes, in the order it makes them.
///
/// On macOS each read is a system call on the accepted socket or on the
/// process the socket's token names; the trait lets the order be run against
/// a script instead.
pub trait OrderReads {
    /// The `(pid, pidversion)` of the connection's peer token, or the
    /// `errno` the operating system answered with.
    ///
    /// # Errors
    ///
    /// Returns the `errno` of a token that cannot be read (for instance
    /// `ENOTCONN` once the peer is gone).
    fn peer_token(&mut self) -> Result<(u32, u32), i32>;

    /// The start of the process `pid`; `None` when it cannot be read.
    fn start_of(&mut self, pid: u32) -> Option<ProcessStart>;

    /// The executable the process `pid` runs; `None` when it cannot be read.
    fn executable_of(&mut self, pid: u32) -> Option<PathBuf>;

    /// The kernel's handle on the other end of this process's own socket;
    /// `None` when the socket names none.
    fn own_peer_pcb(&mut self) -> Option<u64>;

    /// Whether one of the descriptors of the process `pid` is the socket
    /// whose handle is `pcb`.
    fn peer_holds(&mut self, pid: u32, pcb: u64) -> bool;
}

/// Captures a peer through `reads`: the token names a process, that
/// process's start and executable are read, its descriptors must hold the
/// other end of this connection, and the token and start read again must be
/// the same. Any read that fails or disagrees captures nothing.
///
/// The token names whichever process last used the socket, looked up by id,
/// so the descriptor check is what ties the process to this connection, and
/// the second reading is what ties every read to one process.
#[must_use]
pub fn capture_in_order<R: OrderReads + ?Sized>(reads: &mut R) -> Option<CapturedPeer> {
    let token = reads.peer_token().ok()?;
    let (pid, _) = token;
    if pid == 0 {
        return None;
    }
    let start = reads.start_of(pid)?;
    let executable = reads.executable_of(pid)?;
    let pcb = reads.own_peer_pcb()?;
    if !reads.peer_holds(pid, pcb) {
        return None;
    }
    if reads.peer_token().ok()? != token || reads.start_of(pid)? != start {
        return None;
    }
    Some(CapturedPeer {
        pid,
        start,
        executable,
    })
}

/// Checks that this system can capture a connection's peer, which the daemon
/// and the GUI require before serving. On Linux this asks a socket pair for
/// its peer's pidfd (`SO_PEERPIDFD`, Linux 6.5 and later); every other
/// platform can capture.
///
/// # Errors
///
/// Returns the operating system's error when the check fails, whatever its
/// kind: any failure means the capture is unavailable.
pub fn check_peer_capture_available() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        crate::platform_unix::check_peer_capture()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}
