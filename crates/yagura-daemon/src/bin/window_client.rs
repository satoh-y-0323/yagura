//! Test fixture standing in for a process inside a window: it speaks the daemon's
//! IPC with the window's credentials from within the window's process tree, so a
//! test can name a window to a real daemon without the daemon taking the test
//! process for a member of that window.
//!
//! `window_client serve <results> [--log <dir>]` reads one command per line from
//! stdin (a PTY's input or a pipe) until it ends. `window_client once <results>
//! <commands>` runs the lines of the file `<commands>` and ends.
//!
//! A command is one JSON object with an `id` (every result line carries it) and
//! an `op`:
//! - `exchange`: on the named connection `conn` (opened on first use), sends
//!   `message` (a `ClientMessage`) or `frame` (any JSON body, written as one
//!   frame) when given, then reads as `read` says: `none`, `one` (the next
//!   response), `pong` (sends a `Ping`, then reads the responses up to its
//!   `Pong`) or `delivery` (one response;
//!   a `Delivery` is then answered with `DeliverPrinted`), or `frame` (one raw
//!   frame body). Strings in `message` equal to `$YAGURA_WINDOW_ID` or
//!   `$YAGURA_STATUS_TOKEN` are replaced by the window's own values.
//! - `close`: drops the connection `conn`.
//! - `run`: runs `argv` as this process's child to its end, with `stdin` text
//!   when given; the result holds its `code`, `stdout` and `stderr`.
//! - `keep`: starts `argv` as this process's child and keeps it (`env` pairs
//!   are set and `env_remove` names removed first). Each line it writes to
//!   stdout becomes a result line `{"id", "line"}`.
//! - `write`: writes `line` and a line break to the stdin of the kept `target`.
//! - `end`: kills the kept `target` and waits for it.
//!
//! Results are appended to the file `<results>`, one JSON object per line. The
//! endpoint is the one `YAGURA_INSTANCE` names; without it nothing is sent, so
//! no daemon of the user is reached.

// Test-only fixture binary, not the daemon's production spawn path: it runs
// the children a window's process would.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{ClientMessage, DaemonMessage};

type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// How long a connection is retried and a read waits: a test that cannot be
/// answered in this time has failed, and its process tree ends with it.
const LIMIT: Duration = Duration::from_secs(60);

/// Appends one JSON line per call; a line is one write, so lines from several
/// threads never interleave.
struct Results(Mutex<File>);

impl Results {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self(Mutex::new(file)))
    }

    fn line(&self, value: &Value) {
        let mut text = value.to_string();
        text.push('\n');
        let mut file = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = file.write_all(text.as_bytes()).and_then(|()| file.flush());
    }
}

struct Kept {
    child: Child,
    stdin: Option<ChildStdin>,
}

struct Client {
    results: Arc<Results>,
    runtime: tokio::runtime::Runtime,
    connections: HashMap<String, Connection>,
    kept: HashMap<String, Kept>,
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |index: usize| args.get(index).map_or("", String::as_str);
    let results = Arc::new(Results::open(Path::new(arg(2)))?);
    let mut client = Client {
        results,
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?,
        connections: HashMap::new(),
        kept: HashMap::new(),
    };
    match arg(1) {
        "serve" => {
            if let Some(dir) = args
                .iter()
                .position(|arg| arg == "--log")
                .and_then(|at| args.get(at + 1))
            {
                log_argv(Path::new(dir), &args);
            }
            serve(&mut client)
        }
        "once" => {
            let commands = std::fs::read_to_string(arg(3))?;
            for line in commands.lines().filter(|line| !line.trim().is_empty()) {
                client.command(line);
            }
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: window_client serve <results> [--log <dir>] | once <results> <commands>"
            );
            std::process::exit(2);
        }
    }
}

/// Writes the argv this process was started with under `dir`, as the other
/// window fixtures do, so a test can see when the window's CLI has started.
fn log_argv(dir: &Path, args: &[String]) {
    let line = json!({ "event": "argv", "args": args.get(1..).unwrap_or_default() });
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events.jsonl"))
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Reads commands until stdin ends. A PTY delivers raw bytes, so a line ends
/// at a CR or an LF.
fn serve(client: &mut Client) -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("window_client: cannot enter raw console input: {error}");
        std::process::exit(2);
    }
    let (lines, commands) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut chunk = [0u8; 4096];
        let mut line: Vec<u8> = Vec::new();
        loop {
            let count = match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => count,
            };
            for &byte in &chunk[..count] {
                if byte == b'\r' || byte == b'\n' {
                    if !line.is_empty() {
                        let text = String::from_utf8_lossy(&line).into_owned();
                        line.clear();
                        if lines.send(text).is_err() {
                            return;
                        }
                    }
                } else {
                    line.push(byte);
                }
            }
        }
    });
    while let Ok(line) = commands.recv() {
        client.command(&line);
    }
    for kept in client.kept.values_mut() {
        let _ = kept.child.kill();
        let _ = kept.child.wait();
    }
    Ok(())
}

