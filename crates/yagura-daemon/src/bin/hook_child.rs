//! Test fixture standing in for a CLI whose hooks reach the daemon: it calls
//! the `yagura-daemon` doors a real CLI's hooks would (`--deliver-context` at
//! start-up or at the first turn, `--deliver-take` and `--report-session` at
//! the start of a turn, a `--deliver-wait` kept registered while it waits) and
//! writes down everything it received, so a test sees what reached the CLI and
//! by which door.
//!
//! It also paints a composer and accepts what is typed into it, so a delivery
//! that is still typed shows up in its input log.
//!
//! Positional argv, then flags:
//! `hook_child <composer_marker> <separator> <continuation_prefix> <reflect> - [flags]`.
//! The first four select composer-draw mode (`reflect` as in `echo_child`,
//! `pause|<path>` included); `-` stands for "no fold placeholder".
//!
//! Flags:
//! - `--daemon <path>`: the `yagura-daemon` executable the doors run.
//! - `--instance <id>`: `YAGURA_INSTANCE` for every door.
//! - `--bases <dir>`: the data, state and config base directories every door
//!   runs under, so none of them writes into the user's own.
//! - `--log <dir>`: where `events.jsonl`, `woken.jsonl` and `input.log` go.
//! - `--context` / `--context-first-turn`: run `--deliver-context` at
//!   start-up, or within the first turn instead.
//! - `--take`: run `--deliver-take` at the start of every turn.
//! - `--wait`: keep a `--deliver-wait` registered from start-up and again
//!   after every turn ends, and register a new one after a wait was answered.
//! - `--session <id>`: report `<id>` through `--report-session /session_id`
//!   at the start of every turn.
//! - `--emit`: report `running` / `waiting_input` through `--emit-status` at
//!   the start and the end of every turn.
//! - `--credentials <path>`: hand `--credentials <path>` to every door the CLI
//!   itself runs, as a profile's hook commands do for a window whose
//!   credentials are not in its environment.
//! - `--nested-emit <status>`: at start-up, report `<status>` through a door
//!   that is never given `--credentials`, as a CLI started by a tool of this
//!   window would (it inherits only this process's environment).
//! - `--gate <path>`: after the nested report, wait until `<path>` exists.
//! - `--emit-start <status>`: then report `<status>` through a door of the CLI
//!   itself (with `--credentials` when given).
//! - `--exit-on <path>`: end the process as soon as `<path>` exists.
//! - `--prompt <text>`: a first request handed over at launch; logged.
//! - `--woken <session> <text>`: the wake command a profile declares. Writes
//!   both values to `woken.jsonl` and exits; nothing else runs.
//! - `--woken-hold <path>`: in wake-command mode, record that it started,
//!   then wait while `<path>` exists before writing what it received.
//! - `--woken-fail`: in wake-command mode, write nothing and exit with 1.
//!
//! A submitted line ending in `TURN` starts a turn and one ending in `END` ends
//! it. `LOSE` paints a status line no detection rule matches, `FIND` paints the
//! waiting one again.
//! Every other submitted line is only echoed (`SUBMITTED <line>`).

// Test-only fixture binary, not the daemon's production spawn path: it runs
// the `yagura-daemon` doors the way a CLI's hooks do.
#![allow(clippy::disallowed_methods)]

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[path = "shared/composer_paint.rs"]
mod composer_paint;

use composer_paint::{
    ComposerStyle, Output, Overlay, OverlayComposer, Paint, Scan, accept_byte, spawn_reflector,
};

/// The status line a detection definition for this fixture reads as waiting.
const WAITING_LINE: &str = "HC-IDLE";
/// The status line it reads as running.
const BUSY_LINE: &str = "HC-BUSY";
/// A status line no rule matches.
const LOST_LINE: &str = "HC-LOST";

/// Where the flags start: after the four composer arguments and the fold
/// placeholder.
const FIRST_FLAG: usize = 6;

#[derive(Default, Clone)]
struct Options {
    daemon: Option<PathBuf>,
    instance: Option<String>,
    bases: Option<PathBuf>,
    log: Option<PathBuf>,
    context: bool,
    context_first_turn: bool,
    take: bool,
    wait: bool,
    session: Option<String>,
    emit: bool,
    credentials: Option<PathBuf>,
    nested_emit: Option<String>,
    gate: Option<PathBuf>,
    emit_start: Option<String>,
    exit_on: Option<PathBuf>,
    prompt: Option<String>,
    woken: Option<(String, String)>,
    woken_hold: Option<PathBuf>,
    woken_fail: bool,
}

