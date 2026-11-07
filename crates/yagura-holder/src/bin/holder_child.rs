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
//! - `exit-on-input`: prints [`READY_MARKER`], then exits with 0 as soon as
//!   it reads a byte from stdin.
//! - `ignore-end`: ignores every request to end that can be ignored (SIGTERM
//!   and SIGHUP on Unix; console control events, the console close included,
//!   on Windows), prints [`IGNORING_MARKER`] and stays alive, past stdin's end
//!   too. Only a kill that cannot be refused ends it.
//! - `stubborn-tree <pid_file> [<gate_file>]`: like `tree`, but the grandchild
//!   also ignores SIGTERM on Unix, so only a kill that cannot be refused ends
//!   it. Prints [`TREE_MARKER`]; with `gate_file`, exits with 0 once that file
//!   exists, otherwise stays alive until stdin ends.
//! - `escaped-holder <pid_file>` (Unix): starts a grandchild in a session of
//!   its own that keeps the terminal open and never reads it, writes its
//!   process id to `pid_file`, prints [`ESCAPED_MARKER`] and idles until a
//!   Ctrl+C on the terminal ends it.
//! - `script <input_log> [<gate_file> <hex>]`: prints [`SCRIPT_MARKER`], then appends every byte
//!   read from stdin to `input_log` as read, so a test sees what reached the
//!   child and in which order. Lowercase hex digits between `<` and `>` are
//!   written to stdout decoded; a `<` restarts the command, and everything
//!   else is only logged. With `gate_file` and `hex`, it also writes `hex`
//!   decoded to stdout once `gate_file` exists, read or not.
//! - `end-race <info_file> <gate_file> <term_file>` (Unix): starts two
//!   grandchildren: a member of its process group that ignores SIGHUP and,
//!   on each SIGTERM, appends `TERMED` to `term_file` and lives on; and one in
//!   a session of its own that writes `e` to the terminal every few
//!   milliseconds for as long as it lives. Writes `<member> <escaped> <tty>`
//!   to `info_file` once the member is armed, prints [`END_RACE_MARKER`] and
//!   exits with 0 once `gate_file` exists.
//! - `late-tree <gate_file> <pid_file>` (Unix): ignores SIGTERM and SIGHUP,
//!   prints [`LATE_TREE_WAITING_MARKER`] and, once `gate_file` exists, starts a
//!   grandchild in a session of its own that ignores them too, writes its
//!   process id to `pid_file` and prints [`LATE_TREE_MARKER`]. Both then idle,
//!   so only a kill that cannot be refused ends them.
//! - `orphans` (Linux): makes itself a child subreaper, starts a child that
//!   starts [`ORPHANS`] grandchildren in sessions of their own and exits, and
//!   reaps the orphans through `yagura-os` on each SIGCHLD, never the child.
//!   Prints `LEADER <pid>` for the child, `ORPHAN <pid>` for each grandchild
//!   and `REAPED <pid>` for each process reaped, then `ORPHANS-DONE` once
//!   every orphan is reaped, and stays alive until stdin ends.

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
const IGNORING_MARKER: &[u8] = b"HOLDER-CHILD-IGNORING\r\n";
#[cfg(unix)]
const ESCAPED_MARKER: &[u8] = b"HOLDER-CHILD-ESCAPED\r\n";
const SCRIPT_MARKER: &[u8] = b"HOLDER-CHILD-SCRIPT\r\n";
#[cfg(unix)]
const END_RACE_MARKER: &[u8] = b"HOLDER-CHILD-END-RACE\r\n";
#[cfg(unix)]
const LATE_TREE_WAITING_MARKER: &[u8] = b"HOLDER-CHILD-LATE-TREE-WAITING\r\n";
#[cfg(unix)]
const LATE_TREE_MARKER: &[u8] = b"HOLDER-CHILD-LATE-TREE-STARTED\r\n";
/// The grandchildren `orphans` leaves behind.
#[cfg(target_os = "linux")]
const ORPHANS: usize = 10;
const HEX: &[u8; 16] = b"0123456789abcdef";
const FLOOD_BLOCK: usize = 8192;
/// How often `size`, `exit` and a gated `script` look again. The fixture has
/// no event for them on every platform; the test waits on what is printed, not
/// on this period.
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
        "exit-on-input" => exit_on_input(),
        "ignore-end" => ignore_end(),
        "stubborn-tree" => stubborn_tree(Path::new(arg(1)), args.get(2).map(Path::new)),
        #[cfg(unix)]
        "escaped-holder" => escaped_holder(Path::new(arg(1))),
        "script" => script(
            Path::new(arg(1)),
            args.get(2)
                .map(Path::new)
                .zip(args.get(3).map(String::as_str)),
        ),
        #[cfg(unix)]
        "end-race" => end_race(Path::new(arg(1)), Path::new(arg(2)), Path::new(arg(3))),
        #[cfg(unix)]
        "late-tree" => late_tree(Path::new(arg(1)), Path::new(arg(2))),
        #[cfg(unix)]
        "--term-member" => term_member(Path::new(arg(1))),
        #[cfg(unix)]
        "--escaped-writer" => escaped_writer(),
        #[cfg(target_os = "linux")]
        "orphans" => orphans(),
        #[cfg(target_os = "linux")]
        "--orphan-leader" => orphan_leader(),
        #[cfg(target_os = "linux")]
        "--orphan" => orphan(arg(1)),
        _ => {
            eprintln!(
                "usage: holder_child size | hex | flood <count_file> [<total_bytes>] \
                 | tree <pid_file> | exit <code> [<gate_file>] | exit-on-input | ignore-end \
                 | stubborn-tree <pid_file> [<gate_file>] | escaped-holder <pid_file> \
                 | script <input_log> [<gate_file> <hex>] | end-race <info_file> <gate_file> <term_file> \
                 | late-tree <gate_file> <pid_file> | orphans"
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

fn script(input_log: &Path, gated: Option<(&Path, &str)>) -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("holder_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }
    if let Some((gate_file, hex)) = gated {
        let gate_file = gate_file.to_path_buf();
        let bytes = decode_hex(hex.as_bytes());
        std::thread::spawn(move || {
            while !gate_file.exists() {
                std::thread::sleep(POLL);
            }
            let mut stdout = io::stdout();
            let _ = stdout.write_all(&bytes).and_then(|()| stdout.flush());
        });
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(input_log)?;
    let mut stdout = io::stdout();
    stdout.write_all(SCRIPT_MARKER)?;
    stdout.flush()?;

    let mut input = io::stdin().lock();
    let mut bytes = [0u8; 4096];
    let mut command: Option<Vec<u8>> = None;
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        // Logged before acting on it, so the log order is the read order.
        log.write_all(&bytes[..count])?;
        for byte in bytes[..count].iter().copied() {
            match byte {
                b'<' => command = Some(Vec::new()),
                b'>' => {
                    if let Some(digits) = command.take() {
                        stdout.write_all(&decode_hex(&digits))?;
                        stdout.flush()?;
                    }
                }
                b'0'..=b'9' | b'a'..=b'f' => {
                    if let Some(digits) = command.as_mut() {
                        digits.push(byte);
                    }
                }
                _ => {}
            }
        }
    }
}

fn decode_hex(digits: &[u8]) -> Vec<u8> {
    let value = |digit: u8| match digit {
        b'0'..=b'9' => digit - b'0',
        _ => digit - b'a' + 10,
    };
    let (pairs, _) = digits.as_chunks::<2>();
    pairs
        .iter()
        .map(|[high, low]| (value(*high) << 4) | value(*low))
        .collect()
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

fn stubborn_tree(pid_file: &Path, gate_file: Option<&Path>) -> io::Result<()> {
    let grandchild = spawn_stubborn_grandchild()?;
    std::fs::write(pid_file, grandchild.to_string())?;
    let mut stdout = io::stdout();
    stdout.write_all(TREE_MARKER)?;
    stdout.flush()?;
    let Some(gate_file) = gate_file else {
        return stay_alive();
    };
    while !gate_file.exists() {
        std::thread::sleep(POLL);
    }
    std::process::exit(0)
}

/// The grandchild holds the terminal from another session, so neither the
/// child's process group nor the end of the child's session reaches it.
#[cfg(unix)]
fn escaped_holder(pid_file: &Path) -> io::Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--grandchild");
    in_own_session(&mut command);
    let grandchild = command.spawn()?;
    std::fs::write(pid_file, grandchild.id().to_string())?;
    let mut stdout = io::stdout();
    stdout.write_all(ESCAPED_MARKER)?;
    stdout.flush()?;
    idle()
}

/// The grandchild starts only once the gate opens, so a test can open it after
/// the child has been moved to where the grandchild should start.
#[cfg(unix)]
fn late_tree(gate_file: &Path, pid_file: &Path) -> io::Result<()> {
    ignore_end_requests()?;
    let mut stdout = io::stdout();
    stdout.write_all(LATE_TREE_WAITING_MARKER)?;
    stdout.flush()?;
    while !gate_file.exists() {
        std::thread::sleep(POLL);
    }
    let mut command = grandchild_command()?;
    in_own_session(&mut command);
    let grandchild = command.spawn()?;
    let staged = pid_file.with_extension("staged");
    std::fs::write(&staged, grandchild.id().to_string())?;
    std::fs::rename(&staged, pid_file)?;
    stdout.write_all(LATE_TREE_MARKER)?;
    stdout.flush()?;
    idle()
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

fn exit_on_input() -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("holder_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }
    let mut stdout = io::stdout();
    stdout.write_all(READY_MARKER)?;
    stdout.flush()?;
    let mut byte = [0u8; 1];
    io::stdin().lock().read_exact(&mut byte)?;
    std::process::exit(0)
}

fn ignore_end() -> io::Result<()> {
    ignore_end_requests()?;
    let mut stdout = io::stdout();
    stdout.write_all(IGNORING_MARKER)?;
    stdout.flush()?;
    idle()
}

#[cfg(unix)]
fn ignore_end_requests() -> io::Result<()> {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `signal` only changes this process's disposition of the
        // signal; no handler function is installed.
        if unsafe { libc::signal(signal, libc::SIG_IGN) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A console close cannot be refused outright: the system ends the process
/// once the handler returns or times out, so the handler never returns.
#[cfg(windows)]
fn ignore_end_requests() -> io::Result<()> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::{
        CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        if matches!(
            event,
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
        ) {
            idle();
        }
        true.into()
    }
    // SAFETY: `handler` lives as long as the process and only parks its
    // thread or returns.
    unsafe { SetConsoleCtrlHandler(Some(handler), true) }.map_err(io::Error::other)
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
fn spawn_stubborn_grandchild() -> io::Result<u32> {
    spawn_surviving_grandchild()
}

#[cfg(unix)]
fn spawn_stubborn_grandchild() -> io::Result<u32> {
    let ignored = [libc::SIGTERM, libc::SIGHUP];
    // SAFETY: `signal` only changes this process's dispositions; no handler
    // function is installed.
    let previous = ignored.map(|signal| unsafe { libc::signal(signal, libc::SIG_IGN) });
    let spawned = grandchild_command().and_then(|mut command| command.spawn());
    for (signal, disposition) in ignored.into_iter().zip(previous) {
        // SAFETY: restores the disposition read above.
        unsafe { libc::signal(signal, disposition) };
    }
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

/// The file a `--term-member`'s SIGTERM handler appends to; -1 until opened.
#[cfg(unix)]
static TERM_FILE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(unix)]
const TERMED_LINE: &[u8] = b"TERMED\n";
#[cfg(unix)]
const ARMED_LINE: &str = "ARMED\n";

#[cfg(unix)]
extern "C" fn note_term(_: libc::c_int) {
    let fd = TERM_FILE.load(std::sync::atomic::Ordering::SeqCst);
    // SAFETY: write is async-signal-safe and the buffer is a static.
    unsafe { libc::write(fd, TERMED_LINE.as_ptr().cast(), TERMED_LINE.len()) };
}

#[cfg(unix)]
fn term_member(term_file: &Path) -> io::Result<()> {
    use std::os::fd::IntoRawFd as _;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(term_file)?;
    let mut armed = file.try_clone()?;
    TERM_FILE.store(file.into_raw_fd(), std::sync::atomic::Ordering::SeqCst);
    // SAFETY: SIG_IGN installs no handler, and `note_term` only calls write.
    unsafe {
        if libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR
            || libc::signal(
                libc::SIGTERM,
                note_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
            ) == libc::SIG_ERR
        {
            return Err(io::Error::last_os_error());
        }
    }
    armed.write_all(ARMED_LINE.as_bytes())?;
    idle()
}

#[cfg(unix)]
fn escaped_writer() -> io::Result<()> {
    let mut stdout = io::stdout();
    loop {
        // A terminal taken away from the writer fails the write; it writes on.
        let _ = stdout.write_all(b"e").and_then(|()| stdout.flush());
        std::thread::sleep(POLL);
    }
}

#[cfg(unix)]
fn in_own_session(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe and touches only the forked child.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
fn end_race(info_file: &Path, gate_file: &Path, term_file: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let member = Command::new(&exe)
        .arg("--term-member")
        .arg(term_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .id();
    let mut escaped = Command::new(&exe);
    escaped
        .arg("--escaped-writer")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    in_own_session(&mut escaped);
    let escaped = escaped.spawn()?.id();
    // SAFETY: ttyname returns null or a NUL-terminated string valid until the
    // next call, copied out at once.
    let tty = unsafe {
        let name = libc::ttyname(libc::STDIN_FILENO);
        if name.is_null() {
            return Err(io::Error::last_os_error());
        }
        std::ffi::CStr::from_ptr(name)
            .to_string_lossy()
            .into_owned()
    };
    while !std::fs::read_to_string(term_file).is_ok_and(|text| text.contains(ARMED_LINE)) {
        std::thread::sleep(POLL);
    }
    std::fs::write(info_file, format!("{member} {escaped} {tty}\n"))?;
    let mut stdout = io::stdout();
    stdout.write_all(END_RACE_MARKER)?;
    stdout.flush()?;
    while !gate_file.exists() {
        std::thread::sleep(POLL);
    }
    std::process::exit(0)
}

#[cfg(target_os = "linux")]
fn orphans() -> io::Result<()> {
    // Blocked before any child exists, so every SIGCHLD waits for `sigwait`.
    // SAFETY: the set is a stack local initialised by sigemptyset before use.
    let set = unsafe {
        let mut set = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        set
    };
    yagura_os::platform_unix::set_child_subreaper().map_err(io::Error::other)?;
    let leader = Command::new(std::env::current_exe()?)
        .arg("--orphan-leader")
        .stdin(Stdio::null())
        .spawn()?
        .id();
    // The fixture never reaps the leader, so its gate stays unreaped.
    let leader_gate = yagura_os::ReapGate::new();
    let mut stdout = io::stdout();
    stdout.write_all(format!("LEADER {leader}\n").as_bytes())?;
    let mut orphans_reaped = 0;
    while orphans_reaped < ORPHANS {
        let mut signal = 0;
        // SAFETY: `set` holds the blocked SIGCHLD; `signal` is a stack local.
        if unsafe { libc::sigwait(&set, &mut signal) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let reaped = yagura_os::platform_unix::reap_orphans(leader, &leader_gate)
            .map_err(io::Error::other)?;
        for pid in reaped {
            stdout.write_all(format!("REAPED {pid}\n").as_bytes())?;
            if pid != leader {
                orphans_reaped += 1;
            }
        }
    }
    stdout.write_all(b"ORPHANS-DONE\n")?;
    stay_alive()
}

#[cfg(target_os = "linux")]
fn orphan_leader() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let leader = std::process::id().to_string();
    let mut stdout = io::stdout();
    for _ in 0..ORPHANS {
        let mut orphan = Command::new(&exe);
        orphan
            .args(["--orphan", &leader])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        in_own_session(&mut orphan);
        let pid = orphan.spawn()?.id();
        stdout.write_all(format!("ORPHAN {pid}\n").as_bytes())?;
    }
    std::process::exit(0)
}

/// Ends once it is no longer `leader`'s child, so it ends as an orphan.
#[cfg(target_os = "linux")]
fn orphan(leader: &str) -> io::Result<()> {
    let Ok(leader) = leader.parse::<libc::pid_t>() else {
        std::process::exit(2);
    };
    // SAFETY: getppid takes no arguments and cannot fail.
    while unsafe { libc::getppid() } == leader {
        std::thread::sleep(POLL);
    }
    std::process::exit(0)
}