impl Client {
    fn command(&mut self, line: &str) {
        let command: Value = match serde_json::from_str(line) {
            Ok(command) => command,
            Err(error) => {
                self.results
                    .line(&json!({ "id": null, "error": format!("not a command: {error}") }));
                return;
            }
        };
        let id = command["id"].as_str().unwrap_or_default().to_owned();
        let outcome = match command["op"].as_str().unwrap_or_default() {
            "exchange" => self.exchange(&command),
            "close" => {
                self.connections
                    .remove(command["conn"].as_str().unwrap_or_default());
                Ok(json!({ "closed": true }))
            }
            "run" => run(&command),
            "keep" => self.keep(&id, &command),
            "write" => self.write(&command),
            "end" => self.end(&command),
            other => Err(format!("unknown op {other:?}")),
        };
        let mut result = match outcome {
            Ok(result) => result,
            Err(error) => json!({ "error": error }),
        };
        result["id"] = Value::String(id);
        self.results.line(&result);
    }

    fn exchange(&mut self, command: &Value) -> Result<Value, String> {
        let name = command["conn"].as_str().unwrap_or_default().to_owned();
        if !self.connections.contains_key(&name) {
            let connection = self.runtime.block_on(connect())?;
            self.connections.insert(name.clone(), connection);
        }
        let connection = self
            .connections
            .get_mut(&name)
            .ok_or("the connection is not open")?;
        let mut body = None;
        if let Some(message) = command.get("message") {
            let mut message = message.clone();
            with_own_credentials(&mut message);
            let message: ClientMessage =
                serde_json::from_value(message).map_err(|error| format!("message: {error}"))?;
            body = Some(yagura_protocol::encode_client_message(&message));
        }
        if let Some(frame) = command.get("frame") {
            body = Some(serde_json::to_vec(frame).map_err(|error| error.to_string())?);
        }
        let read = command["read"].as_str().unwrap_or("none");
        self.runtime.block_on(async {
            if let Some(body) = body {
                connection
                    .write_all(&yagura_protocol::frame_encode(&body))
                    .await
                    .map_err(|error| format!("write: {error}"))?;
                connection
                    .flush()
                    .await
                    .map_err(|error| format!("flush: {error}"))?;
            }
            read_as(connection, read).await
        })
    }

