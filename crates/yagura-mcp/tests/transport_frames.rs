#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The transport itself is what is under test here, so the scripted server
// is built from the raw platform listener rather than from `IpcTransport`.
#![allow(clippy::disallowed_types)]

//! Robustness tests for the daemon transport.
//!
//! Two layers: seam unit tests for the pure pieces the transport is built from
//! (`FrameReader`, `should_resend`), which are `cfg`-free and run everywhere;
//! and behavior tests driving the real `EndpointTransport` against a scripted
//! server over a real IPC endpoint, which alone can show the seams are wired
//! into the shipped path. Only the server harness differs by platform.
//!
//! On Windows the scripted server creates the *next* pipe instance before it
//! handles an accepted connection, so an instance is always listening: a
//! server accepting a single connection would let a resending implementation
//! pass the at-most-once test and would fail the frame-exact and cancel-safety
//! tests even for a correct one. A Unix domain socket has no per-instance
//! lifetime and keeps accepting on its own.

use yagura_mcp::transport::{FrameReader, should_resend};
use yagura_protocol::{FrameError, MAX_FRAME_BODY_LEN, frame_encode};
use yagura_test_support::ipc::{reply_within, unique_endpoint_name};

// =====================================================================
// FrameReader -- auxiliary seam unit tests
// =====================================================================

#[test]
fn frame_reader_returns_both_bodies_in_order_when_two_frames_arrive_together() {
    let mut reader = FrameReader::new();
    let mut bytes = frame_encode(b"first");
    bytes.extend_from_slice(&frame_encode(b"second"));

    let bodies = reader.push(&bytes).expect("well-formed frames must decode");

    assert_eq!(bodies, vec![b"first".to_vec(), b"second".to_vec()]);
    assert!(!reader.has_remaining());
}

#[test]
fn frame_reader_returns_a_body_only_once_its_last_byte_has_arrived() {
    let mut reader = FrameReader::new();
    let frame = frame_encode(b"drip");
    let (last, head) = frame.split_last().expect("an encoded frame is never empty");

    for byte in head {
        let bodies = reader
            .push(std::slice::from_ref(byte))
            .expect("a partial frame is not an error");
        assert!(
            bodies.is_empty(),
            "an incomplete frame must not yield a body"
        );
    }

    let bodies = reader
        .push(std::slice::from_ref(last))
        .expect("the completed frame must decode");

    assert_eq!(bodies, vec![b"drip".to_vec()]);
    assert!(!reader.has_remaining());
}

#[test]
fn frame_reader_assembles_a_frame_whose_length_prefix_arrived_split() {
    let mut reader = FrameReader::new();
    let frame = frame_encode(b"split-prefix");

    // Two bytes of the 4-byte little-endian length prefix, then the rest:
    // the reader must not mistake a partial prefix for a zero-length frame.
    let bodies = reader
        .push(&frame[..2])
        .expect("half a length prefix is not an error");
    assert!(bodies.is_empty());
    assert!(reader.has_remaining());

    let bodies = reader
        .push(&frame[2..])
        .expect("the completed frame must decode");

    assert_eq!(bodies, vec![b"split-prefix".to_vec()]);
}

#[test]
fn frame_reader_rejects_a_declared_length_above_the_frame_size_limit() {
    let mut reader = FrameReader::new();
    let declared = (MAX_FRAME_BODY_LEN + 1) as u32;

    let err = reader
        .push(&declared.to_le_bytes())
        .expect_err("an oversized declared length must fail loud");

    assert_eq!(err, FrameError::TooLarge);
}

#[test]
fn frame_reader_reports_remaining_bytes_only_while_a_frame_is_incomplete() {
    let mut reader = FrameReader::new();
    assert!(!reader.has_remaining(), "a fresh reader holds nothing");

    let complete = frame_encode(b"done");
    let trailing = frame_encode(b"still-arriving");
    let mut bytes = complete;
    bytes.extend_from_slice(&trailing[..3]);

    let bodies = reader.push(&bytes).expect("the first frame must decode");

    assert_eq!(bodies, vec![b"done".to_vec()]);
    assert!(
        reader.has_remaining(),
        "the leftover bytes of the next frame must be visible to the caller"
    );

    let bodies = reader
        .push(&trailing[3..])
        .expect("the second frame must decode");

    assert_eq!(bodies, vec![b"still-arriving".to_vec()]);
    assert!(!reader.has_remaining());
}

// =====================================================================
// should_resend -- auxiliary seam unit test
// =====================================================================

