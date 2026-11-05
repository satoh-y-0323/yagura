//! The daemon's end of one window's holder: starts it, lets itself in, and
//! carries the window's output, writes, resizes and end over the holder's
//! endpoint on a thread and runtime of its own, so a blocked caller runtime
//! never stops the output.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use yagura_os::{DetachedProcess, IpcTransport, PlatformIpc};
use yagura_protocol::holder::{
    AckStep, HOLDER_PROTOCOL, HolderEnvOp, HolderFrame, HolderFrameBuffer, HolderLaunch,
    HolderSpawnFailure, encode_holder_frame, encode_holder_launch, holder_record_path,
    read_holder_record,
};

use super::{PtyError, lock_recover};
use crate::holder_host::LinkContext;

/// How long a holder waits for its first `Hello`; also bounds how long the
/// daemon tries to be let in.
const HELLO_DEADLINE: Duration = Duration::from_secs(30);

/// How many connections the daemon makes to be let in, when the holder
/// closes one without a word (one of many handshakes at once timing out).
const HELLO_ATTEMPTS: usize = 3;

/// The most the holder's ready notice may take.
const MAX_NOTICE_BYTES: usize = 256;

/// A safety bound, not a point the daemon waits for: a holder that has not
/// taken its launch and told it is ready by then is ended, and the window
/// does not open.
const READY_LIMIT: Duration = HELLO_DEADLINE;

/// A safety bound on a holder that let the daemon in for a window that then
/// did not open: it is killed if it has not ended the child's tree and closed
/// the connection by then.
const RELEASE_LIMIT: Duration = HELLO_DEADLINE;

type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// The window a holder is started for.
pub(super) struct Launch {
    pub(super) window_id: String,
    pub(super) argv: Vec<String>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) env: Vec<HolderEnvOp>,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

/// What the window's feeder receives, in the order it was sent: the link's
/// outputs and end, and the window's own wakes and settles.
pub(super) enum Feed {
    Output {
        seq: u64,
        bytes: Vec<u8>,
    },
    /// Nothing more comes: the child exited or the connection ended. Sent
    /// after the exit is published, so whoever takes it sees the exit.
    End,
    /// The window's view may want a frame. Sent only by the window itself,
    /// at most once while one is still untaken (see `ViewSlot`).
    View,
    /// Answered once every item sent before it has been taken in, its frame
    /// handed to the view's connection and its output acknowledged; dropped
    /// unanswered when the feeder ends first.
    Settle(oneshot::Sender<()>),
}

/// Where the child's exit is published.
pub(super) struct ExitReport {
    pub(super) code: Arc<AtomicI32>,
    pub(super) exited: Arc<AtomicBool>,
    pub(super) notify: watch::Sender<bool>,
}

impl ExitReport {
    /// The code first, then the flag, then the notification: whoever is woken
    /// reads both. A missing code (the connection was lost, or the OS gave
    /// none) is `-1`.
    fn publish(&self, code: Option<i32>) {
        self.code.store(code.unwrap_or(-1), Ordering::SeqCst);
        self.exited.store(true, Ordering::SeqCst);
        let _ = self.notify.send(true);
    }
}

/// Where the link hands what it receives: each output to the feeder, and the
/// child's exit, published as soon as the holder reports it so a model held
/// by a reader never hides it. The feeder still sees the end after the last
/// output, which is what a read after the exit waits for.
pub(super) struct Sink {
    pub(super) feed: std::sync::mpsc::Sender<Feed>,
    pub(super) exit: ExitReport,
}

impl Sink {
    fn output(&self, seq: u64, bytes: Vec<u8>) {
        let _ = self.feed.send(Feed::Output { seq, bytes });
    }

    fn end(&self, code: Option<i32>) {
        self.exit.publish(code);
        let _ = self.feed.send(Feed::End);
    }
}

#[derive(Default)]
struct Pending {
    closed: bool,
    next_write: u64,
    writes: HashMap<u64, std::sync::mpsc::SyncSender<bool>>,
    resize: Option<oneshot::Sender<bool>>,
}

/// The window's way to its holder; cheap to clone.
#[derive(Clone)]
pub(super) struct Link {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<Pending>>,
}

impl Link {
    fn send(&self, frame: &HolderFrame) {
        let _ = self.out.send(encode_holder_frame(frame));
    }