    fn keep(&mut self, id: &str, command: &Value) -> Result<Value, String> {
        let argv = strings(&command["argv"]);
        let (program, args) = argv.split_first().ok_or("keep needs an argv")?;
        let mut process = Command::new(program);
        process.args(args);
        for name in strings(&command["env_remove"]) {
            process.env_remove(name);
        }
        for pair in command["env"].as_array().into_iter().flatten() {
            if let (Some(name), Some(value)) = (pair[0].as_str(), pair[1].as_str()) {
                process.env(name, value);
            }
        }
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = process
            .spawn()
            .map_err(|error| format!("start {program}: {error}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("the child's stdout is piped")?;
        let results = Arc::clone(&self.results);
        let id = id.to_owned();
        let pump_id = id.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                results.line(&json!({ "id": pump_id, "line": line }));
            }
            results.line(&json!({ "id": pump_id, "stdout_closed": true }));
        });
        let pid = child.id();
        self.kept.insert(id, Kept { child, stdin });
        Ok(json!({ "started": true, "pid": pid }))
    }

    fn write(&mut self, command: &Value) -> Result<Value, String> {
        let target = command["target"].as_str().unwrap_or_default();
        let kept = self.kept.get_mut(target).ok_or("no such kept child")?;
        let stdin = kept.stdin.as_mut().ok_or("the child's stdin is closed")?;
        let line = command["line"].as_str().unwrap_or_default();
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("write: {error}"))?;
        Ok(json!({ "written": true }))
    }

    fn end(&mut self, command: &Value) -> Result<Value, String> {
        let target = command["target"].as_str().unwrap_or_default();
        let mut kept = self.kept.remove(target).ok_or("no such kept child")?;
        let _ = kept.child.kill();
        let _ = kept.child.wait();
        Ok(json!({ "ended": true }))
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Replaces the strings that name the window's own credentials.
fn with_own_credentials(value: &mut Value) {
    match value {
        Value::String(text) => {
            let name = match text.as_str() {
                "$YAGURA_WINDOW_ID" => "YAGURA_WINDOW_ID",
                "$YAGURA_STATUS_TOKEN" => "YAGURA_STATUS_TOKEN",
                _ => return,
            };
            *text = std::env::var(name).unwrap_or_default();
        }
        Value::Array(items) => items.iter_mut().for_each(with_own_credentials),
        Value::Object(fields) => fields.values_mut().for_each(with_own_credentials),
        _ => {}
    }
}

fn run(command: &Value) -> Result<Value, String> {
    let argv = strings(&command["argv"]);
    let (program, args) = argv.split_first().ok_or("run needs an argv")?;
    let stdin = command["stdin"].as_str();
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start {program}: {error}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait: {error}"))?;
    Ok(json!({
        "code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    }))
}

/// The endpoint `YAGURA_INSTANCE` names, opened with retries (a daemon taking
/// the instance over may not be listening yet).
async fn connect() -> Result<Connection, String> {
    let instance = std::env::var_os("YAGURA_INSTANCE").ok_or("YAGURA_INSTANCE is not set")?;
    let namespace = yagura_os::resolve_instance_from(Some(&instance))
        .map_err(|error| format!("YAGURA_INSTANCE: {error}"))?;
    let ipc = PlatformIpc::default();
    let user = ipc
        .current_user_id()
        .map_err(|error| format!("user id: {error}"))?;
    let endpoint = yagura_os::default_endpoint(&user, &namespace);
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        match ipc.connect(&endpoint).await {
            Ok(connection) => return Ok(connection),
            Err(error) if tokio::time::Instant::now() >= deadline => {
                return Err(format!("connect {endpoint}: {error}"));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// The next frame body, or `None` when the peer closed the connection.
async fn read_body(connection: &mut Connection) -> Result<Option<Vec<u8>>, String> {
    let read = async {
        let mut length = [0u8; 4];
        if connection.read_exact(&mut length).await.is_err() {
            return None;
        }
        let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
        connection.read_exact(&mut body).await.ok()?;
        Some(body)
    };
    tokio::time::timeout(LIMIT, read)
        .await
        .map_err(|_| "no frame within the limit".to_owned())
}

/// The next response: what a push the connection is sent unasked is skipped.
async fn read_response(connection: &mut Connection) -> Result<Option<DaemonMessage>, String> {
    loop {
        let Some(body) = read_body(connection).await? else {
            return Ok(None);
        };
        match yagura_protocol::decode_daemon_message(&body)
            .map_err(|error| format!("decode: {error}"))?
        {
            DaemonMessage::Frame(_)
            | DaemonMessage::StatusChanged { .. }
            | DaemonMessage::ChatParticipantsChanged => {}
            message => return Ok(Some(message)),
        }
    }
}

async fn read_as(connection: &mut Connection, read: &str) -> Result<Value, String> {
    let messages = |list: &[DaemonMessage]| serde_json::to_value(list).map_err(|e| e.to_string());
    match read {
        "none" => Ok(json!({ "sent": true })),
        "frame" => match read_body(connection).await? {
            Some(body) => Ok(json!({ "frame": serde_json::from_slice::<Value>(&body)
                .map_err(|error| format!("frame: {error}"))? })),
            None => Ok(json!({ "closed": true })),
        },
        "one" => match read_response(connection).await? {
            Some(message) => Ok(json!({ "messages": messages(&[message])? })),
            None => Ok(json!({ "closed": true, "messages": [] })),
        },
        "pong" => {
            let ping = yagura_protocol::encode_client_message(&ClientMessage::Ping);
            connection
                .write_all(&yagura_protocol::frame_encode(&ping))
                .await
                .map_err(|error| format!("write: {error}"))?;
            let mut seen = Vec::new();
            loop {
                match read_response(connection).await? {
                    Some(DaemonMessage::Pong) => {
                        return Ok(json!({ "messages": messages(&seen)? }));
                    }
                    Some(message) => seen.push(message),
                    None => return Ok(json!({ "closed": true, "messages": messages(&seen)? })),
                }
            }
        }
        "delivery" => match read_response(connection).await? {
            Some(message) => {
                if matches!(message, DaemonMessage::Delivery { .. }) {
                    let printed =
                        yagura_protocol::encode_client_message(&ClientMessage::DeliverPrinted);
                    connection
                        .write_all(&yagura_protocol::frame_encode(&printed))
                        .await
                        .map_err(|error| format!("write: {error}"))?;
                }
                Ok(json!({ "messages": messages(&[message])? }))
            }
            None => Ok(json!({ "closed": true, "messages": [] })),
        },
        other => Err(format!("unknown read {other:?}")),
    }
}
