//! Test fixture for the one-shot capture path (`yagura_os::spawn_captured`):
//! one self-contained binary, selected by its first argument, that
//! reproduces every child behavior those tests observe without depending on
//! any real vendor CLI or an external script interpreter.
//!
//! Positional argv, no parser:
//! - `capture_child echo <exit_code> <stdout_text> [<stderr_text>]`: writes
//!   `stdout_text` to stdout and, when given, `stderr_text` to stderr, then
//!   exits with `exit_code`.
//! - `capture_child bytes <n>`: writes `n` bytes of a fixed filler pattern to
//!   stdout, then exits `0`.
//! - `capture_child sleep <ms>`: sleeps `ms` milliseconds, writing nothing,
//!   then exits `0`.
//! - `capture_child heartbeat <tick_path> <hang|flood> [<param>]`: spawns a
//!   `tick` grandchild targeting `tick_path` (see below), then either sleeps
//!   without writing (`hang`) or writes filler to stdout in a loop (`flood`;
//!   `param` overrides the per-write chunk size in bytes) until it is killed
//!   or a safety bound elapses.
//! - `capture_child tick <tick_path> [<interval_ms>] [<lifetime_ms>]`: appends
//!   one line to `tick_path` every `interval_ms` (default 100) until it is
//!   killed or `lifetime_ms` (default: the safety bound) elapses -- the
//!   descendant a heartbeat test watches.
//! - `capture_child touch <marker_path> <exit|hang> [<param>]`: writes
//!   `marker_path` at startup, then either exits with `param` as the exit
//!   code (default `0`) or sleeps like `heartbeat`'s `hang`.
//! - `capture_child env <name>...`: writes one line per `name`: that
//!   variable's value as this process sees it, or `not-set` when it is absent.
//! - `capture_child pipe-holder <tick_path> <lifetime_ms> <text> <exit|hang>
//!   [<launcher>...]`: starts a `tick <tick_path> 100 <lifetime_ms>`
//!   grandchild that inherits this process's stdout (so the pipe stays open
//!   while it lives), then writes `text` and either exits `0` or sleeps like
//!   `heartbeat`'s `hang`. With a `launcher`, the grandchild is started as
//!   `<launcher>... <this exe> tick ...` and the launcher is waited for, so a
//!   caller can have it placed outside this process's process group without
//!   this fixture knowing which OS it runs on.
//! - `capture_child write-after <text> <delay_ms> <file_path> [<late_text>
//!   [<late_repeat>]]`: writes `text`, sleeps `delay_ms`, then writes
//!   `late_text` repeated `late_repeat` times (default once) to stdout, then
//!   writes `file_path`, then exits `0`. A late write that fails (a closed
//!   pipe) makes it exit non-zero without writing `file_path`.
//! - `capture_child cat-stdin`: reads standard input to end of file, writes
//!   the same bytes to stdout, then exits `0`.
//! - `capture_child stderr-bytes <n> [<stdout_text>]`: writes `stdout_text`
//!   (empty when omitted) to stdout, then `n` bytes of the same filler
//!   pattern `bytes` uses to stderr, then exits `0`.
//! - `capture_child pipe-holder-stderr <tick_path> <lifetime_ms> <text>
//!   <exit|hang> [<launcher>...]`: the stderr-holding counterpart of
//!   `pipe-holder` -- the `tick <tick_path> 100 <lifetime_ms>` grandchild
//!   inherits this process's **stderr** instead of stdout, so the pipe held
//!   open belongs to stderr. This process still writes `text` to stdout
//!   itself, then either exits `0` or sleeps like `heartbeat`'s `hang`. With a
//!   `launcher`, the grandchild is started the same way `pipe-holder`'s is.
//! - `capture_child grandchild-first <pid_file> <child_lifetime_ms>`: as its
//!   first action starts a `pid-holder` grandchild, waits until that grandchild
//!   has published its process id in `pid_file`, then lives `child_lifetime_ms`
//!   (`0` = exits at once) and exits `0`. The grandchild stays in this process's
//!   process group (Unix, with SIGHUP ignored) / job (Windows, with no console
//!   window of its own) and holds none of its standard handles.
//! - `capture_child tick-grandchild <tick_path> <child_lifetime_ms>`: as its
//!   first action starts a `tick <tick_path>` grandchild placed the same way
//!   `grandchild-first`'s is, then lives `child_lifetime_ms` and exits `0`.
//! - `capture_child pid-holder <pid_file> <lifetime_ms>`: publishes its own
//!   process id in `pid_file` (written whole, then renamed into place), then
//!   sleeps `lifetime_ms`.
//!
//! The safety bound keeps a fixture that a test's own teardown never reaches
//! from running forever; every test that relies on being killed finishes well
//! inside it.