    pub(super) fn ack(&self, seq: u64) {
        self.send(&HolderFrame::Ack { seq });
    }

    /// Asks the holder to end the child (and with `tree`, its whole tree now);
    /// nothing to do once the holder is gone.
    pub(super) fn terminate(&self, tree: bool) {
        self.send(&HolderFrame::Terminate { tree });
    }

    /// Writes `bytes` to the child after every earlier write, and returns
    /// once the holder answered.
    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        {
            let mut pending = lock_recover(&self.pending);
            if pending.closed {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let id = pending.next_write;
            pending.next_write = pending.next_write.wrapping_add(1);
            pending.writes.insert(id, answer);
            self.send(&HolderFrame::Write {
                id,
                bytes: bytes.to_vec(),
            });
        }
        match answered.recv() {
            Ok(true) => Ok(()),
            Ok(false) => Err(std::io::Error::other(
                "the holder could not write to the child",
            )),
            Err(_) => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Whether the holder's terminal took the size `cols` x `rows`.
    pub(super) async fn resize(&self, cols: u16, rows: u16) -> bool {
        let (answer, answered) = oneshot::channel();
        {
            let mut pending = lock_recover(&self.pending);
            if pending.closed {
                return false;
            }
            pending.resize = Some(answer);
            self.send(&HolderFrame::Resize { cols, rows });
        }
        answered.await.unwrap_or(false)
    }

    /// The child's input as a writer, each write answered by the holder.
    pub(super) fn writer(&self) -> Box<dyn Write + Send> {
        Box::new(HolderWriter(self.clone()))
    }

    fn answer_write(&self, id: u64, written: bool) {
        if let Some(answer) = lock_recover(&self.pending).writes.remove(&id) {
            let _ = answer.send(written);
        }
    }

    fn answer_resize(&self, resized: bool) {
        if let Some(answer) = lock_recover(&self.pending).resize.take() {
            let _ = answer.send(resized);
        }
    }

    /// Fails every write and resize waiting for an answer, and every later one.
    fn close(&self) {
        let mut pending = lock_recover(&self.pending);
        pending.closed = true;
        pending.writes.clear();
        pending.resize = None;
    }
}

struct HolderWriter(Link);

impl Write for HolderWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.0.write(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What the window learns once its holder let the daemon in.
pub(super) struct Opened {
    pub(super) child_pid: u32,
    /// When to ack the holder's output, from the window its `Welcome` named.
    pub(super) ack_step: AckStep,
}

/// A link whose holder is being started.
pub(super) struct Started {
    pub(super) link: Link,
    /// What the holder's `Welcome` told once it let the daemon in, or why
    /// the window cannot open.
    pub(super) opened: oneshot::Receiver<Result<Opened, PtyError>>,
    /// `true` once the holder's process has ended.
    pub(super) released: watch::Receiver<bool>,
}

/// Starts the holder of `launch` on a thread of its own; its output goes to
/// `feed`.
pub(super) fn start(context: LinkContext, launch: Launch, feed: Sink) -> Result<Started, PtyError> {
    let (out, out_rx) = mpsc::unbounded_channel();
    let link = Link {
        out,
        pending: Arc::default(),
    };
    let (opened_tx, opened) = oneshot::channel();
    let (released_tx, released) = watch::channel(false);
    std::thread::Builder::new()
        .name("holder-link".to_owned())
        .spawn({
            let link = link.clone();
            move || {
                run(context, &launch, &link, out_rx, &feed, opened_tx);
                let _ = released_tx.send(true);
            }
        })
        .map_err(|error| {
            tracing::error!(%error, "cannot start the thread of a window's holder");
            PtyError::SpawnFailed
        })?;
    Ok(Started {
        link,
        opened,
        released,
    })
}

/// How the link's time with the holder ended.
enum Ending {
    /// The connection is over; the holder ends by itself and removes its
    /// own files.
    Served,
    /// The window never opened and the holder has not ended: it may still
    /// wait for a `Hello`, or overran [`RELEASE_LIMIT`]. It is killed and its
    /// record is removed here.
    Refused,
}

fn run(
    context: LinkContext,
    launch: &Launch,
    link: &Link,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    feed: &Sink,
    opened: oneshot::Sender<Result<Opened, PtyError>>,
) {
    let LinkContext {
        holder_exe,
        instance,
        boot,
        daemon_endpoint,
        holders_dir,
        stderr,
        fallback_logged,
        keep,
        live,
    } = context;
    let window_id = launch.window_id.as_str();
    let args: [&OsStr; 2] = [OsStr::new(window_id), &instance];
    // Started in its own directory: a holder (and the pseudoconsole host it
    // starts) never keeps the holders directory in use, so it can be removed.
    let mut process =
        match yagura_os::spawn_detached_process(&holder_exe, &args, holder_exe.parent(), stderr) {
            Ok(process) => process,
            Err(error) => {
                tracing::error!(
                    %error,
                    holder = %holder_exe.display(),
                    "cannot start the window's holder"
                );
                let _ = opened.send(Err(PtyError::SpawnFailed));
                return;
            }
        };
    if process.used_fallback() && !fallback_logged.swap(true, Ordering::SeqCst) {
        tracing::warn!(
            "a window's holder could not leave the daemon's job; such windows end with the daemon"
        );
    }

    let holder = HolderAt {
        holders_dir: &holders_dir,
        window_id,
        daemon_endpoint: &daemon_endpoint,
        boot: &boot,
    };
    let ending = match holder.become_ready(&mut process, launch) {
        Ok(endpoint) => serve(&holder, &endpoint, process.id(), link, out_rx, feed, opened),
        Err(error) => {
            let _ = opened.send(Err(error));
            Ending::Refused
        }
    };
    link.close();
    let refused = matches!(ending, Ending::Refused);
    if refused {
        let _ = process.kill();
    }
    match process.wait() {
        Ok(Some(code)) if code != 0 => {
            tracing::debug!(window_id, code, "the window's holder ended with a failure");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(window_id, %error, "cannot wait for the window's holder"),
    }
    // A killed holder cannot remove its record. Other leftovers (a holder
    // killed from outside, or ended with the daemon's job) are for the
    // start-up reconciliation, not a second remover here.
    if refused {
        remove_holder_record(&holders_dir, window_id);
    }
    drop(keep);
    drop(live);
}

/// One holder's names.
struct HolderAt<'a> {
    holders_dir: &'a Path,
    window_id: &'a str,
    daemon_endpoint: &'a str,
    boot: &'a str,
}

impl HolderAt<'_> {
    /// Hands the holder its launch, waits for its ready notice and checks its
    /// record names it; returns the endpoint the record names.
    fn become_ready(
        &self,
        process: &mut DetachedProcess,
        launch: &Launch,
    ) -> Result<String, PtyError> {
        let encoded = encode_holder_launch(&HolderLaunch {
            argv: launch.argv.clone(),
            cwd: launch.cwd.clone(),
            env: launch.env.clone(),
            cols: launch.cols,
            rows: launch.rows,
            record: String::new(),
            hello_deadline_ms: u64::try_from(HELLO_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            holders_dir: self.holders_dir.to_path_buf(),
            daemon_endpoint: self.daemon_endpoint.to_owned(),
            end_child_on_disconnect: true,
        });
        match hand_launch(process, encoded) {
            Notice::Ready => {}
            Notice::Late => {
                tracing::warn!(
                    window_id = self.window_id,
                    "the window's holder was not ready in time; it is ended"
                );
                return Err(PtyError::SpawnFailed);
            }
            // Not waited for here: a holder whose pipes broke may still run,
            // and only the caller's kill bounds that wait.
            Notice::Ended => {
                tracing::warn!(
                    window_id = self.window_id,
                    "the window's holder ended before it was ready; see its messages in the \
                     daemon log"
                );
                return Err(PtyError::SpawnFailed);
            }
        }
        let record = read_holder_record(self.holders_dir, self.window_id).map_err(|error| {
            tracing::warn!(window_id = self.window_id, %error, "cannot read the window holder's record");
            PtyError::SpawnFailed
        })?;
        if record.holder_pid != process.id() || record.window_id != self.window_id {
            tracing::warn!(
                window_id = self.window_id,
                "the window holder's record names another process"
            );
            return Err(PtyError::SpawnFailed);
        }
        Ok(record.endpoint)
    }
}

/// How a holder answered its launch.
enum Notice {
    Ready,
    /// It ended, or its pipes broke, before it was ready.
    Ended,
    /// Still not ready at [`READY_LIMIT`]; the caller kills it.
    Late,
}

/// Writes `launch` to the holder's standard input and reads its ready notice
/// on a thread of their own, so a holder that stalls is waited for no longer
/// than [`READY_LIMIT`]. Once the holder is killed, the thread's write or read
/// fails and it ends.
fn hand_launch(process: &mut DetachedProcess, launch: Vec<u8>) -> Notice {
    let (Some(mut stdin), Some(stdout)) = (process.take_stdin(), process.take_stdout()) else {
        return Notice::Ended;
    };
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    let started = std::thread::Builder::new()
        .name("holder-notice".to_owned())
        .spawn(move || {
            let ready = stdin.write_all(&launch).and_then(|()| {
                drop(stdin);
                read_notice(stdout)
            });
            let _ = answer.send(matches!(ready, Ok(true)));
        });
    if let Err(error) = started {
        tracing::error!(%error, "cannot start the thread that reads a holder's notice");
        return Notice::Late;
    }
    match answered.recv_timeout(READY_LIMIT) {
        Ok(true) => Notice::Ready,
        Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Notice::Ended,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Notice::Late,
    }
}

/// Reads the holder's one-line ready notice: `true` once a line ended,
/// `false` when the output ended first.
fn read_notice(mut stdout: std::fs::File) -> std::io::Result<bool> {
    let mut seen = 0usize;
    let mut byte = [0u8; 1];
    while seen < MAX_NOTICE_BYTES {
        if stdout.read(&mut byte)? == 0 {
            return Ok(false);
        }
        if byte[0] == b'\n' {
            return Ok(true);
        }
        seen += 1;
    }
    Ok(false)
}

fn serve(
    holder: &HolderAt<'_>,
    endpoint: &str,
    holder_pid: u32,
    link: &Link,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    feed: &Sink,
    opened: oneshot::Sender<Result<Opened, PtyError>>,
) -> Ending {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the runtime of a window's holder");
            let _ = opened.send(Err(PtyError::SpawnFailed));
            return Ending::Refused;
        }
    };
    runtime.block_on(async {
        let (connection, frames, child_pid, ack_step) =
            match handshake(holder, endpoint, holder_pid).await {
                Ok(connected) => connected,
                Err(Refusal { error, let_in }) => {
                    let _ = opened.send(Err(error));
                    return match let_in {
                        Some(connection) => give_up(connection, RELEASE_LIMIT).await,
                        None => Ending::Refused,
                    };
                }
            };
        let Some(child_pid) = child_pid else {
            tracing::warn!(
                window_id = holder.window_id,
                "the window's holder named no child in its welcome"
            );
            let _ = opened.send(Err(PtyError::SpawnFailed));
            link.terminate(true);
            pump(holder.window_id, connection, frames, out_rx, link, feed).await;
            return Ending::Served;
        };
        // Nobody waits for the window any more: it is closed at once.
        if opened
            .send(Ok(Opened {
                child_pid,
                ack_step,
            }))
            .is_err()
        {
            link.terminate(true);
        }
        pump(holder.window_id, connection, frames, out_rx, link, feed).await;
        Ending::Served
    })
}

/// Why the daemon was not let in, and the connection when the holder may
/// have started the child on it.
struct Refusal {
    error: PtyError,
    let_in: Option<Connection>,
}

impl From<PtyError> for Refusal {
    fn from(error: PtyError) -> Self {
        Self {
            error,
            let_in: None,
        }
    }
}

/// Connects to the holder's endpoint, checks the holder serves it, and is let
/// in: `Hello`, then `Replay` once welcomed; returns the child's pid the
/// `Welcome` names and the ack step its window makes. A holder that could not
/// start the child answers with why, then `Exited`, in place of a `Welcome`.
async fn handshake(
    holder: &HolderAt<'_>,
    endpoint: &str,
    holder_pid: u32,
) -> Result<(Connection, HolderFrameBuffer, Option<u32>, AckStep), Refusal> {
    let deadline = tokio::time::Instant::now() + HELLO_DEADLINE;
    let window_id = holder.window_id;
    for _ in 0..HELLO_ATTEMPTS {
        let ipc = PlatformIpc::default();
        let mut connection = ipc.connect(endpoint).await.map_err(|error| {
            tracing::warn!(window_id, %error, "cannot connect to the window's holder");
            PtyError::SpawnFailed
        })?;
        if ipc.server_process_id(&connection) != Some(holder_pid) {
            tracing::warn!(
                window_id,
                "the holder's endpoint is served by another process"
            );
            return Err(PtyError::SpawnFailed.into());
        }
        let hello = encode_holder_frame(&HolderFrame::Hello {
            protocol: HOLDER_PROTOCOL,
            daemon_boot: holder.boot.to_owned(),
        });
        if connection.write_all(&hello).await.is_err() {
            continue;
        }
        let mut frames = HolderFrameBuffer::new();
        let Ok(first) =
            tokio::time::timeout_at(deadline, next_frame(&mut connection, &mut frames)).await
        else {
            tracing::warn!(window_id, "the window's holder did not answer in time");
            return Err(Refusal {
                error: PtyError::SpawnFailed,
                let_in: Some(connection),
            });
        };
        match first {
            Some(HolderFrame::Welcome {
                child_pid,
                ack_window,
                ..
            }) => {
                let replay = encode_holder_frame(&HolderFrame::Replay);
                if connection.write_all(&replay).await.is_err() {
                    return Err(Refusal {
                        error: PtyError::SpawnFailed,
                        let_in: Some(connection),
                    });
                }
                return Ok((connection, frames, child_pid, AckStep::new(ack_window)));
            }
            Some(HolderFrame::SpawnFailed {
                reason: HolderSpawnFailure::BundledConptyUnavailable,
            }) => {
                tracing::error!(
                    window_id,
                    "bundled ConPTY unavailable; refusing to open a window"
                );
                return Err(PtyError::BundledConptyUnavailable.into());
            }
            Some(
                HolderFrame::SpawnFailed {
                    reason: HolderSpawnFailure::Spawn,
                }
                | HolderFrame::Exited { .. },
            ) => {
                tracing::debug!(window_id, "the window's holder could not start the child");
                return Err(PtyError::SpawnFailed.into());
            }
            Some(HolderFrame::VersionRejected { protocol }) => {
                tracing::warn!(
                    window_id,
                    protocol,
                    "the window's holder speaks another version"
                );
                return Err(PtyError::SpawnFailed.into());
            }
            Some(other) => {
                tracing::warn!(window_id, frame = ?other, "the window's holder answered out of turn");
                return Err(Refusal {
                    error: PtyError::SpawnFailed,
                    let_in: Some(connection),
                });
            }
            // Closed without a word: tried again while the holder still waits.
            None if tokio::time::Instant::now() < deadline => {}
            None => break,
        }
    }
    tracing::warn!(window_id, "the window's holder did not let the daemon in");
    Err(PtyError::SpawnFailed.into())
}

/// Asks a holder that let the daemon in, for a window that will not open, to
/// end the child's tree, and waits for the connection to close, which the
/// holder does as it ends. A holder still there at `limit` ([`RELEASE_LIMIT`]
/// in production) is killed instead: the kill is only the bound, since a
/// killed holder cannot end the child's tree on Unix.
async fn give_up(mut connection: Connection, limit: Duration) -> Ending {
    let terminate = encode_holder_frame(&HolderFrame::Terminate { tree: true });
    let closed = tokio::time::timeout(limit, async {
        if connection.write_all(&terminate).await.is_err() {
            return;
        }
        let mut chunk = [0u8; 4096];
        while matches!(connection.read(&mut chunk).await, Ok(read) if read > 0) {}
    })
    .await;
    if closed.is_ok() {
        Ending::Served
    } else {
        Ending::Refused
    }
}

/// The next whole frame, or `None` once the connection ended or broke.
async fn next_frame(
    connection: &mut (impl AsyncReadExt + Unpin),
    frames: &mut HolderFrameBuffer,
) -> Option<HolderFrame> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(frame) = frames.next_frame().ok()? {
            return Some(frame);
        }
        let read = connection.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        frames.push(&chunk[..read]);
    }
}