#[test]
fn should_resend_is_true_while_the_request_bytes_were_not_fully_sent() {
    // Nothing (or only part) of the request reached the daemon, so retrying
    // cannot execute a destructive operation twice.
    assert!(should_resend(false));
}

#[test]
fn should_resend_is_false_once_the_request_bytes_were_fully_sent() {
    // The daemon may already have acted on the request; retrying would risk
    // a second interrupt/close (at-most-once).
    assert!(!should_resend(true));
}

// =====================================================================
// Behavior tests against a scripted server over a real IPC endpoint (named
// pipe on Windows, Unix domain socket elsewhere)
// =====================================================================

/// How the scripted server treats the connections it accepts.
#[derive(Debug, Clone, Copy)]
enum Behavior {
    /// Read exactly one request frame, then close without replying.
    CloseAfterOneRequest,
    /// On the first connection, answer the first request with two response
    /// frames written in a single `write_all`; afterwards behave normally.
    ExtraFrameOnFirstReply,
    /// Delay the first connection's first reply long enough for the client's
    /// `round_trip` future to be dropped; afterwards behave normally.
    SlowFirstReply,
    /// Reply to every request, including `identify`, with a payload-bearing
    /// message that is neither `identified` nor `error` -- the fixture for
    /// the identify path's "unexpected reply" error. The payload carries a
    /// distinctive marker so a test can tell a variant-name-only error text
    /// apart from one that dumped the reply's full `{:?}`.
    UnexpectedDuringIdentify,
}

/// How long `SlowFirstReply` stalls -- comfortably longer than the
/// cancellation timeout used by the cancel-safety test.
const SLOW_REPLY: std::time::Duration = std::time::Duration::from_secs(3);

/// Upper bound on a reply the scripted server sends promptly or never, so a
/// reply the transport cannot read fails the test instead of hanging it.
const REPLY_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Settling delay before reading a counter for a "must NOT have happened"
/// assertion, so the connection under test is closed first.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// Endpoint-name prefix for this file's scripted server, kept distinct from
/// the one `tests/e2e.rs` uses so the two files' endpoints never collide.
const PIPE_PREFIX: &str = "yagura-mcp-frames";

/// A scripted server (a named pipe on Windows, a Unix domain socket
/// elsewhere) that counts accepted connections and received request frames.
struct ScriptedServer {
    endpoint: String,
    accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    frames_received: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn frames_received(&self) -> usize {
        self.frames_received
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Waits until the accept counter reaches `target`, panicking on
    /// timeout so a missing connection fails loudly instead of hanging.
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
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `cfg(windows)`: a named pipe instance serves exactly one connection, so
/// the accept loop hand-manages instance lifetime; the Unix counterpart binds
/// one socket that keeps accepting.
#[cfg(windows)]
fn spawn_scripted_server(tag: &str, behavior: Behavior) -> ScriptedServer {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::windows::named_pipe::ServerOptions;

    let endpoint = unique_endpoint_name(PIPE_PREFIX, tag);
    let accepted = Arc::new(AtomicUsize::new(0));
    let frames_received = Arc::new(AtomicUsize::new(0));

    let name = endpoint.clone();
    // Created synchronously, before the task starts, so the endpoint exists
    // the moment this function returns -- mirroring the `cfg(not(windows))`
    // `bind` below, so a client's first connect never races the task.
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)
        .expect("the first pipe instance must be creatable");
    let accepted_for_task = Arc::clone(&accepted);
    let frames_for_task = Arc::clone(&frames_received);
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

            let frames = Arc::clone(&frames_for_task);
            tokio::spawn(async move {
                handle_connection(conn, index, behavior, frames).await;
            });
            index += 1;
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        frames_received,
        task,
    }
}

/// `cfg(not(windows))`: see the `cfg(windows)` overload's doc for why the two
/// bodies differ. A single bound `UnixListener` keeps accepting on its own,
/// so there is no per-instance recreation step to replicate.
#[cfg(not(windows))]
fn spawn_scripted_server(tag: &str, behavior: Behavior) -> ScriptedServer {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::UnixListener;

    let endpoint = unique_endpoint_name(PIPE_PREFIX, tag);
    let accepted = Arc::new(AtomicUsize::new(0));
    let frames_received = Arc::new(AtomicUsize::new(0));

    let listener =
        UnixListener::bind(&endpoint).expect("bind must succeed on a fresh unique endpoint");
    let accepted_for_task = Arc::clone(&accepted);
    let frames_for_task = Arc::clone(&frames_received);
    let task = tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let Ok((conn, _addr)) = listener.accept().await else {
                return;
            };
            accepted_for_task.fetch_add(1, Ordering::SeqCst);

            let frames = Arc::clone(&frames_for_task);
            tokio::spawn(async move {
                handle_connection(conn, index, behavior, frames).await;
            });
            index += 1;
        }
    });

    ScriptedServer {
        endpoint,
        accepted,
        frames_received,
        task,
    }
}