// Test-only fixture binary, not the daemon's production spawn path: it
// spawns its own descendant so a one-shot capture test has something to
// observe being torn down. `clippy.toml`'s `disallowed-methods` ban is
// reserved for the PTY path and the single locked one-shot spawn function.
#![allow(clippy::disallowed_methods)]

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Upper bound on how long a fixture that is meant to be killed keeps running
/// on its own, so a test whose teardown never reaches it cannot leak a
/// process forever.
const SAFETY_BOUND: Duration = Duration::from_secs(600);

/// Default interval between two heartbeat ticks, when the caller does not
/// override it.
const TICK_INTERVAL_DEFAULT_MS: u64 = 100;

/// Default per-write chunk size for `heartbeat ... flood`, when the caller
/// does not override it.
const FLOOD_CHUNK_DEFAULT: usize = 4096;

/// The byte every filler write repeats.
const FILLER_BYTE: u8 = b'x';

/// What `env` writes for a variable that is absent.
const NOT_SET: &str = "not-set";

/// How long a `pid-holder` grandchild lives on its own: far beyond any case
/// that expects a teardown to end it, yet short enough that a case whose
/// teardown never reaches it does not leave it behind for long.
const PID_HOLDER_LIFETIME_MS: u64 = 120_000;

/// How long `grandchild-first` waits for its grandchild to publish its id
/// before giving up with a non-zero exit.
const PID_PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("echo") => run_echo(&args[2..]),
        Some("bytes") => run_bytes(&args[2..]),
        Some("sleep") => run_sleep(&args[2..]),
        Some("heartbeat") => run_heartbeat(&args[2..]),
        Some("tick") => run_tick(&args[2..]),
        Some("touch") => run_touch(&args[2..]),
        Some("env") => run_env(&args[2..]),
        Some("pipe-holder") => run_pipe_holder(&args[2..]),
        Some("write-after") => run_write_after(&args[2..]),
        Some("cat-stdin") => run_cat_stdin(),
        Some("stderr-bytes") => run_stderr_bytes(&args[2..]),
        Some("pipe-holder-stderr") => run_pipe_holder_stderr(&args[2..]),
        Some("grandchild-first") => run_grandchild_first(&args[2..]),
        Some("tick-grandchild") => run_tick_grandchild(&args[2..]),
        Some("pid-holder") => run_pid_holder(&args[2..]),
        _ => {
            eprintln!(
                "usage: capture_child \
                 <echo|bytes|sleep|heartbeat|tick|touch|env|pipe-holder|write-after|cat-stdin| \
                 stderr-bytes|pipe-holder-stderr|grandchild-first|tick-grandchild|pid-holder> ..."
            );
            std::process::exit(2);
        }
    }
}

fn run_echo(args: &[String]) -> io::Result<()> {
    let (Some(exit_code), Some(stdout_text)) = (args.first(), args.get(1)) else {
        eprintln!("usage: capture_child echo <exit_code> <stdout_text> [<stderr_text>]");
        std::process::exit(2);
    };
    let Ok(code) = exit_code.parse::<i32>() else {
        eprintln!("capture_child echo: exit_code must be an integer");
        std::process::exit(2);
    };
    io::stdout().write_all(stdout_text.as_bytes())?;
    io::stdout().flush()?;
    if let Some(stderr_text) = args.get(2) {
        io::stderr().write_all(stderr_text.as_bytes())?;
        io::stderr().flush()?;
    }
    std::process::exit(code);
}

