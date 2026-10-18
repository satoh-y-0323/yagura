#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The gate lives inside the production `EndpointTransport::round_trip`,
// so only a real IPC endpoint can show whether a rejected
// message's bytes ever reached the wire; the scripted server is therefore
// built from the raw platform listener, mirroring `transport_frames.rs`.
#![allow(clippy::disallowed_types)]

//! Tests for the MCP-side protocol revision gate: the bridge asks each
//! connection's daemon for its wire revision and refuses to send a message
//! that daemon is too old to understand.
//!
//! `required_protocol_revision` (the pure function each write-side message
//! answers for itself) is tested once, in `yagura-protocol`; this file does
//! not re-derive that mapping. What is tested here is the *transport's own*
//! behavior: when it probes, what it latches, what it retries, and -- the
//! property no fixture-based double can show -- whether a rejected message's
//! frame is ever written to the wire at all. A hand-rolled `DaemonTransport`
//! double cannot exercise this: the probe, the latch and the gate all live
//! inside `EndpointTransport`'s own connection handling, so this file drives
//! the real transport against a scripted server over a real IPC endpoint,
//! the same shape `transport_frames.rs` uses for the transport's other
//! internal behavior. Only the server harness differs by platform.
//!
//! Several tests below reference `TransportError::Unsupported`, the variant
//! that carries a gate's rejection to the caller.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use yagura_mcp::tools::{OpenWindowArgs, SendHandoffArgs, ToolError, open_window, send_handoff};
use yagura_mcp::transport::{DaemonTransport, EndpointTransport, TransportError};
use yagura_protocol::{
    ClientMessage, DaemonMessage, decode_client_message, encode_daemon_message, frame_decode,
    frame_encode,
};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

/// How long a test waits for the scripted server to observe an expected
/// number of accepted connections before failing loudly instead of hanging.
const AWAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Endpoint-name prefix for this file's scripted server, kept distinct from
/// `transport_frames.rs`'s so the two files' endpoints never collide.
const PIPE_PREFIX: &str = "yagura-mcp-revision-gate";

// =====================================================================
// The scripted server
// =====================================================================

/// One scripted reply to a single request received on a connection.
// `CloseWithoutReply` names the absence of a reply, which is what this script
// step does; the lint reads the shared suffix as a redundant prefix.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy)]
enum Reply {
    /// Answers `get_protocol_revision` with a settled wire revision.
    Revision(u32),
    /// Answers with the refusal shape a daemon built before a given message
    /// type existed gives for it.
    UnknownType,
    /// Answers with a daemon error that is neither of the two terminal
    /// answers (a settled revision or `unknown_type`): must not be latched.
    OtherError,
    /// Answers with a harmless, generic success reply.
    Pong,
    /// Answers `open_window_from_profile` / `send_handoff` with a synthetic
    /// success reply.
    WindowOpened,
    /// Closes the connection without answering: the request's bytes arrived,
    /// but no reply was ever written.
    CloseWithoutReply,
}

/// The scripted replies for one connection, indexed by the request's arrival
/// order on that connection. A request past the end of the list repeats the
/// script's last entry.
type ConnectionScript = Vec<Reply>;

