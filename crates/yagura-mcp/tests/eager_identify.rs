#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The scripted server built here is a fixture for the transport under test,
// so it is built from the raw platform listener rather than from
// `IpcTransport` (same rationale as tests/transport_frames.rs).
#![allow(clippy::disallowed_types)]

//! Tests for the bridge's startup-time identity claim:
//! `EndpointTransport::eager_identify` connects once and asserts `identify`
//! when given an identity, and the caller's own first `round_trip` reuses
//! that same connection rather than opening a second one or re-identifying.
//!
//! Uses a scripted server over a real IPC endpoint (a named pipe on Windows,
//! a Unix domain socket elsewhere), the same fixture shape as
//! `tests/transport_frames.rs`, so the wiring under test is the real
//! `EndpointTransport`, not a mock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use yagura_mcp::transport::{DaemonTransport, EndpointTransport, WindowIdentity};
use yagura_protocol::{
    ClientMessage, DaemonMessage, PROTOCOL_REVISION, decode_client_message, encode_daemon_message,
    frame_encode,
};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

/// Endpoint-name prefix for this file's scripted server, kept distinct from
/// every other test file's so their endpoints never collide.
const PIPE_PREFIX: &str = "yagura-mcp-eager-identify";

/// Settling delay before reading a counter for a "must NOT have grown"
/// assertion, so the connection under test has had time to act if it were
/// going to.
const SETTLE: Duration = Duration::from_millis(400);

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A scripted server that answers `GetProtocolRevision` with the current
/// revision, `Identify` with `Identified`, and everything else with `Pong`,
/// counting accepted connections and identify requests received.
struct ScriptedServer {
    endpoint: String,
    accepted: Arc<AtomicUsize>,
    identify_count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    fn identify_count(&self) -> usize {
        self.identify_count.load(Ordering::SeqCst)
    }