fn run_bytes(args: &[String]) -> io::Result<()> {
    let Some(n) = args.first().and_then(|n| n.parse::<usize>().ok()) else {
        eprintln!("usage: capture_child bytes <n>");
        std::process::exit(2);
    };
    io::stdout().write_all(&vec![FILLER_BYTE; n])?;
    io::stdout().flush()
}

fn run_sleep(args: &[String]) -> io::Result<()> {
    let Some(ms) = args.first().and_then(|n| n.parse::<u64>().ok()) else {
        eprintln!("usage: capture_child sleep <ms>");
        std::process::exit(2);
    };
    std::thread::sleep(Duration::from_millis(ms));
    Ok(())
}

fn run_heartbeat(args: &[String]) -> io::Result<()> {
    let (Some(tick_path), Some(behavior)) = (args.first(), args.get(1)) else {
        eprintln!("usage: capture_child heartbeat <tick_path> <hang|flood> [<param>]");
        std::process::exit(2);
    };
    spawn_tick_grandchild(tick_path)?;
    match behavior.as_str() {
        "hang" => sleep_until_safety_bound(),
        "flood" => {
            let chunk_size = args
                .get(2)
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(FLOOD_CHUNK_DEFAULT)
                .max(1);
            flood_stdout(chunk_size)
        }
        other => {
            eprintln!("capture_child heartbeat: unknown behavior {other}");
            std::process::exit(2);
        }
    }
}

fn run_tick(args: &[String]) -> io::Result<()> {
    let Some(tick_path) = args.first() else {
        eprintln!("usage: capture_child tick <tick_path> [<interval_ms>] [<lifetime_ms>]");
        std::process::exit(2);
    };
    let interval_ms = args
        .get(1)
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(TICK_INTERVAL_DEFAULT_MS);
    let lifetime = args
        .get(2)
        .and_then(|n| n.parse::<u64>().ok())
        .map_or(SAFETY_BOUND, Duration::from_millis)
        .min(SAFETY_BOUND);
    tick_loop(&PathBuf::from(tick_path), interval_ms, lifetime)
}

fn run_touch(args: &[String]) -> io::Result<()> {
    let (Some(marker_path), Some(behavior)) = (args.first(), args.get(1)) else {
        eprintln!("usage: capture_child touch <marker_path> <exit|hang> [<param>]");
        std::process::exit(2);
    };
    write_marker(marker_path)?;
    match behavior.as_str() {
        "hang" => sleep_until_safety_bound(),
        "exit" => {
            let code: i32 = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0);
            std::process::exit(code);
        }
        other => {
            eprintln!("capture_child touch: unknown behavior {other}");
            std::process::exit(2);
        }
    }
}

fn write_marker(marker_path: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(marker_path)?;
    file.write_all(b"started\n")?;
    file.flush()
}

