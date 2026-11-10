#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A relay session, spoken the way a CLI's relay speaks it, while a
//! `send_message` waits on a receiver that shows nothing.
//!
//! Such a call no longer ends on a clock, so the client's own way out -- the
//! `notifications/cancelled` an MCP client sends when it gives up -- has to
//! reach the daemon as the requester leaving, and the call must not hold up
//! the agent's other calls meanwhile. The cases run a real `IpcServer`
//! against `submit_child` in composer-draw mode. Every daemon binds a named
//! instance reserved for its case. The hang guards only bound a failure.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use yagura_daemon::control::{ApprovalPolicy, ApprovalSection};
use yagura_daemon::window::InputOrigin;
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowEntry, WindowRegistry};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::WindowStatus;
use yagura_test_support::detection::{composer_section_toml, submit_definition_toml};
use yagura_test_support::paths::unique_temp_dir;
use yagura_test_support::process::{own_canonical_exe, submit_child_command_with_composer};
use yagura_test_support::scan::contains_subsequence;

use super::{McpSession, tool_reply};

use yagura_test_support::HANG_GUARD;

const FIXTURE_PREFIX: &str = "yagura-mcp-cancellation";

const DEFINITION: &str = "cancel-submit";

const MARKER: &str = "CANCEL-READY";

/// A judged delivery: a probe, and a clear sequence that takes a body back.
const INPUT_TOML: &str = "bracketed_paste = true\nready_hold_ms = 0\nsubmit_delay_ms = 120\nclear = \"\\u0015\"\ndelivery_confirm_ms = 2000\nresend_max = 0\nprobe = \"x\"";

/// How many calls one relay session runs at once.
const CALLS_AT_ONCE: usize = 4;

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

/// The endpoint of the instance `instance_id`, claimed for one case.
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
    endpoint: String,
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
        let server = Arc::new(IpcServer::new(registry.clone(), endpoint.clone(), config));
        let server_task = tokio::spawn(server.run_with_listener(listener, user_id));
        Self {
            endpoint,
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

    /// A relay session to this daemon. With `identity` it claims that window,
    /// as a window's own agent CLI does.
    async fn open_session(&self, identity: Option<&str>) -> McpSession {
        McpSession::open(
            &self.endpoint,
            identity.map(|window_id| (window_id, VALID_TOKEN)),
        )
        .await
    }

    /// Polls `window_id`'s text, as `read_output` reads it, until `needle` appears.
    async fn wait_for_output(&self, window_id: &str, needle: &[u8]) {
        let entry = self.entry(window_id);
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while !contains_subsequence(
            &entry
                .session()
                .text_tail(100_000, 256 * 1024)
                .await
                .map(|(text, _)| text.into_bytes())
                .unwrap_or_default(),
            needle,
        ) {
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
async fn start_stuck_send(daemon: &Daemon, bridge: &mut McpSession, receiver: &str) -> i64 {
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
    let mut bridge = daemon.open_session(Some(&requester)).await;
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

// =====================================================================
// A waiting send_message does not hold up the agent's other calls
// =====================================================================

#[tokio::test]
async fn get_window_status_answers_while_a_send_message_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-status").await;
    let log = daemon.dir.join("receiver-input.log");
    let receiver = daemon.open_window("receiver", Some(&log)).await;
    let mut bridge = daemon.open_session(None).await;
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
    let mut bridge = daemon.open_session(None).await;
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
    let mut bridge = daemon.open_session(None).await;
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
// A session runs a bounded number of calls at once, and a full set waits
// =====================================================================

#[tokio::test]
async fn a_session_runs_at_most_four_calls_at_once_and_the_next_proceeds_once_a_cancel_frees_one() {
    let daemon = Daemon::bind("mcp-call-limit").await;
    let mut receivers = Vec::new();
    for n in 0..=CALLS_AT_ONCE {
        let log = daemon.dir.join(format!("receiver-{n}-input.log"));
        receivers.push(
            daemon
                .open_window(&format!("receiver-{n}"), Some(&log))
                .await,
        );
    }
    let mut bridge = daemon.open_session(None).await;
    bridge.initialize().await;

    // Each waiting send_message has reached its receiver, so each runs.
    let mut stuck_calls = Vec::new();
    for receiver in &receivers[..CALLS_AT_ONCE] {
        stuck_calls.push(start_stuck_send(&daemon, &mut bridge, receiver).await);
    }

    let status = bridge
        .start_tool_call(
            "get_window_status",
            json!({ "window_id": receivers[CALLS_AT_ONCE] }),
        )
        .await;
    // The server dispatches each request as it reads it, so the call above
    // has been taken up before this ping is answered; its answer must not
    // come first.
    bridge.ping().await;

    bridge.cancel(stuck_calls[0]).await;
    let response = tokio::time::timeout(HANG_GUARD, bridge.response_to(status))
        .await
        .expect("the waiting call must answer once a cancel freed a place");
    let (is_error, text) = tool_reply(&response);
    assert!(
        !is_error,
        "the waiting call must return the daemon's answer, got {text}"
    );

    bridge.kill().await;
}
