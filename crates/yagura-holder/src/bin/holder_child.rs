//! Test utility: the child a holder runs in its tests, with one observable
//! behaviour per mode.
//!
//! Positional argv, no parser: `holder_child <mode> [<arg>...]`.
//! - `size`: prints `SIZE <cols>x<rows>\r\n` for the terminal's size, then
//!   again whenever it changes.
//! - `hex`: prints [`READY_MARKER`], then writes every byte read from stdin as
//!   two lowercase hexadecimal characters.
//! - `flood <count_file> [<total_bytes>]`: writes blocks of output and, after
//!   each block the write accepted, appends the running byte total as one line
//!   to `count_file`. A block stuck in a write is not counted, so the last line
//!   shows where back-pressure stopped the child. With `total_bytes`, stops
//!   after that many bytes, prints [`FLOODED_MARKER`] and stays alive until
//!   stdin ends.
//! - `tree <pid_file>`: starts a grandchild (itself, with `--grandchild`) that
//!   survives the terminal going away (SIGHUP ignored on Unix, no console on
//!   Windows), writes its process id to `pid_file`, prints [`TREE_MARKER`] and
//!   stays alive until stdin ends. Only a teardown of the whole tree ends the
//!   grandchild.
//! - `exit <code> [<gate_file>]`: prints `EXITING <code>\r\n` and exits with
//!   `code`; with `gate_file`, first waits until that file exists.

// Test-only fixture binary, not the holder's production spawn path: it spawns
// its own descendant to observe tree teardown.
#![allow(clippy::disallowed_methods)]

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const READY_MARKER: &[u8] = b"HOLDER-CHILD-READY\r\n";
const FLOODED_MARKER: &[u8] = b"\r\nHOLDER-CHILD-FLOODED\r\n";
const TREE_MARKER: &[u8] = b"HOLDER-CHILD-TREE\r\n";
const HEX: &[u8; 16] = b"0123456789abcdef";
const FLOOD_BLOCK: usize = 8192;
/// How often `size` and `exit` look again. The fixture has no event for either
/// on every platform; the test waits on the printed line, not on this period.
const POLL: Duration = Duration::from_millis(10);

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |index: usize| args.get(index).map_or("", String::as_str);
    match arg(0) {
        "--grandchild" => idle(),
        "size" => report_size(),
        "hex" => hex_echo(),
        "flood" => flood(Path::new(arg(1)), arg(2).parse().ok()),
        "tree" => tree(Path::new(arg(1))),
        "exit" => exit_with(arg(1), args.get(2).map(Path::new)),
        _ => {
            eprintln!(
                "usage: holder_child size | hex | flood <count_file> [<total_bytes>] \
                 | tree <pid_file> | exit <code> [<gate_file>]"
            );
            std::process::exit(2);
        }
    }
}

fn idle() -> ! {
    loop {
        std::thread::park();
    }
}

/// Lives until stdin reports end of input.
fn stay_alive() -> io::Result<()> {
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    Ok(())
}

fn report_size() -> io::Result<()> {
    let mut stdout = io::stdout();
    let mut last = None;
    loop {
        let size = terminal_size()?;
        if last != Some(size) {
            let (cols, rows) = size;
            stdout.write_all(format!("SIZE {cols}x{rows}\r\n").as_bytes())?;
            stdout.flush()?;
            last = Some(size);
        }
        std::thread::sleep(POLL);
    }
}

fn hex_echo() -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("holder_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }
    let mut stdout = io::stdout();
    stdout.write_all(READY_MARKER)?;
    stdout.flush()?;

    let mut input = io::stdin().lock();
    let mut bytes = [0u8; 4096];
    let mut encoded = [0u8; 8192];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        for (index, byte) in bytes[..count].iter().copied().enumerate() {
            encoded[index * 2] = HEX[usize::from(byte >> 4)];
            encoded[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        stdout.write_all(&encoded[..count * 2])?;
        stdout.flush()?;
    }
}

fn flood(count_file: &Path, total_bytes: Option<u64>) -> io::Result<()> {
    let mut counts = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(count_file)?;
    let block = [b'x'; FLOOD_BLOCK];
    let mut stdout = io::stdout();
    let mut written: u64 = 0;
    loop {
        let len = match total_bytes {
            Some(total) if written >= total => break,
            Some(total) => {
                usize::try_from(total - written).map_or(FLOOD_BLOCK, |rest| rest.min(FLOOD_BLOCK))
            }
            None => FLOOD_BLOCK,
        };
        stdout.write_all(&block[..len])?;
        stdout.flush()?;
        written += len as u64;
        counts.write_all(format!("{written}\n").as_bytes())?;
    }
    stdout.write_all(FLOODED_MARKER)?;
    stdout.flush()?;
    stay_alive()
}

fn tree(pid_file: &Path) -> io::Result<()> {
    let grandchild = spawn_surviving_grandchild()?;
    std::fs::write(pid_file, grandchild.to_string())?;
    let mut stdout = io::stdout();
    stdout.write_all(TREE_MARKER)?;
    stdout.flush()?;
    stay_alive()
}

fn exit_with(code: &str, gate_file: Option<&Path>) -> io::Result<()> {
    let Ok(code) = code.parse::<i32>() else {
        eprintln!("holder_child: exit code {code:?} is not an integer");
        std::process::exit(2);
    };
    if let Some(gate_file) = gate_file {
        while !gate_file.exists() {
            std::thread::sleep(POLL);
        }
    }
    let mut stdout = io::stdout();
    stdout.write_all(format!("EXITING {code}\r\n").as_bytes())?;
    stdout.flush()?;
    std::process::exit(code)
}

fn grandchild_command() -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--grandchild")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// With no console attached, closing the pseudoconsole sends the grandchild
/// nothing; it stays in the child's job.
#[cfg(windows)]
fn spawn_surviving_grandchild() -> io::Result<u32> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let child = grandchild_command()?
        .creation_flags(DETACHED_PROCESS)
        .spawn()?;
    Ok(child.id())
}

/// An ignored signal stays ignored across exec, so the grandchild ignores
/// SIGHUP from its first instruction; it stays in the child's process group.
#[cfg(unix)]
fn spawn_surviving_grandchild() -> io::Result<u32> {
    // SAFETY: `signal` only changes this process's disposition of SIGHUP; no
    // handler function is installed.
    let previous = unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let spawned = grandchild_command().and_then(|mut command| command.spawn());
    // SAFETY: restores the disposition read above.
    unsafe { libc::signal(libc::SIGHUP, previous) };
    Ok(spawned?.id())
}

#[cfg(windows)]
fn terminal_size() -> io::Result<(u16, u16)> {
    use windows::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: the handle is the process's own standard output, borrowed and
    // not closed; `info` is a stack local the call writes before it is read.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE).map_err(io::Error::other)?;
        GetConsoleScreenBufferInfo(stdout, &mut info).map_err(io::Error::other)?;
    }
    let window = info.srWindow;
    let cols = u16::try_from(i32::from(window.Right) - i32::from(window.Left) + 1);
    let rows = u16::try_from(i32::from(window.Bottom) - i32::from(window.Top) + 1);
    match (cols, rows) {
        (Ok(cols), Ok(rows)) => Ok((cols, rows)),
        _ => Err(io::Error::other("the console window has a negative size")),
    }
}

#[cfg(unix)]
fn terminal_size() -> io::Result<(u16, u16)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` into the pointed-to stack local.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((size.ws_col, size.ws_row))
}