    /// Waits until the accept counter reaches `target`, panicking on timeout
    /// so a missing connection fails loudly instead of hanging.
    async fn await_accepted(&self, target: usize, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.accepted() < target {
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {target} accepted connections (saw {})",
                    self.accepted()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(windows)]
type ScriptedConnection = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(not(windows))]
type ScriptedConnection = tokio::net::UnixStream;

fn spawn_scripted_server(tag: &str) -> ScriptedServer {
    spawn_scripted_server_at(unique_endpoint_name(PIPE_PREFIX, tag))
}

/// `cfg(windows)`: a named pipe instance serves exactly one connection, so
/// the accept loop hand-manages instance lifetime, mirroring
/// `tests/transport_frames.rs::spawn_scripted_server`. Created synchronously,
/// before the task starts, so the endpoint exists the moment this function
/// returns.
#[cfg(windows)]
fn spawn_scripted_server_at(endpoint: String) -> ScriptedServer {
    use tokio::net::windows::named_pipe::ServerOptions;

    let accepted = Arc::new(AtomicUsize::new(0));
    let identify_count = Arc::new(AtomicUsize::new(0));

    let name = endpoint.clone();
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .expect("the first pipe instance must be creatable");
    let accepted_for_task = Arc::clone(&accepted);
    let identify_for_task = Arc::clone(&identify_count);
    let task = tokio::spawn(async move {
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

            let identify_count = Arc::clone(&identify_for_task);
            tokio::spawn(async move {
                serve_connection(conn, identify_count).await;
            });
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        identify_count,
        task,
    }
}

/// `cfg(not(windows))` counterpart: a single bound `UnixListener` keeps
/// accepting on its own, so there is no per-instance recreation step.
#[cfg(not(windows))]
fn spawn_scripted_server_at(endpoint: String) -> ScriptedServer {
    use tokio::net::UnixListener;

    let accepted = Arc::new(AtomicUsize::new(0));
    let identify_count = Arc::new(AtomicUsize::new(0));

    let listener =
        UnixListener::bind(&endpoint).expect("bind must succeed on a fresh unique endpoint");
    let accepted_for_task = Arc::clone(&accepted);
    let identify_for_task = Arc::clone(&identify_count);
    let task = tokio::spawn(async move {
        loop {
            let Ok((conn, _addr)) = listener.accept().await else {
                return;
            };
            accepted_for_task.fetch_add(1, Ordering::SeqCst);

            let identify_count = Arc::clone(&identify_for_task);
            tokio::spawn(async move {
                serve_connection(conn, identify_count).await;
            });
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        identify_count,
        task,
    }
}

/// Serves one connection for its whole lifetime: answers every request frame
/// until the peer disconnects, counting `identify` requests as they arrive.
async fn serve_connection(mut conn: ScriptedConnection, identify_count: Arc<AtomicUsize>) {
    use tokio::io::AsyncWriteExt;

    loop {
        let Some(body) = read_one_raw_frame(&mut conn).await else {
            return;
        };
        let reply = match decode_client_message(&body) {
            Ok(ClientMessage::GetProtocolRevision) => {
                encode_daemon_message(&DaemonMessage::ProtocolRevision {
                    revision: PROTOCOL_REVISION,
                })
            }
            Ok(ClientMessage::Identify { window_id, .. }) => {
                identify_count.fetch_add(1, Ordering::SeqCst);
                encode_daemon_message(&DaemonMessage::Identified { window_id })
            }
            _ => encode_daemon_message(&DaemonMessage::Pong),
        };
        if conn.write_all(&frame_encode(&reply)).await.is_err() {
            return;
        }
    }
}

/// Reads one complete frame body without interpreting or counting it.
async fn read_one_raw_frame(conn: &mut ScriptedConnection) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    use yagura_protocol::frame_decode;

    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match frame_decode(&buffer) {
            Ok(Some((body, _consumed))) => return Some(body.to_vec()),
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

fn sample_identity() -> WindowIdentity {
    WindowIdentity {
        window_id: "w-eager".to_string(),
        token: "0123456789abcdef0123456789abcdef".to_string(),
    }
}

// --- An identity claims the connection once, up front ----------------------

#[tokio::test]
async fn eager_identify_with_an_identity_connects_and_identifies_once_and_the_next_round_trip_reuses_it()
 {
    let server = spawn_scripted_server("with-identity");
    let transport = EndpointTransport::new(server.endpoint.clone(), Some(sample_identity()));

    reply_within(WAIT_TIMEOUT, "eager_identify", transport.eager_identify())
        .await
        .expect("eager_identify must succeed against a live daemon");

    server.await_accepted(1, WAIT_TIMEOUT).await;
    assert_eq!(
        server.identify_count(),
        1,
        "eager_identify must send exactly one identify request up front"
    );

    let reply = reply_within(
        WAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the caller's own first round trip must succeed");
    assert_eq!(reply, DaemonMessage::Pong);

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        1,
        "the caller's own round trip must reuse the connection eager_identify opened, not open \
         a second one"
    );
    assert_eq!(
        server.identify_count(),
        1,
        "the caller's own round trip must not send a second identify over the same connection"
    );
}

// --- No identity means nothing is sent at all -------------------------------

#[tokio::test]
async fn eager_identify_with_no_identity_never_connects() {
    let server = spawn_scripted_server("no-identity");
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    reply_within(WAIT_TIMEOUT, "eager_identify", transport.eager_identify())
        .await
        .expect("eager_identify with no identity must not fail");

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        0,
        "eager_identify must not open a connection at all when this transport carries no \
         identity"
    );
}

// --- An absent daemon fails eager_identify without disturbing the ----------
// --- transport's own ordinary lazy-connect path -----------------------------

#[tokio::test]
async fn eager_identify_fails_when_the_daemon_is_absent_but_a_later_round_trip_still_connects_lazily()
 {
    let endpoint = unique_endpoint_name(PIPE_PREFIX, "absent-daemon");
    let transport = EndpointTransport::new(endpoint.clone(), Some(sample_identity()));

    let outcome = reply_within(WAIT_TIMEOUT, "eager_identify", transport.eager_identify()).await;
    assert!(
        outcome.is_err(),
        "eager_identify must report an error when no daemon is listening, got {outcome:?}"
    );

    // The daemon "arrives" only now: eager_identify's failure must not have
    // left the transport unable to connect on its own, ordinary lazy path.
    let _server = spawn_scripted_server_at(endpoint);

    let reply = reply_within(
        WAIT_TIMEOUT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("a later round trip must still connect lazily, exactly as before this feature");
    assert_eq!(reply, DaemonMessage::Pong);
}
