#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end tests: the yagura-mcp tool layer against a *real* daemon.
//!
//! `tests/tools.rs` covers the tool layer against a scripted transport
//! double; this file drives the same tools through the real
//! `EndpointTransport` into an in-process `IpcServer` + `WindowRegistry`.
//! Nothing here is mocked between the tool function and the child process:
//! a `send_message` success is only accepted once the bytes show up in the
//! child's PTY output, an `interrupt_window` success only once the configured
//! bytes reach the child, and a `close_window` success only once the child's
//! *detached grandchild* has demonstrably stopped running.
//!
//! ## Control-tool fixtures
//!
//! - Policies, detection definitions and the `StatusEvent` token are injected
//!   straight into `DaemonConfig`. The `--playbook`, `--detection-dir` and
//!   environment entry points belong to a real daemon process and are covered
//!   by the manual checks, not here.
//! - A non-`Unknown` window uses a detection definition written to a temporary
//!   directory, and its transition to `idle` is *observed* before the
//!   assertion under test runs. `Unknown` windows are built by running with no
//!   detection directory at all.
//! - An MCP client sees `wait_for_status` as a blocking call: its reply does
//!   not arrive until the wait ends, so these tests assert only its outcomes
//!   -- never non-blocking behaviour.
//!
//! The daemon runs in-process on a per-test unique endpoint name, so these
//! tests are shell-independent and safe to run concurrently with each other
//! and with a resident daemon.
//!
//! `cargo build -p yagura-daemon --bins` must have run first: the `echo_child`
//! binary is located next to the test executable.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use yagura_test_support::detection::{
    CONTROL_DETECTION_NAME, CONTROL_READY_LINE, composer_section_toml, idle_definition_toml,
};
use yagura_test_support::ipc::{
    Connection, connect_with_retry, read_daemon_frame, reply_within, send_client_message,
    unique_endpoint_name,
};
use yagura_test_support::paths::unique_temp_dir;
use yagura_test_support::process::{
    COMPOSER_MARKER, COMPOSER_SEPARATOR, assert_file_len_stable, echo_child_command,
    echo_child_command_with_composer, own_canonical_exe, surviving_grandchild_command,
    wait_until_file_grows,
};
use yagura_test_support::scan::contains_subsequence;

use yagura_daemon::control::{ApprovalPolicy, ApprovalSection};
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowEntry, WindowRegistry};
use yagura_mcp::tools::{
    CloseWindowArgs, GetWindowStatusArgs, InterruptWindowArgs, OpenWindowArgs, ReadOutputArgs,
    SendHandoffArgs, SendMessageArgs, ToolError, WaitForStatusArgs, close_window,
    get_window_status, interrupt_window, list_windows, open_window, read_output, send_handoff,
    send_message, wait_for_status,
};
use yagura_mcp::transport::{
    DaemonTransport, EndpointTransport, RequesterIdentity, TransportError, WindowIdentity,
};
use yagura_protocol::{
    ApprovalRequest, ApprovalRequesterKind, ClientMessage, DaemonMessage, WindowStatus,
};

/// Marker text written through `send_message` and looked for in the child's
/// PTY output. Kept short so ConPTY's 80-column wrapping cannot split it.
const SEND_MESSAGE_MARKER: &str = "mcp-e2e-send;";

/// Upper bound for every "wait until the daemon/child catches up" loop.
const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound for one tool round trip. No daemon-side wait this file sets up
/// outlasts [`CONVERGENCE_TIMEOUT`]; the margin lets such a wait answer for
/// itself before a reply that never arrives fails the test.
const ROUND_TRIP_LIMIT: Duration = CONVERGENCE_TIMEOUT.saturating_add(Duration::from_secs(2));

/// The `[input]` table of [`composer_detection_config`]'s definition: only the
/// probe a judged delivery needs.
const COMPOSER_INPUT_TABLE: &str = "\n[input]\nprobe = \"x\"\n";

/// Replaces `DaemonConfig.interrupt_bytes` so that interrupt delivery is
/// observable: the default `0x03` becomes an interrupt (`CTRL_C_EVENT` under
/// ConPTY, `SIGINT` from a Unix pty) rather than text in the scrollback.
const INTERRUPT_MARKER: &str = "INTERRUPT-MARK\r\n";

/// Token injected through `DaemonConfig.status_token_override`, in the only
/// shape `StatusToken::from_override` accepts. It is the sole way a test can
/// hold a valid token, since generated per-window tokens are never exposed;
/// every window adopts the override, so per-window isolation is off here.
const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Same shape as [`VALID_TOKEN`] but a different value, so a rejection can
/// only be attributed to the comparison, not to a shape check.
const WRONG_TOKEN: &str = "fedcba9876543210fedcba9876543210";

// =====================================================================
// Test harness (this file only; the fixture and wire helpers it builds on
// come from `yagura-test-support`)
// =====================================================================

/// Prefix for this file's endpoint names and temporary directories, so
/// neither can collide with another test binary's or with a resident
/// daemon's.
const FIXTURE_PREFIX: &str = "yagura-mcp-e2e";

/// A config whose detection directory holds the control definition under
/// [`CONTROL_DETECTION_NAME`], and whose default `detection_name` is that
/// same name, so a window opened without a `detection` argument still
/// resolves it and can leave `Unknown`.
///
/// `approver_exe` defaults to this test binary's own path -- the same value a
/// real GUI passes via `--approver-exe` -- so tests that never
/// touch `register_approver` are unaffected, while a test that does register
/// can expect it to succeed.
fn detection_config(dir: &Path) -> DaemonConfig {
    std::fs::write(
        dir.join(format!("{CONTROL_DETECTION_NAME}.toml")),
        idle_definition_toml(CONTROL_DETECTION_NAME, "", ""),
    )
    .expect("write the detection definition file");
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: CONTROL_DETECTION_NAME.to_string(),
        approver_exe: Some(own_canonical_exe()),
        ..DaemonConfig::default()
    }
}

/// [`detection_config`] plus [`composer_section_toml`], under the same
/// [`CONTROL_DETECTION_NAME`] file: for every test below whose window must
/// actually take text the daemon writes (a `send_message` body or a
/// `send_handoff` envelope).
///
/// The daemon writes text of its own only once the definition carries a
/// `[composer]` table, and a judged delivery (`send_handoff`, and a terminated
/// `send_message`) additionally needs the definition's probe and the child
/// launched in composer-draw mode.
/// The `not_written` test deliberately relies on [`detection_config`], whose
/// definition has no such table.
fn composer_detection_config(dir: &Path) -> DaemonConfig {
    std::fs::write(
        dir.join(format!("{CONTROL_DETECTION_NAME}.toml")),
        idle_definition_toml(
            CONTROL_DETECTION_NAME,
            "",
            &format!("{COMPOSER_INPUT_TABLE}{}", composer_section_toml()),
        ),
    )
    .expect("write the composer detection definition file");
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: CONTROL_DETECTION_NAME.to_string(),
        approver_exe: Some(own_canonical_exe()),
        ..DaemonConfig::default()
    }
}

/// The literal prefix every handoff envelope starts with on the wire.
const HANDOFF_ENVELOPE_PREFIX: &str = "[yagura handoff] ";

/// What a composer-draw-mode fixture paints immediately before any body: an
/// envelope prefix glued straight onto this is still an unsubmitted draft of
/// it, not the line an accepted delivery's `output.line()` call writes on its
/// own (see [`find_accepted_handoff_envelope`]).
fn composer_draft_prefix() -> String {
    format!("{COMPOSER_MARKER}{COMPOSER_SEPARATOR}")
}

