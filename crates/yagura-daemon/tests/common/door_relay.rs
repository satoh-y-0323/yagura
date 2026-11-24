#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the compiled `yagura-daemon` binary as a real child process, the way a CLI runs
// its hook.
#![allow(clippy::disallowed_methods)]

//! A door run through a relay to an in-process daemon.
//!
//! The door is a child process of the test with a window's credentials. It reaches an
//! endpoint the test itself holds, and the test carries every frame between that endpoint
//! and the daemon over one connection of its own, in both directions as they come. The
//! daemon's peer is then the test process, which is the daemon's own process.
//!
//! The relay ends in two steps: the door's connection ends ([`ConnectionEnded`], with what
//! the door sent and the relay's connection to the daemon), then the door's process ends
//! ([`Relayed`], with what it printed and how it ended). A caller that wants the daemon to
//! see the door's end as a direct connection shows it drops the connection at the first
//! step; one that goes on using the connection keeps it.

use std::future::Future;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;

use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::Connection;
use yagura_test_support::paths::TempDir;

use super::{Harness, VALID_TOKEN, bins};

const WINDOW_ID_VAR: &str = "YAGURA_WINDOW_ID";
const STATUS_TOKEN_VAR: &str = "YAGURA_STATUS_TOKEN";
const CHUNK: usize = 64 * 1024;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// An instance identifier of the relay's own, so a door never reaches another daemon.
pub(crate) fn unique_instance() -> String {
    format!(
        "relay{:x}{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// What a relay between a door and its daemon does with the door's notice that it printed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Printed {
    /// Passes it on, as a door's own connection does.
    Forward,
    /// Drops it: the daemon sees the read end without it once the relay lets go.
    Withhold,
}

/// The door `args` as a command, run in the instance `instance` with base directories under
/// `bases` and, when given, a window's credentials.
pub(crate) fn door_command(
    args: &[&str],
    instance: &str,
    bases: &TempDir,
    window: Option<(&str, &str)>,
) -> Command {
    let mut command = Command::new(bins::daemon_exe());
    command
        .args(args)
        .env("YAGURA_INSTANCE", instance)
        .env_remove("YAGURA_STORE_DB")
        .env_remove(WINDOW_ID_VAR)
        .env_remove(STATUS_TOKEN_VAR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        command
            .env("LOCALAPPDATA", bases.make_dir("local"))
            .env("APPDATA", bases.make_dir("roaming"));
    } else {
        command
            .env("XDG_DATA_HOME", bases.make_dir("data"))
            .env("XDG_STATE_HOME", bases.make_dir("state"))
            .env("XDG_CONFIG_HOME", bases.make_dir("config"))
            .env("HOME", bases.make_dir("home"));
    }
    if let Some((window_id, token)) = window {
        command
            .env(WINDOW_ID_VAR, window_id)
            .env(STATUS_TOKEN_VAR, token);
    }
    command
}

/// Runs `command` with `input` written on its stdin from a thread of its own, as a CLI
/// writes the hook input; the returned result is that write's.
pub(crate) fn run_with_input(
    mut command: Command,
    input: Vec<u8>,
) -> (Output, std::io::Result<()>) {
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the yagura-daemon binary");
    let mut pipe = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || pipe.write_all(&input).and_then(|()| pipe.flush()));
    let output = child.wait_with_output().expect("wait for the child");
    let written = writer.join().expect("writer thread");
    (output, written)
}

/// Reads one frame of a door's as JSON; `None` once the door has closed.
pub(crate) async fn read_json_frame<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Option<serde_json::Value> {
    let guard = HANG_GUARD;
    let mut len = [0u8; 4];
    match tokio::time::timeout(guard, stream.read_exact(&mut len)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return None,
        Err(_) => panic!("the door neither wrote nor closed within {guard:?}"),
    }
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    tokio::time::timeout(guard, stream.read_exact(&mut body))
        .await
        .expect("the door finishes a frame it began")
        .expect("the door finishes a frame it began");
    Some(serde_json::from_slice(&body).expect("a door writes JSON frames"))
}

