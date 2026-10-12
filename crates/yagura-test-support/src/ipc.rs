//! IPC endpoint fixtures: naming, connecting and frame exchange, portable
//! across the named-pipe (Windows) and Unix-domain-socket (Unix) transports.

use std::time::Duration;

#[cfg(not(windows))]
use std::os::unix::fs::DirBuilderExt;
#[cfg(not(windows))]
use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use yagura_os::{IpcTransport, PlatformIpc};
use yagura_protocol::{ClientMessage, DaemonMessage};

#[cfg(windows)]
use crate::paths::unique_suffix;
#[cfg(not(windows))]
use crate::paths::{run_dir, run_entry_name, socket_run_dir};

/// The client-side connection type of the platform being compiled for.
pub type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// An endpoint name, `\\.\pipe\{prefix}-{tag}-{suffix}`, that collides with
/// neither a concurrently running test binary nor a resident daemon.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch (see
/// [`unique_suffix`]).
#[cfg(windows)]
#[must_use]
pub fn unique_endpoint_name(prefix: &str, tag: &str) -> String {
    format!(r"\\.\pipe\{prefix}-{tag}-{}", unique_suffix())
}

/// Reports whether `candidate` fits the `sockaddr_un` budget derived from a
/// macOS `$TMPDIR` origin. It is the single definition of
/// the threshold; callers must not re-test the length inline.
#[must_use]
pub fn endpoint_candidate_within_budget(candidate: &str) -> bool {
    candidate.len() <= 95
}

/// Builds the endpoint name `{primary_root}/{leaf}/e.sock`, switching to
/// `{fallback_root}/{leaf}/e.sock` when that candidate exceeds
/// [`endpoint_candidate_within_budget`]. `fallback_root` is asked for only
/// then, so a fallback that has to be created is created only when used.
#[must_use]
pub fn endpoint_name_from(
    primary_root: &str,
    fallback_root: impl FnOnce() -> String,
    leaf: &str,
) -> String {
    let candidate = format!("{primary_root}/{leaf}/e.sock");
    if endpoint_candidate_within_budget(&candidate) {
        candidate
    } else {
        format!("{}/{leaf}/e.sock", fallback_root())
    }
}

/// Unix branch of [`unique_endpoint_name`]: builds
/// `{run}/ytest-{prefix}-{n}/e.sock` via [`endpoint_name_from`], in this
/// process's run directory under the OS temp directory, or in its run
/// directory under `/tmp` when that candidate would exceed the `sockaddr_un`
/// budget. Either run directory is swept with the process's other fixtures.
///
/// `tag` is unused: a Unix domain socket has no name segment analogous to a
/// pipe's tag, and it is accepted only to keep one call signature across
/// platforms. The parent directory is created `0700` to satisfy the
/// parent-directory check `UnixIpc::bind` applies.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch, if a run
/// directory cannot be created or its path is not valid UTF-8, or if the
/// parent directory cannot be created.
#[cfg(not(windows))]
#[must_use]
pub fn unique_endpoint_name(prefix: &str, tag: &str) -> String {
    let _ = tag;
    let name = endpoint_name_from(
        utf8(run_dir()),
        || utf8(socket_run_dir()).to_string(),
        &run_entry_name("ytest", prefix),
    );
    let dir = Path::new(&name)
        .parent()
        .expect("endpoint name must have a parent directory");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .expect("create unique endpoint directory");
    name
}

#[cfg(not(windows))]
fn utf8(path: &Path) -> &str {
    path.to_str()
        .expect("run directory path must be valid UTF-8")
}