/// A scripted server (a named pipe on Windows, a Unix domain socket
/// elsewhere) that records every request it decodes, per accepted
/// connection, and replies to each one per a per-connection script. A
/// connection past the end of the script list repeats the last script.
struct ScriptedServer {
    endpoint: String,
    accepted: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<Vec<ClientMessage>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Waits until the accept counter reaches `target`, panicking on timeout
    /// so a missing connection fails loudly instead of hanging.
    async fn await_accepted(&self, target: usize, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.accepted() < target {
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {target} accepted connections (saw {})",
                    self.accepted()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Every request decoded on connection `index`, in arrival order.
    fn received(&self, index: usize) -> Vec<ClientMessage> {
        self.received
            .lock()
            .expect("received log mutex must not be poisoned")
            .get(index)
            .cloned()
            .unwrap_or_default()
    }

    /// [`Self::received`], reduced to each message's wire tag, so a test can
    /// assert an arrival sequence without constructing full messages.
    fn received_kinds(&self, index: usize) -> Vec<&'static str> {
        self.received(index)
            .iter()
            .map(client_message_kind)
            .collect()
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Names the variant of `msg` without dumping its payload, mirroring
/// `yagura_mcp::transport::message_kind`'s treatment of `DaemonMessage`. Only
/// the variants this file's scripts need are named; everything else is
/// `"other"`.
fn client_message_kind(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::GetProtocolRevision => "get_protocol_revision",
        ClientMessage::Ping => "ping",
        ClientMessage::OpenWindowFromProfile { .. } => "open_window_from_profile",
        ClientMessage::SendHandoff { .. } => "send_handoff",
        _ => "other",
    }
}

#[cfg(windows)]
type ScriptedConnection = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(not(windows))]
type ScriptedConnection = tokio::net::UnixStream;

/// `cfg(windows)`: a named pipe instance serves exactly one connection, so
/// the accept loop hand-manages instance lifetime; the Unix counterpart binds
/// one socket that keeps accepting. Both are adapted from
/// `transport_frames.rs`'s `spawn_scripted_server`.
#[cfg(windows)]
fn spawn_scripted_server(tag: &str, scripts: Vec<ConnectionScript>) -> ScriptedServer {
    use tokio::net::windows::named_pipe::ServerOptions;

    let endpoint = unique_endpoint_name(PIPE_PREFIX, tag);
    let accepted = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(Mutex::new(Vec::new()));
    let scripts = Arc::new(scripts);

    let name = endpoint.clone();
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .expect("the first pipe instance must be creatable");
    let accepted_for_task = Arc::clone(&accepted);
    let received_for_task = Arc::clone(&received);
    let task = tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            if server.connect().await.is_err() {
                return;
            }
            // Create the next instance *before* handling the accepted
            // connection, so the pipe name never disappears.
            let next = match ServerOptions::new().create(&name) {
                Ok(next) => next,
                Err(_) => return,
            };
            let conn = std::mem::replace(&mut server, next);
            accepted_for_task.fetch_add(1, Ordering::SeqCst);
            received_for_task
                .lock()
                .expect("received log mutex must not be poisoned")
                .push(Vec::new());

            let script = connection_script(&scripts, index);
            let received = Arc::clone(&received_for_task);
            tokio::spawn(async move {
                handle_connection(conn, index, script, received).await;
            });
            index += 1;
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        received,
        task,
    }
}

/// `cfg(not(windows))` counterpart; see the `cfg(windows)` overload's doc.
#[cfg(not(windows))]
fn spawn_scripted_server(tag: &str, scripts: Vec<ConnectionScript>) -> ScriptedServer {
    use tokio::net::UnixListener;

    let endpoint = unique_endpoint_name(PIPE_PREFIX, tag);
    let accepted = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(Mutex::new(Vec::new()));
    let scripts = Arc::new(scripts);

    let listener =
        UnixListener::bind(&endpoint).expect("bind must succeed on a fresh unique endpoint");
    let accepted_for_task = Arc::clone(&accepted);
    let received_for_task = Arc::clone(&received);
    let task = tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let Ok((conn, _addr)) = listener.accept().await else {
                return;
            };
            accepted_for_task.fetch_add(1, Ordering::SeqCst);
            received_for_task
                .lock()
                .expect("received log mutex must not be poisoned")
                .push(Vec::new());

            let script = connection_script(&scripts, index);
            let received = Arc::clone(&received_for_task);
            tokio::spawn(async move {
                handle_connection(conn, index, script, received).await;
            });
            index += 1;
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        received,
        task,
    }
}

/// The script for the `index`-th accepted connection: a connection past the
/// end of `scripts` repeats the last one, so a test need not script every
/// connection an unexpected extra reconnect might open.
fn connection_script(scripts: &[ConnectionScript], index: usize) -> ConnectionScript {
    scripts
        .get(index)
        .or_else(|| scripts.last())
        .cloned()
        .unwrap_or_default()
}

async fn handle_connection(
    mut conn: ScriptedConnection,
    conn_index: usize,
    script: ConnectionScript,
    received: Arc<Mutex<Vec<Vec<ClientMessage>>>>,
) {
    use tokio::io::AsyncWriteExt;

    let mut request_index = 0usize;
    loop {
        let Some(msg) = read_one_request(&mut conn).await else {
            return;
        };
        received
            .lock()
            .expect("received log mutex must not be poisoned")
            .get_mut(conn_index)
            .expect("this connection's slot must have been pushed on accept")
            .push(msg);

        let reply = script
            .get(request_index)
            .or_else(|| script.last())
            .copied()
            .unwrap_or(Reply::Pong);
        request_index += 1;

        let frame = match reply {
            Reply::Revision(revision) => Some(frame_encode(&encode_daemon_message(
                &DaemonMessage::ProtocolRevision { revision },
            ))),
            Reply::UnknownType => Some(frame_encode(&encode_daemon_message(
                &DaemonMessage::Error {
                    code: "unknown_type".to_string(),
                    message: "the daemon does not recognize this message type".to_string(),
                    rejected_argument: None,
                },
            ))),
            Reply::OtherError => Some(frame_encode(&encode_daemon_message(
                &DaemonMessage::Error {
                    code: "internal".to_string(),
                    message: "scripted non-terminal failure".to_string(),
                    rejected_argument: None,
                },
            ))),
            Reply::Pong => Some(frame_encode(&encode_daemon_message(&DaemonMessage::Pong))),
            Reply::WindowOpened => Some(frame_encode(&encode_daemon_message(
                &DaemonMessage::WindowOpened {
                    window_id: "scripted-window".to_string(),
                    detection_warning: None,
                    name: None,
                },
            ))),
            Reply::CloseWithoutReply => None,
        };

        let Some(frame) = frame else {
            return;
        };
        if conn.write_all(&frame).await.is_err() {
            return;
        }
    }
}

