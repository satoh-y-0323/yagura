#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The compiled bridge, driven over MCP stdio, while a `send_message` waits on
//! a receiver that shows nothing.
//!
//! Such a call no longer ends on a clock, so the client's own way out -- the
//! `notifications/cancelled` an MCP client sends when it gives up -- has to
//! reach the daemon as the requester leaving, and the call must not hold up
//! the agent's other calls meanwhile. The daemon cases run a real `IpcServer`
//! against `submit_child` in composer-draw mode; the connection-count case
//! runs a scripted daemon that never answers a `send_text`, so it can count
//! the bridge's connections. Every daemon binds a named instance reserved for
//! its case before the bridge starts, so the bridge can only reach it. The
//! hang guards only bound a failure.
//!
//! Each case runs on the test's single-threaded runtime: a connection's end
//! releases its window claim before any task it signalled runs, which is
//! what lets the reconnect case read the claim after the take-back.

mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use yagura_daemon::control::{ApprovalPolicy, ApprovalSection};
use yagura_daemon::window::InputOrigin;
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowEntry, WindowRegistry};
use yagura_mcp::transport::MAX_DAEMON_CONNECTIONS;
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{
    ClientMessage, DaemonMessage, INSTANCE_ENV, PROTOCOL_REVISION, STATUS_TOKEN_ENV, WINDOW_ID_ENV,
    WindowStatus, decode_client_message, encode_daemon_message, frame_decode, frame_encode,
};
use yagura_test_support::detection::{composer_section_toml, submit_definition_toml};
use yagura_test_support::paths::unique_temp_dir;
use yagura_test_support::process::{own_canonical_exe, submit_child_command_with_composer};
use yagura_test_support::scan::contains_subsequence;

use common::{LINE_TIMEOUT, McpChild, tool_reply};

/// The longest any single step may take before the case is called hung.
const HANG_GUARD: Duration = Duration::from_secs(30);

const FIXTURE_PREFIX: &str = "yagura-mcp-cancellation";

const DEFINITION: &str = "cancel-submit";

const MARKER: &str = "CANCEL-READY";

/// A judged delivery: a probe, and a clear sequence that takes a body back.
const INPUT_TOML: &str = "bracketed_paste = true\nready_hold_ms = 0\nsubmit_delay_ms = 120\nclear = \"\\u0015\"\ndelivery_confirm_ms = 2000\nresend_max = 0\nprobe = \"x\"";

/// Written in place of a console interrupt, which ConPTY would turn into a
/// signal that ends the receiver.
const INTERRUPT_BYTES: &[u8] = b"INTR";

/// The only token shape `status_token_override` accepts; every window adopts
/// it, so the bridge can claim a window with it.
const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Typed into a window last; read back, it proves the child has read
/// everything written before it.
const SENTINEL: &[u8] = b"SENTINEL-AFTER";

const PASTE_END: &[u8] = b"\x1b[201~";

const CLEAR: u8 = 0x15;

// =====================================================================
// A real daemon at a reserved instance
// =====================================================================

fn policy(tools: &[&str]) -> ApprovalPolicy {
    ApprovalPolicy {
        approval: Some(ApprovalSection {
            auto_approve: tools.iter().map(|tool| (*tool).to_string()).collect(),
        }),
        limits: None,
        chat: None,
        plans: None,
    }
}

fn daemon_config(dir: &Path) -> DaemonConfig {
    std::fs::write(
        dir.join(format!("{DEFINITION}.toml")),
        format!(
            "{}{}",
            submit_definition_toml(DEFINITION, MARKER, 5, INPUT_TOML),
            composer_section_toml()
        ),
    )
    .expect("write the submit_child definition");
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: DEFINITION.to_string(),
        approval_policy: Some(policy(&["send_message", "send_keys", "interrupt_window"])),
        status_token_override: Some(VALID_TOKEN.into()),
        approver_exe: Some(own_canonical_exe()),
        interrupt_bytes: INTERRUPT_BYTES.to_vec(),
        ..DaemonConfig::default()
    }
}