fn run_env(args: &[String]) -> io::Result<()> {
    let mut text = String::new();
    for name in args {
        match env::var_os(name) {
            Some(value) => text.push_str(&value.to_string_lossy()),
            None => text.push_str(NOT_SET),
        }
        text.push('\n');
    }
    let mut stdout = io::stdout();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

fn run_pipe_holder(args: &[String]) -> io::Result<()> {
    let (Some(tick_path), Some(lifetime_ms), Some(text), Some(behavior)) =
        (args.first(), args.get(1), args.get(2), args.get(3))
    else {
        eprintln!(
            "usage: capture_child pipe-holder <tick_path> <lifetime_ms> <text> <exit|hang> \
             [<launcher>...]"
        );
        std::process::exit(2);
    };
    let exe = env::current_exe()?;
    let launcher = &args[4..];
    let mut command = match launcher.split_first() {
        Some((program, launcher_args)) => {
            let mut command = Command::new(program);
            command.args(launcher_args).arg(exe);
            command
        }
        None => Command::new(exe),
    };
    // stdout is inherited on purpose: the grandchild keeps the capture pipe
    // open for as long as it lives.
    command
        .arg("tick")
        .arg(tick_path)
        .arg(TICK_INTERVAL_DEFAULT_MS.to_string())
        .arg(lifetime_ms)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if launcher.is_empty() {
        command.spawn()?;
    } else {
        // The launcher must have started the grandchild before this process
        // writes its text, or the text could arrive with no holder alive yet.
        command.status()?;
    }
    let mut stdout = io::stdout();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    match behavior.as_str() {
        "exit" => Ok(()),
        "hang" => sleep_until_safety_bound(),
        other => {
            eprintln!("capture_child pipe-holder: unknown behavior {other}");
            std::process::exit(2);
        }
    }
}

fn run_write_after(args: &[String]) -> io::Result<()> {
    let (Some(text), Some(delay_ms), Some(file_path)) = (
        args.first(),
        args.get(1).and_then(|n| n.parse::<u64>().ok()),
        args.get(2),
    ) else {
        eprintln!(
            "usage: capture_child write-after <text> <delay_ms> <file_path> [<late_text> \
             [<late_repeat>]]"
        );
        std::process::exit(2);
    };
    let mut stdout = io::stdout();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    std::thread::sleep(Duration::from_millis(delay_ms));
    if let Some(late_text) = args.get(3) {
        let repeat: usize = args.get(4).and_then(|n| n.parse().ok()).unwrap_or(1);
        stdout.write_all(late_text.repeat(repeat).as_bytes())?;
        stdout.flush()?;
    }
    write_marker(file_path)
}

fn run_cat_stdin() -> io::Result<()> {
    let mut buf = Vec::new();
    io::stdin().read_to_end(&mut buf)?;
    let mut stdout = io::stdout();
    stdout.write_all(&buf)?;
    stdout.flush()
}

fn run_stderr_bytes(args: &[String]) -> io::Result<()> {
    let Some(n) = args.first().and_then(|n| n.parse::<usize>().ok()) else {
        eprintln!("usage: capture_child stderr-bytes <n> [<stdout_text>]");
        std::process::exit(2);
    };
    let stdout_text = args.get(1).map(String::as_str).unwrap_or("");
    let mut stdout = io::stdout();
    stdout.write_all(stdout_text.as_bytes())?;
    stdout.flush()?;
    let mut stderr = io::stderr();
    stderr.write_all(&vec![FILLER_BYTE; n])?;
    stderr.flush()
}

fn run_pipe_holder_stderr(args: &[String]) -> io::Result<()> {
    let (Some(tick_path), Some(lifetime_ms), Some(text), Some(behavior)) =
        (args.first(), args.get(1), args.get(2), args.get(3))
    else {
        eprintln!(
            "usage: capture_child pipe-holder-stderr <tick_path> <lifetime_ms> <text> \
             <exit|hang> [<launcher>...]"
        );
        std::process::exit(2);
    };
    let exe = env::current_exe()?;
    let launcher = &args[4..];
    let mut command = match launcher.split_first() {
        Some((program, launcher_args)) => {
            let mut command = Command::new(program);
            command.args(launcher_args).arg(exe);
            command
        }
        None => Command::new(exe),
    };
    // stderr is inherited on purpose: the grandchild keeps the capture's
    // stderr pipe open for as long as it lives. stdout is closed instead, so
    // this process's own exit still lets the capture's stdout reach EOF.
    command
        .arg("tick")
        .arg(tick_path)
        .arg(TICK_INTERVAL_DEFAULT_MS.to_string())
        .arg(lifetime_ms)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if launcher.is_empty() {
        command.spawn()?;
    } else {
        // The launcher must have started the grandchild before this process
        // writes its text, or the text could arrive with no holder alive yet.
        command.status()?;
    }
    let mut stdout = io::stdout();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    match behavior.as_str() {
        "exit" => Ok(()),
        "hang" => sleep_until_safety_bound(),
        other => {
            eprintln!("capture_child pipe-holder-stderr: unknown behavior {other}");
            std::process::exit(2);
        }
    }
}

fn run_grandchild_first(args: &[String]) -> io::Result<()> {
    let (Some(pid_path), Some(lifetime_ms)) = (
        args.first(),
        args.get(1).and_then(|n| n.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: capture_child grandchild-first <pid_file> <child_lifetime_ms>");
        std::process::exit(2);
    };
    spawn_detached_grandchild(&["pid-holder", pid_path, &PID_HOLDER_LIFETIME_MS.to_string()])?;
    let deadline = Instant::now() + PID_PUBLISH_TIMEOUT;
    while !std::fs::read_to_string(pid_path).is_ok_and(|text| text.trim().parse::<u32>().is_ok()) {
        if Instant::now() >= deadline {
            eprintln!("capture_child grandchild-first: the grandchild never published its id");
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(lifetime_ms).min(SAFETY_BOUND));
    Ok(())
}

fn run_tick_grandchild(args: &[String]) -> io::Result<()> {
    let (Some(tick_path), Some(lifetime_ms)) = (
        args.first(),
        args.get(1).and_then(|n| n.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: capture_child tick-grandchild <tick_path> <child_lifetime_ms>");
        std::process::exit(2);
    };
    spawn_detached_grandchild(&["tick", tick_path])?;
    std::thread::sleep(Duration::from_millis(lifetime_ms).min(SAFETY_BOUND));
    Ok(())
}

fn run_pid_holder(args: &[String]) -> io::Result<()> {
    let (Some(pid_path), Some(lifetime_ms)) = (
        args.first(),
        args.get(1).and_then(|n| n.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: capture_child pid-holder <pid_file> <lifetime_ms>");
        std::process::exit(2);
    };
    let staging = format!("{pid_path}.staging");
    std::fs::write(&staging, std::process::id().to_string())?;
    std::fs::rename(&staging, pid_path)?;
    std::thread::sleep(Duration::from_millis(lifetime_ms).min(SAFETY_BOUND));
    Ok(())
}

/// Starts this binary again with `mode_args` as a grandchild that shares no
/// standard handle with this process, so a capture's pipes stay open only for
/// as long as this process itself lives. On Unix it is started through `sh`
/// with SIGHUP ignored (the way a backgrounded job survives its terminal
/// closing) and `exec`s at once, so its process id is its own; on Windows it
/// has no console, so only a teardown of this process's job can end it.
fn spawn_detached_grandchild(mode_args: &[&str]) -> io::Result<()> {
    stop_inheriting_standard_handles();
    let exe = env::current_exe()?;
    let mut command = if cfg!(unix) {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' HUP; exec \"$0\" \"$@\"")
            .arg(&exe);
        command
    } else {
        Command::new(&exe)
    };
    command
        .args(mode_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map(|_child| ())
}

/// Windows hands every inheritable handle of a parent to a child it starts,
/// whatever that child's own standard handles are set to; the capture pipes
/// are inheritable in this process, so without this the grandchild would hold
/// them open and the capture could not see this process's exit as the end of
/// its output. Unix closes them on exec and needs nothing.
#[cfg(windows)]
fn stop_inheriting_standard_handles() {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    const HANDLE_FLAG_INHERIT: u32 = 1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    for raw in [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ] {
        // SAFETY: the handles are this process's own standard handles; an
        // invalid or absent one only makes the call fail, which is ignored.
        unsafe {
            SetHandleInformation(raw, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

#[cfg(not(windows))]
fn stop_inheriting_standard_handles() {}

fn tick_loop(tick_path: &PathBuf, interval_ms: u64, lifetime: Duration) -> io::Result<()> {
    let deadline = Instant::now() + lifetime;
    while Instant::now() < deadline {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(tick_path)?;
        file.write_all(b"tick\n")?;
        file.flush()?;
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
    Ok(())
}

fn sleep_until_safety_bound() -> io::Result<()> {
    std::thread::sleep(SAFETY_BOUND);
    Ok(())
}

fn flood_stdout(chunk_size: usize) -> io::Result<()> {
    let chunk = vec![FILLER_BYTE; chunk_size];
    let deadline = Instant::now() + SAFETY_BOUND;
    let mut stdout = io::stdout();
    while Instant::now() < deadline {
        stdout.write_all(&chunk)?;
        stdout.flush()?;
    }
    Ok(())
}

/// Spawns a detached `tick` grandchild targeting `tick_path`, inheriting this
/// process's process group (Unix) / job (Windows) rather than starting a new
/// one of its own, so a teardown that reaches this process's descendants
/// reaches it too.
fn spawn_tick_grandchild(tick_path: &str) -> io::Result<()> {
    let exe = env::current_exe()?;
    Command::new(exe)
        .arg("tick")
        .arg(tick_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
}