/// Reads one complete request frame and decodes it. Returns `None` when the
/// client closed the connection, sent an undecodable frame, or the stream
/// became unusable.
async fn read_one_request(conn: &mut ScriptedConnection) -> Option<ClientMessage> {
    use tokio::io::AsyncReadExt;

    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match frame_decode(&buffer) {
            Ok(Some((body, _consumed))) => {
                return decode_client_message(body).ok();
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        let read = conn.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// A launch-option-bearing `open_window_from_profile` message, the fixture
/// every gated-call test in this file sends.
fn gated_open_window_from_profile() -> ClientMessage {
    ClientMessage::OpenWindowFromProfile {
        profile: "claude".to_string(),
        cwd: None,
        name: None,
        model: Some("opus".to_string()),
        effort: None,
        on_name_taken: Default::default(),
    }
}

// =====================================================================
// Transport-level behavior
// =====================================================================

/// A settled `protocol_revision` answer is latched, so a second call
/// on the same connection does not re-probe.
#[tokio::test]
async fn a_confirmed_revision_is_latched_and_not_reprobed() {
    let server = spawn_scripted_server(
        "mg1",
        vec![vec![Reply::Revision(2), Reply::Pong, Reply::Pong]],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let first = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await;
    assert!(
        matches!(first, Ok(DaemonMessage::Pong)),
        "the first call must still succeed once the probe settles, got {first:?}"
    );
    let second = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await;
    assert!(
        matches!(second, Ok(DaemonMessage::Pong)),
        "the second call must succeed too, got {second:?}"
    );

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(0),
        vec!["get_protocol_revision", "ping", "ping"],
        "a confirmed revision must not be reprobed on the second round trip"
    );
    assert_eq!(
        server.accepted(),
        1,
        "the same connection must be reused across both calls"
    );
}

/// An `"unknown_type"` probe answer confirms baseline, and that
/// confirmation is latched exactly like an explicit revision answer.
#[tokio::test]
async fn unknown_type_confirms_baseline_and_is_latched() {
    let server = spawn_scripted_server(
        "mg2",
        vec![vec![Reply::UnknownType, Reply::Pong, Reply::Pong]],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("an unrelated request must still succeed once an unknown_type probe settles");
    reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the second call must succeed too");

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(0),
        vec!["get_protocol_revision", "ping", "ping"],
        "an unknown_type probe result must confirm baseline once and not be re-probed"
    );
    assert_eq!(server.accepted(), 1);
}

/// A non-terminal probe result (neither a settled revision nor
/// `"unknown_type"`) is not latched, so the next call retries the probe.
#[tokio::test]
async fn a_non_terminal_probe_result_is_not_latched_and_retries_on_the_next_call() {
    let server = spawn_scripted_server(
        "mg3",
        vec![vec![
            Reply::OtherError,
            Reply::Pong,
            Reply::Revision(2),
            Reply::Pong,
        ]],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("a non-terminal probe result must not fail the caller's own unrelated request");
    reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the second call must succeed too");

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(0),
        vec![
            "get_protocol_revision",
            "ping",
            "get_protocol_revision",
            "ping",
        ],
        "a non-terminal probe result must not be latched, so the next call retries the probe"
    );
    assert_eq!(
        server.accepted(),
        1,
        "the connection must survive a non-terminal probe result"
    );
}

/// A replaced connection does not inherit the previous connection's
/// confirmed revision -- the new connection must probe again from scratch.
#[tokio::test]
async fn a_replaced_connection_does_not_inherit_the_previous_connections_confirmed_revision() {
    let server = spawn_scripted_server(
        "mg4",
        vec![
            vec![Reply::Revision(2), Reply::Pong, Reply::CloseWithoutReply],
            vec![Reply::UnknownType, Reply::Pong],
        ],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let first = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the first call must succeed once the probe settles");
    assert_eq!(first, DaemonMessage::Pong);

    let second = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await;
    assert!(
        second.is_err(),
        "the scripted server closes the connection mid-exchange, so the second call must fail \
         and drop the connection, got {second:?}"
    );

    let third = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("a retried call on a fresh connection must succeed");
    assert_eq!(third, DaemonMessage::Pong);

    server.await_accepted(2, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(1),
        vec!["get_protocol_revision", "ping"],
        "the replacement connection must re-probe rather than inherit the previous \
         connection's confirmed revision"
    );
}

/// A baseline peer never sees the launch-option frame on the wire at
/// all -- the gate rejects before the request is written, not after a reply
/// comes back.
#[tokio::test]
async fn a_baseline_connection_never_writes_the_launch_option_frame_to_the_wire() {
    let server = spawn_scripted_server("mg5", vec![vec![Reply::UnknownType]]);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let result = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(gated_open_window_from_profile()),
    )
    .await;
    assert!(
        matches!(result, Err(TransportError::Unsupported(_))),
        "a baseline peer must refuse a launch-option message before it is written, got {result:?}"
    );

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(0),
        vec!["get_protocol_revision"],
        "only the probe may reach the wire; the launch-option frame itself must never be written"
    );
}

/// While a connection's revision is unconfirmed, a call that needs a
/// revision is refused, but the connection stays open and a call that needs
/// no revision still goes through.
#[tokio::test]
async fn an_unconfirmed_connection_refuses_a_gated_call_but_stays_open_for_an_ungated_one() {
    let server = spawn_scripted_server(
        "mg6",
        vec![vec![Reply::OtherError, Reply::OtherError, Reply::Pong]],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let gated = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(gated_open_window_from_profile()),
    )
    .await;
    assert!(
        matches!(gated, Err(TransportError::Unsupported(_))),
        "an unconfirmed connection must refuse a launch-option message, got {gated:?}"
    );

    let ungated = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await;
    assert!(
        matches!(ungated, Ok(DaemonMessage::Pong)),
        "an unconfirmed connection must still serve a request that needs no revision, \
         got {ungated:?}"
    );

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.accepted(),
        1,
        "the connection must stay open across the refused and the served call"
    );
    assert_eq!(
        server.received_kinds(0),
        vec!["get_protocol_revision", "get_protocol_revision", "ping"],
        "the gated call's own frame must never reach the wire, and the next call must still \
         be served"
    );
}

/// The probe and the gate run again on a second call even though
/// the connection was already established by an earlier one -- the check is
/// not confined to the "just connected" branch.
#[tokio::test]
async fn mg7a_an_already_established_connection_still_runs_the_probe_and_gate_when_unconfirmed() {
    let server = spawn_scripted_server(
        "mg7a",
        vec![vec![
            Reply::OtherError,
            Reply::Pong,
            Reply::Revision(2),
            Reply::WindowOpened,
        ]],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the first call establishes the connection without confirming a revision");

    let opened = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(gated_open_window_from_profile()),
    )
    .await;
    assert!(
        matches!(opened, Ok(DaemonMessage::WindowOpened { .. })),
        "once the probe confirms a sufficient revision, the gated call must go out, \
         got {opened:?}"
    );

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(0),
        vec![
            "get_protocol_revision",
            "ping",
            "get_protocol_revision",
            "open_window_from_profile",
        ],
        "the probe and the gate must run again on the second call even though the connection \
         was already open"
    );
}