#[cfg(windows)]
type ScriptedConnection = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(not(windows))]
type ScriptedConnection = tokio::net::UnixStream;

async fn handle_connection(
    mut conn: ScriptedConnection,
    index: usize,
    behavior: Behavior,
    frames: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::AsyncWriteExt;
    use yagura_protocol::{DaemonMessage, encode_daemon_message};

    if read_one_frame(&mut conn, &frames).await.is_none() {
        return;
    }

    match behavior {
        Behavior::CloseAfterOneRequest => return,
        Behavior::ExtraFrameOnFirstReply if index == 0 => {
            let mut batch = frame_encode(&encode_daemon_message(&DaemonMessage::Pong));
            batch.extend_from_slice(&frame_encode(&encode_daemon_message(
                &DaemonMessage::WindowOpened {
                    window_id: "leftover".to_string(),
                    detection_warning: None,
                },
            )));
            if conn.write_all(&batch).await.is_err() {
                return;
            }
        }
        Behavior::SlowFirstReply if index == 0 => {
            tokio::time::sleep(SLOW_REPLY).await;
            if write_pong(&mut conn).await.is_err() {
                return;
            }
        }
        Behavior::UnexpectedDuringIdentify => {
            let reply = frame_encode(&encode_daemon_message(&DaemonMessage::WindowOpened {
                window_id: "should-not-leak-into-the-error-message".to_string(),
                detection_warning: None,
            }));
            if conn.write_all(&reply).await.is_err() {
                return;
            }
        }
        _ => {
            if write_pong(&mut conn).await.is_err() {
                return;
            }
        }
    }

    // Keep serving this connection: a client that reuses it gets a well-formed
    // answer, so the test fails on an assertion instead of a timeout.
    while read_one_frame(&mut conn, &frames).await.is_some() {
        if write_pong(&mut conn).await.is_err() {
            return;
        }
    }
}

async fn write_pong(conn: &mut ScriptedConnection) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    use yagura_protocol::{DaemonMessage, encode_daemon_message};

    conn.write_all(&frame_encode(&encode_daemon_message(&DaemonMessage::Pong)))
        .await
}