fn parse(args: &[String]) -> Options {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--daemon" => options.daemon = rest.next().map(PathBuf::from),
            "--instance" => options.instance = rest.next().cloned(),
            "--bases" => options.bases = rest.next().map(PathBuf::from),
            "--log" => options.log = rest.next().map(PathBuf::from),
            "--context" => options.context = true,
            "--context-first-turn" => options.context_first_turn = true,
            "--take" => options.take = true,
            "--wait" => options.wait = true,
            "--session" => options.session = rest.next().cloned(),
            "--emit" => options.emit = true,
            "--credentials" => options.credentials = rest.next().map(PathBuf::from),
            "--nested-emit" => options.nested_emit = rest.next().cloned(),
            "--gate" => options.gate = rest.next().map(PathBuf::from),
            "--emit-start" => options.emit_start = rest.next().cloned(),
            "--exit-on" => options.exit_on = rest.next().map(PathBuf::from),
            "--prompt" => options.prompt = rest.next().cloned(),
            "--woken-hold" => options.woken_hold = rest.next().map(PathBuf::from),
            "--woken-fail" => options.woken_fail = true,
            "--woken" => {
                let session = rest.next().cloned().unwrap_or_default();
                let text = rest.next().cloned().unwrap_or_default();
                options.woken = Some((session, text));
                // What follows is the text's own business, never a flag.
                break;
            }
            _ => {}
        }
    }
    options
}

/// Appends JSON lines to one file; each line is written with a single call so
/// that two processes appending to the same file never interleave a line.
struct Log {
    file: Mutex<Option<File>>,
}

impl Log {
    fn open(dir: Option<&Path>, name: &str) -> Self {
        let file = dir.and_then(|dir| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
                .ok()
        });
        Self {
            file: Mutex::new(file),
        }
    }

    fn write(&self, value: &serde_json::Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.write_bytes(line.as_bytes());
    }

    fn write_bytes(&self, bytes: &[u8]) {
        let mut guard = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
    }
}

/// What one door printed and how it ended.
struct DoorRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl DoorRun {
    fn to_json(&self, event: &str) -> serde_json::Value {
        serde_json::json!({
            "event": event,
            "code": self.code,
            "stdout": self.stdout,
            "stderr": self.stderr,
        })
    }
}

/// Runs `yagura-daemon <args>` the way a hook does: inside the window's own
/// environment (its token and window id), with `stdin` written to it.
fn run_door(options: &Options, args: &[&str], stdin: Option<&[u8]>) -> DoorRun {
    run_door_as(options, args, stdin, true)
}