/// When a probe fails before the caller's own request is sent, the
/// transport's existing reconnect-and-resend retries the whole attempt --
/// including the probe and the gate, not just the caller's request.
#[tokio::test]
async fn mg7b_a_retried_attempt_after_a_failed_probe_also_runs_the_gate() {
    let server = spawn_scripted_server(
        "mg7b",
        vec![
            vec![Reply::CloseWithoutReply],
            vec![Reply::Revision(2), Reply::WindowOpened],
        ],
    );
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let opened = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(gated_open_window_from_profile()),
    )
    .await;
    assert!(
        matches!(opened, Ok(DaemonMessage::WindowOpened { .. })),
        "a failed probe must not abandon the caller's own request: the retried attempt must \
         probe and gate again, got {opened:?}"
    );

    server.await_accepted(2, AWAIT_TIMEOUT).await;
    assert_eq!(
        server.received_kinds(1),
        vec!["get_protocol_revision", "open_window_from_profile"],
        "the retried attempt must re-probe and then let the now-permitted call through"
    );
}

/// A baseline peer still receives a request that carries no launch
/// option, byte-for-byte unchanged from what the caller built.
#[tokio::test]
async fn a_baseline_connection_still_sends_a_non_launch_option_call_unchanged() {
    let server = spawn_scripted_server("mg9", vec![vec![Reply::UnknownType, Reply::WindowOpened]]);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let sent = ClientMessage::OpenWindowFromProfile {
        profile: "claude".to_string(),
        cwd: Some("/work".to_string()),
        name: Some("main".to_string()),
        model: None,
        effort: None,
        on_name_taken: Default::default(),
    };

    let result = reply_within(
        AWAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(sent.clone()),
    )
    .await;
    assert!(
        matches!(result, Ok(DaemonMessage::WindowOpened { .. })),
        "a baseline peer must still serve a request that carries no launch option, got {result:?}"
    );

    server.await_accepted(1, AWAIT_TIMEOUT).await;
    let received = server.received(0);
    assert_eq!(
        received.len(),
        2,
        "expected exactly the probe and the caller's own request, got {received:?}"
    );
    assert_eq!(
        received[1], sent,
        "a baseline peer must receive the caller's request unchanged"
    );
}

