//! The end mark: a one-time run of digits written to a terminal once every
//! process that wrote to it has died, so the reader knows where their output
//! ends even while someone else keeps the terminal open.

use std::ffi::{CStr, CString};
use std::io::{self, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// Random bytes a mark is made from: 128 bits, which no process can guess.
const MARK_ENTROPY: usize = 16;

/// Decimal digits of the largest `u128`, so every mark has the same length.
const MARK_LEN: usize = 39;

/// The name of the terminal whose master end is `master`.
pub(crate) fn slave_name(master: RawFd) -> io::Result<CString> {
    let mut name = [0u8; 128];
    slave_name_into(master, &mut name)?;
    let name = CStr::from_bytes_until_nul(&name)
        .map_err(|_| io::Error::other("the terminal's name is not terminated"))?;
    Ok(name.to_owned())
}

#[cfg(target_os = "linux")]
fn slave_name_into(master: RawFd, name: &mut [u8; 128]) -> io::Result<()> {
    // SAFETY: `name` is a live buffer of the length passed.
    let result = unsafe { libc::ptsname_r(master, name.as_mut_ptr().cast(), name.len()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

/// `TIOCPTYGNAME` from `<sys/ttycom.h>`: `_IOC(IOC_OUT, 't', 83, 128)`. It is
/// what `ptsname_r` asks, without `ptsname`'s buffer shared by every thread.
#[cfg(target_os = "macos")]
const TIOCPTYGNAME: libc::c_ulong = 0x4080_7453;

#[cfg(target_os = "macos")]
fn slave_name_into(master: RawFd, name: &mut [u8; 128]) -> io::Result<()> {
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes, the size of `name`.
    if unsafe { libc::ioctl(master, TIOCPTYGNAME, name.as_mut_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn slave_name_into(_master: RawFd, _name: &mut [u8; 128]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the terminal's name is not known on this platform",
    ))
}

/// A new mark: digits of 128 random bits, always [`MARK_LEN`] long.
pub(crate) fn new_mark() -> io::Result<Vec<u8>> {
    let mut entropy = [0u8; MARK_ENTROPY];
    crate::fill_random(&mut entropy).map_err(io::Error::other)?;
    let mark = format!("{:0width$}", u128::from_le_bytes(entropy), width = MARK_LEN);
    Ok(mark.into_bytes())
}

/// The open end of a written end mark: the terminal's descriptor, kept so the
/// terminal stays open until the reader has fixed where the output ends.
///
/// Keep it until the mark was read back or the output's end of file was read,
/// then drop it. Where a terminal's last close waits for its output to be
/// read (macOS), the close would otherwise wait for a reader that is gone;
/// dropping sets `O_NONBLOCK` first, which makes that close discard the unread
/// output instead of waiting. That is lossless only because the reader has
/// already decided where the output ends: dropping it earlier can discard the
/// child's last output that the reader has not read yet.
#[derive(Debug)]
pub struct EndMarkWriter {
    terminal: std::fs::File,
}

impl Drop for EndMarkWriter {
    fn drop(&mut self) {
        let fd = self.terminal.as_raw_fd();
        // SAFETY: the descriptor is open for as long as `self.terminal` lives;
        // F_GETFL and F_SETFL only read and set its status flags.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
}

/// Opens the terminal `slave` by its name without making it a controlling
/// terminal, lets stopped output go on and writes `mark`, leaving the
/// terminal open in the returned [`EndMarkWriter`].
///
/// Blocks while the terminal's buffer is full, so its output must be read
/// meanwhile, by another thread than the caller.
pub(crate) fn write_mark(slave: &CStr, mark: &[u8]) -> io::Result<EndMarkWriter> {
    let terminal = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(Path::new(std::ffi::OsStr::from_bytes(slave.to_bytes())))?;
    let mut writer = EndMarkWriter { terminal };
    // SAFETY: the descriptor is open for as long as `writer` lives.
    if unsafe { libc::tcflow(writer.terminal.as_raw_fd(), libc::TCOON) } != 0 {
        return Err(io::Error::last_os_error());
    }
    writer.terminal.write_all(mark)?;
    Ok(writer)
}