/// [`run_door`], where `own` says whether the door is the CLI's own hook (it is
/// handed `--credentials`) or a nested CLI's (it never is).
fn run_door_as(options: &Options, args: &[&str], stdin: Option<&[u8]>, own: bool) -> DoorRun {
    let Some(daemon) = options.daemon.as_ref() else {
        return DoorRun {
            code: None,
            stdout: String::new(),
            stderr: "hook_child: no --daemon given".to_string(),
        };
    };
    let mut command = Command::new(daemon);
    if let Some(path) = options.credentials.as_ref().filter(|_| own) {
        command.arg("--credentials").arg(path);
    }
    command
        .args(args)
        .env_remove("YAGURA_STORE_DB")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(instance) = options.instance.as_ref() {
        command.env("YAGURA_INSTANCE", instance);
    }
    if let Some(bases) = options.bases.as_ref() {
        for (variable, sub) in [
            ("LOCALAPPDATA", "local"),
            ("APPDATA", "roaming"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CONFIG_HOME", "config"),
        ] {
            let dir = bases.join(sub);
            let _ = std::fs::create_dir_all(&dir);
            command.env(variable, dir);
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return DoorRun {
                code: None,
                stdout: String::new(),
                stderr: format!("hook_child: cannot run the daemon: {error}"),
            };
        }
    };
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(bytes);
    }
    match child.wait_with_output() {
        Ok(output) => DoorRun {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(error) => DoorRun {
            code: None,
            stdout: String::new(),
            stderr: format!("hook_child: lost the daemon: {error}"),
        },
    }
}

/// Keeps a wait registered until one ends with nothing to say: a wait
/// answered with text is followed by a new one, as a CLI's next turn end
/// would register it.
fn spawn_wait(options: Arc<Options>, events: Arc<Log>) {
    std::thread::spawn(move || {
        loop {
            let run = run_door(&options, &["--deliver-wait"], None);
            events.write(&run.to_json("wait"));
            let said_something = !run.stdout.is_empty() || !run.stderr.is_empty();
            // 1 is the doors' refusal or a failure to reach the daemon.
            if !said_something || run.code == Some(1) || run.code.is_none() {
                return;
            }
        }
    });
}

fn status_line(output: &Output, line: &str) -> io::Result<()> {
    output.set_overlay(Some(Overlay {
        lines: vec![line.to_string()],
        composer: OverlayComposer::Shown,
        paint: Paint::Finished,
        closes_sync: false,
    }))
}

fn emit(options: &Options, status: &str) {
    if options.emit {
        let _ = run_door(
            options,
            &["--emit-status", status, "--source", "hook-child"],
            None,
        );
    }
}

/// The wake command a profile declares: records what the daemon handed over.
fn woken(options: &Options, session: &str, text: &str) -> io::Result<()> {
    let log = Log::open(options.log.as_deref(), "woken.jsonl");
    if options.woken_fail {
        log.write(&serde_json::json!({ "event": "woken_failed" }));
        std::process::exit(1);
    }
    if let Some(hold) = options.woken_hold.as_ref() {
        log.write(&serde_json::json!({ "event": "woken_started" }));
        while hold.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    log.write(&serde_json::json!({
        "event": "woken",
        "session": session,
        "text": text,
    }));
    Ok(())
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let options = Arc::new(parse(args.get(FIRST_FLAG..).unwrap_or_default()));
    if let Some((session, text)) = options.woken.clone() {
        return woken(&options, &session, &text);
    }

    let events = Arc::new(Log::open(options.log.as_deref(), "events.jsonl"));
    let input_log = Log::open(options.log.as_deref(), "input.log");
    events.write(&serde_json::json!({
        "event": "argv",
        "args": args.get(1..).unwrap_or_default(),
        "prompt": options.prompt,
    }));

    let Some((style, reflect)) = ComposerStyle::from_args(1) else {
        eprintln!("hook_child: the four composer arguments are required");
        std::process::exit(2);
    };
    // A real CLI raw-modes its own terminal on start; without that, the
    // platform's default line discipline reshapes what is written to it.
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("hook_child: cannot enter raw console input: {error}");
        std::process::exit(2);
    }

    if options.context {
        let run = run_door(&options, &["--deliver-context"], None);
        events.write(&run.to_json("context"));
    }

    let output = Arc::new(Output::new(Some(style)));
    let reflector = spawn_reflector(Arc::clone(&output), reflect);
    status_line(&output, WAITING_LINE)?;
    if options.wait {
        spawn_wait(Arc::clone(&options), Arc::clone(&events));
    }
    if let Some(path) = options.exit_on.clone() {
        std::thread::spawn(move || {
            while !path.exists() {
                std::thread::sleep(Duration::from_millis(20));
            }
            std::process::exit(0);
        });
    }
    if let Some(status) = options.nested_emit.as_deref() {
        let args = ["--emit-status", status, "--source", "hook-child"];
        let run = run_door_as(&options, &args, None, false);
        events.write(&run.to_json("nested_emit"));
    }
    if let Some(gate) = options.gate.as_ref() {
        while !gate.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if let Some(status) = options.emit_start.as_deref() {
        let args = ["--emit-status", status, "--source", "hook-child"];
        let run = run_door(&options, &args, None);
        events.write(&run.to_json("emit_start"));
    }

    let stdin = io::stdin();
    let mut chunk = vec![0u8; 4096];
    let mut buffer: Vec<u8> = Vec::new();
    let mut scan = Scan::Text;
    let mut turns = 0usize;

    loop {
        let n = stdin.lock().read(&mut chunk)?;
        if n == 0 {
            break;
        }
        input_log.write_bytes(&chunk[..n]);
        for &byte in &chunk[..n] {
            let Some(submitted) = accept_byte(&mut buffer, &mut scan, byte) else {
                continue;
            };
            let line = String::from_utf8_lossy(&submitted).into_owned();
            output.set_body(&String::from_utf8_lossy(&buffer));
            output.line(&format!("SUBMITTED {line}"))?;
            // A trigger is the end of the line: what the daemon typed into the composer
            // ahead of it stays in front.
            let trigger = ["TURN", "END", "LOSE", "FIND"]
                .into_iter()
                .find(|trigger| line.ends_with(trigger));
            match trigger {
                Some("TURN") => {
                    turns += 1;
                    status_line(&output, BUSY_LINE)?;
                    emit(&options, "running");
                    if options.context_first_turn && turns == 1 {
                        let run = run_door(&options, &["--deliver-context"], None);
                        events.write(&run.to_json("context"));
                    }
                    if options.take {
                        let run = run_door(&options, &["--deliver-take"], None);
                        events.write(&run.to_json("take"));
                    }
                    if let Some(session) = options.session.as_ref() {
                        let input = serde_json::json!({ "session_id": session }).to_string();
                        let run = run_door(
                            &options,
                            &["--report-session", "/session_id"],
                            Some(input.as_bytes()),
                        );
                        events.write(&run.to_json("session"));
                    }
                    events.write(&serde_json::json!({ "event": "turn_started", "turn": turns }));
                }
                Some("END") => {
                    status_line(&output, WAITING_LINE)?;
                    emit(&options, "waiting_input");
                    if options.wait {
                        spawn_wait(Arc::clone(&options), Arc::clone(&events));
                    }
                    events.write(&serde_json::json!({ "event": "turn_ended", "turn": turns }));
                }
                Some("LOSE") => status_line(&output, LOST_LINE)?,
                Some("FIND") => status_line(&output, WAITING_LINE)?,
                _ => {}
            }
        }
        output.set_body(&String::from_utf8_lossy(&buffer));
        let _ = reflector.send(());
    }

    Ok(())
}