pub(crate) async fn write_json_frame<S: AsyncWrite + Unpin>(
    stream: &mut S,
    frame: &serde_json::Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame).expect("a frame encodes");
    stream
        .write_all(&yagura_protocol::frame_encode(&body))
        .await?;
    stream.flush().await
}

/// Claims `instance`'s endpoint; the returned future accepts the one door that connects.
pub(crate) async fn claim_endpoint(
    instance: &str,
) -> impl Future<Output = <yagura_os::PlatformIpc as yagura_os::PeerAuth>::Connection> + Send + 'static
{
    use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};

    let ipc = PlatformIpc::default();
    let user = ipc.current_user_id().expect("the current user id resolves");
    let namespace = yagura_os::resolve_instance_from(Some(std::ffi::OsStr::new(instance)))
        .expect("the test's instance id is valid");
    let endpoint = yagura_os::default_endpoint_owning(&user, &namespace);
    let security = ipc.endpoint_security().expect("endpoint security resolves");
    let mut listener = ipc
        .bind(&endpoint, security)
        .await
        .unwrap_or_else(|err| panic!("the stand-in must claim {endpoint:?}: {err}"));
    async move {
        tokio::time::timeout(HANG_GUARD, ipc.accept(&mut listener))
            .await
            .expect("the door connects")
            .expect("the door connects")
    }
}

/// The bytes of the frames one side of the relay has written so far, kept so that a read
/// asks for no more than the frame in progress still needs: a read that is cancelled
/// loses nothing, and the relay never reads past a frame it is about to leave unread.
#[derive(Default)]
struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    /// The bytes the frame in progress still needs; at least 1.
    fn needed(&self) -> usize {
        let Some(len) = self.buf.get(..4) else {
            return 4 - self.buf.len();
        };
        let body = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        (4 + body - self.buf.len()).max(1)
    }

    /// The next whole frame, taken out of the buffer.
    fn take(&mut self) -> Option<serde_json::Value> {
        let len = self.buf.get(..4)?;
        let body = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let rest = self.buf.get(4..4 + body)?;
        let frame = serde_json::from_slice(rest).expect("a frame is JSON");
        self.buf.drain(..4 + body);
        Some(frame)
    }

    fn in_progress(&self) -> bool {
        !self.buf.is_empty()
    }
}

/// Reads at most what `frames`' frame in progress needs from `stream`; `false` once the
/// stream has ended or failed. Cancel-safe: a cancelled call has read nothing.
async fn fill<S: AsyncRead + Unpin>(stream: &mut S, frames: &mut Frames) -> bool {
    let mut chunk = vec![0u8; frames.needed().min(CHUNK)];
    match stream.read(&mut chunk).await {
        Ok(0) | Err(_) => false,
        Ok(read) => {
            frames.buf.extend_from_slice(&chunk[..read]);
            true
        }
    }
}

fn is_push(frame: &serde_json::Value) -> bool {
    frame["type"]
        .as_str()
        .is_some_and(|kind| kind.ends_with("_changed"))
}

/// Carries frames between `door` and `upstream` as they come, until the door's side ends or
/// fails. What the door sent goes to `sent`. A daemon's push that nobody asked for is not
/// carried to the door. On return nothing is being read from `upstream`, and what was read
/// from it ends at a frame boundary.
async fn carry<D, U>(
    door: &mut D,
    upstream: &mut U,
    printed: Printed,
    sent: &mut Vec<serde_json::Value>,
) where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut from_door, mut from_daemon) = (Frames::default(), Frames::default());
    'relay: loop {
        tokio::select! {
            more = fill(door, &mut from_door) => {
                if !more {
                    break 'relay;
                }
                while let Some(frame) = from_door.take() {
                    let notice = frame["type"] == "deliver_printed";
                    sent.push(frame.clone());
                    if notice && printed == Printed::Withhold {
                        continue;
                    }
                    if write_json_frame(upstream, &frame).await.is_err() {
                        return;
                    }
                }
            }
            more = fill(upstream, &mut from_daemon) => {
                if !more {
                    break 'relay;
                }
                while let Some(frame) = from_daemon.take() {
                    if is_push(&frame) {
                        continue;
                    }
                    if write_json_frame(door, &frame).await.is_err() {
                        break 'relay;
                    }
                }
            }
            () = tokio::time::sleep(HANG_GUARD) => {
                panic!("neither the door nor the daemon wrote or closed within {HANG_GUARD:?}");
            }
        }
    }
    // A frame the daemon began is read to its end, so the connection goes on from a boundary.
    while from_daemon.in_progress() {
        let more = tokio::time::timeout(HANG_GUARD, fill(upstream, &mut from_daemon))
            .await
            .expect("the daemon finishes a frame it began");
        let _ = from_daemon.take();
        if !more {
            break;
        }
    }
}