/// The endpoint a bridge started with `YAGURA_INSTANCE=instance_id` dials.
async fn bind_instance(
    instance_id: &str,
) -> (
    PlatformIpc,
    <PlatformIpc as IpcTransport>::Listener,
    String,
    String,
) {
    let namespace = yagura_os::resolve_instance_from(Some(OsStr::new(instance_id)))
        .unwrap_or_else(|err| panic!("{instance_id:?} must be a valid instance id: {err}"));
    let ipc = PlatformIpc::default();
    let user_id = ipc
        .current_user_id()
        .expect("current_user_id must resolve in a normal test-runner session");
    let endpoint = yagura_os::default_endpoint_owning(&user_id, &namespace);
    let security = ipc
        .endpoint_security()
        .expect("endpoint_security must succeed in a normal test-runner session");
    let listener = ipc
        .bind(&endpoint, security)
        .await
        .unwrap_or_else(|err| panic!("cannot claim endpoint {endpoint:?}: {err}"));
    (ipc, listener, endpoint, user_id)
}

struct Daemon {
    instance_id: &'static str,
    dir: PathBuf,
    registry: Arc<WindowRegistry>,
    server_task: JoinHandle<Result<(), yagura_daemon::IpcError>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

impl Daemon {
    async fn bind(instance_id: &'static str) -> Self {
        let dir = unique_temp_dir(FIXTURE_PREFIX, instance_id);
        let config = daemon_config(&dir);
        let (_ipc, listener, endpoint, user_id) = bind_instance(instance_id).await;
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let server = Arc::new(IpcServer::new(registry.clone(), endpoint, config));
        let server_task = tokio::spawn(server.run_with_listener(listener, user_id));
        Self {
            instance_id,
            dir,
            registry,
            server_task,
        }
    }

    fn entry(&self, window_id: &str) -> Arc<WindowEntry> {
        self.registry
            .get(window_id)
            .unwrap_or_else(|| panic!("window {window_id} must be present in the registry"))
    }