/// Carries frames both ways until the child's exit is known or the
/// connection ends, then lets go of the connection.
async fn pump(
    window_id: &str,
    connection: Connection,
    mut frames: HolderFrameBuffer,
    mut out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    link: &Link,
    feed: &Sink,
) {
    let (mut read, mut write) = tokio::io::split(connection);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut sending = true;
    'connection: loop {
        loop {
            match frames.next_frame() {
                Ok(Some(frame)) => match handle(frame, link, feed) {
                    None => {}
                    Some(End::Exited) => return,
                    Some(End::Displaced) => {
                        tracing::warn!(window_id, "another daemon took the window's holder over");
                        feed.end(None);
                        return;
                    }
                },
                Ok(None) => break,
                Err(_) => break 'connection,
            }
        }
        tokio::select! {
            read_result = read.read(&mut chunk) => match read_result {
                Ok(0) | Err(_) => break,
                Ok(n) => frames.push(&chunk[..n]),
            },
            bytes = out_rx.recv(), if sending => match bytes {
                Some(bytes) => {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                None => sending = false,
            },
        }
    }
    tracing::warn!(
        window_id,
        "the connection to the window's holder was lost; the window is treated as exited"
    );
    feed.end(None);
}

enum End {
    Exited,
    Displaced,
}