/// The byte offset, in `text`, of an *accepted* handoff envelope's own
/// [`HANDOFF_ENVELOPE_PREFIX`] -- one that is not immediately glued to
/// [`composer_draft_prefix`] -- or `None` if every occurrence found is such a
/// draft (including none at all).
///
/// A composer-draw-mode fixture paints the identical prefix, unmodified,
/// right after its own marker while an envelope is still sitting unsubmitted
/// in the composer; an occurrence checked only for presence would therefore
/// stay true whether or not the daemon ever wrote (and pressed the submit
/// key for) the envelope. The distinguishing test is where the prefix
/// starts, not merely that it occurs, so the accepted line's own occurrence
/// -- which starts at the very beginning of what `output.line()` wrote,
/// immediately after that call's own screen-erase sequence -- is never
/// glued to the draft's marker+separator.
fn find_accepted_handoff_envelope(text: &str) -> Option<usize> {
    let draft_prefix = composer_draft_prefix();
    yagura_test_support::scan::find_accepted_marker(text, HANDOFF_ENVELOPE_PREFIX, &draft_prefix)
}

/// The handoff envelope's JSON body from `scrollback`, taken from the
/// *accepted* occurrence [`find_accepted_handoff_envelope`] locates rather
/// than the first occurrence in the text (which, in composer-draw mode, may
/// still be an unsubmitted draft whose escape-sequence-laden repaint would
/// fail to parse, or simply be the wrong copy of the value).
fn extract_accepted_handoff_envelope_json(scrollback: &[u8]) -> serde_json::Value {
    let draft_prefix = composer_draft_prefix();
    yagura_test_support::scan::extract_accepted_handoff_envelope_json(scrollback, &draft_prefix)
}

/// A playbook that auto-approves exactly `tools`, named by their external MCP
/// tool names. Built through the public fields rather than a TOML file, since
/// file loading is covered in `yagura-daemon`.
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

/// A real daemon (`IpcServer` + `WindowRegistry`) serving a private endpoint,
/// with one window the harness opens up front for the cases to act on.
struct DaemonHarness {
    registry: Arc<WindowRegistry>,
    endpoint: String,
    server_task: JoinHandle<Result<(), yagura_daemon::IpcError>>,
    initial_window_id: String,
}

impl DaemonHarness {
    /// The default daemon for this file: a detection directory holding the
    /// control definition is always available, so any test may drive a
    /// window (including the harness's own) to `idle` on demand via
    /// [`DaemonHarness::write_window_input`] plus [`CONTROL_READY_LINE`]. A window
    /// nobody stimulates still stays `Unknown`.
    async fn start(tag: &str) -> Self {
        let dir = unique_temp_dir(FIXTURE_PREFIX, tag);
        Self::start_with_config(tag, detection_config(&dir)).await
    }

    /// Same daemon, with the policy / detection / token knobs the control
    /// tools are governed by injected through `DaemonConfig`.
    async fn start_with_config(tag: &str, mut config: DaemonConfig) -> Self {
        // Opening the window a case then acts on is not what it measures, so
        // `open_window` is auto-approved on top of whatever policy it gives.
        let mut approved = config.approval_policy.take().unwrap_or_else(|| policy(&[]));
        if let Some(section) = approved.approval.as_mut()
            && !section
                .auto_approve
                .iter()
                .any(|name| name == "open_window")
        {
            section.auto_approve.push("open_window".to_string());
        }
        config.approval_policy = Some(approved);
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let endpoint = unique_endpoint_name(FIXTURE_PREFIX, tag);
        let server = Arc::new(IpcServer::new(
            registry.clone(),
            endpoint.clone(),
            config.clone(),
        ));
        let server_task = tokio::spawn(server.run());

        let initial_window_id = registry
            .open(OpenWindowRequest {
                command: echo_child_command(),
                cwd: None,
                name: Some("initial".to_string()),
                vendor: None,
                detection: None,
                on_name_taken: Default::default(),
            })
            .await
            .expect("the initial window must open")
            .window_id;

        Self {
            registry,
            endpoint,
            server_task,
            initial_window_id,
        }
    }

    fn transport(&self) -> BoundedTransport {
        BoundedTransport::new(EndpointTransport::new(self.endpoint.clone(), None))
    }

    /// A transport that claims `window_id` with `token` on every connection
    /// it opens, standing in for the environment a window's own agent CLI
    /// inherits.
    fn transport_as(&self, window_id: &str, token: &str) -> BoundedTransport {
        BoundedTransport::new(EndpointTransport::new(
            self.endpoint.clone(),
            Some(WindowIdentity {
                window_id: window_id.to_string(),
                token: token.to_string(),
            }),
        ))
    }

    fn entry(&self, window_id: &str) -> Arc<WindowEntry> {
        self.registry
            .get(window_id)
            .unwrap_or_else(|| panic!("window {window_id} must be present in the registry"))
    }