/// The door's process: still running, or already ended.
enum Door {
    Running(JoinHandle<(Output, std::io::Result<()>)>),
    Ended((Output, std::io::Result<()>)),
}

impl Door {
    async fn finish(self) -> (Output, std::io::Result<()>) {
        match self {
            Self::Running(handle) => handle.await.expect("join"),
            Self::Ended(result) => result,
        }
    }
}

/// A door whose connection has ended: what it sent, and the relay's connection to the
/// daemon, still open.
pub(crate) struct ConnectionEnded {
    /// Every frame the door sent, in order.
    pub(crate) sent: Vec<serde_json::Value>,
    /// The relay's connection to the daemon, which stays open until it is dropped.
    pub(crate) upstream: Connection,
    door: Door,
}

impl ConnectionEnded {
    /// Waits for the door's process to end, keeping the connection to the daemon open.
    pub(crate) async fn finish(self) -> Relayed {
        let (output, written) = self.door.finish().await;
        Relayed {
            output,
            written,
            sent: self.sent,
            upstream: self.upstream,
        }
    }

    /// Lets the connection to the daemon go now, as a door's own connection goes with the
    /// door, then waits for the door's process to end.
    pub(crate) async fn finish_dropping_upstream(self) -> (Output, std::io::Result<()>) {
        drop(self.upstream);
        self.door.finish().await
    }
}

/// A door run to its end through a relay.
pub(crate) struct Relayed {
    /// What the door printed and how it ended.
    pub(crate) output: Output,
    /// The result of the CLI's write of the hook input.
    #[allow(dead_code)]
    pub(crate) written: std::io::Result<()>,
    /// Every frame the door sent, in order.
    pub(crate) sent: Vec<serde_json::Value>,
    /// The relay's connection to the daemon, which stays open until it is dropped.
    pub(crate) upstream: Connection,
}

/// Runs the door `args` in `window_id` with `input` on its stdin, relaying every frame
/// between it and `h`'s daemon over one connection of the relay's own, until the door's
/// connection ends. A door that ends without connecting ends the relay without waiting for
/// a connection. The door's process may still be running.
pub(crate) async fn relay_door_connection(
    h: &Harness,
    instance: &str,
    window_id: &str,
    args: &[&str],
    input: Vec<u8>,
    printed: Printed,
) -> ConnectionEnded {
    relay_door_with(
        h,
        instance,
        Some((window_id, VALID_TOKEN)),
        args,
        input,
        printed,
    )
    .await
}

/// [`relay_door_connection`] for a door that may carry no credentials in its environment,
/// as a door naming its window through `--credentials` does.
pub(crate) async fn relay_door_with(
    h: &Harness,
    instance: &str,
    window: Option<(&str, &str)>,
    args: &[&str],
    input: Vec<u8>,
    printed: Printed,
) -> ConnectionEnded {
    let bases = TempDir::new("yagura-door", "relay");
    let accept = claim_endpoint(instance).await;
    tokio::pin!(accept);
    let mut upstream = h.connect().await;
    let command = door_command(args, instance, &bases, window);
    let mut process = tokio::task::spawn_blocking(move || {
        let ended = run_with_input(command, input);
        drop(bases);
        ended
    });
    let mut sent = Vec::new();
    let door = tokio::select! {
        biased;
        conn = &mut accept => {
            let mut conn = conn;
            carry(&mut conn, &mut upstream, printed, &mut sent).await;
            Door::Running(process)
        }
        ended = &mut process => Door::Ended(ended.expect("join")),
    };
    ConnectionEnded {
        sent,
        upstream,
        door,
    }
}

