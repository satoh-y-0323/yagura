#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A relay session, spoken the way a CLI's relay speaks it, while a
//! `wait_for_status` waits for a status its window never takes.
//!
//! Such a call must not hold up the agent's other calls meanwhile, and the
//! client's own way out -- the `notifications/cancelled` an MCP client sends
//! when it gives up -- frees its place among the calls a session runs at
//! once. The cases run a real `IpcServer` against `echo_child`. Every
//! daemon binds a named instance reserved for its case. The hang guards only
//! bound a failure.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::task::JoinHandle;

use yagura_daemon::control::{ApprovalPolicy, ApprovalSection};
use yagura_daemon::window::InputOrigin;
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowEntry, WindowRegistry};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::WindowStatus;
use yagura_test_support::detection::{CONTROL_READY_LINE, idle_definition_toml};
use yagura_test_support::paths::unique_temp_dir;
use yagura_test_support::process::{echo_child_command, own_canonical_exe};
use yagura_test_support::scan::contains_subsequence;

use super::{McpSession, tool_reply};

use yagura_test_support::HANG_GUARD;

const FIXTURE_PREFIX: &str = "yagura-mcp-cancellation";

const DEFINITION: &str = "cancel-echo";

/// How many calls one relay session runs at once.
const CALLS_AT_ONCE: usize = 4;

/// Written in place of a console interrupt, which ConPTY would turn into a
/// signal that ends the receiver.
const INTERRUPT_BYTES: &[u8] = b"INTR";

/// The only token shape `status_token_override` accepts; every window adopts
/// it, so the bridge can claim a window with it.
const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef";

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

fn daemon_config(dir: &std::path::Path) -> DaemonConfig {
    std::fs::write(
        dir.join(format!("{DEFINITION}.toml")),
        idle_definition_toml(DEFINITION, "", ""),
    )
    .expect("write the echo_child definition");
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: DEFINITION.to_string(),
        approval_policy: Some(policy(&["interrupt_window"])),
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
            registry,
            server_task,
        }
    }

    fn entry(&self, window_id: &str) -> Arc<WindowEntry> {
        self.registry
            .get(window_id)
            .unwrap_or_else(|| panic!("window {window_id} must be present in the registry"))
    }

    /// An `echo_child` window, driven to `idle`.
    async fn open_window(&self, name: &str) -> String {
        let window_id = self
            .registry
            .open(OpenWindowRequest {
                command: echo_child_command(),
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
        entry
            .write_pty_input(CONTROL_READY_LINE.as_bytes(), InputOrigin::Interactive)
            .await
            .expect("the ready line must be written");
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while entry.current_status() != WindowStatus::Idle {
            assert!(
                tokio::time::Instant::now() < deadline,
                "window {name} never became idle"
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
}

/// Starts a `wait_for_status` on `receiver` for a status it never takes, and
/// returns once the session has taken the call up: the server dispatches each
/// request as it reads it, so the call is under way before a later ping is
/// answered.
async fn start_waiting_call(bridge: &mut McpSession, receiver: &str) -> i64 {
    let call = bridge
        .start_tool_call(
            "wait_for_status",
            json!({ "window_id": receiver, "target_status": "exited", "timeout_sec": 600 }),
        )
        .await;
    bridge.ping().await;
    call
}

// =====================================================================
// A waiting call does not hold up the agent's other calls
// =====================================================================

#[tokio::test]
async fn get_window_status_answers_while_a_wait_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-status").await;
    let receiver = daemon.open_window("receiver").await;
    let mut bridge = daemon.open_session(None).await;
    bridge.initialize().await;

    start_waiting_call(&mut bridge, &receiver).await;
    let status = bridge
        .start_tool_call("get_window_status", json!({ "window_id": receiver }))
        .await;
    let response = bridge.response_to(status).await;
    let (is_error, _) = tool_reply(&response);
    assert!(!is_error, "get_window_status must succeed, got {response}");

    bridge.kill().await;
}

#[tokio::test]
async fn interrupt_window_is_written_while_a_wait_from_the_same_bridge_waits() {
    let daemon = Daemon::bind("mcp-stuck-intr").await;
    let receiver = daemon.open_window("receiver").await;
    let mut bridge = daemon.open_session(None).await;
    bridge.initialize().await;

    start_waiting_call(&mut bridge, &receiver).await;
    let interrupt = bridge
        .start_tool_call("interrupt_window", json!({ "window_id": receiver }))
        .await;
    let interrupt_response = bridge.response_to(interrupt).await;
    let (is_error, _) = tool_reply(&interrupt_response);
    assert!(
        !is_error,
        "interrupt_window must succeed, got {interrupt_response}"
    );
    // echo_child prints what it reads, so the interrupt shows once it reached the child.
    daemon.wait_for_output(&receiver, INTERRUPT_BYTES).await;

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
        receivers.push(daemon.open_window(&format!("receiver-{n}")).await);
    }
    let mut bridge = daemon.open_session(None).await;
    bridge.initialize().await;

    let mut waiting_calls = Vec::new();
    for receiver in &receivers[..CALLS_AT_ONCE] {
        waiting_calls.push(start_waiting_call(&mut bridge, receiver).await);
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

    bridge.cancel(waiting_calls[0]).await;
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