    /// Waits until the accept loop has the endpoint listening, so that the
    /// first *asserted* tool call is not racing daemon startup.
    /// `EndpointTransport` connects lazily and does not retry an initial connect
    /// failure, so the readiness probe has to go through a tool call.
    async fn wait_until_ready<T: DaemonTransport>(&self, transport: &T) {
        let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
        loop {
            match list_windows(transport).await {
                Ok(_) => return,
                Err(err) => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("daemon never became reachable on {}: {err}", self.endpoint);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    }

    /// Polls a window's PTY scrollback until `needle` appears.
    async fn await_child_output(&self, window_id: &str, needle: &[u8]) {
        let entry = self.entry(window_id);
        let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
        loop {
            let tail = entry.session().captured_output();
            if contains_subsequence(&tail, needle) {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {:?} to appear in window {window_id}'s output; \
                     tail so far: {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&tail)
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Opens a window through the MCP `open_window` tool and returns its id.
    ///
    /// `detection` is forwarded verbatim; pass `None` to exercise the
    /// daemon's own default.
    async fn open_via_tool<T: DaemonTransport>(
        &self,
        transport: &T,
        command: Vec<String>,
        name: &str,
        detection: Option<&str>,
    ) -> String {
        let opened = open_window(
            transport,
            OpenWindowArgs {
                command,
                cwd: None,
                name: Some(name.to_string()),
                vendor: None,
                detection: detection.map(str::to_string),
                profile: None,
                model: None,
                effort: None,
            },
        )
        .await
        .expect("open_window must succeed against a live daemon");
        opened
            .get("window_id")
            .and_then(|id| id.as_str())
            .expect("open_window must return a string window_id")
            .to_string()
    }

    /// The window's status as the MCP `get_window_status` tool reports it
    /// (the snake_case wire spelling).
    async fn status_via_tool<T: DaemonTransport>(&self, transport: &T, window_id: &str) -> String {
        let status = get_window_status(
            transport,
            GetWindowStatusArgs {
                window_id: Some(window_id.to_string()),
            },
        )
        .await
        .expect("get_window_status must succeed for an existing window");
        status
            .get("status")
            .and_then(|value| value.as_str())
            .expect("get_window_status must return a string status")
            .to_string()
    }

    async fn await_status<T: DaemonTransport>(
        &self,
        transport: &T,
        window_id: &str,
        expected: &str,
    ) {
        let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
        loop {
            let observed = self.status_via_tool(transport, window_id).await;
            if observed == expected {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("window {window_id} never reached {expected:?} (last seen {observed:?})");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Writes `bytes` straight into a window's PTY input through the
    /// registry, bypassing the approval-gated `send_message` tool: a
    /// `send_message` aimed at an `Unknown` window is refused before any
    /// policy is even consulted, so it cannot be used to reach `idle` in the
    /// first place.
    async fn write_window_input(&self, window_id: &str, bytes: &[u8]) {
        self.entry(window_id)
            .session()
            .write_input(bytes)
            .await
            .expect("write_input");
    }

    /// Opens an `echo_child` window under the control definition, drives it to
    /// `idle` with the marker and *observes* the transition before returning:
    /// no assertion may run against an unobserved transition.
    async fn open_idle_window<T: DaemonTransport>(&self, transport: &T) -> String {
        self.open_idle_window_named(transport, "idle-window").await
    }

    /// [`DaemonHarness::open_idle_window`] under a name of the caller's choice,
    /// for a test that holds two idle windows at once: running windows cannot
    /// share a name.
    async fn open_idle_window_named<T: DaemonTransport>(
        &self,
        transport: &T,
        name: &str,
    ) -> String {
        let window_id = self
            .open_via_tool(transport, echo_child_command(), name, None)
            .await;
        self.write_window_input(&window_id, CONTROL_READY_LINE.as_bytes())
            .await;
        self.await_status(transport, &window_id, "idle").await;
        window_id
    }

    /// [`DaemonHarness::open_idle_window`] in composer-draw mode, opened the way a
    /// profile that declares delivery with no way to wake its CLI would open it: a
    /// handoff reaches it typed while it waits. A raw argv window is handed nothing.
    async fn open_idle_delivering_composer_window<T: DaemonTransport>(
        &self,
        transport: &T,
    ) -> String {
        let window_id = self
            .registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command: echo_child_command_with_composer(0),
                    cwd: None,
                    name: Some("idle-delivering-window".to_string()),
                    vendor: None,
                    detection: None,
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                yagura_daemon::window::WindowProvenance {
                    delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                        output: yagura_daemon::inbox::DoorOutputs::all(
                            &yagura_daemon::inbox::OutputForm::Plain,
                        ),
                        max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
                        wake: yagura_daemon::profile_delivery::WakeMode::None,
                        wake_stream: yagura_protocol::DeliveryStream::Stderr,
                        wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
                        launch_prompt: None,
                        launch_context: None,
                    }),
                    ..yagura_daemon::window::WindowProvenance::default()
                },
            )
            .await
            .expect("registry.open_with_extra_env should spawn the child")
            .window_id;
        self.write_window_input(&window_id, CONTROL_READY_LINE.as_bytes())
            .await;
        self.await_status(transport, &window_id, "idle").await;
        window_id
    }

    /// Polls a window's PTY scrollback until an *accepted* handoff envelope
    /// appears: see [`find_accepted_handoff_envelope`] for what
    /// distinguishes that from a composer-draw-mode child's still-unsubmitted
    /// draft of the same envelope, which paints the identical
    /// `[yagura handoff] ` prefix and would otherwise make this wait
    /// vacuously true whether or not the daemon ever wrote the envelope.
    async fn await_accepted_handoff_envelope(&self, window_id: &str) {
        let entry = self.entry(window_id);
        let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
        loop {
            let tail = entry.session().captured_output();
            let text = String::from_utf8_lossy(&tail).into_owned();
            if find_accepted_handoff_envelope(&text).is_some() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for an accepted handoff envelope in window \
                     {window_id}'s output; tail so far: {text:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Shuts the daemon down over IPC and asserts that every window's child
    /// process really terminated, so no test leaves an orphan behind.
    async fn shutdown_and_assert_children_exited(self) {
        let entries: Vec<Arc<WindowEntry>> = self
            .registry
            .list()
            .into_iter()
            .filter_map(|info| self.registry.get(&info.window_id))
            .collect();
        assert!(
            !entries.is_empty(),
            "the harness must still hold at least one window at shutdown time"
        );

        let mut client = connect_with_retry(&self.endpoint, Duration::from_secs(5)).await;
        let frame = yagura_protocol::frame_encode(&yagura_protocol::encode_client_message(
            &ClientMessage::Shutdown,
        ));
        client
            .write_all(&frame)
            .await
            .expect("write the shutdown frame");

        let run_result = tokio::time::timeout(CONVERGENCE_TIMEOUT, self.server_task)
            .await
            .expect("server.run() must complete after shutdown")
            .expect("server task must not panic (JoinError)");
        assert!(
            run_result.is_ok(),
            "server.run() must return Ok after a clean shutdown, got {run_result:?}"
        );

        for entry in entries {
            let window_id = entry.window_id().to_string();
            let mut exited = entry.session().exit_notified();
            let observed = tokio::time::timeout(CONVERGENCE_TIMEOUT, async {
                // `exit_notified` may already hold `true`, so inspect before
                // awaiting a change.
                while !*exited.borrow_and_update() {
                    if exited.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            assert!(
                observed.is_ok(),
                "window {window_id}'s child process did not terminate on shutdown"
            );
            assert!(
                entry.session().exit_status().is_some(),
                "window {window_id}'s child process must report an exit status after shutdown"
            );
        }
    }
}

/// The real transport with every round trip held to [`ROUND_TRIP_LIMIT`]:
/// the wait loops below check their deadlines only between calls, so without
/// it a reply the transport cannot read would block the first call forever.
struct BoundedTransport {
    inner: EndpointTransport,
}

impl BoundedTransport {
    fn new(inner: EndpointTransport) -> Self {
        Self { inner }
    }
}

impl DaemonTransport for BoundedTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        self.inner.requester_identity()
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        reply_within(ROUND_TRIP_LIMIT, "a round trip", self.inner.round_trip(msg)).await
    }
}

/// Wraps a real transport and counts round trips, so a test can assert that
/// a locally rejected request never reached the wire. Delegation keeps the
/// real `EndpointTransport` in the path for everything that *is* sent.
struct CountingTransport<T: DaemonTransport> {
    inner: T,
    round_trips: AtomicUsize,
}

impl<T: DaemonTransport> CountingTransport<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            round_trips: AtomicUsize::new(0),
        }
    }

    fn round_trips(&self) -> usize {
        self.round_trips.load(Ordering::SeqCst)
    }
}

impl<T: DaemonTransport> DaemonTransport for CountingTransport<T> {
    // Delegated rather than left to the default: a wrapper that answered for
    // itself would report an external client however the wrapped transport's
    // claim was settled.
    fn requester_identity(&self) -> RequesterIdentity {
        self.inner.requester_identity()
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.round_trips.fetch_add(1, Ordering::SeqCst);
        self.inner.round_trip(msg).await
    }
}

fn window_ids(list: &serde_json::Value) -> Vec<String> {
    list.get("windows")
        .and_then(|w| w.as_array())
        .expect("list_windows must return a `windows` array")
        .iter()
        .map(|window| {
            window
                .get("window_id")
                .and_then(|id| id.as_str())
                .expect("every listed window must carry a string window_id")
                .to_string()
        })
        .collect()
}

// =====================================================================
// Raw IPC client: `status_event` has no MCP tool, because a status hook
// talks to the daemon directly. The token path is therefore
// driven over the same real endpoint, and *observed* through the MCP tools.
// =====================================================================

/// What the daemon did with a `status_event`. An accepted event produces no
/// response at all, so acceptance is observed by a trailing `Ping` whose
/// `Pong` arrives first.
#[derive(Debug, PartialEq, Eq)]
enum EventOutcome {
    Accepted,
    Rejected(String),
}

async fn probe_status_event(
    client: &mut Connection,
    window_id: &str,
    token: &str,
    status: WindowStatus,
) -> EventOutcome {
    send_client_message(
        client,
        &ClientMessage::StatusEvent {
            source: "hook-e2e".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: token.to_string(),
        },
    )
    .await;
    send_client_message(client, &ClientMessage::Ping).await;
    let reply = read_daemon_frame(client, CONVERGENCE_TIMEOUT)
        .await
        .expect_message("status_event probe reply");
    match reply {
        DaemonMessage::Pong => EventOutcome::Accepted,
        DaemonMessage::Error { code, .. } => {
            let pong = read_daemon_frame(client, CONVERGENCE_TIMEOUT)
                .await
                .expect_message("Pong after a rejected status_event");
            assert!(
                matches!(pong, DaemonMessage::Pong),
                "the connection must stay usable after a rejected status_event, got {pong:?}"
            );
            EventOutcome::Rejected(code)
        }
        other => panic!("unexpected frame while probing status_event: {other:?}"),
    }
}

// =====================================================================
// Happy path: the four tools over the real transport
// =====================================================================

#[tokio::test]
async fn open_window_adds_a_second_window_visible_to_list_windows_in_insertion_order() {
    let harness = DaemonHarness::start("open-list").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let opened = open_window(
        &transport,
        OpenWindowArgs {
            command: echo_child_command(),
            cwd: None,
            name: Some("second".to_string()),
            vendor: Some("test-vendor".to_string()),
            detection: None,
            profile: None,
            model: None,
            effort: None,
        },
    )
    .await
    .expect("open_window must succeed against a live daemon");
    let second_window_id = opened
        .get("window_id")
        .and_then(|id| id.as_str())
        .expect("open_window must return a string window_id")
        .to_string();
    assert_ne!(
        second_window_id, harness.initial_window_id,
        "the second window must get its own id"
    );

    let listed = list_windows(&transport)
        .await
        .expect("list_windows must succeed against a live daemon");
    assert_eq!(
        window_ids(&listed),
        vec![harness.initial_window_id.clone(), second_window_id],
        "list_windows must report both windows in insertion order"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn open_window_detection_argument_selects_a_per_window_definition() {
    // The daemon's default `detection_name` is valid but unresolvable, so a
    // window opened without a `detection` argument cannot reach `idle`
    // through it. That separates "the MCP argument selected the per-window
    // definition" from "the daemon's own default happened to work", which a
    // single window reaching `idle` cannot tell apart.
    let dir = unique_temp_dir(FIXTURE_PREFIX, "detection-arg");
    let config = DaemonConfig {
        detection_name: "no-such-detection".to_string(),
        ..detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("detection-arg", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let with_detection = harness
        .open_via_tool(
            &transport,
            echo_child_command(),
            "with-detection",
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    // Written straight into the PTY: `send_message` on an `Unknown` window is
    // refused before any policy is even consulted, so it cannot be used to
    // stimulate the very transition this test observes.
    harness
        .write_window_input(&with_detection, CONTROL_READY_LINE.as_bytes())
        .await;
    harness
        .await_status(&transport, &with_detection, "idle")
        .await;

    // The omission case is probed only after the explicit-detection window's
    // transition was observed, so a slow-but-eventually-idle implementation
    // cannot be mistaken for one that never forwards `detection`.
    let without_detection = harness
        .open_via_tool(&transport, echo_child_command(), "without-detection", None)
        .await;
    harness
        .write_window_input(&without_detection, CONTROL_READY_LINE.as_bytes())
        .await;
    // The settle window is far longer than the definition's confirm delay, so
    // a silent fallback to the other window's definition would show up here.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        harness
            .status_via_tool(&transport, &without_detection)
            .await,
        "unknown",
        "a window opened without `detection` must keep resolving the \
         daemon's unresolvable default, not the per-window definition \
         another window asked for"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn get_window_status_answers_for_a_live_window_id_and_always_reports_since_ms() {
    let harness = DaemonHarness::start("get-status").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let status = get_window_status(
        &transport,
        GetWindowStatusArgs {
            window_id: Some(harness.initial_window_id.clone()),
        },
    )
    .await
    .expect("get_window_status must succeed for an existing window");

    assert_eq!(
        status.get("window_id").and_then(|id| id.as_str()),
        Some(harness.initial_window_id.as_str()),
        "the response must echo back the queried window_id"
    );
    assert!(
        status
            .get("status")
            .and_then(|s| s.as_str())
            .is_some_and(|s| !s.is_empty()),
        "status must be one of the snake_case WindowStatus strings, got {:?}",
        status.get("status")
    );
    // `since_ms` is never null: window creation counts as the first status
    // change.
    assert!(
        status.get("since_ms").and_then(|v| v.as_u64()).is_some(),
        "since_ms must always be present as a number, got {:?}",
        status.get("since_ms")
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn send_message_immediate_delivers_the_text_to_the_child_process() {
    // send_message goes through the same status/approval gate as
    // interrupt_window / close_window, so the target must be driven to
    // `idle` and a policy must approve it first (neither of which the
    // initial window, alone, provides).
    let dir = unique_temp_dir(FIXTURE_PREFIX, "send-immediate");
    // The daemon writes its own text only into a window whose definition
    // declares a composer, so the target's definition carries one.
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..composer_detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("send-immediate", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness.open_idle_window(&transport).await;

    let sent = send_message(
        &transport,
        SendMessageArgs {
            window_id: Some(window_id.clone()),
            text: Some(SEND_MESSAGE_MARKER.to_string()),
            mode: Some("immediate".to_string()),
        },
    )
    .await
    .expect("send_message must succeed for a live window");
    assert_eq!(
        sent.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "a successful send_message must report ok:true (a text_sent reply)"
    );

    // The success flag alone would pass against a daemon that swallowed the
    // text, so require the bytes to actually surface in the child's output.
    harness
        .await_child_output(&window_id, SEND_MESSAGE_MARKER.as_bytes())
        .await;

    harness.shutdown_and_assert_children_exited().await;
}

/// The target window's definition resolves and reaches `idle`, but
/// carries no `[composer]` section at all, so the daemon declines to write
/// its own text into it. The only property under test is that the daemon
/// must not report success for a line it never wrote; the specific error
/// code this takes is `tools.rs`'s contract, not this file's.
#[tokio::test]
async fn send_message_with_a_terminated_line_to_an_unobservable_window_does_not_report_success() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "send-not-written");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("send-not-written", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness.open_idle_window(&transport).await;

    let result = send_message(
        &transport,
        SendMessageArgs {
            window_id: Some(window_id.clone()),
            text: Some("mcp-e2e-not-written;\n".to_string()),
            mode: Some("immediate".to_string()),
        },
    )
    .await;
    assert!(
        result.is_err(),
        "a send_message the daemon never wrote to (its target window's definition has no \
         [composer] section, so it can never be observed) must not report success, got \
         {result:?}"
    );

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// Error paths
// =====================================================================

#[tokio::test]
async fn get_window_status_for_an_unknown_window_id_is_window_not_found() {
    let harness = DaemonHarness::start("unknown-window").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let error = get_window_status(
        &transport,
        GetWindowStatusArgs {
            window_id: Some("w-does-not-exist".to_string()),
        },
    )
    .await
    .expect_err("an unknown window_id must be an error, not a success");
    assert!(
        matches!(error, ToolError::WindowNotFound(_)),
        "expected WindowNotFound, got {error:?}"
    );
    assert_eq!(error.code(), "window_not_found");

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn send_message_with_an_unknown_mode_is_unsupported_and_never_reaches_the_daemon() {
    let harness = DaemonHarness::start("unsupported-mode").await;
    let counting = CountingTransport::new(harness.transport());
    harness.wait_until_ready(&counting).await;
    let round_trips_before = counting.round_trips();

    let error = send_message(
        &counting,
        SendMessageArgs {
            window_id: Some(harness.initial_window_id.clone()),
            text: Some(SEND_MESSAGE_MARKER.to_string()),
            mode: Some("queue_until_input".to_string()),
        },
    )
    .await
    .expect_err("a non-immediate mode must be rejected");
    assert!(
        matches!(error, ToolError::Unsupported(_)),
        "expected Unsupported, got {error:?}"
    );
    assert_eq!(error.code(), "unsupported");
    // The rejection is local and unconditional: not a single byte may go to
    // the daemon, whatever the window's state.
    assert_eq!(
        counting.round_trips(),
        round_trips_before,
        "an unsupported mode must be rejected locally, without any daemon round trip"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn tool_calls_after_the_daemon_stops_are_reported_as_daemon_unavailable() {
    let harness = DaemonHarness::start("daemon-stopped").await;
    let endpoint = harness.endpoint.clone();
    let warm_up = harness.transport();
    harness.wait_until_ready(&warm_up).await;

    // This models "the daemon process is gone", but the harness runs the
    // daemon in-process: shutdown stops accepting without tearing down the
    // connection tasks already spawned, so an open socket would keep being
    // served by a task production would have killed with the process.
    drop(warm_up);

    // Stop the daemon through the same shutdown path every other test uses,
    // so this case leaves no orphaned child process either.
    harness.shutdown_and_assert_children_exited().await;

    // A fresh transport is what a real MCP bridge does: it connects lazily on
    // the first tool call, the moment an absent daemon must surface as
    // `daemon_unavailable`.
    let transport = BoundedTransport::new(EndpointTransport::new(endpoint, None));
    let error = list_windows(&transport)
        .await
        .expect_err("a tool call after shutdown must fail rather than hang or succeed");
    assert!(
        matches!(error, ToolError::DaemonUnavailable(_)),
        "expected DaemonUnavailable, got {error:?}"
    );
    assert_eq!(error.code(), "daemon_unavailable");
}

// =====================================================================
// Destructive tools are fail-closed without a playbook
// =====================================================================

#[tokio::test]
async fn interrupt_window_is_refused_as_approval_required_when_no_playbook_is_loaded() {
    // The window is driven to `idle` first, so the refusal can only come from
    // the approval gate: on an `Unknown` window the status gate would answer
    // first and hide the very thing under test.
    let dir = unique_temp_dir(FIXTURE_PREFIX, "interrupt-denied");
    let harness =
        DaemonHarness::start_with_config("interrupt-denied", detection_config(&dir)).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness.open_idle_window(&transport).await;

    let error = interrupt_window(
        &transport,
        InterruptWindowArgs {
            window_id: Some(window_id.clone()),
        },
    )
    .await
    .expect_err("an unapproved interrupt must be refused, not carried out");
    assert!(
        matches!(error, ToolError::ApprovalRequired(_)),
        "expected ApprovalRequired, got {error:?}"
    );
    assert_eq!(error.code(), "approval_required");

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn close_window_is_refused_as_approval_required_when_no_playbook_is_loaded() {
    let harness = DaemonHarness::start("close-denied").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let error = close_window(
        &transport,
        CloseWindowArgs {
            window_id: Some(harness.initial_window_id.clone()),
            force: None,
        },
    )
    .await
    .expect_err("an unapproved close must be refused, not carried out");
    assert!(
        matches!(error, ToolError::ApprovalRequired(_)),
        "expected ApprovalRequired, got {error:?}"
    );
    assert_eq!(error.code(), "approval_required");

    // Fail-closed means the refusal is not merely reported: the child must
    // still be running.
    assert!(
        harness
            .entry(&harness.initial_window_id)
            .session()
            .exit_status()
            .is_none(),
        "a refused close_window must leave the child process alive"
    );

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// Destructive tools once the playbook approves them
// =====================================================================

#[tokio::test]
async fn interrupt_window_delivers_the_configured_bytes_to_the_child_when_approved() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "interrupt-ok");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_bytes: INTERRUPT_MARKER.as_bytes().to_vec(),
        ..detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("interrupt-ok", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness.open_idle_window(&transport).await;

    let interrupted = interrupt_window(
        &transport,
        InterruptWindowArgs {
            window_id: Some(window_id.clone()),
        },
    )
    .await
    .expect("an approved interrupt on an idle window must succeed");
    assert_eq!(
        interrupted.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str()),
        "the response must echo back the interrupted window_id"
    );

    // The acknowledgement alone would pass against a daemon that dropped the
    // bytes, so require them to reach the child: `echo_child` writes back
    // whatever arrives on its PTY input.
    harness
        .await_child_output(&window_id, INTERRUPT_MARKER.as_bytes())
        .await;

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn close_window_terminates_the_windows_whole_process_tree_when_approved() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "close-ok");
    let tick = dir.join("tick.txt");
    let tick_path = tick.to_str().expect("tick path must be valid UTF-8");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..DaemonConfig::default()
    };
    let harness = DaemonHarness::start_with_config("close-ok", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness
        .open_via_tool(
            &transport,
            surviving_grandchild_command(tick_path),
            "grandchild",
            None,
        )
        .await;

    wait_until_file_grows(&tick, Duration::from_secs(20)).await;

    assert!(
        harness.entry(&window_id).session().exit_status().is_none(),
        "fixture precondition: the direct child must still be running when close_window is sent, \
         otherwise path A did the teardown and this test proves nothing about close_window"
    );

    let closed = close_window(
        &transport,
        CloseWindowArgs {
            window_id: Some(window_id.clone()),
            force: None,
        },
    )
    .await
    .expect("an approved close must succeed");
    assert_eq!(
        closed.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str()),
        "the response must echo back the closed window_id"
    );

    // Zero orphans is the contract: the detached grandchild outlives a plain
    // `terminate()` and is only reached through the whole-tree teardown (the
    // Job Object on Windows, the process group on Unix).
    assert_file_len_stable(&tick, Duration::from_secs(3), "after close_window").await;

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// wait_for_status
// =====================================================================

#[tokio::test]
async fn wait_for_status_reports_reached_once_the_window_reaches_the_target() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "wait-reached");
    let harness = DaemonHarness::start_with_config("wait-reached", detection_config(&dir)).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness
        .open_via_tool(&transport, echo_child_command(), "waited-on", None)
        .await;

    // Written straight into the PTY (send_message on an Unknown window is
    // refused before any policy is consulted, so it cannot stimulate this
    // transition), concurrently with the wait below.
    let stimulate = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        harness
            .write_window_input(&window_id, CONTROL_READY_LINE.as_bytes())
            .await;
    };
    let waiting = wait_for_status(
        &transport,
        WaitForStatusArgs {
            window_id: Some(window_id.clone()),
            target_status: Some("idle".to_string()),
            timeout_sec: Some(10),
        },
    );
    let (waited, ()) = tokio::join!(waiting, stimulate);
    let waited = waited.expect("a wait that reaches its target must be a success");

    assert_eq!(
        waited.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str())
    );
    assert_eq!(
        waited.get("outcome").and_then(|value| value.as_str()),
        Some("reached"),
        "the wait must report `reached`, got {waited:?}"
    );
    assert_eq!(
        waited.get("status").and_then(|value| value.as_str()),
        Some("idle"),
        "the reported status is the one observed when the wait ended"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn wait_for_status_reports_timeout_with_the_status_observed_at_reply_time() {
    // A detection definition is available (this file's default harness
    // always has one), but nothing ever writes the marker into this window,
    // so it never leaves `Unknown` -- the wait can only end in a timeout,
    // with a successful result carrying the *real* status, not the requested
    // target.
    let harness = DaemonHarness::start("wait-timeout").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;

    let waited = wait_for_status(
        &transport,
        WaitForStatusArgs {
            window_id: Some(harness.initial_window_id.clone()),
            target_status: Some("idle".to_string()),
            timeout_sec: Some(1),
        },
    )
    .await
    .expect("a timed-out wait must be a successful result, not an error");

    assert_eq!(
        waited.get("outcome").and_then(|value| value.as_str()),
        Some("timeout"),
        "the wait must report `timeout`, got {waited:?}"
    );
    assert_eq!(
        waited.get("status").and_then(|value| value.as_str()),
        Some("unknown"),
        "the reported status must be the one measured at reply time, not the target"
    );

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// read_output
// =====================================================================

#[tokio::test]
async fn read_output_returns_the_childs_trailing_lines_with_escape_sequences_stripped() {
    let harness = DaemonHarness::start("read-output").await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness.initial_window_id.clone();

    // Written straight into the PTY: this test exercises read_output, not
    // send_message, and the initial window stays Unknown here (nothing drives
    // it to idle), which a gated send_message could not write to at all.
    for index in 1..=3 {
        harness
            .write_window_input(&window_id, format!("RO-LINE-{index}\r\n").as_bytes())
            .await;
    }
    harness.await_child_output(&window_id, b"RO-LINE-3").await;

    let output = read_output(
        &transport,
        ReadOutputArgs {
            window_id: Some(window_id.clone()),
            lines: Some(20),
        },
    )
    .await
    .expect("read_output must succeed for a live window");

    assert_eq!(
        output.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str())
    );
    let text = output
        .get("text")
        .and_then(|value| value.as_str())
        .expect("read_output must return a string text");
    assert!(
        text.contains("RO-LINE-3"),
        "the trailing lines of the real PTY output must be present, got {text:?}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "escape sequences must be stripped before the text leaves the daemon, got {text:?}"
    );
    assert!(
        output
            .get("truncated")
            .and_then(|value| value.as_bool())
            .is_some(),
        "truncated must be reported as a boolean, got {:?}",
        output.get("truncated")
    );

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// StatusEvent authentication
// =====================================================================

/// Config for the token tests: a known-valid token plus the detection
/// definition, so a window exists whose status an accepted event can move.
fn token_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        status_token_override: Some(VALID_TOKEN.into()),
        ..detection_config(dir)
    }
}

#[tokio::test]
async fn a_status_event_carrying_the_daemons_token_is_accepted_and_moves_the_reported_status() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "token-accepted");
    let harness = DaemonHarness::start_with_config("token-accepted", token_config(&dir)).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness
        .open_via_tool(&transport, echo_child_command(), "event-target", None)
        .await;
    assert_eq!(
        harness.status_via_tool(&transport, &window_id).await,
        "unknown",
        "fixture precondition: without the marker the window is still Unknown"
    );

    let mut client = connect_with_retry(&harness.endpoint, Duration::from_secs(5)).await;
    let outcome =
        probe_status_event(&mut client, &window_id, VALID_TOKEN, WindowStatus::Running).await;
    assert_eq!(
        outcome,
        EventOutcome::Accepted,
        "an event carrying the daemon's own token must be accepted"
    );

    harness
        .await_status(&transport, &window_id, "running")
        .await;

    drop(client);
    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn a_status_event_without_the_daemons_token_is_auth_failed_and_changes_nothing() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "token-refused");
    let harness = DaemonHarness::start_with_config("token-refused", token_config(&dir)).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness
        .open_via_tool(&transport, echo_child_command(), "event-target", None)
        .await;

    let mut client = connect_with_retry(&harness.endpoint, Duration::from_secs(5)).await;
    for token in ["", WRONG_TOKEN] {
        assert_eq!(
            probe_status_event(&mut client, &window_id, token, WindowStatus::Running).await,
            EventOutcome::Rejected("auth_failed".to_string()),
            "an event with token {token:?} must be refused"
        );
    }

    // Give a (hypothetical) unauthenticated write time to land before
    // asserting that nothing moved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        harness.status_via_tool(&transport, &window_id).await,
        "unknown",
        "a refused event must never reach the detector"
    );

    drop(client);
    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// Requester identity and the human decision a destructive call can wait on
// =====================================================================

/// How many identify attempts carrying a credential that does not match a
/// connection survives. The daemon drops the connection on that attempt,
/// which is how these tests take a transport's connection away from it
/// without stopping the daemon.
const IDENTIFY_FAILURE_BUDGET: usize = 3;

/// Config for the tests below: the detection definition (so a window can be
/// driven out of `Unknown`), the token every window adopts (so a transport
/// can prove an identity at all) and no playbook, so a destructive call is
/// allowed only by the caller's own identity or by a human.
fn approval_config(dir: &Path, approval_timeout_secs: u64) -> DaemonConfig {
    DaemonConfig {
        approval_timeout_secs,
        ..token_config(dir)
    }
}

/// Registers a raw connection as an approver and returns the requests that
/// were already awaiting a decision.
async fn register_approver(client: &mut Connection) -> Vec<ApprovalRequest> {
    send_client_message(client, &ClientMessage::RegisterApprover).await;
    match read_daemon_frame(client, CONVERGENCE_TIMEOUT)
        .await
        .expect_message("the reply to register_approver")
    {
        DaemonMessage::ApproverRegistered { pending } => pending,
        other => panic!("expected approver_registered, got {other:?}"),
    }
}

/// Waits for the next request put to the approver, answers it with
/// `approved`, and returns the request as it arrived.
async fn settle_next_request(client: &mut Connection, approved: bool) -> ApprovalRequest {
    let request = match read_daemon_frame(client, CONVERGENCE_TIMEOUT)
        .await
        .expect_message("a request put to the approver")
    {
        DaemonMessage::ApprovalRequested { request } => request,
        other => panic!("expected approval_requested, got {other:?}"),
    };
    send_client_message(
        client,
        &ClientMessage::ApprovalDecision {
            request_id: request.request_id.clone(),
            approved,
        },
    )
    .await;
    match read_daemon_frame(client, CONVERGENCE_TIMEOUT)
        .await
        .expect_message("the reply to approval_decision")
    {
        DaemonMessage::ApprovalResolved { request_id } => {
            assert_eq!(
                request_id, request.request_id,
                "the daemon must settle the request that was decided"
            );
        }
        other => panic!("expected approval_resolved, got {other:?}"),
    }
    request
}

/// Waits until a window's child process has really gone: an acknowledgement
/// alone would also be sent by a daemon that closed nothing.
async fn wait_until_child_exited(entry: &Arc<WindowEntry>) {
    let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
    while entry.session().exit_status().is_none() {
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "window {}'s child process is still running",
                entry.window_id()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn close_window_on_the_transports_own_window_needs_neither_a_playbook_nor_an_approver() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "own-close");
    let harness = DaemonHarness::start_with_config("own-close", approval_config(&dir, 30)).await;
    let opener = harness.transport();
    harness.wait_until_ready(&opener).await;
    let window_id = harness.open_idle_window(&opener).await;
    let entry = harness.entry(&window_id);

    let own = harness.transport_as(&window_id, VALID_TOKEN);
    let closed = close_window(
        &own,
        CloseWindowArgs {
            window_id: Some(window_id.clone()),
            force: None,
        },
    )
    .await
    .expect("a window's own connection must be allowed to close that window");

    assert_eq!(
        closed.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str()),
        "the response must echo back the closed window_id"
    );
    assert_eq!(
        closed.get("requester").and_then(|value| value.as_str()),
        Some("window"),
        "the daemon accepted this connection's claim, so the caller must be \
         told it acted as a window, got {closed}"
    );
    assert!(
        closed.get("identity_warning").is_none(),
        "an accepted claim must carry no warning, got {closed}"
    );

    // The acknowledgement alone would also come from a daemon that closed
    // nothing, so require the child to be gone.
    wait_until_child_exited(&entry).await;

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn close_window_without_an_identity_is_refused_while_nobody_can_approve_it() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "external-close");
    let harness =
        DaemonHarness::start_with_config("external-close", approval_config(&dir, 30)).await;
    let external = harness.transport();
    harness.wait_until_ready(&external).await;
    let window_id = harness.open_idle_window(&external).await;

    let error = close_window(
        &external,
        CloseWindowArgs {
            window_id: Some(window_id.clone()),
            force: None,
        },
    )
    .await
    .expect_err("a close nobody can approve must be refused, not carried out");

    assert_eq!(error.code(), "approval_required");
    assert!(
        error.to_string().contains("no approver is connected"),
        "the caller must be told why the operation cannot be decided, got {error}"
    );
    assert!(
        harness.entry(&window_id).session().exit_status().is_none(),
        "a refused close must leave the child process alive"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn a_close_held_for_a_human_is_carried_out_once_the_approver_approves_it() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "approve-close");
    let harness =
        DaemonHarness::start_with_config("approve-close", approval_config(&dir, 30)).await;
    let external = harness.transport();
    harness.wait_until_ready(&external).await;
    let window_id = harness.open_idle_window(&external).await;
    let entry = harness.entry(&window_id);

    let mut approver = connect_with_retry(&harness.endpoint, Duration::from_secs(5)).await;
    let already_pending = register_approver(&mut approver).await;
    assert!(
        already_pending.is_empty(),
        "nothing was requested yet, got {already_pending:?}"
    );

    // The call blocks until the decision arrives, so the approver has to be
    // driven from the same task rather than after the call returns.
    let (result, request) = tokio::join!(
        close_window(
            &external,
            CloseWindowArgs {
                window_id: Some(window_id.clone()),
                force: None,
            },
        ),
        settle_next_request(&mut approver, true),
    );

    let closed = result.expect("an approved close must be carried out");
    assert_eq!(request.tool, "close_window");
    assert_eq!(
        request.requester.kind,
        ApprovalRequesterKind::External,
        "a transport with no claim must reach the approver as an external client"
    );
    assert_eq!(
        request
            .target
            .as_ref()
            .map(|target| target.window_id.as_str()),
        Some(window_id.as_str())
    );
    assert_eq!(
        closed.get("requester").and_then(|value| value.as_str()),
        Some("external"),
        "the caller must be told it acted as an external client, got {closed}"
    );
    wait_until_child_exited(&entry).await;

    drop(approver);
    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn a_denied_close_reaches_the_approver_named_as_the_requesting_window_and_is_refused() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "deny-close");
    let harness = DaemonHarness::start_with_config("deny-close", approval_config(&dir, 30)).await;
    let opener = harness.transport();
    harness.wait_until_ready(&opener).await;
    // Two windows: the one the transport claims, and the one it acts on. A
    // claim only lifts the gate for the caller's *own* window.
    let caller_window = harness.open_idle_window(&opener).await;
    let target_window = harness
        .open_idle_window_named(&opener, "idle-target-window")
        .await;

    let caller = harness.transport_as(&caller_window, VALID_TOKEN);
    let mut approver = connect_with_retry(&harness.endpoint, Duration::from_secs(5)).await;
    register_approver(&mut approver).await;

    let (result, request) = tokio::join!(
        close_window(
            &caller,
            CloseWindowArgs {
                window_id: Some(target_window.clone()),
                force: None,
            },
        ),
        settle_next_request(&mut approver, false),
    );

    let error = result.expect_err("a denied close must be refused, not carried out");
    assert_eq!(error.code(), "approval_denied");
    // The claim is only observable here because it travelled to the daemon:
    // a transport that kept it to itself would show up as external.
    assert_eq!(
        request.requester.kind,
        ApprovalRequesterKind::Window,
        "the claim must reach the daemon, got {request:?}"
    );
    assert_eq!(
        request.requester.window_id.as_deref(),
        Some(caller_window.as_str()),
        "the approver must be told which window asked, got {request:?}"
    );
    assert_eq!(
        request
            .target
            .as_ref()
            .map(|target| target.window_id.as_str()),
        Some(target_window.as_str())
    );
    assert!(
        harness
            .entry(&target_window)
            .session()
            .exit_status()
            .is_none(),
        "a denied close must leave the child process alive"
    );

    drop(approver);
    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn a_transport_that_lost_its_connection_claims_its_window_again_on_the_next_call() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "reclaim");
    let config = DaemonConfig {
        interrupt_bytes: INTERRUPT_MARKER.as_bytes().to_vec(),
        ..approval_config(&dir, 30)
    };
    let harness = DaemonHarness::start_with_config("reclaim", config).await;
    let opener = harness.transport();
    harness.wait_until_ready(&opener).await;
    let window_id = harness.open_idle_window(&opener).await;

    let own = harness.transport_as(&window_id, VALID_TOKEN);
    interrupt_window(
        &own,
        InterruptWindowArgs {
            window_id: Some(window_id.clone()),
        },
    )
    .await
    .expect("a window's own connection must be allowed to interrupt that window");
    harness
        .await_child_output(&window_id, INTERRUPT_MARKER.as_bytes())
        .await;

    // Spend this connection's identify budget with a token that does not
    // match: the daemon drops the connection, so the transport can only get
    // back in by opening a new one.
    for attempt in 1..=IDENTIFY_FAILURE_BUDGET {
        match own
            .round_trip(ClientMessage::Identify {
                window_id: window_id.clone(),
                token: WRONG_TOKEN.to_string(),
            })
            .await
        {
            Ok(DaemonMessage::Error { code, .. }) => assert_eq!(
                code, "auth_failed",
                "attempt {attempt} must be refused as an authentication failure"
            ),
            other => panic!("attempt {attempt}: expected auth_failed, got {other:?}"),
        }
    }
    // The disconnect travels the other way, so let it land before the next
    // request is written into a connection that no longer has a peer.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stimulated through the registry again so that the status gate cannot be
    // what answers the second interrupt.
    harness
        .write_window_input(&window_id, CONTROL_READY_LINE.as_bytes())
        .await;
    harness.await_status(&opener, &window_id, "idle").await;

    let mut attempts = 0;
    let interrupted = loop {
        attempts += 1;
        match interrupt_window(
            &own,
            InterruptWindowArgs {
                window_id: Some(window_id.clone()),
            },
        )
        .await
        {
            Ok(value) => break value,
            // Noticing that a connection is gone can cost one round trip.
            // Every other refusal is the failure under test: a connection
            // that did not claim its window again is an external client, and
            // an external interrupt needs an approver nobody registered.
            Err(ToolError::DaemonUnavailable(_)) if attempts < 3 => continue,
            Err(err) => panic!(
                "attempt {attempts}: the reconnected transport must still act as its own \
                 window, got {err:?}"
            ),
        }
    };

    assert_eq!(
        interrupted
            .get("requester")
            .and_then(|value| value.as_str()),
        Some("window"),
        "the caller must still be told it acted as a window, got {interrupted}"
    );

    harness.shutdown_and_assert_children_exited().await;
}

#[tokio::test]
async fn a_transport_whose_claim_the_daemon_refuses_keeps_working_as_an_external_client() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "refused-claim");
    // `approval_config`'s settings over a definition that declares a
    // composer: the daemon writes its own text into no other window.
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        approval_timeout_secs: 30,
        status_token_override: Some(VALID_TOKEN.into()),
        ..composer_detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("refused-claim", config).await;
    let opener = harness.transport();
    harness.wait_until_ready(&opener).await;
    let window_id = harness.open_idle_window(&opener).await;

    let refused = harness.transport_as(&window_id, WRONG_TOKEN);
    // Twice over the same connection: the refusal is settled once and stays
    // settled, rather than being forgotten after it was reported.
    for call in 1..=2 {
        let sent = send_message(
            &refused,
            SendMessageArgs {
                window_id: Some(window_id.clone()),
                text: Some(SEND_MESSAGE_MARKER.to_string()),
                mode: None,
            },
        )
        .await
        .unwrap_or_else(|err| {
            panic!("call {call}: a refused claim must not fail the call itself, got {err:?}")
        });

        assert_eq!(
            sent.get("ok").and_then(|value| value.as_bool()),
            Some(true),
            "call {call}: the policy approves this tool, so it must succeed"
        );
        assert_eq!(
            sent.get("requester").and_then(|value| value.as_str()),
            Some("external"),
            "call {call}: a refused claim must not be reported as a window, got {sent}"
        );
        let warning = sent
            .get("identity_warning")
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("call {call}: the refusal must be reported, got {sent}"));
        assert!(
            !warning.is_empty(),
            "call {call}: the warning must say something, got {sent}"
        );
    }

    harness
        .await_child_output(&window_id, SEND_MESSAGE_MARKER.as_bytes())
        .await;

    harness.shutdown_and_assert_children_exited().await;
}

// =====================================================================
// send_handoff
// =====================================================================

#[tokio::test]
async fn send_handoff_writes_the_envelope_into_an_existing_window_when_approved() {
    let dir = unique_temp_dir(FIXTURE_PREFIX, "handoff-window");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..composer_detection_config(&dir)
    };
    let harness = DaemonHarness::start_with_config("handoff-window", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let window_id = harness
        .open_idle_delivering_composer_window(&transport)
        .await;

    let result = send_handoff(
        &transport,
        SendHandoffArgs {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            reference: Some("src/foo.rs".to_string()),
            note: Some("Please take a look.".to_string()),
            model: None,
            effort: None,
        },
    )
    .await
    .expect("an approved send_handoff must succeed");
    assert_eq!(
        result.get("window_id").and_then(|id| id.as_str()),
        Some(window_id.as_str()),
        "the response must echo back the target window_id"
    );
    assert_eq!(
        result.get("opened").and_then(|value| value.as_bool()),
        Some(false),
        "a window_id target must not report opening a new window"
    );
    assert_eq!(
        result.get("delivery").and_then(|value| value.as_str()),
        Some("taken"),
        "the reply must carry the delivery the daemon reported, got {result}"
    );

    harness.await_accepted_handoff_envelope(&window_id).await;
    let scrollback = harness.entry(&window_id).session().captured_output();
    let envelope = extract_accepted_handoff_envelope_json(&scrollback);
    assert_eq!(envelope["reference"], "src/foo.rs");
    assert_eq!(envelope["note"], "Please take a look.");

    harness.shutdown_and_assert_children_exited().await;
}

/// The `hook_child` fixture, next to the other fixtures of this test binary.
fn hook_child_path() -> PathBuf {
    yagura_test_support::process::argv_probe_path()
        .with_file_name(format!("hook_child{}", std::env::consts::EXE_SUFFIX))
}

/// A `hook_child` profile handed a first request at launch as `--prompt <request>`,
/// logging the argv it was started with under `logs`.
fn launch_profile_source(display_name: &str, logs: &Path) -> String {
    let command = [
        hook_child_path().display().to_string(),
        COMPOSER_MARKER.to_string(),
        COMPOSER_SEPARATOR.to_string(),
        yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        "0".to_string(),
        "-".to_string(),
        "--log".to_string(),
        logs.display().to_string(),
    ]
    .iter()
    .map(|arg| format!("'{arg}'"))
    .collect::<Vec<_>>()
    .join(", ");
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = [{command}]\n{}",
        "\n[delivery]\nwake = \"none\"\nlaunch_prompt = [\"--prompt\", \"{prompt}\"]\n"
    )
}

/// The launch prompt the `hook_child` logging under `logs` was started with.
async fn launch_prompt_logged_in(logs: &Path) -> String {
    let deadline = tokio::time::Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let events = std::fs::read_to_string(logs.join("events.jsonl")).unwrap_or_default();
        if let Some(argv) = events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|line| line["event"] == "argv")
        {
            return argv["prompt"]
                .as_str()
                .unwrap_or_else(|| panic!("the window was started with no launch prompt: {argv}"))
                .to_string();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window never logged its argv under {}",
            logs.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn send_handoff_opens_a_new_window_from_a_profile_with_the_envelope_as_its_launch_prompt() {
    let detection_dir = unique_temp_dir(FIXTURE_PREFIX, "handoff-profile-detection");
    let profiles_dir = unique_temp_dir(FIXTURE_PREFIX, "handoff-profile-profiles");
    let logs = unique_temp_dir(FIXTURE_PREFIX, "handoff-profile-logs");
    let profile_id = "handoff-target";
    std::fs::write(
        profiles_dir.join(format!("{profile_id}.toml")),
        launch_profile_source("Handoff Target", &logs),
    )
    .expect("write the profile fixture");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        profiles_dir: Some(profiles_dir),
        ..composer_detection_config(&detection_dir)
    };
    let harness = DaemonHarness::start_with_config("handoff-profile", config).await;
    let transport = harness.transport();
    harness.wait_until_ready(&transport).await;
    let before = window_ids(
        &list_windows(&transport)
            .await
            .expect("list_windows must succeed"),
    );

    let result = send_handoff(
        &transport,
        SendHandoffArgs {
            window_id: None,
            profile: Some(profile_id.to_string()),
            cwd: None,
            name: None,
            reference: Some("src/foo.rs".to_string()),
            note: Some("Please take a look.".to_string()),
            model: None,
            effort: None,
        },
    )
    .await
    .expect("an approved profile handoff must succeed");
    assert_eq!(
        result.get("opened").and_then(|value| value.as_bool()),
        Some(true),
        "a profile target must report that a new window was opened"
    );
    let new_window_id = result
        .get("window_id")
        .and_then(|value| value.as_str())
        .expect("send_handoff must return the new window's window_id")
        .to_string();
    assert!(
        !before.contains(&new_window_id),
        "the returned window_id must not be one that already existed before the handoff"
    );

    assert_eq!(
        result.get("delivery").and_then(|value| value.as_str()),
        Some("taken"),
        "the reply must report the envelope taken once the window started, got {result}"
    );

    let prompt = launch_prompt_logged_in(&logs).await;
    let envelope: serde_json::Value = serde_json::from_str(
        prompt
            .strip_prefix("[yagura handoff] ")
            .unwrap_or_else(|| panic!("the launch prompt is not a handoff envelope: {prompt:?}")),
    )
    .expect("the envelope's body is JSON");
    assert_eq!(envelope["reference"], "src/foo.rs");
    assert_eq!(envelope["note"], "Please take a look.");

    harness.shutdown_and_assert_children_exited().await;
}