/// Reads one complete request frame that the behaviors above apply to,
/// counting it. Returns `None` when the client closed the connection or the
/// stream became unusable.
///
/// The transport asks each connection which wire revision its peer speaks
/// before the caller's own request goes out. That exchange sets the connection
/// up rather than being the request under test, so it is answered here and
/// left out of both the count and the scripted behavior.
async fn read_one_frame(
    conn: &mut ScriptedConnection,
    frames: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Option<Vec<u8>> {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;
    use yagura_protocol::{
        ClientMessage, DaemonMessage, PROTOCOL_REVISION, decode_client_message,
        encode_daemon_message,
    };

    loop {
        let body = read_one_raw_frame(conn).await?;
        if matches!(
            decode_client_message(&body),
            Ok(ClientMessage::GetProtocolRevision)
        ) {
            let reply = frame_encode(&encode_daemon_message(&DaemonMessage::ProtocolRevision {
                revision: PROTOCOL_REVISION,
            }));
            conn.write_all(&reply).await.ok()?;
            continue;
        }
        frames.fetch_add(1, Ordering::SeqCst);
        return Some(body);
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

#[tokio::test]
async fn a_request_that_was_fully_sent_is_never_resent_on_a_second_connection() {
    use yagura_mcp::transport::{DaemonTransport, EndpointTransport, TransportError};
    use yagura_protocol::ClientMessage;

    let server = spawn_scripted_server("atmostonce", Behavior::CloseAfterOneRequest);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let err = reply_within(
        REPLY_LIMIT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect_err("a daemon that closes without replying must surface an error");
    assert!(
        matches!(err, TransportError::Unavailable(_)),
        "expected Unavailable, got {err:?}"
    );

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        1,
        "a failure after the request was fully sent must not be retried (at-most-once)"
    );
    assert_eq!(
        server.frames_received(),
        1,
        "the request frame must have reached the daemon exactly once"
    );

    // Proves the assertions above are not vacuous: an instance was listening
    // throughout, so a resend would have shown up as a second connection.
    let _probe = connect_scripted_probe(&server.endpoint).await;
    server
        .await_accepted(2, std::time::Duration::from_secs(5))
        .await;
}

#[cfg(windows)]
type ProbeConnection = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(not(windows))]
type ProbeConnection = tokio::net::UnixStream;

/// Connects a throwaway client to `endpoint`, proving something is still
/// listening there without going through `EndpointTransport`.
#[cfg(windows)]
async fn connect_scripted_probe(endpoint: &str) -> ProbeConnection {
    tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .expect("a server instance must still be listening")
}

/// `cfg(not(windows))` counterpart of the overload above.
#[cfg(not(windows))]
async fn connect_scripted_probe(endpoint: &str) -> ProbeConnection {
    tokio::net::UnixStream::connect(endpoint)
        .await
        .expect("a server instance must still be listening")
}

#[tokio::test]
async fn a_batched_response_returns_the_first_frame_and_abandons_the_connection() {
    use yagura_mcp::transport::{DaemonTransport, EndpointTransport};
    use yagura_protocol::{ClientMessage, DaemonMessage};

    let server = spawn_scripted_server("frameexact", Behavior::ExtraFrameOnFirstReply);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let first = reply_within(
        REPLY_LIMIT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the first response frame must be returned");
    assert_eq!(
        first,
        DaemonMessage::Pong,
        "frame-exact reading must return the first frame, not a merged/second one"
    );

    server
        .await_accepted(1, std::time::Duration::from_secs(5))
        .await;
    let before = server.accepted();

    let second = reply_within(
        REPLY_LIMIT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the next round trip must succeed on a fresh connection");
    assert_eq!(second, DaemonMessage::Pong);

    server
        .await_accepted(before + 1, std::time::Duration::from_secs(5))
        .await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        before + 1,
        "leftover bytes must make the transport drop the connection and reconnect exactly once"
    );
}

#[tokio::test]
async fn a_dropped_round_trip_future_abandons_its_connection() {
    use yagura_mcp::transport::{DaemonTransport, EndpointTransport};
    use yagura_protocol::{ClientMessage, DaemonMessage};

    let server = spawn_scripted_server("cancel", Behavior::SlowFirstReply);
    let transport = EndpointTransport::new(server.endpoint.clone(), None);

    let cancelled = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        transport.round_trip(ClientMessage::Ping),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the round trip must still be in flight when its future is dropped"
    );

    server
        .await_accepted(1, std::time::Duration::from_secs(5))
        .await;
    let before = server.accepted();

    let second = reply_within(
        REPLY_LIMIT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await
    .expect("the next round trip must succeed on a fresh connection");
    assert_eq!(
        second,
        DaemonMessage::Pong,
        "the late reply to the cancelled request must not be handed to the next caller"
    );

    server
        .await_accepted(before + 1, std::time::Duration::from_secs(5))
        .await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        before + 1,
        "a cancelled round trip must poison its connection so the next call reconnects"
    );
}

#[tokio::test]
async fn identify_gets_an_unexpected_reply_and_the_protocol_error_names_the_variant_without_its_payload()
 {
    use yagura_mcp::transport::{
        DaemonTransport, EndpointTransport, TransportError, WindowIdentity,
    };
    use yagura_protocol::ClientMessage;

    let server = spawn_scripted_server("identify-unexpected", Behavior::UnexpectedDuringIdentify);
    let identity = WindowIdentity {
        window_id: "w1-abc".to_string(),
        token: "0123456789abcdef0123456789abcdef".to_string(),
    };
    let transport = EndpointTransport::new(server.endpoint.clone(), Some(identity));

    let result = reply_within(
        REPLY_LIMIT,
        "a round trip",
        transport.round_trip(ClientMessage::Ping),
    )
    .await;

    match result {
        Err(TransportError::Protocol(message)) => {
            assert!(
                !message.contains("should-not-leak-into-the-error-message"),
                "the protocol error must name the unexpected reply's variant, not dump its \
                 full payload via {{:?}}, got {message:?}"
            );
            assert!(
                message.contains("WindowOpened"),
                "the protocol error must still name which variant arrived, got {message:?}"
            );
        }
        other => panic!(
            "expected a protocol error from the identify step's unexpected reply, got {other:?}"
        ),
    }

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        server.accepted(),
        2,
        "the retry after the identify failure must reconnect on a fresh connection, not reuse \
         the one that already returned the unexpected reply"
    );
    assert_eq!(
        server.frames_received(),
        2,
        "only the two identify attempts must have reached the daemon; the caller's own request \
         must never go out on a connection whose identity never settled"
    );
}