    /// A `submit_child` window in composer-draw mode, ready for input.
    /// `frozen_input_log` makes it paint nothing from the first paste on and
    /// append everything it reads to that file.
    async fn open_window(&self, name: &str, frozen_input_log: Option<&Path>) -> String {
        let mut command = submit_child_command_with_composer(MARKER, 0, 0, 0, 0);
        if let Some(log) = frozen_input_log {
            command.push(String::new()); // fold_placeholder: none
            command.push("frozen:".to_string());
            command.push(String::new()); // overlay: none
            command.push(String::new()); // overlay_heading
            command.push(
                log.to_str()
                    .expect("the input log path must be valid UTF-8")
                    .to_string(),
            );
        }
        let window_id = self
            .registry
            .open(OpenWindowRequest {
                command,
                cwd: None,
                name: Some(name.to_string()),
                vendor: None,
                detection: Some(DEFINITION.to_string()),
                on_name_taken: Default::default(),
            })
            .await
            .expect("the window must open")
            .window_id;
        let entry = self.entry(&window_id);
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while entry.current_status() != WindowStatus::WaitingInput {
            assert!(
                tokio::time::Instant::now() < deadline,
                "window {name} never became ready for input"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        window_id
    }

    /// The bridge under test, dialing this daemon only. With `identity` it
    /// claims that window, as a window's own agent CLI does.
    fn spawn_bridge(&self, identity: Option<&str>) -> McpChild {
        let mut env = vec![(INSTANCE_ENV, self.instance_id)];
        if let Some(window_id) = identity {
            env.push((WINDOW_ID_ENV, window_id));
            env.push((STATUS_TOKEN_ENV, VALID_TOKEN));
        }
        McpChild::spawn(&env)
    }

    fn mcp_connected(&self, window_id: &str) -> Option<bool> {
        self.registry
            .list()
            .into_iter()
            .find(|info| info.window_id == window_id)
            .and_then(|info| info.mcp_connected)
    }

    async fn wait_until_mcp_connected(&self, window_id: &str, what: &str) {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while self.mcp_connected(window_id) != Some(true) {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_output(&self, window_id: &str, needle: &[u8]) {
        let entry = self.entry(window_id);
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while !contains_subsequence(&entry.session().captured_output(), needle) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "window {window_id} never printed {:?}",
                String::from_utf8_lossy(needle)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Writes [`SENTINEL`] as another writer and returns everything the child
    /// read up to it.
    async fn read_input_through_sentinel(&self, window_id: &str, log: &Path) -> Vec<u8> {
        self.entry(window_id)
            .write_pty_input(SENTINEL, InputOrigin::Interactive)
            .await
            .expect("the sentinel must not fail");
        wait_until_input_log(log, "the sentinel", |read| {
            contains_subsequence(read, SENTINEL)
        })
        .await
    }
}

async fn wait_until_input_log(
    log: &Path,
    what: &str,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let read = std::fs::read(log).unwrap_or_default();
        if accept(&read) {
            return read;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the receiver never read {what}; it read {read:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether the child read a clear byte after the end of a bracketed paste.
fn taken_back_after_paste(read: &[u8]) -> bool {
    read.windows(PASTE_END.len())
        .position(|window| window == PASTE_END)
        .is_some_and(|end| read[end..].contains(&CLEAR))
}

fn assert_not_written(response: &Value) {
    let (is_error, text) = tool_reply(response);
    assert!(
        is_error && text.starts_with("the text was not written into the window"),
        "the waiting send_message must end not_written, got {response}"
    );
}

/// Starts a judged `send_message` into `receiver` and returns once its body
/// has reached the receiver, which then shows nothing.
async fn start_stuck_send(daemon: &Daemon, bridge: &mut McpChild, receiver: &str) -> i64 {
    let call = bridge
        .start_tool_call(
            "send_message",
            json!({ "window_id": receiver, "text": "BODY\n" }),
        )
        .await;
    daemon.wait_for_output(receiver, b"PASTE 1").await;
    call
}

// =====================================================================
// A cancelled call reaches the daemon as the requester leaving
// =====================================================================

#[tokio::test]
async fn a_cancelled_send_message_presses_no_submit_key_and_its_body_is_taken_back() {
    let daemon = Daemon::bind("mcp-cancel-send").await;
    let requester = daemon.open_window("requester", None).await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.spawn_bridge(Some(&requester));
    bridge.initialize().await;

    let call = start_stuck_send(&daemon, &mut bridge, &receiver).await;
    bridge.cancel(call).await;

    wait_until_input_log(&log, "a take-back after the cancelled body", |read| {
        taken_back_after_paste(read)
    })
    .await;
    let read = daemon.read_input_through_sentinel(&receiver, &log).await;
    assert!(
        !read.contains(&b'\r'),
        "no submit key may follow a cancelled send_message's body: {read:?}"
    );

    bridge.kill().await;
}

#[tokio::test]
async fn after_a_cancelled_call_the_window_shows_its_mcp_connection_again_without_another_call() {
    let daemon = Daemon::bind("mcp-cancel-ident").await;
    let requester = daemon.open_window("requester", None).await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.spawn_bridge(Some(&requester));
    bridge.initialize().await;
    daemon
        .wait_until_mcp_connected(&requester, "the bridge never claimed its window at startup")
        .await;

    let call = start_stuck_send(&daemon, &mut bridge, &receiver).await;
    bridge.cancel(call).await;
    // The take-back runs only after the cancelled connection's end has
    // released its claim, so a claim seen from here on is a fresh one.
    wait_until_input_log(&log, "a take-back after the cancelled body", |read| {
        taken_back_after_paste(read)
    })
    .await;

    daemon
        .wait_until_mcp_connected(
            &requester,
            "after a cancelled call the window must show its MCP connection again before the \
             agent makes another call",
        )
        .await;

    bridge.kill().await;
}

// =====================================================================
// A waiting send_message does not hold up the agent's other calls
// =====================================================================

#[tokio::test]
async fn get_window_status_answers_while_a_send_message_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-status").await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.spawn_bridge(None);
    bridge.initialize().await;

    let stuck = start_stuck_send(&daemon, &mut bridge, &receiver).await;
    let status = bridge
        .start_tool_call("get_window_status", json!({ "window_id": receiver }))
        .await;
    let response = bridge.response_to(status).await;
    let (is_error, _) = tool_reply(&response);
    assert!(!is_error, "get_window_status must succeed, got {response}");

    daemon
        .entry(&receiver)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    assert_not_written(&bridge.response_to(stuck).await);

    bridge.kill().await;
}

#[tokio::test]
async fn send_keys_is_written_while_a_send_message_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-keys").await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.spawn_bridge(None);
    bridge.initialize().await;

    let stuck = start_stuck_send(&daemon, &mut bridge, &receiver).await;
    let keys = bridge
        .start_tool_call(
            "send_keys",
            json!({ "window_id": receiver, "keys": ["backspace"] }),
        )
        .await;
    // The keys end the waiting call, so the two answers may come either way.
    let [keys_response, stuck_response] =
        <[Value; 2]>::try_from(bridge.responses_to(&[keys, stuck]).await).expect("two answers");
    let (is_error, _) = tool_reply(&keys_response);
    assert!(!is_error, "send_keys must succeed, got {keys_response}");
    assert_not_written(&stuck_response);
    let read = daemon.read_input_through_sentinel(&receiver, &log).await;
    assert!(
        read.contains(&0x08) || read.contains(&0x7f),
        "the keys must have reached the receiver: {read:?}"
    );

    bridge.kill().await;
}

#[tokio::test]
async fn interrupt_window_is_written_while_a_send_message_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-intr").await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.spawn_bridge(None);
    bridge.initialize().await;

    let stuck = start_stuck_send(&daemon, &mut bridge, &receiver).await;
    let interrupt = bridge
        .start_tool_call("interrupt_window", json!({ "window_id": receiver }))
        .await;
    let [interrupt_response, stuck_response] =
        <[Value; 2]>::try_from(bridge.responses_to(&[interrupt, stuck]).await)
            .expect("two answers");
    let (is_error, _) = tool_reply(&interrupt_response);
    assert!(
        !is_error,
        "interrupt_window must succeed, got {interrupt_response}"
    );
    assert_not_written(&stuck_response);
    let read = daemon.read_input_through_sentinel(&receiver, &log).await;
    assert!(
        contains_subsequence(&read, INTERRUPT_BYTES),
        "the interrupt must have reached the receiver: {read:?}"
    );

    bridge.kill().await;
}

// =====================================================================
// The bridge's connections are bounded, and a full set waits for one
// =====================================================================

/// What the scripted daemon saw, per connection, in the order it saw it.
#[derive(Debug)]
enum WireEvent {
    Request { conn: usize, request: Value },
    Closed { conn: usize },
}

/// A daemon that answers the transport's own protocol, refuses every other
/// request, and never answers a `send_text`, so each `send_message` keeps
/// its connection busy until the bridge lets go of it.
struct ScriptedDaemon {
    events: mpsc::UnboundedReceiver<WireEvent>,
    task: JoinHandle<()>,
}

impl Drop for ScriptedDaemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedDaemon {
    async fn bind(instance_id: &str) -> Self {
        let (ipc, mut listener, _endpoint, _user_id) = bind_instance(instance_id).await;
        let (sender, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut next_conn = 0;
            while let Ok(conn) = ipc.accept(&mut listener).await {
                tokio::spawn(serve_scripted(conn, next_conn, sender.clone()));
                next_conn += 1;
            }
        });
        Self { events, task }
    }

    async fn next_event(&mut self, waiting_for: &str) -> WireEvent {
        tokio::time::timeout(HANG_GUARD, self.events.recv())
            .await
            .unwrap_or_else(|_| panic!("the daemon never saw {waiting_for}"))
            .expect("the scripted daemon stopped")
    }
}

async fn serve_scripted<C: AsyncRead + AsyncWrite + Unpin>(
    mut conn: C,
    id: usize,
    events: mpsc::UnboundedSender<WireEvent>,
) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        while let Ok(Some((body, consumed))) = frame_decode(&buffer) {
            let request: Value =
                serde_json::from_slice(body).expect("the bridge must send JSON frames");
            let reply = scripted_answer(body, &request);
            buffer.drain(..consumed);
            let _ = events.send(WireEvent::Request { conn: id, request });
            if let Some(reply) = reply
                && conn
                    .write_all(&frame_encode(&encode_daemon_message(&reply)))
                    .await
                    .is_err()
            {
                let _ = events.send(WireEvent::Closed { conn: id });
                return;
            }
        }
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => {
                let _ = events.send(WireEvent::Closed { conn: id });
                return;
            }
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

fn scripted_answer(body: &[u8], request: &Value) -> Option<DaemonMessage> {
    match decode_client_message(body) {
        Ok(ClientMessage::GetProtocolRevision) => Some(DaemonMessage::ProtocolRevision {
            revision: PROTOCOL_REVISION,
        }),
        Ok(ClientMessage::Identify { window_id, .. }) => {
            Some(DaemonMessage::Identified { window_id })
        }
        Ok(ClientMessage::SendText { .. }) => None,
        _ => Some(DaemonMessage::Error {
            code: "invalid_argument".to_string(),
            message: format!("daemon received {}", wire_type(request)),
            rejected_argument: None,
        }),
    }
}

fn wire_type(request: &Value) -> &str {
    request
        .get("type")
        .and_then(Value::as_str)
        .expect("every request must carry its wire type")
}

#[tokio::test]
async fn a_bridge_holds_at_most_four_daemon_connections_and_the_next_call_proceeds_once_a_cancel_frees_one()
 {
    let mut daemon = ScriptedDaemon::bind("mcp-conn-limit").await;
    let mut bridge = McpChild::spawn(&[(INSTANCE_ENV, "mcp-conn-limit")]);
    bridge.initialize().await;

    let mut stuck_calls = Vec::new();
    for n in 0..MAX_DAEMON_CONNECTIONS {
        let call = bridge
            .start_tool_call(
                "send_message",
                json!({ "window_id": "w-1", "text": format!("STUCK-{n}") }),
            )
            .await;
        stuck_calls.push((call, format!("STUCK-{n}")));
    }

    // Connections that have carried a request and not yet ended.
    let mut open: Vec<usize> = Vec::new();
    let mut stuck_on: Vec<(String, usize)> = Vec::new();
    while stuck_on.len() < MAX_DAEMON_CONNECTIONS {
        let waiting_for = format!(
            "all {MAX_DAEMON_CONNECTIONS} send_message calls at once (only {} arrived)",
            stuck_on.len()
        );
        match daemon.next_event(&waiting_for).await {
            WireEvent::Request { conn, request } => {
                if !open.contains(&conn) {
                    open.push(conn);
                }
                if wire_type(&request) == "send_text" {
                    let text = request
                        .get("text")
                        .and_then(Value::as_str)
                        .expect("send_text carries its text")
                        .to_string();
                    stuck_on.push((text, conn));
                }
            }
            WireEvent::Closed { conn } => open.retain(|open_conn| *open_conn != conn),
        }
    }
    let mut stuck_conns: Vec<usize> = stuck_on.iter().map(|(_, conn)| *conn).collect();
    stuck_conns.sort_unstable();
    stuck_conns.dedup();
    assert_eq!(
        stuck_conns.len(),
        MAX_DAEMON_CONNECTIONS,
        "each waiting send_message must hold a connection of its own: {stuck_on:?}"
    );

    let status = bridge
        .start_tool_call("get_window_status", json!({ "window_id": "w-1" }))
        .await;
    // The server dispatches each request as it reads it, so the call above
    // has been taken up before this ping is answered.
    bridge.ping().await;

    let (cancelled_call, cancelled_text) = &stuck_calls[0];
    let cancelled_conn = stuck_on
        .iter()
        .find(|(text, _)| text == cancelled_text)
        .map(|(_, conn)| *conn)
        .expect("the first send_message reached the daemon");
    bridge.cancel(*cancelled_call).await;

    let mut cancelled_conn_ended = false;
    loop {
        match daemon
            .next_event("the waiting call once a connection was freed")
            .await
        {
            WireEvent::Request { conn, request } => {
                if !open.contains(&conn) {
                    open.push(conn);
                }
                if wire_type(&request) == "get_window_status" {
                    assert!(
                        cancelled_conn_ended,
                        "the call made while {MAX_DAEMON_CONNECTIONS} connections were busy must \
                         wait for the cancelled one to end, but reached the daemon first on \
                         connection {conn} (open: {open:?})"
                    );
                    assert!(
                        open.len() <= MAX_DAEMON_CONNECTIONS,
                        "at most {MAX_DAEMON_CONNECTIONS} connections may be open at once, got \
                         {open:?}"
                    );
                    break;
                }
            }
            WireEvent::Closed { conn } => {
                if conn == cancelled_conn {
                    cancelled_conn_ended = true;
                }
                open.retain(|open_conn| *open_conn != conn);
            }
        }
    }

    let response = tokio::time::timeout(LINE_TIMEOUT, bridge.response_to(status))
        .await
        .expect("the waiting call must answer once it reached the daemon");
    let (is_error, text) = tool_reply(&response);
    assert!(
        is_error && text.ends_with("daemon received get_window_status"),
        "the waiting call must return the daemon's answer, got {response}"
    );

    bridge.kill().await;
}