/// Opens `endpoint`, retrying until it exists or `timeout` elapses: a server
/// task spawned moments earlier has not necessarily created its endpoint yet.
///
/// # Panics
///
/// Panics if no connection succeeded within `timeout`, reporting the last
/// connection error.
pub async fn connect_with_retry(endpoint: &str, timeout: Duration) -> Connection {
    let transport = PlatformIpc::default();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match transport.connect(endpoint).await {
            Ok(conn) => return conn,
            Err(err) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("timed out connecting to endpoint {endpoint}: {err}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// Awaits `exchange`, a call that waits on a peer's reply, for at most
/// `limit`: a reply the code under test fails to read must fail the test
/// rather than leave it waiting forever.
///
/// # Panics
///
/// Panics, naming `what`, if `exchange` has not finished within `limit`.
pub async fn reply_within<F: std::future::Future>(
    limit: Duration,
    what: &str,
    exchange: F,
) -> F::Output {
    match tokio::time::timeout(limit, exchange).await {
        Ok(output) => output,
        Err(_) => panic!("{what} got no reply within {limit:?}"),
    }
}

/// Encodes `msg` and writes it to `client` as one length-prefixed frame.
///
/// # Panics
///
/// Panics if the frame cannot be written to the connection.
pub async fn send_client_message(client: &mut Connection, msg: &ClientMessage) {
    let body = yagura_protocol::encode_client_message(msg);
    let frame = yagura_protocol::frame_encode(&body);
    client
        .write_all(&frame)
        .await
        .expect("write_all frame to endpoint");
}

/// The result of reading from a connection: a message, or a peer that closed
/// it.
///
/// A disconnect is a *value* rather than a panic, so a test can assert that
/// the daemon closed the connection -- the specified reaction to several
/// classes of malformed input.
#[derive(Debug)]
// A decoded frame is moved through this enum exactly once, by the test that
// asked for it, so boxing the payload would buy nothing and cost every match
// site an indirection.
#[allow(clippy::large_enum_variant)]
pub enum ReadOutcome {
    /// One decoded frame arrived.
    Message(DaemonMessage),
    /// The peer closed the connection.
    Closed,
}

impl ReadOutcome {
    /// Unwraps the frame a caller expected to arrive, naming the step with
    /// `ctx` if it did not.
    ///
    /// # Panics
    ///
    /// Panics if the connection was closed instead.
    pub fn expect_message(self, ctx: &str) -> DaemonMessage {
        match self {
            ReadOutcome::Message(m) => m,
            ReadOutcome::Closed => panic!("{ctx}: connection closed unexpectedly"),
        }
    }

    /// Asserts that the peer closed the connection, naming the step with `ctx`
    /// if it did not.
    ///
    /// # Panics
    ///
    /// Panics if a frame arrived instead, reporting the frame.
    pub fn expect_closed(self, ctx: &str) {
        if let ReadOutcome::Message(m) = self {
            panic!("{ctx}: expected connection to be closed, but received {m:?}");
        }
    }
}

/// Reads exactly one frame of whatever kind arrives next.
///
/// # Panics
///
/// Panics if nothing arrives within `timeout` (silence where a frame was
/// expected is a test failure), or if the daemon emitted a frame that does not
/// decode. A closed connection is not a panic: it is returned as
/// [`ReadOutcome::Closed`].
pub async fn read_daemon_frame(client: &mut Connection, timeout: Duration) -> ReadOutcome {
    let mut len_buf = [0u8; 4];
    match tokio::time::timeout(timeout, client.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return ReadOutcome::Closed,
        Err(_) => panic!("timed out waiting for a frame length prefix"),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    match tokio::time::timeout(timeout, client.read_exact(&mut body)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return ReadOutcome::Closed,
        Err(_) => panic!("timed out waiting for a frame body"),
    }
    ReadOutcome::Message(
        yagura_protocol::decode_daemon_message(&body)
            .expect("daemon must only ever emit well-formed frames"),
    )
}

/// Reads the next frame that is not a `StatusChanged` push.
///
/// An attached connection interleaves status pushes with the responses to its
/// own requests, so a request/response test would otherwise consume a push in
/// place of the frame it meant to inspect. A test whose subject *is* the
/// notification must read raw frames with [`read_daemon_frame`].
///
/// # Panics
///
/// Panics under the same conditions as [`read_daemon_frame`].
pub async fn read_status_skipping_message(
    client: &mut Connection,
    timeout: Duration,
) -> ReadOutcome {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match read_daemon_frame(client, remaining).await {
            ReadOutcome::Message(DaemonMessage::StatusChanged { .. }) => continue,
            outcome => return outcome,
        }
    }
}

/// Reads one frame, reporting both a timeout and a closed connection as
/// `None`.
///
/// For asserting that *no* frame follows. It cannot tell silence from a
/// disconnect, so where that distinction is under test use
/// [`read_daemon_frame`].
///
/// # Panics
///
/// Panics if the daemon emitted a frame that does not decode.
pub async fn try_read_daemon_message(
    client: &mut Connection,
    idle_timeout: Duration,
) -> Option<DaemonMessage> {
    let mut len_buf = [0u8; 4];
    if tokio::time::timeout(idle_timeout, client.read_exact(&mut len_buf))
        .await
        .ok()?
        .is_err()
    {
        return None;
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    if tokio::time::timeout(idle_timeout, client.read_exact(&mut body))
        .await
        .ok()?
        .is_err()
    {
        return None;
    }
    Some(
        yagura_protocol::decode_daemon_message(&body)
            .expect("daemon must only ever emit well-formed frames"),
    )
}