/// [`relay_door_connection`] on an instance of its own, then the door's process to its end,
/// with the connection to the daemon kept open for the caller to use and drop.
pub(crate) async fn relay_door(
    h: &Harness,
    window_id: &str,
    args: &[&str],
    input: Vec<u8>,
    printed: Printed,
) -> Relayed {
    relay_door_connection(h, &unique_instance(), window_id, args, input, printed)
        .await
        .finish()
        .await
}

/// Runs the door as a CLI does and as a direct connection ends: the connection to the daemon
/// is dropped when the door's own ends. Returns what the door printed and the result of the
/// CLI's write of its input.
pub(crate) async fn run_door(
    h: &Harness,
    window_id: &str,
    args: &[&str],
    input: Vec<u8>,
) -> (Output, std::io::Result<()>) {
    run_door_as(h, &unique_instance(), window_id, args, input).await
}

/// [`run_door`] with the door's `YAGURA_INSTANCE` named by the caller.
pub(crate) async fn run_door_as(
    h: &Harness,
    instance: &str,
    window_id: &str,
    args: &[&str],
    input: Vec<u8>,
) -> (Output, std::io::Result<()>) {
    relay_door_connection(h, instance, window_id, args, input, Printed::Forward)
        .await
        .finish_dropping_upstream()
        .await
}

/// [`run_door`] for a door with no credentials in its environment.
pub(crate) async fn run_door_without_credentials(
    h: &Harness,
    args: &[&str],
    input: Vec<u8>,
) -> (Output, std::io::Result<()>) {
    relay_door_with(h, &unique_instance(), None, args, input, Printed::Forward)
        .await
        .finish_dropping_upstream()
        .await
}

/// A relay running in the background for a door the test spawned itself.
pub(crate) struct SpawnedRelay {
    task: JoinHandle<Vec<serde_json::Value>>,
}

impl SpawnedRelay {
    /// Waits for the relay to end, which it does once the door's connection ends or the
    /// door's process ended without connecting; the connection to the daemon has been
    /// dropped by then. Returns every frame the door sent.
    pub(crate) async fn ended(self) -> Vec<serde_json::Value> {
        self.task.await.expect("the relay task ends")
    }
}

/// Relays the connection of the door process `door_pid`, which the test spawned with
/// `YAGURA_INSTANCE` set to the instance `accept` was claimed for, to `h`'s daemon, in the
/// background. A door's process ending decides that it will not connect only after the
/// accept had its chance first, so a door that connected and then ended is still carried.
/// The connection to the daemon is dropped when the door's connection ends.
pub(crate) async fn relay_spawned_door<A>(
    h: &Harness,
    accept: A,
    door_pid: u32,
    printed: Printed,
) -> SpawnedRelay
where
    A: Future<Output = <yagura_os::PlatformIpc as yagura_os::PeerAuth>::Connection>
        + Send
        + 'static,
{
    let mut upstream = h.connect().await;
    let task = tokio::spawn(async move {
        let mut sent = Vec::new();
        let ended = door_ended(door_pid);
        tokio::pin!(accept, ended);
        let connected = tokio::select! {
            biased;
            conn = &mut accept => Some(conn),
            () = &mut ended => std::future::poll_fn(|cx| {
                std::task::Poll::Ready(match Future::poll(accept.as_mut(), cx) {
                    std::task::Poll::Ready(conn) => Some(conn),
                    std::task::Poll::Pending => None,
                })
            })
            .await,
        };
        if let Some(mut conn) = connected {
            carry(&mut conn, &mut upstream, printed, &mut sent).await;
        }
        drop(upstream);
        sent
    });
    SpawnedRelay { task }
}

/// Resolves once the process `pid` has exited, without reaping it: the test's own `Child`
/// still kills, polls and waits.
async fn door_ended(pid: u32) {
    tokio::task::spawn_blocking(move || {
        let Some(start) = yagura_os::process_start(pid).expect("the door's start reads") else {
            return;
        };
        if let Some(handle) =
            yagura_os::ProcessHandle::open(pid, &start).expect("the door's process opens")
        {
            handle.wait().expect("the door's end is awaited");
        }
    })
    .await
    .expect("join");
}