// =====================================================================
// The tool layer's mapping of a gate rejection
// =====================================================================

/// The rejection reaching the caller names the remedy (restarting the
/// daemon) and its consequence (open windows close), for both tools that can
/// trigger the gate.
#[tokio::test]
async fn the_rejection_message_carries_the_restart_remedy_and_its_consequence() {
    let open_server = spawn_scripted_server("mg10-open", vec![vec![Reply::UnknownType]]);
    let open_transport = EndpointTransport::new(open_server.endpoint.clone(), None);
    let open_result = reply_within(
        AWAIT_TIMEOUT,
        "open_window",
        open_window(
            &open_transport,
            OpenWindowArgs {
                command: Vec::new(),
                cwd: None,
                name: None,
                vendor: None,
                detection: None,
                profile: Some("claude".to_string()),
                model: Some("opus".to_string()),
                effort: None,
            },
        ),
    )
    .await;
    let open_message = match open_result {
        Err(ToolError::Unsupported(message)) => message,
        other => panic!(
            "open_window with a launch option against a baseline peer must be unsupported, \
             got {other:?}"
        ),
    };

    let handoff_server = spawn_scripted_server("mg10-handoff", vec![vec![Reply::UnknownType]]);
    let handoff_transport = EndpointTransport::new(handoff_server.endpoint.clone(), None);
    let handoff_result = reply_within(
        AWAIT_TIMEOUT,
        "send_handoff",
        send_handoff(
            &handoff_transport,
            SendHandoffArgs {
                window_id: None,
                profile: Some("claude".to_string()),
                cwd: None,
                name: None,
                reference: Some("look at this".to_string()),
                note: Some("please continue".to_string()),
                model: Some("opus".to_string()),
                effort: None,
            },
        ),
    )
    .await;
    let handoff_message = match handoff_result {
        Err(ToolError::Unsupported(message)) => message,
        other => panic!(
            "send_handoff with a launch option against a baseline peer must be unsupported, \
             got {other:?}"
        ),
    };

    for message in [&open_message, &handoff_message] {
        let lower = message.to_lowercase();
        assert!(
            lower.contains("restart"),
            "the rejection must name the remedy (restarting the daemon), got {message:?}"
        );
        assert!(
            lower.contains("close") && lower.contains("window"),
            "the rejection must name the remedy's consequence (open windows close), \
             got {message:?}"
        );
    }
}

/// A gate rejection surfaces as "this feature is not
/// supported", not "the daemon is unreachable" and not an internal error --
/// the daemon was reached and answered the probe; only the launch-option
/// call itself was refused before being sent.
#[tokio::test]
async fn the_gate_rejection_maps_to_unsupported_not_unavailable_or_internal() {
    let server = spawn_scripted_server("mg11", vec![vec![Reply::UnknownType]]);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let result = reply_within(
        AWAIT_TIMEOUT,
        "open_window",
        open_window(
            &transport,
            OpenWindowArgs {
                command: Vec::new(),
                cwd: None,
                name: None,
                vendor: None,
                detection: None,
                profile: Some("claude".to_string()),
                model: Some("opus".to_string()),
                effort: None,
            },
        ),
    )
    .await;

    match result {
        Err(ToolError::Unsupported(_)) => {}
        other => panic!(
            "a gate rejection must surface as \"unsupported\", not \"daemon_unavailable\" or \
             \"internal\" (the daemon was reachable and answered the probe), got {other:?}"
        ),
    }
}