fn handle(frame: HolderFrame, link: &Link, feed: &Sink) -> Option<End> {
    match frame {
        HolderFrame::Output { seq, bytes } => {
            feed.output(seq, bytes);
        }
        HolderFrame::Exited { code } => {
            feed.end(code);
            return Some(End::Exited);
        }
        HolderFrame::Written { id } => link.answer_write(id, true),
        HolderFrame::WriteFailed { id, .. } => link.answer_write(id, false),
        HolderFrame::Resized { .. } => link.answer_resize(true),
        HolderFrame::ResizeFailed { .. } => link.answer_resize(false),
        HolderFrame::Displaced => return Some(End::Displaced),
        HolderFrame::Hello { .. }
        | HolderFrame::Terminate { .. }
        | HolderFrame::VersionRejected { .. }
        | HolderFrame::Welcome { .. }
        | HolderFrame::Replay
        | HolderFrame::Checkpoint { .. }
        | HolderFrame::Ack { .. }
        | HolderFrame::StoreCheckpoint { .. }
        | HolderFrame::Write { .. }
        | HolderFrame::Resize { .. }
        | HolderFrame::SpawnFailed { .. } => {}
    }
    None
}

/// Removes the record of a holder this daemon killed. It never wrote an exit
/// file: a provisional holder writes none.
fn remove_holder_record(holders_dir: &Path, window_id: &str) {
    let Ok(path) = holder_record_path(holders_dir, window_id) else {
        return;
    };
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => tracing::warn!(
            %error,
            path = %path.display(),
            "cannot remove a window holder's record"
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use yagura_os::PeerAuth;

    use super::*;

    /// A safety bound only: reaching it fails the test.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        HolderReadTerminateTree,
        HolderClosed,
        GiveUpReturned,
    }

    /// Plays a holder that takes the daemon's connection and answers the
    /// `Hello` out of turn.
    async fn accept_and_answer_out_of_turn(
        mut listener: <PlatformIpc as IpcTransport>::Listener,
    ) -> (<PlatformIpc as PeerAuth>::Connection, HolderFrameBuffer) {
        let ipc = PlatformIpc::default();
        let mut connection = ipc.accept(&mut listener).await.expect("accept the daemon");
        let mut frames = HolderFrameBuffer::new();
        let hello = next_frame(&mut connection, &mut frames).await;
        assert!(
            matches!(hello, Some(HolderFrame::Hello { .. })),
            "the daemon opens with a Hello, got {hello:?}"
        );
        connection
            .write_all(&encode_holder_frame(&HolderFrame::Replay))
            .await
            .expect("answer out of turn");
        (connection, frames)
    }

    /// Plays a holder that answers the `Hello` out of turn, then reads frames
    /// until a `Terminate` and closes the connection, as a holder ending the
    /// child's tree does.
    async fn holder_answering_out_of_turn(
        listener: <PlatformIpc as IpcTransport>::Listener,
        steps: Arc<Mutex<Vec<Step>>>,
    ) {
        let (mut connection, mut frames) = accept_and_answer_out_of_turn(listener).await;
        loop {
            match next_frame(&mut connection, &mut frames).await {
                Some(HolderFrame::Terminate { tree }) => {
                    assert!(tree, "the daemon asks to end the child's tree");
                    steps.lock().unwrap().push(Step::HolderReadTerminateTree);
                    break;
                }
                Some(_) => {}
                None => panic!("the connection ended before a Terminate"),
            }
        }
        steps.lock().unwrap().push(Step::HolderClosed);
        drop(connection);
    }

    /// Binds a fresh endpoint for a played holder.
    async fn bind_holder_endpoint(tag: &str) -> (String, <PlatformIpc as IpcTransport>::Listener) {
        let endpoint = yagura_test_support::ipc::unique_endpoint_name("holder-link", tag);
        let ipc = PlatformIpc::default();
        let security = ipc.endpoint_security().expect("endpoint security");
        let listener = ipc.bind(&endpoint, security).await.expect("bind");
        (endpoint, listener)
    }

    /// Lets the daemon fail its handshake with the holder at `endpoint` and
    /// gives up on it with `limit`.
    async fn handshake_then_give_up(endpoint: &str, limit: Duration) -> Ending {
        let holders_dir = PathBuf::from("unused-holders-dir");
        let holder = HolderAt {
            holders_dir: &holders_dir,
            window_id: "w-out-of-turn",
            daemon_endpoint: "unused-daemon-endpoint",
            boot: "boot-out-of-turn",
        };
        tokio::time::timeout(HANG_GUARD, async {
            let refusal = match handshake(&holder, endpoint, std::process::id()).await {
                Ok(_) => panic!("an out-of-turn answer must not let the daemon in"),
                Err(refusal) => refusal,
            };
            assert!(matches!(refusal.error, PtyError::SpawnFailed));
            let connection = refusal
                .let_in
                .expect("the holder may have started the child: the connection is handed on");
            give_up(connection, limit).await
        })
        .await
        .expect("HANG_GUARD")
    }

    #[tokio::test]
    async fn a_holder_that_let_the_daemon_in_and_then_answered_out_of_turn_is_asked_to_end_and_not_killed()
     {
        let (endpoint, listener) = bind_holder_endpoint("out-of-turn").await;
        let steps = Arc::new(Mutex::new(Vec::new()));
        let holder_task = tokio::spawn(holder_answering_out_of_turn(listener, Arc::clone(&steps)));

        let ending = handshake_then_give_up(&endpoint, RELEASE_LIMIT).await;
        steps.lock().unwrap().push(Step::GiveUpReturned);
        tokio::time::timeout(HANG_GUARD, holder_task)
            .await
            .expect("HANG_GUARD")
            .expect("the holder's side");

        assert!(
            matches!(ending, Ending::Served),
            "a holder that closes after the Terminate is not killed"
        );
        assert_eq!(
            *steps.lock().unwrap(),
            [
                Step::HolderReadTerminateTree,
                Step::HolderClosed,
                Step::GiveUpReturned
            ],
            "give_up returns only once the holder closed the connection"
        );
    }

    /// A holder that let the daemon in and never closes the connection,
    /// whatever it is asked, is refused once the limit passes: it is killed
    /// rather than waited for. It holds the connection until `give_up` has
    /// returned, so the outcome does not depend on how fast either side runs.
    #[tokio::test]
    async fn a_holder_that_never_closes_once_asked_to_end_is_killed_at_the_limit() {
        let (endpoint, listener) = bind_holder_endpoint("never-closes").await;
        let steps = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = oneshot::channel::<()>();
        let holder_task = tokio::spawn({
            let steps = Arc::clone(&steps);
            async move {
                let (connection, _frames) = accept_and_answer_out_of_turn(listener).await;
                let _ = released.await;
                steps.lock().unwrap().push(Step::HolderClosed);
                drop(connection);
            }
        });

        let ending = handshake_then_give_up(&endpoint, Duration::from_millis(100)).await;
        steps.lock().unwrap().push(Step::GiveUpReturned);
        let _ = release.send(());
        tokio::time::timeout(HANG_GUARD, holder_task)
            .await
            .expect("HANG_GUARD")
            .expect("the holder's side");

        assert!(
            matches!(ending, Ending::Refused),
            "a holder that does not close by the limit is killed"
        );
        assert_eq!(
            *steps.lock().unwrap(),
            [Step::GiveUpReturned, Step::HolderClosed],
            "give_up returns while the holder still holds the connection"
        );
    }
}
