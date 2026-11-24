//! The daemon's end of one window's holder: starts it, or takes back one a
//! previous daemon of the instance left running, lets itself in, and carries
//! the window's screen copies, view frames, writes, requests and end over the
//! holder's endpoint on a thread and runtime of its own, so a blocked caller
//! runtime never stops them.
//!
//! The link takes the holder's frames in the order they were sent, one at a
//! time: a copy's rows are held until its close, checked, and applied at once,
//! so readers only ever see whole copies, and everything the holder sent
//! before an answer has been taken in when that answer is handed on.
//!
//! The link's thread is the only owner of the holder's process: it alone
//! waits for the holder's end, kills it, and hands what follows a holder's
//! end to [`finish_after_holder`]. It kills a holder only in [`end_unconnected_holder`]
//! (a window closed while it is not connected to its holder) and for a window
//! that never opened. A failed take-back itself ends nothing: only the user's
//! later close of that window does.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use yagura_os::{DetachedProcess, IpcTransport, PlatformIpc, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderEnvOp, HolderFault, HolderFrame, HolderFrameBuffer, HolderLaunch,
    HolderSpawnFailure, HolderViewNote, HolderViewOp, encode_holder_frame, encode_holder_launch,
    read_holder_record,
};
use yagura_protocol::{MAX_WINDOW_DIMENSION, Palette, ScreenPush, ScreenRows};

use super::views::ViewSlot;
use super::{ChildExit, LostNotice, PtyError, PublishedInput, lock_recover};
use crate::detector::SharedCopy;
use crate::holder_host::{LinkContext, TakeBackContext};
use crate::recovery::{HolderFiles, finish_after_holder};
use crate::window_table::RowScopes;

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

/// The most rows one copy may carry: a copy is no taller than the largest
/// window, and each of its rows comes once.
const MAX_COPY_ROWS: usize = MAX_WINDOW_DIMENSION as usize;

type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// The window a holder is started for.
pub(super) struct Launch {
    pub(super) window_id: String,
    pub(super) argv: Vec<String>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) env: Vec<HolderEnvOp>,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) history_rows: usize,
    pub(super) palette: Option<Palette>,
    pub(super) fault: Option<HolderFault>,
    /// What the holder keeps in memory and returns in its `Welcome`.
    pub(super) record: String,
    /// The scope names the window's row was written with, for the holder to
    /// ask for; `None` asks for none.
    pub(super) scopes: Option<RowScopes>,
}

/// A window a previous daemon of the instance left, whose holder runs.
pub(super) struct TakeBack {
    pub(super) window_id: String,
    /// The endpoint the holder's record names.
    pub(super) endpoint: String,
    /// The holder's process as its record names it.
    pub(super) holder_pid: u32,
    /// The holder's process, opened by its id and start; `None` when the OS
    /// would not tell whether it runs.
    pub(super) holder: Option<ProcessHandle>,
    pub(super) cols: u16,
    pub(super) rows: u16,
    /// Turns `true` once the daemon accepts connections, which a holder's
    /// check of whom it lets in needs.
    pub(super) begin: watch::Receiver<bool>,
    /// Given the launch record the holder's `Welcome` returns, before anything
    /// else of the holder is taken in; dropped uncalled as soon as the
    /// take-back fails or the window is closed first.
    pub(super) on_welcome: Box<dyn FnOnce(&str) + Send>,
    pub(super) lost: LostNotice,
}

/// Where the child's exit is published.
pub(super) struct ExitReport {
    pub(super) exit: Arc<OnceLock<ChildExit>>,
    pub(super) notify: watch::Sender<bool>,
}

impl ExitReport {
    /// The exit before the notification, so whoever is woken reads it. A
    /// missing code (the connection was lost, or the OS gave none) stays
    /// `None`: `-1` is a code a child can exit with. Only the first exit is
    /// published.
    fn publish(&self, code: Option<i32>) {
        if self.exit.set(ChildExit { code }).is_ok() {
            let _ = self.notify.send(true);
        }
    }
}

/// Where the link hands what it receives.
pub(super) struct Sink {
    /// The copy the window's readers share.
    pub(super) copy: SharedCopy,
    /// The input state of the copy last applied.
    pub(super) input: Arc<PublishedInput>,
    /// The output count of the copy last applied; sent each time it moves,
    /// and on a refused copy, whose loss readers are to learn of.
    pub(super) arrivals: watch::Sender<u64>,
    /// Where the attached view's frames go.
    pub(super) views: Arc<ViewSlot>,
    pub(super) exit: ExitReport,
}

impl Sink {
    /// Readers learn the screen they hold is gone until a whole copy comes:
    /// the window is not connected to its holder.
    fn lose_screen(&self) {
        self.copy.refuse();
        self.arrivals.send_modify(|_| {});
    }
}

/// What waits for the holder's answers, and the sizes a write's take-back
/// keys are counted at.
struct Pending {
    closed: bool,
    /// Set once the holder let the daemon in. Until then, a window being
    /// taken back has no screen to read: reads answer at once, while writes
    /// and resizes wait for the holder.
    connected: bool,
    next_id: u64,
    writes: HashMap<u64, std::sync::mpsc::SyncSender<bool>>,
    resize: Option<oneshot::Sender<bool>>,
    tails: HashMap<u64, oneshot::Sender<(String, bool)>>,
    selections: HashMap<u64, oneshot::Sender<Option<String>>>,
    settles: HashMap<u64, oneshot::Sender<()>>,
    /// The size the holder last confirmed: its `Welcome`, then each
    /// `Resized`.
    confirmed: (u16, u16),
    /// The child the holder's `Welcome` named running; `None` before the
    /// `Welcome`, when it named none, or when the child had already exited.
    child_pid: Option<u32>,
    /// The running child with the start the holder read before it could be
    /// reaped; `None` whenever the `Welcome` did not name both.
    anchor: Option<(u32, ProcessStart)>,
}

impl Pending {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            closed: false,
            connected: false,
            next_id: 0,
            writes: HashMap::new(),
            resize: None,
            tails: HashMap::new(),
            selections: HashMap::new(),
            settles: HashMap::new(),
            confirmed: (cols, rows),
            child_pid: None,
            anchor: None,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// The window's way to its holder; cheap to clone.
#[derive(Clone)]
pub(super) struct Link {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<Mutex<Pending>>,
    /// Sends one `Resize` at a time, the next only once the last is answered.
    resizing: Arc<tokio::sync::Mutex<()>>,
    /// Turns `true` once the window is told to end, which a link that is not
    /// connected to its holder learns from here rather than from a frame.
    ending: Arc<watch::Sender<bool>>,
}

impl Link {
    fn new(cols: u16, rows: u16) -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (out, out_rx) = mpsc::unbounded_channel();
        let link = Self {
            out,
            pending: Arc::new(Mutex::new(Pending::new(cols, rows))),
            resizing: Arc::default(),
            ending: Arc::new(watch::Sender::new(false)),
        };
        (link, out_rx)
    }

    fn send(&self, frame: &HolderFrame) {
        let _ = self.out.send(encode_holder_frame(frame));
    }

    /// Asks the holder to end the child (and with `tree`, its whole tree now),
    /// and then itself; nothing to do once the holder is gone.
    pub(super) fn terminate(&self, tree: bool) {
        self.ending.send_replace(true);
        self.send(&HolderFrame::Terminate { tree });
    }

    fn ending(&self) -> watch::Receiver<bool> {
        self.ending.subscribe()
    }

    /// Writes `bytes` to the child as a report: after every earlier write,
    /// not counted and not answered.
    pub(super) fn report(&self, bytes: Vec<u8>) {
        self.send(&HolderFrame::Report { bytes });
    }

    pub(super) fn set_palette(&self, palette: Palette) {
        self.send(&HolderFrame::SetPalette { palette });
    }

    pub(super) fn set_view_focus(&self, focused: bool) {
        self.send(&HolderFrame::SetViewFocus { focused });
    }

    pub(super) fn attach(&self, generation: u64) {
        self.send(&HolderFrame::Attach { generation });
    }

    pub(super) fn detach(&self, generation: u64) {
        self.send(&HolderFrame::Detach { generation });
    }

    pub(super) fn view_note(&self, generation: u64, note: HolderViewNote) {
        self.send(&HolderFrame::ViewNote { generation, note });
    }

    /// Asks the holder for an operation on the view without waiting for it.
    pub(super) fn view_op(&self, op: HolderViewOp) {
        let id = lock_recover(&self.pending).next_id();
        self.send(&HolderFrame::ViewOp { id, op });
    }

    /// The view's selected text, as the holder answers it; `None` when
    /// nothing is selected, the window is being taken back or the holder is
    /// gone.
    pub(super) async fn selected_text(&self) -> Option<String> {
        let (answer, answered) = oneshot::channel();
        {
            let mut pending = lock_recover(&self.pending);
            if pending.closed || !pending.connected {
                return None;
            }
            let id = pending.next_id();
            pending.selections.insert(id, answer);
            self.send(&HolderFrame::ViewOp {
                id,
                op: HolderViewOp::SelectedText,
            });
        }
        answered.await.ok().flatten()
    }

    /// The window's text as the holder answers it: empty while the window is
    /// being taken back, `None` once the holder is gone.
    pub(super) async fn read_tail(
        &self,
        max_lines: usize,
        max_bytes: usize,
    ) -> Option<(String, bool)> {
        let (answer, answered) = oneshot::channel();
        {
            let mut pending = lock_recover(&self.pending);
            if pending.closed {
                return None;
            }
            if !pending.connected {
                return Some((String::new(), false));
            }
            let id = pending.next_id();
            pending.tails.insert(id, answer);
            self.send(&HolderFrame::ReadTail {
                id,
                max_lines,
                max_bytes,
            });
        }
        answered.await.ok()
    }

    /// Resolves once the holder has taken in the output it read before the
    /// settle and the link has handed on every frame it sent before its
    /// answer; ends unanswered (an error) once the holder is gone. Resolves at
    /// once while the window is being taken back, when nothing of the holder
    /// has come yet.
    pub(super) fn settle(&self) -> oneshot::Receiver<()> {
        let (answer, answered) = oneshot::channel();
        let mut pending = lock_recover(&self.pending);
        if pending.closed {
            return answered;
        }
        if !pending.connected {
            let _ = answer.send(());
            return answered;
        }
        let id = pending.next_id();
        pending.settles.insert(id, answer);
        self.send(&HolderFrame::Settle { id });
        answered
    }

    /// Whether the holder's terminal took the size `cols` x `rows`. One size
    /// is asked for at a time.
    pub(super) async fn resize(&self, cols: u16, rows: u16) -> bool {
        let _one_at_a_time = self.resizing.lock().await;
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

    /// The size the holder last confirmed.
    pub(super) fn confirmed_size(&self) -> (u16, u16) {
        lock_recover(&self.pending).confirmed
    }

    /// The child the holder's `Welcome` named running.
    pub(super) fn child_pid(&self) -> Option<u32> {
        lock_recover(&self.pending).child_pid
    }

    /// The child the holder's `Welcome` named running, with its start.
    pub(super) fn child_anchor(&self) -> Option<(u32, ProcessStart)> {
        lock_recover(&self.pending).anchor.clone()
    }

    /// Writes `data` to the child, after every earlier write, and returns
    /// once the holder answered.
    fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        {
            let mut pending = lock_recover(&self.pending);
            if pending.closed {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            if data.is_empty() {
                return Ok(());
            }
            let id = pending.next_id();
            pending.writes.insert(id, answer);
            self.send(&HolderFrame::Write {
                id,
                bytes: data.to_vec(),
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

    /// The child's input, each write answered by the holder.
    pub(super) fn writer(&self) -> Writer {
        Writer(self.clone())
    }

    fn answer_write(&self, id: u64, written: bool) {
        if let Some(answer) = lock_recover(&self.pending).writes.remove(&id) {
            let _ = answer.send(written);
        }
    }

    /// `size` is the size the holder now has, `None` when it kept its size.
    fn answer_resize(&self, size: Option<(u16, u16)>) {
        let mut pending = lock_recover(&self.pending);
        if let Some(size) = size {
            pending.confirmed = size;
        }
        if let Some(answer) = pending.resize.take() {
            let _ = answer.send(size.is_some());
        }
    }

    fn answer_tail(&self, id: u64, text: String, truncated: bool) {
        if let Some(answer) = lock_recover(&self.pending).tails.remove(&id) {
            let _ = answer.send((text, truncated));
        }
    }

    fn answer_view_op(&self, id: u64, text: Option<String>) {
        if let Some(answer) = lock_recover(&self.pending).selections.remove(&id) {
            let _ = answer.send(text);
        }
    }

    fn answer_settle(&self, id: u64) {
        if let Some(answer) = lock_recover(&self.pending).settles.remove(&id) {
            let _ = answer.send(());
        }
    }

    fn welcomed(&self, cols: u16, rows: u16, welcomed: &Welcomed) {
        let mut pending = lock_recover(&self.pending);
        pending.child_pid = welcomed.running_child();
        pending.anchor = welcomed.anchor();
        pending.confirmed = (cols, rows);
        pending.connected = true;
    }

    /// Fails every request waiting for an answer, and every later one.
    fn close(&self) {
        let mut pending = lock_recover(&self.pending);
        pending.closed = true;
        pending.writes.clear();
        pending.resize = None;
        pending.tails.clear();
        pending.selections.clear();
        pending.settles.clear();
    }
}

/// The child's input through the holder.
pub(super) struct Writer(Link);

impl Writer {
    /// See `Link::write`.
    pub(super) fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.0.write(data)
    }
}

/// What the window learns once its holder let the daemon in and sent the
/// first copy of the screen; the child it named is the link's
/// [`Link::child_pid`].
pub(super) struct Opened;

/// A link whose holder is being started.
pub(super) struct Started {
    pub(super) link: Link,
    /// What the holder's `Welcome` told once it let the daemon in and its
    /// first copy was taken in, or why the window cannot open.
    pub(super) opened: oneshot::Receiver<Result<Opened, PtyError>>,
    /// `true` once the link has let go of the holder: the holder's process
    /// has ended, or a window closed with its holder unnameable was left.
    pub(super) released: watch::Receiver<bool>,
}

/// Starts the holder of `launch` on a thread of its own; what it sends goes
/// to `sink`, and `lost` marks the window's row once its holder is found gone
/// with the window still open.
pub(super) fn start(
    context: LinkContext,
    launch: Launch,
    sink: Sink,
    lost: LostNotice,
) -> Result<Started, PtyError> {
    let (link, out_rx) = Link::new(launch.cols, launch.rows);
    let (opened_tx, opened) = oneshot::channel();
    let (released_tx, released) = watch::channel(false);
    std::thread::Builder::new()
        .name("holder-link".to_owned())
        .spawn({
            let link = link.clone();
            move || {
                run(context, &launch, &link, out_rx, &sink, opened_tx, lost);
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

/// Starts taking back the window `take` names on a thread of its own: once
/// the daemon accepts connections, the thread lets itself into the holder
/// and goes on as for a window it opened. Returns the link at once; writes
/// and resizes made meanwhile wait for the holder.
pub(super) fn start_take_back(
    context: TakeBackContext,
    take: TakeBack,
    sink: Sink,
) -> Result<(Link, watch::Receiver<bool>), PtyError> {
    let (link, out_rx) = Link::new(take.cols, take.rows);
    let (released_tx, released) = watch::channel(false);
    std::thread::Builder::new()
        .name("holder-link".to_owned())
        .spawn({
            let link = link.clone();
            move || {
                run_take_back(&context, take, &link, out_rx, &sink);
                let _ = released_tx.send(true);
            }
        })
        .map_err(|error| {
            tracing::error!(%error, "cannot start the thread of a window's holder");
            PtyError::SpawnFailed
        })?;
    Ok((link, released))
}

/// Lets the daemon into a holder already serving `endpoint` from process
/// `holder_pid`, without starting one, on a thread of its own.
#[cfg(test)]
pub(super) fn start_over(
    endpoint: String,
    holder_pid: u32,
    (cols, rows): (u16, u16),
    sink: Sink,
) -> Result<Started, PtyError> {
    let (link, out_rx) = Link::new(cols, rows);
    let (opened_tx, opened) = oneshot::channel();
    let (released_tx, released) = watch::channel(false);
    std::thread::Builder::new()
        .name("holder-link".to_owned())
        .spawn({
            let link = link.clone();
            move || {
                let holders_dir = PathBuf::from("unused-holders-dir");
                let holder = HolderAt {
                    holders_dir: &holders_dir,
                    window_id: "played",
                    daemon_endpoint: "unused-daemon-endpoint",
                    boot: "played-boot",
                };
                serve(
                    &holder, &endpoint, holder_pid, &link, out_rx, &sink, opened_tx, false,
                );
                link.close();
                let _ = released_tx.send(true);
            }
        })
        .map_err(|_| PtyError::SpawnFailed)?;
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
    /// Another daemon took the holder over.
    Displaced,
    /// The connection was lost with the holder awaiting a successor: the
    /// window stays, not connected, until its holder ends or it is closed.
    Lost,
    /// The window never opened and the holder has not ended: it may still
    /// wait for a `Hello`, or overran [`RELEASE_LIMIT`]. It is killed, and its
    /// record and endpoint are removed here.
    Refused,
}

fn run(
    context: LinkContext,
    launch: &Launch,
    link: &Link,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Sink,
    opened: oneshot::Sender<Result<Opened, PtyError>>,
    lost: LostNotice,
) {
    let LinkContext {
        holder_exe,
        instance,
        boot,
        daemon_endpoint,
        holders_dir,
        stderr,
        log_file,
        awaits_successor,
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
    let files = HolderFiles {
        instance: &instance,
        holders_dir: &holders_dir,
        window_id,
    };
    let ready = holder.become_ready(&mut process, launch, log_file, awaits_successor);
    let not_ready = ready.is_err();
    let (ending, named) = match ready {
        Ok((endpoint, start)) => {
            // Named while it runs: once a holder dies, the daemon learns of it
            // only as its connection breaks, and by then the holder is an
            // unreaped child that no longer opens as running.
            let named = HolderProcess::name(process.id(), start.as_ref());
            let ending = serve(
                &holder,
                &endpoint,
                process.id(),
                link,
                out_rx,
                sink,
                opened,
                awaits_successor,
            );
            (ending, named)
        }
        Err(error) => {
            let _ = opened.send(Err(error));
            (Ending::Refused, HolderProcess::Unknown)
        }
    };
    link.close();
    let mut killed = matches!(ending, Ending::Refused);
    if matches!(ending, Ending::Lost) {
        let left = on_runtime(after_lost_connection(
            sink,
            &named,
            link.ending(),
            &files,
            lost,
        ));
        // A holder whose process cannot be named is still this daemon's
        // child, which only it can be killed through.
        killed = matches!(left, Some(AfterLoss::KillLeft));
    }
    if killed {
        let _ = process.kill();
    }
    let finishes = killed || matches!(ending, Ending::Served);
    match process.wait() {
        // Whatever the holder could say about it went to its log, if any.
        Ok(code) if not_ready => tracing::warn!(
            window_id,
            code = ?code,
            "the window's holder ended without becoming ready"
        ),
        Ok(Some(code)) if code != 0 => {
            tracing::debug!(window_id, code, "the window's holder ended with a failure");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(window_id, %error, "cannot wait for the window's holder"),
    }
    // A killed holder cannot end its tree or remove its files, nor can one
    // killed from outside while served; one that ended by itself leaves
    // nothing to do.
    if finishes {
        finish_after_holder(&files, keep);
    } else {
        drop(keep);
    }
    drop(live);
}

/// Takes back the window `take` names: waits for the daemon to accept
/// connections, lets itself into the holder and goes on as for a window it
/// opened. A take-back that fails sends the holder nothing and never ends
/// it; the window stays, not connected, until its holder ends or it is
/// closed. A window closed while it is being taken back ends its holder.
fn run_take_back(
    context: &TakeBackContext,
    take: TakeBack,
    link: &Link,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Sink,
) {
    let TakeBack {
        window_id,
        endpoint,
        holder_pid,
        holder,
        cols: _,
        rows: _,
        mut begin,
        on_welcome,
        lost,
    } = take;
    let holder = holder.map_or(HolderProcess::Unknown, |holder| {
        HolderProcess::Named(Arc::new(holder))
    });
    let at = HolderAt {
        holders_dir: &context.holders_dir,
        window_id: &window_id,
        daemon_endpoint: &context.daemon_endpoint,
        boot: &context.boot,
    };
    let files = HolderFiles {
        instance: &context.instance,
        holders_dir: &context.holders_dir,
        window_id: &window_id,
    };
    let taken = on_runtime(async {
        let mut ending = link.ending();
        let began = tokio::select! {
            began = begin.wait_for(|began| *began) => began.is_ok(),
            () = told_to_end(&mut ending) => false,
        };
        let handshake = if began {
            tokio::select! {
                handshake = take_back_handshake(&at, &endpoint, holder_pid) => Some(handshake),
                () = told_to_end(&mut ending) => None,
            }
        } else {
            told_to_end(&mut ending).await;
            None
        };
        match handshake {
            None => {
                // Dropped uncalled, so whoever waits on the window's token
                // learns at once that it will not come.
                drop(on_welcome);
                link.close();
                let ended = end_unconnected_holder(&holder, &files);
                if !ended {
                    tracing::warn!(
                        window_id = window_id.as_str(),
                        "a window closed while it was taken back has a holder that cannot be named; \
                         it is left running"
                    );
                }
                sink.exit.publish(None);
                if ended {
                    finish_after_holder(&files, ());
                }
            }
            Some(Err(reason)) => {
                drop(on_welcome);
                tracing::warn!(
                    window_id = window_id.as_str(),
                    reason,
                    "the window could not be taken back; it stays, unconnected, with its holder \
                     running until it is closed (a holder accepts only the daemon endpoint and version it was \
                     started with)"
                );
                link.close();
                stay_refused(sink, &holder, ending, &files, lost).await;
            }
            Some(Ok(welcomed)) => {
                // Before the record gives the token back, so a request the
                // token admits finds the link let in and the child named.
                let (cols, rows) = welcomed.size;
                link.welcomed(cols, rows, &welcomed);
                on_welcome(&welcomed.record);
                let mut reader = Reader {
                    window_id: &window_id,
                    link,
                    sink,
                    opening: None,
                    rows: Vec::new(),
                    row_count: 0,
                    outputs_fed: None,
                    exited: false,
                    late_copy_logged: false,
                };
                let end = pump(&mut reader, welcomed.connection, welcomed.frames, out_rx).await;
                link.close();
                match end {
                    PumpEnd::Displaced => {}
                    PumpEnd::Closed { exited: true } => {
                        // The holder closes once it is told to end, and then ends.
                        if let HolderProcess::Named(holder) = &holder {
                            let _ = holder_ended(holder).await;
                        }
                    }
                    PumpEnd::Closed { exited: false } => {
                        tracing::warn!(
                            window_id = window_id.as_str(),
                            "the connection to the window's holder was lost; the window stays, \
                             unconnected, until its holder ends or it is closed"
                        );
                        let left = after_lost_connection(sink, &holder, ending, &files, lost).await;
                        if matches!(left, AfterLoss::KillLeft) {
                            tracing::warn!(
                                window_id = window_id.as_str(),
                                "the closed window's holder cannot be named; it is left running"
                            );
                        }
                    }
                }
            }
        }
    });
    if taken.is_none() {
        link.close();
        sink.exit.publish(None);
    }
}

/// Runs `work` on a runtime of this thread's own; `None` when none can be
/// made.
fn on_runtime<F: std::future::Future>(work: F) -> Option<F::Output> {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => Some(runtime.block_on(work)),
        Err(error) => {
            tracing::error!(%error, "cannot start the runtime of a window's holder");
            None
        }
    }
}

/// Resolves once the window is told to end.
async fn told_to_end(ending: &mut watch::Receiver<bool>) {
    // A link nobody holds any more can be told nothing; that is its end.
    let _ = ending.wait_for(|ending| *ending).await;
}

/// A window's holder as the daemon can name it while not connected to it.
enum HolderProcess {
    /// Opened while it ran, so its end is seen however late it is waited for.
    Named(Arc<ProcessHandle>),
    /// It had already ended when it was looked for.
    Ended,
    /// Whether it runs cannot be told: its record carries no start, or the
    /// OS would not open it. Its end is never taken for granted.
    Unknown,
}

impl HolderProcess {
    /// The holder `pid` that started at `start`.
    fn name(pid: u32, start: Option<&ProcessStart>) -> Self {
        let Some(start) = start else {
            return Self::Unknown;
        };
        match ProcessHandle::open(pid, start) {
            Ok(Some(handle)) => Self::Named(Arc::new(handle)),
            Ok(None) => Self::Ended,
            Err(error) => {
                tracing::warn!(pid, %error, "cannot open a window holder's process");
                Self::Unknown
            }
        }
    }
}

/// Resolves once `holder` has ended; never when the wait cannot be made, since
/// a process whose end is not known is not taken for gone.
async fn holder_ended(holder: &Arc<ProcessHandle>) {
    let (ended, waited) = oneshot::channel();
    let started = std::thread::Builder::new()
        .name("holder-wait".to_owned())
        .spawn({
            let holder = Arc::clone(holder);
            move || {
                let _ = ended.send(holder.wait());
            }
        });
    if started.is_ok()
        && let Ok(Ok(())) = waited.await
    {
        return;
    }
    std::future::pending::<()>().await;
}

/// What ended a stretch in which a window is not connected to its holder.
enum Unconnected {
    HolderEnded,
    Closed,
}

async fn wait_unconnected(
    holder: &HolderProcess,
    ending: &mut watch::Receiver<bool>,
) -> Unconnected {
    let ended = async {
        match holder {
            HolderProcess::Named(holder) => holder_ended(holder).await,
            HolderProcess::Ended => {}
            HolderProcess::Unknown => std::future::pending().await,
        }
    };
    tokio::select! {
        () = ended => Unconnected::HolderEnded,
        () = told_to_end(ending) => Unconnected::Closed,
    }
}

/// A window whose take-back failed: the failure ends nothing, and the window
/// stays, unconnected. Once the holder ends by itself, the window's row is
/// marked lost; once the user closes the window, it is closed as any
/// unconnected window is. Either way the window then reads as exited, and
/// what follows the holder's end is handed on.
async fn stay_refused(
    sink: &Sink,
    holder: &HolderProcess,
    mut ending: watch::Receiver<bool>,
    files: &HolderFiles<'_>,
    lost: LostNotice,
) {
    let ended = match wait_unconnected(holder, &mut ending).await {
        Unconnected::HolderEnded => {
            lost();
            true
        }
        Unconnected::Closed => end_unconnected_holder(holder, files),
    };
    if !ended {
        tracing::warn!(
            window_id = files.window_id,
            "the closed window's holder cannot be named; it is left running"
        );
    }
    sink.exit.publish(None);
    if ended {
        finish_after_holder(files, ());
    }
}

/// What is left after [`after_lost_connection`].
enum AfterLoss {
    /// The holder has ended, and what follows its end is handed on.
    Ended,
    /// The window was closed and its holder could not be named again: the
    /// caller kills it if it can, and hands on what follows.
    KillLeft,
}

/// A window whose connection to a holder awaiting a successor was lost with
/// both still running: what was waiting fails, the window shows no screen
/// (so its status is `Unknown`), and it stays until its holder ends (its row
/// is marked lost) or it is closed (its holder is ended). Either way the
/// window then reads as exited, and what follows the holder's end is handed
/// on.
async fn after_lost_connection(
    sink: &Sink,
    holder: &HolderProcess,
    mut ending: watch::Receiver<bool>,
    files: &HolderFiles<'_>,
    lost: LostNotice,
) -> AfterLoss {
    sink.lose_screen();
    let left = match wait_unconnected(holder, &mut ending).await {
        Unconnected::HolderEnded => {
            lost();
            AfterLoss::Ended
        }
        Unconnected::Closed if end_unconnected_holder(holder, files) => AfterLoss::Ended,
        Unconnected::Closed => AfterLoss::KillLeft,
    };
    sink.exit.publish(None);
    if matches!(left, AfterLoss::Ended) {
        finish_after_holder(files, ());
    }
    left
}

/// Ends the holder of a window closed while the daemon is not connected to
/// it, and waits for it. On Unix the child's tree is ended first, from
/// outside it, only when no watcher may end it once the holder is killed
/// ([`sweeps_before_killing_holder`]): the record names none, or the one it
/// names is dead. On Windows the holder's job ends the child's tree with it.
/// Returns `true`, doing nothing, when the holder has already ended, and
/// `false`, doing nothing, when its process cannot be named.
/// The one place a window's holder is killed once its window opened.
fn end_unconnected_holder(holder: &HolderProcess, files: &HolderFiles<'_>) -> bool {
    let holder = match holder {
        HolderProcess::Named(holder) => holder,
        HolderProcess::Ended => return true,
        HolderProcess::Unknown => return false,
    };
    // Watched before anything can end it, so its end is not missed.
    let exit = holder.watch_exit();
    #[cfg(unix)]
    if let Ok(record) = read_holder_record(files.holders_dir, files.window_id)
        && sweeps_before_killing_holder(watcher_liveness(files.window_id, &record))
    {
        // No scope is killed by name: the holder runs, and the scope bound to
        // its own, if made, ends as it is killed.
        crate::recovery::end_recorded_tree(files.window_id, &record);
    }
    #[cfg(not(unix))]
    let _ = files;
    if let Err(error) = holder.kill() {
        tracing::warn!(%error, "cannot end a window's holder");
        return true;
    }
    if let Err(error) = exit.and_then(yagura_os::ExitWatch::wait) {
        tracing::warn!(%error, "cannot wait for a window's holder");
    }
    true
}

/// Whether an unconnected close ends the child's tree before it kills the
/// holder, given the liveness of the watcher the holder's record names
/// (`None` when it names none). A watcher that may live ends the tree once
/// its holder is gone, and the daemon sending meanwhile would run alongside
/// it; `Unknown` is never taken for dead.
#[cfg(unix)]
fn sweeps_before_killing_holder(watcher: Option<yagura_os::Liveness>) -> bool {
    matches!(watcher, None | Some(yagura_os::Liveness::Dead))
}

/// The liveness of the watcher `record` names; `None` when it names none. A
/// watcher named without a start that can be read cannot be told from
/// another process, so it reads as `Unknown`.
#[cfg(unix)]
fn watcher_liveness(
    window_id: &str,
    record: &yagura_protocol::holder::HolderRecord,
) -> Option<yagura_os::Liveness> {
    let pid = record.watcher_pid?;
    let liveness = record
        .watcher_start
        .as_deref()
        .and_then(|start| start.parse::<ProcessStart>().ok())
        .map_or(yagura_os::Liveness::Unknown, |start| {
            yagura_os::process_liveness(pid, &start)
        });
    if liveness == yagura_os::Liveness::Unknown {
        tracing::warn!(
            window_id,
            watcher_pid = pid,
            "whether a closed window's watcher runs cannot be told; its tree is left to it"
        );
    }
    Some(liveness)
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
    /// record names it; returns the endpoint the record names and the
    /// holder's start, when the record carries one.
    fn become_ready(
        &self,
        process: &mut DetachedProcess,
        launch: &Launch,
        log_file: Option<PathBuf>,
        awaits_successor: bool,
    ) -> Result<(String, Option<ProcessStart>), PtyError> {
        let encoded = encode_holder_launch(&HolderLaunch {
            argv: launch.argv.clone(),
            cwd: launch.cwd.clone(),
            env: launch.env.clone(),
            cols: launch.cols,
            rows: launch.rows,
            record: launch.record.clone(),
            hello_deadline_ms: u64::try_from(HELLO_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            holders_dir: self.holders_dir.to_path_buf(),
            daemon_endpoint: self.daemon_endpoint.to_owned(),
            awaits_successor,
            log_file,
            history_rows: launch.history_rows,
            palette: launch.palette.clone(),
            fault: launch.fault,
            scopes: launch.scopes.as_ref().map(RowScopes::launch),
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
                    "the window's holder ended before it was ready"
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
        let start = record
            .holder_start
            .as_deref()
            .and_then(|start| start.parse().ok());
        Ok((record.endpoint, start))
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
            let ready = std::io::Write::write_all(&mut stdin, &launch).and_then(|()| {
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

// Each argument is one part of the connection the window is served over.
#[allow(clippy::too_many_arguments)]
fn serve(
    holder: &HolderAt<'_>,
    endpoint: &str,
    holder_pid: u32,
    link: &Link,
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    sink: &Sink,
    opened: oneshot::Sender<Result<Opened, PtyError>>,
    awaits_successor: bool,
) -> Ending {
    let served = on_runtime(async {
        let welcomed = match handshake(holder, endpoint, holder_pid).await {
            Ok(welcomed) => welcomed,
            Err(Refusal { error, leftover }) => {
                let _ = opened.send(Err(error));
                return let_go(leftover, RELEASE_LIMIT).await;
            }
        };
        let (cols, rows) = welcomed.size;
        link.welcomed(cols, rows, &welcomed);
        let opening = match welcomed.child_pid {
            Some(_) => {
                if welcomed.running_child().is_some() && welcomed.anchor().is_none() {
                    tracing::warn!(
                        window_id = holder.window_id,
                        "the window's holder did not name its child's start; hooks and MCP \
                         bridges of this window are refused until it is opened again"
                    );
                }
                Some(Opening { opened })
            }
            None => {
                tracing::warn!(
                    window_id = holder.window_id,
                    "the window's holder named no child in its welcome"
                );
                let _ = opened.send(Err(PtyError::SpawnFailed));
                link.terminate(true);
                None
            }
        };
        let mut reader = Reader {
            window_id: holder.window_id,
            link,
            sink,
            opening,
            rows: Vec::new(),
            row_count: 0,
            outputs_fed: None,
            exited: false,
            late_copy_logged: false,
        };
        match pump(&mut reader, welcomed.connection, welcomed.frames, out_rx).await {
            PumpEnd::Displaced => Ending::Displaced,
            PumpEnd::Closed { exited: true } => Ending::Served,
            PumpEnd::Closed { exited: false } if awaits_successor => {
                tracing::warn!(
                    window_id = holder.window_id,
                    "the connection to the window's holder was lost; the window stays, \
                     unconnected, until its holder ends or it is closed"
                );
                reader.tell_opened();
                Ending::Lost
            }
            PumpEnd::Closed { exited: false } => {
                tracing::warn!(
                    window_id = holder.window_id,
                    "the connection to the window's holder was lost; the window is treated as exited"
                );
                reader.end(None);
                Ending::Served
            }
        }
    });
    served.unwrap_or(Ending::Refused)
}

/// Why the daemon was not let in, and what is left of the holder.
struct Refusal {
    error: PtyError,
    leftover: Leftover,
}

impl From<PtyError> for Refusal {
    fn from(error: PtyError) -> Self {
        Self {
            error,
            leftover: Leftover::Unknown,
        }
    }
}

/// What a holder that did not let the daemon in goes on to do.
enum Leftover {
    /// Not known, or it goes on waiting for a `Hello` (it closes a refused
    /// connection and stays): it is killed.
    Unknown,
    /// It let the daemon in and may have started the child on the
    /// connection: it is asked to end the child's tree.
    LetIn(Connection),
    /// It answered that it is ending (the child could not start): it closes
    /// the connection as it ends and removes its own files.
    Closing(Connection),
}

/// What a holder's `Welcome` told, with the connection it came on.
struct Welcomed {
    connection: Connection,
    frames: HolderFrameBuffer,
    child_pid: Option<u32>,
    /// The child's start as the holder's side read it before the child could
    /// be reaped; with `child_pid`, what names the window's child.
    child_start: Option<String>,
    exited: bool,
    size: (u16, u16),
    /// The launch record the holder keeps.
    record: String,
}

/// Connects to the holder's endpoint, checks the holder serves it, and is let
/// in with a `Hello`; returns what its `Welcome` told. A holder that could
/// not start the child answers with why, then `Exited`, in place of a
/// `Welcome`.
async fn handshake(
    holder: &HolderAt<'_>,
    endpoint: &str,
    holder_pid: u32,
) -> Result<Welcomed, Refusal> {
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
        if connection.write_all(&hello(holder)).await.is_err() {
            continue;
        }
        let mut frames = HolderFrameBuffer::new();
        let Ok(first) =
            tokio::time::timeout_at(deadline, next_frame(&mut connection, &mut frames)).await
        else {
            tracing::warn!(window_id, "the window's holder did not answer in time");
            return Err(Refusal {
                error: PtyError::SpawnFailed,
                leftover: Leftover::LetIn(connection),
            });
        };
        match first {
            Some(HolderFrame::Welcome {
                child_pid,
                child_start,
                cols,
                rows,
                record,
                exited,
                ..
            }) => {
                return Ok(Welcomed {
                    connection,
                    frames,
                    child_pid,
                    child_start,
                    exited,
                    size: (cols, rows),
                    record,
                });
            }
            Some(HolderFrame::SpawnFailed {
                reason: HolderSpawnFailure::BundledConptyUnavailable,
            }) => {
                tracing::error!(
                    window_id,
                    "bundled ConPTY unavailable; refusing to open a window"
                );
                return Err(Refusal {
                    error: PtyError::BundledConptyUnavailable,
                    leftover: Leftover::Closing(connection),
                });
            }
            Some(
                HolderFrame::SpawnFailed {
                    reason: HolderSpawnFailure::Spawn,
                }
                | HolderFrame::Exited { .. },
            ) => {
                tracing::debug!(window_id, "the window's holder could not start the child");
                return Err(Refusal {
                    error: PtyError::SpawnFailed,
                    leftover: Leftover::Closing(connection),
                });
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
                    leftover: Leftover::LetIn(connection),
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

impl Welcomed {
    /// The child the `Welcome` named, unless it had already exited by then.
    fn running_child(&self) -> Option<u32> {
        self.child_pid.filter(|_| !self.exited)
    }

    /// The running child and its start, when the `Welcome` named both; a
    /// start that does not read as one is none.
    fn anchor(&self) -> Option<(u32, ProcessStart)> {
        let pid = self.running_child()?;
        let start = self.child_start.as_deref()?.parse::<ProcessStart>().ok()?;
        Some((pid, start))
    }
}

fn hello(holder: &HolderAt<'_>) -> Vec<u8> {
    encode_holder_frame(&HolderFrame::Hello {
        protocol: HOLDER_PROTOCOL,
        daemon_boot: holder.boot.to_owned(),
    })
}

/// Lets the daemon into a holder a previous daemon of the instance left: one
/// connection and one `Hello`, waited on for as long as the holder takes,
/// since the holder serves a live window. Every answer but a `Welcome` fails
/// the take-back with why; the connection is then dropped, closing it with
/// nothing more sent.
async fn take_back_handshake(
    holder: &HolderAt<'_>,
    endpoint: &str,
    holder_pid: u32,
) -> Result<Welcomed, &'static str> {
    let ipc = PlatformIpc::default();
    let Ok(mut connection) = ipc.connect(endpoint).await else {
        return Err("the holder's endpoint cannot be connected to");
    };
    if ipc.server_process_id(&connection) != Some(holder_pid) {
        return Err("the holder's endpoint is served by another process");
    }
    if connection.write_all(&hello(holder)).await.is_err() {
        return Err("the holder closed the connection");
    }
    let mut frames = HolderFrameBuffer::new();
    match next_frame(&mut connection, &mut frames).await {
        Some(HolderFrame::Welcome {
            child_pid,
            child_start,
            cols,
            rows,
            record,
            exited,
            ..
        }) => Ok(Welcomed {
            connection,
            frames,
            child_pid,
            child_start,
            exited,
            size: (cols, rows),
            record,
        }),
        Some(HolderFrame::VersionRejected { .. }) => Err("the holder speaks another version"),
        Some(_) => Err("the holder answered out of turn"),
        None => Err("the holder closed the connection without a word"),
    }
}

/// Lets go of a holder whose window will not open. One that let the daemon
/// in is asked to end the child's tree; one that is ending is only waited
/// for. Either closes the connection as it ends, which is waited for; a holder
/// still there at `limit` ([`RELEASE_LIMIT`] in production) is killed
/// instead. The kill is only the bound: a killed holder cannot end the child's
/// tree on Unix, nor remove its own files.
async fn let_go(leftover: Leftover, limit: Duration) -> Ending {
    let (mut connection, ask) = match leftover {
        Leftover::Unknown => return Ending::Refused,
        Leftover::LetIn(connection) => (
            connection,
            Some(encode_holder_frame(&HolderFrame::Terminate { tree: true })),
        ),
        Leftover::Closing(connection) => (connection, None),
    };
    let closed = tokio::time::timeout(limit, async {
        if let Some(ask) = ask {
            // A failed ask leaves the close to wait for, as an answered one does.
            let _ = connection.write_all(&ask).await;
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

/// How [`pump`] ended.
enum PumpEnd {
    /// Another daemon took the holder over; the exit was published.
    Displaced,
    /// The connection ended; `exited` tells whether the holder reported the
    /// child's exit first, after which the end is the holder's own.
    Closed { exited: bool },
}

/// Carries frames both ways until the connection ends; the holder closes it
/// once it is told to end. Only a frame whose framing is broken ends it from
/// this side; a failed write stops the sending, not the reading. After the
/// child's exit the connection is kept, so the holder can still be told to
/// end.
async fn pump(
    reader: &mut Reader<'_>,
    connection: Connection,
    mut frames: HolderFrameBuffer,
    mut out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) -> PumpEnd {
    let (mut read, mut write) = tokio::io::split(connection);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut sending = true;
    'connection: loop {
        loop {
            match frames.next_frame() {
                Ok(Some(frame)) => {
                    if reader.handle(frame) == Flow::Displaced {
                        tracing::warn!(
                            window_id = reader.window_id,
                            "another daemon took the window's holder over"
                        );
                        reader.end(None);
                        return PumpEnd::Displaced;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(
                        window_id = reader.window_id,
                        %error,
                        "the window's holder sent a broken frame"
                    );
                    break 'connection;
                }
            }
        }
        tokio::select! {
            read_result = read.read(&mut chunk) => match read_result {
                Ok(0) | Err(_) => break,
                Ok(n) => frames.push(&chunk[..n]),
            },
            bytes = out_rx.recv(), if sending => match bytes {
                Some(bytes) => {
                    // The holder may have closed after frames still to be
                    // read, such as its `Exited`; only the read side ends.
                    if write.write_all(&bytes).await.is_err() {
                        sending = false;
                        out_rx.close();
                    }
                }
                None => sending = false,
            },
        }
    }
    PumpEnd::Closed {
        exited: reader.exited,
    }
}

/// The window still waiting to learn it opened.
struct Opening {
    opened: oneshot::Sender<Result<Opened, PtyError>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Displaced,
}

/// Takes the holder's frames in, in the order they came.
struct Reader<'a> {
    window_id: &'a str,
    link: &'a Link,
    sink: &'a Sink,
    opening: Option<Opening>,
    /// The rows of the copy not closed yet.
    rows: Vec<ScreenRows>,
    row_count: usize,
    /// The output count of the copy last applied.
    outputs_fed: Option<u64>,
    exited: bool,
    late_copy_logged: bool,
}

impl Reader<'_> {
    fn handle(&mut self, frame: HolderFrame) -> Flow {
        match frame {
            HolderFrame::ScreenRows(rows) => self.hold_rows(rows),
            HolderFrame::ScreenPush(push) => self.close_copy(push),
            HolderFrame::Exited { code } => {
                self.rows.clear();
                self.row_count = 0;
                self.end(code);
            }
            HolderFrame::Written { id } => self.link.answer_write(id, true),
            HolderFrame::WriteFailed { id, .. } => self.link.answer_write(id, false),
            HolderFrame::Resized { cols, rows } => self.link.answer_resize(Some((cols, rows))),
            HolderFrame::ResizeFailed { .. } => self.link.answer_resize(None),
            HolderFrame::ViewFrame { generation, frame } => {
                if self.sink.views.deliver(generation, frame) {
                    self.link.detach(generation);
                }
            }
            HolderFrame::ViewOpDone { id, text, .. } => self.link.answer_view_op(id, text),
            HolderFrame::Tail {
                id,
                text,
                truncated,
            } => self.link.answer_tail(id, text, truncated),
            HolderFrame::Settled { id } => self.link.answer_settle(id),
            HolderFrame::Displaced => return Flow::Displaced,
            HolderFrame::Hello { .. }
            | HolderFrame::Terminate { .. }
            | HolderFrame::VersionRejected { .. }
            | HolderFrame::Welcome { .. }
            | HolderFrame::Write { .. }
            | HolderFrame::Resize { .. }
            | HolderFrame::SpawnFailed { .. }
            | HolderFrame::PushAck { .. }
            | HolderFrame::ScreenResync
            | HolderFrame::Report { .. }
            | HolderFrame::SetPalette { .. }
            | HolderFrame::SetViewFocus { .. }
            | HolderFrame::Attach { .. }
            | HolderFrame::Detach { .. }
            | HolderFrame::ViewNote { .. }
            | HolderFrame::ViewOp { .. }
            | HolderFrame::ReadTail { .. }
            | HolderFrame::Settle { .. } => {}
        }
        Flow::Continue
    }

    /// Holds a copy's rows until its close. Rows past what any copy can
    /// carry are not kept; their copy is refused at its close.
    fn hold_rows(&mut self, rows: ScreenRows) {
        if self.exited {
            return;
        }
        self.row_count = self.row_count.saturating_add(rows.lines.len());
        if self.row_count <= MAX_COPY_ROWS {
            self.rows.push(rows);
        }
    }

    /// Checks the copy `push` closes and applies it whole, or refuses it: a
    /// refused copy loses the window's screen until a whole one comes, which
    /// the holder is asked for in place of the acknowledgement. A copy after
    /// the exit is not applied.
    fn close_copy(&mut self, push: ScreenPush) {
        let rows = std::mem::take(&mut self.rows);
        let row_count = std::mem::replace(&mut self.row_count, 0);
        if self.exited {
            if !self.late_copy_logged {
                self.late_copy_logged = true;
                tracing::warn!(
                    window_id = self.window_id,
                    "the window's holder sent a copy of the screen after the exit; it is not applied"
                );
            }
            return;
        }
        let number = push.number;
        let outputs_fed = push.outputs_fed;
        let applied = if row_count > MAX_COPY_ROWS {
            self.sink.copy.refuse();
            Err(crate::detector::CopyRefused::BadRow)
        } else {
            self.sink.copy.apply(rows, push.clone())
        };
        match applied {
            Ok(()) => {
                self.sink.input.publish(&push);
                if self.outputs_fed != Some(outputs_fed) {
                    self.outputs_fed = Some(outputs_fed);
                    self.sink.arrivals.send_replace(outputs_fed);
                }
                self.link.send(&HolderFrame::PushAck { number });
            }
            Err(refused) => {
                tracing::warn!(
                    window_id = self.window_id,
                    %refused,
                    "a copy of the window's screen was refused; the screen is lost until a whole one comes"
                );
                self.sink.arrivals.send_modify(|_| {});
                self.link.send(&HolderFrame::ScreenResync);
            }
        }
        self.tell_opened();
    }

    /// Publishes the child's exit, once; the window that waited to open
    /// opens on an exited child.
    fn end(&mut self, code: Option<i32>) {
        self.exited = true;
        self.sink.exit.publish(code);
        self.tell_opened();
    }

    fn tell_opened(&mut self) {
        let Some(Opening { opened }) = self.opening.take() else {
            return;
        };
        // Nobody waits for the window any more: it is closed at once.
        if opened.send(Ok(Opened)).is_err() {
            self.link.terminate(true);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Mutex;

    use yagura_os::PeerAuth;

    use super::*;

    /// A safety bound only: reaching it fails the test.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        HolderReadTerminateTree,
        HolderClosed,
        LetGoReturned,
    }

    /// How a played holder answers the daemon's `Hello`.
    #[derive(Debug, Clone, Copy)]
    enum Answer {
        /// A frame out of turn: the holder let the daemon in and may have
        /// started the child.
        OutOfTurn,
        /// Why the child could not start, then `Exited`, as a holder ending by
        /// itself answers.
        SpawnFailed,
    }

    /// Plays a holder that takes the daemon's connection and answers the
    /// `Hello` with `answer`.
    async fn accept_and_answer(
        mut listener: <PlatformIpc as IpcTransport>::Listener,
        answer: Answer,
    ) -> (<PlatformIpc as PeerAuth>::Connection, HolderFrameBuffer) {
        let ipc = PlatformIpc::default();
        let mut connection = ipc.accept(&mut listener).await.expect("accept the daemon");
        let mut frames = HolderFrameBuffer::new();
        let hello = next_frame(&mut connection, &mut frames).await;
        assert!(
            matches!(hello, Some(HolderFrame::Hello { .. })),
            "the daemon opens with a Hello, got {hello:?}"
        );
        let reply = match answer {
            Answer::OutOfTurn => encode_holder_frame(&HolderFrame::Written { id: 0 }),
            Answer::SpawnFailed => [
                encode_holder_frame(&HolderFrame::SpawnFailed {
                    reason: HolderSpawnFailure::Spawn,
                }),
                encode_holder_frame(&HolderFrame::Exited { code: None }),
            ]
            .concat(),
        };
        connection
            .write_all(&reply)
            .await
            .expect("answer the Hello");
        (connection, frames)
    }

    /// Plays a holder that answers the `Hello` out of turn, then reads frames
    /// until a `Terminate` and closes the connection, as a holder ending the
    /// child's tree does.
    async fn holder_answering_out_of_turn(
        listener: <PlatformIpc as IpcTransport>::Listener,
        steps: Arc<Mutex<Vec<Step>>>,
    ) {
        let (mut connection, mut frames) = accept_and_answer(listener, Answer::OutOfTurn).await;
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
    pub(crate) async fn bind_holder_endpoint(
        tag: &str,
    ) -> (String, <PlatformIpc as IpcTransport>::Listener) {
        let endpoint = yagura_test_support::ipc::unique_endpoint_name("holder-link", tag);
        let ipc = PlatformIpc::default();
        let security = ipc.endpoint_security().expect("endpoint security");
        let listener = ipc.bind(&endpoint, security).await.expect("bind");
        (endpoint, listener)
    }

    /// Lets the daemon fail its handshake with the holder at `endpoint`,
    /// which answers with `answer`, and lets go of it with `limit`.
    async fn handshake_then_let_go(endpoint: &str, answer: Answer, limit: Duration) -> Ending {
        let holders_dir = PathBuf::from("unused-holders-dir");
        let holder = HolderAt {
            holders_dir: &holders_dir,
            window_id: "w-refused",
            daemon_endpoint: "unused-daemon-endpoint",
            boot: "boot-refused",
        };
        tokio::time::timeout(HANG_GUARD, async {
            let refusal = match handshake(&holder, endpoint, std::process::id()).await {
                Ok(_) => panic!("{answer:?} must not let the daemon in"),
                Err(refusal) => refusal,
            };
            assert!(matches!(refusal.error, PtyError::SpawnFailed));
            match (answer, &refusal.leftover) {
                (Answer::OutOfTurn, Leftover::LetIn(_))
                | (Answer::SpawnFailed, Leftover::Closing(_)) => {}
                _ => panic!(
                    "{answer:?}: the connection is handed on, to ask the holder to end the child's \
                     tree when it may have started it and to wait for one that is ending"
                ),
            }
            let_go(refusal.leftover, limit).await
        })
        .await
        .expect("HANG_GUARD")
    }

    /// Closing a window while not connected: the daemon ends the child's tree before it
    /// kills the holder only when no watcher may still end it, that is when the record
    /// names none or the one it names is dead. A watcher the OS would not tell about is
    /// never taken for dead.
    #[cfg(unix)]
    #[test]
    fn an_unconnected_close_ends_the_tree_first_only_without_a_watcher_that_may_live() {
        use yagura_os::Liveness;

        assert!(
            !sweeps_before_killing_holder(Some(Liveness::Alive)),
            "a living watcher ends the tree itself once its holder is killed"
        );
        assert!(
            !sweeps_before_killing_holder(Some(Liveness::Unknown)),
            "a watcher that may live is not taken for dead"
        );
        assert!(
            sweeps_before_killing_holder(Some(Liveness::Dead)),
            "with a dead watcher, nothing else ends the tree once the holder is killed"
        );
        assert!(
            sweeps_before_killing_holder(None),
            "a record naming no watcher leaves the tree to the daemon"
        );
    }

    #[tokio::test]
    async fn a_holder_that_let_the_daemon_in_and_then_answered_out_of_turn_is_asked_to_end_and_not_killed()
     {
        let (endpoint, listener) = bind_holder_endpoint("out-of-turn").await;
        let steps = Arc::new(Mutex::new(Vec::new()));
        let holder_task = tokio::spawn(holder_answering_out_of_turn(listener, Arc::clone(&steps)));

        let ending = handshake_then_let_go(&endpoint, Answer::OutOfTurn, RELEASE_LIMIT).await;
        steps.lock().unwrap().push(Step::LetGoReturned);
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
                Step::LetGoReturned
            ],
            "the daemon lets go only once the holder closed the connection"
        );
    }

    /// A holder that could not start the child ends by itself and closes the
    /// connection as it does; it is waited for, not killed, so it removes its
    /// own files.
    #[tokio::test]
    async fn a_holder_that_could_not_start_the_child_is_waited_for_and_not_killed() {
        let (endpoint, listener) = bind_holder_endpoint("spawn-failed").await;
        let steps = Arc::new(Mutex::new(Vec::new()));
        let holder_task = tokio::spawn({
            let steps = Arc::clone(&steps);
            async move {
                let (connection, _frames) = accept_and_answer(listener, Answer::SpawnFailed).await;
                steps.lock().unwrap().push(Step::HolderClosed);
                drop(connection);
            }
        });

        let ending = handshake_then_let_go(&endpoint, Answer::SpawnFailed, RELEASE_LIMIT).await;
        steps.lock().unwrap().push(Step::LetGoReturned);
        tokio::time::timeout(HANG_GUARD, holder_task)
            .await
            .expect("HANG_GUARD")
            .expect("the holder's side");

        assert!(
            matches!(ending, Ending::Served),
            "a holder ending by itself is not killed"
        );
        assert_eq!(
            *steps.lock().unwrap(),
            [Step::HolderClosed, Step::LetGoReturned],
            "the daemon lets go only once the holder closed the connection"
        );
    }

    /// A holder that never closes the connection, whether it let the daemon
    /// in or answered that it is ending, is refused once the limit passes: it
    /// is killed rather than waited for. It holds the connection until the
    /// daemon has let go, so the outcome does not depend on how fast either
    /// side runs.
    #[tokio::test]
    async fn a_holder_that_never_closes_once_asked_to_end_is_killed_at_the_limit() {
        for answer in [Answer::OutOfTurn, Answer::SpawnFailed] {
            let (endpoint, listener) = bind_holder_endpoint("never-closes").await;
            let steps = Arc::new(Mutex::new(Vec::new()));
            let (release, released) = oneshot::channel::<()>();
            let holder_task = tokio::spawn({
                let steps = Arc::clone(&steps);
                async move {
                    let (connection, _frames) = accept_and_answer(listener, answer).await;
                    let _ = released.await;
                    steps.lock().unwrap().push(Step::HolderClosed);
                    drop(connection);
                }
            });

            let ending = handshake_then_let_go(&endpoint, answer, Duration::from_millis(100)).await;
            steps.lock().unwrap().push(Step::LetGoReturned);
            let _ = release.send(());
            tokio::time::timeout(HANG_GUARD, holder_task)
                .await
                .expect("HANG_GUARD")
                .expect("the holder's side");

            assert!(
                matches!(ending, Ending::Refused),
                "a holder that does not close by the limit is killed ({answer:?})"
            );
            assert_eq!(
                *steps.lock().unwrap(),
                [Step::LetGoReturned, Step::HolderClosed],
                "the daemon lets go while the holder still holds the connection ({answer:?})"
            );
        }
    }

    pub(in crate::pty) mod played {
        //! A holder played by the test in its own process, over the holder's
        //! endpoint, so the daemon's side of the link is driven frame by frame.

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use yagura_os::PeerAuth;
        use yagura_protocol::holder::{
            HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, encode_holder_frame,
        };
        use yagura_protocol::{
            DetectedStatus, InputModes, ScreenCell, ScreenCursor, ScreenLine, ScreenPush,
            ScreenRows, UnknownReason,
        };

        use super::{HANG_GUARD, bind_holder_endpoint};
        use crate::config::DaemonConfig;
        use crate::detector::DetectionEngine;
        use crate::detector::definition::parse_definition;
        use crate::detector::reading;
        use crate::pty::PtySession;
        use yagura_os::{IpcTransport, PlatformIpc};

        const COLS: u16 = 80;
        const ROWS: u16 = 24;

        /// Judges `PLAYED-PROMPT>` idle at once, so a status follows each
        /// reading without a clock.
        const PROMPT_DEF: &str = r#"
schema_version = 1
name = "played-holder"

[debounce]
confirm_ms = 0

[[rules]]
status = "idle"
pattern = 'PLAYED-PROMPT>'
priority = 1
region = "bottom"
"#;

        /// The window's session over the holder this test plays at
        /// `endpoint`, which this process serves.
        async fn played_session(endpoint: &str) -> PtySession {
            PtySession::over_holder_endpoint(endpoint, std::process::id())
                .await
                .expect("the played holder lets the daemon in")
        }

        pub(crate) fn line(index: u16, text: &str) -> ScreenLine {
            ScreenLine {
                line: index,
                cells: text
                    .chars()
                    .enumerate()
                    .map(|(col, c)| ScreenCell {
                        col: u16::try_from(col).unwrap(),
                        c,
                        zerowidth: Vec::new(),
                        dim: false,
                        wide: false,
                    })
                    .collect(),
                wraps: false,
            }
        }

        pub(crate) struct Holder {
            connection: <PlatformIpc as PeerAuth>::Connection,
            frames: HolderFrameBuffer,
            number: u64,
            outputs_fed: u64,
            size: (u16, u16),
            /// Every frame the daemon sent that the test did not wait for.
            passed: Vec<HolderFrame>,
        }

        impl Holder {
            /// Takes the daemon's connection, answers its `Hello` with a
            /// `Welcome` of `COLS` x `ROWS`, and sends the whole copy that
            /// follows a welcome, showing `first`.
            pub(crate) async fn accept(
                mut listener: <PlatformIpc as IpcTransport>::Listener,
            ) -> Self {
                let ipc = PlatformIpc::default();
                let connection = ipc.accept(&mut listener).await.expect("accept the daemon");
                let mut holder = Self {
                    connection,
                    frames: HolderFrameBuffer::new(),
                    number: 0,
                    outputs_fed: 0,
                    size: (COLS, ROWS),
                    passed: Vec::new(),
                };
                let hello = holder.next().await;
                assert!(
                    matches!(hello, HolderFrame::Hello { .. }),
                    "the daemon opens with a Hello, got {hello:?}"
                );
                holder
                    .send(&HolderFrame::Welcome {
                        protocol: HOLDER_PROTOCOL,
                        record: "played".to_owned(),
                        cols: COLS,
                        rows: ROWS,
                        exited: false,
                        child_pid: Some(std::process::id()),
                        child_start: None,
                    })
                    .await;
                holder.push(true, &[line(0, "first output")], 0).await;
                holder
            }

            async fn send(&mut self, frame: &HolderFrame) {
                self.connection
                    .write_all(&encode_holder_frame(frame))
                    .await
                    .expect("send to the daemon");
            }

            async fn next(&mut self) -> HolderFrame {
                tokio::time::timeout(
                    HANG_GUARD,
                    super::super::next_frame(&mut self.connection, &mut self.frames),
                )
                .await
                .expect("HANG_GUARD")
                .expect("the daemon kept the connection")
            }

            /// The next frame `wanted` takes, keeping the others in `passed`.
            async fn next_of(&mut self, wanted: impl Fn(&HolderFrame) -> bool) -> HolderFrame {
                loop {
                    let frame = self.next().await;
                    if wanted(&frame) {
                        return frame;
                    }
                    self.passed.push(frame);
                }
            }

            /// Sends a copy numbered after the last one, taking in one more
            /// output, and returns its number.
            pub(crate) async fn push(
                &mut self,
                full: bool,
                lines: &[ScreenLine],
                generation: u64,
            ) -> u64 {
                self.number += 1;
                self.outputs_fed += 1;
                let number = self.number;
                if !lines.is_empty() {
                    self.send(&HolderFrame::ScreenRows(ScreenRows {
                        number,
                        lines: lines.to_vec(),
                    }))
                    .await;
                }
                let (cols, rows) = self.size;
                self.send(&HolderFrame::ScreenPush(ScreenPush {
                    number,
                    full,
                    cols,
                    rows,
                    cursor: ScreenCursor::default(),
                    cursor_hidden: false,
                    sync_open: false,
                    input_modes: InputModes::default(),
                    generation,
                    outputs_fed: self.outputs_fed,
                }))
                .await;
                number
            }

            /// Waits for the daemon's answer to copy `number`: its acknowledgement
            /// or its ask for a whole copy.
            async fn answer_to_copy(&mut self, number: u64) -> HolderFrame {
                self.next_of(|frame| match frame {
                    HolderFrame::PushAck { number: acked } => *acked == number,
                    HolderFrame::ScreenResync => true,
                    _ => false,
                })
                .await
            }

            async fn expect_ack(&mut self, number: u64) {
                let answer = self.answer_to_copy(number).await;
                assert!(
                    matches!(answer, HolderFrame::PushAck { .. }),
                    "copy {number} is applied and acknowledged, got {answer:?}"
                );
            }

            /// Answers the next `ReadTail` with `text`.
            async fn answer_read_tail(&mut self, text: &str) {
                let asked = self
                    .next_of(|frame| matches!(frame, HolderFrame::ReadTail { .. }))
                    .await;
                let HolderFrame::ReadTail { id, .. } = asked else {
                    unreachable!()
                };
                self.send(&HolderFrame::Tail {
                    id,
                    text: text.to_owned(),
                    truncated: false,
                })
                .await;
            }

            /// Reads at least `count` bytes of whatever the daemon sends,
            /// without taking them as frames.
            async fn drain_at_least(&mut self, count: usize) {
                let mut chunk = vec![0u8; 64 * 1024];
                let mut read = 0;
                while read < count {
                    let n = tokio::time::timeout(HANG_GUARD, self.connection.read(&mut chunk))
                        .await
                        .expect("HANG_GUARD")
                        .expect("read from the daemon");
                    assert_ne!(n, 0, "the daemon kept the connection");
                    read += n;
                }
            }

            /// Waits, at most `HANG_GUARD`, until the daemon has sent a
            /// `Terminate`, including one already passed over.
            pub(crate) async fn told_to_end(&mut self) {
                if self.was_told_to_end() {
                    return;
                }
                tokio::time::timeout(
                    HANG_GUARD,
                    self.next_of(|frame| matches!(frame, HolderFrame::Terminate { .. })),
                )
                .await
                .expect("the daemon tells the holder to end within HANG_GUARD");
            }

            fn was_told_to_end(&self) -> bool {
                self.passed
                    .iter()
                    .any(|frame| matches!(frame, HolderFrame::Terminate { .. }))
            }
        }

        fn shown(session: &PtySession) -> String {
            session
                .model_for_detection()
                .read(|copy| reading::bottom_lines(copy, usize::from(copy.rows)).join("\n"))
                .value
                .unwrap_or_default()
        }

        #[tokio::test]
        async fn a_copy_that_only_counts_more_output_advances_the_output_arrivals() {
            let (endpoint, listener) = bind_holder_endpoint("arrival").await;
            let (session, mut holder) =
                tokio::join!(played_session(&endpoint), Holder::accept(listener));
            holder.expect_ack(1).await;
            // The daemon's link takes the holder's frames in order, so once the
            // tail is answered the first copy has been taken in entirely.
            let (tail, ()) = tokio::join!(session.text_tail(1, 64), holder.answer_read_tail(""));
            assert!(tail.is_some(), "the played holder answered the read");

            let mut arrivals = session.output_arrivals();
            arrivals.borrow_and_update();
            let number = holder.push(false, &[], 0).await;
            tokio::time::timeout(HANG_GUARD, arrivals.changed())
                .await
                .expect("a copy with more output taken in is an arrival, rows changed or not")
                .expect("the arrivals channel stays open while the session lives");
            holder.expect_ack(number).await;
        }

        #[tokio::test]
        async fn a_copy_that_fails_the_check_loses_the_screen_and_asks_for_a_whole_one() {
            let (endpoint, listener) = bind_holder_endpoint("bad-copy").await;
            let (session, mut holder) =
                tokio::join!(played_session(&endpoint), Holder::accept(listener));
            holder.expect_ack(1).await;
            let definition = parse_definition(PROMPT_DEF).expect("the definition parses");
            let mut engine = DetectionEngine::new(
                Some(definition),
                &DaemonConfig::default(),
                session.model_for_detection(),
            );
            assert_ne!(
                engine.on_output(),
                Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
                "the copy after the welcome is the screen, not a lost one"
            );

            let bad = holder
                .push(false, &[line(ROWS + 5, "below the screen")], 0)
                .await;
            let answer = holder.answer_to_copy(bad).await;
            assert!(
                matches!(answer, HolderFrame::ScreenResync),
                "a copy outside its size is answered with an ask for a whole copy, got {answer:?}"
            );
            assert_eq!(
                engine.on_output(),
                Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
                "a copy that fails the check loses the window's screen"
            );

            let whole = holder.push(true, &[line(0, "PLAYED-PROMPT>")], 0).await;
            holder.expect_ack(whole).await;
            assert_eq!(
                engine.on_output(),
                Some(DetectedStatus::Idle),
                "a whole copy after the loss is judged again"
            );
            assert!(
                !holder.was_told_to_end(),
                "a bad copy does not end the child: {:?}",
                holder.passed
            );
            assert_eq!(session.exit_status(), None, "the connection is kept");

            // A holder that could not learn the code reports the exit without one.
            holder.send(&HolderFrame::Exited { code: None }).await;
            let late = holder.push(true, &[line(0, "AFTER-EXIT")], 0).await;
            let mut exited = session.exit_notified();
            tokio::time::timeout(HANG_GUARD, exited.wait_for(|&gone| gone))
                .await
                .expect("the exit is announced")
                .expect("the exit channel stays open while the session lives");
            assert_eq!(
                session.exit_status(),
                Some(crate::pty::ChildExit { code: None }),
                "an exit without a code is an ended child whose code is unknown, never a number"
            );
            let (tail, ()) = tokio::join!(
                session.text_tail(5, 1024),
                holder.answer_read_tail("read after the exit")
            );
            assert_eq!(
                tail.map(|(text, _)| text).as_deref(),
                Some("read after the exit"),
                "a copy after the exit is no error of the connection"
            );
            let screen = shown(&session);
            assert!(
                screen.contains("PLAYED-PROMPT>") && !screen.contains("AFTER-EXIT"),
                "copy {late} came after the exit and is not applied; the screen shows {screen:?}"
            );
        }

        /// A holder sends `Exited` and closes; a write the daemon is in the
        /// middle of then fails, which does not lose the exit still to be
        /// read.
        #[tokio::test]
        async fn the_exit_sent_before_the_close_is_read_after_a_write_to_the_holder_failed() {
            /// More than any pipe or socket holds: the daemon is still writing
            /// it when the holder closes.
            const LARGE: usize = 16 * 1024 * 1024;
            let (endpoint, listener) = bind_holder_endpoint("write-fails").await;
            let (session, mut holder) =
                tokio::join!(played_session(&endpoint), Holder::accept(listener));
            holder.expect_ack(1).await;

            session.send_report(vec![b'r'; LARGE]);
            // Where the write is buffered (Windows), its failure comes out on
            // the next one, which waits behind it.
            session.send_report(vec![b's']);
            holder.drain_at_least(1024 * 1024).await;
            holder.send(&HolderFrame::Exited { code: Some(7) }).await;
            drop(holder);

            let mut exited = session.exit_notified();
            tokio::time::timeout(HANG_GUARD, exited.wait_for(|&gone| gone))
                .await
                .expect("the exit is announced")
                .expect("the exit channel stays open while the session lives");
            assert_eq!(
                session.exit_status(),
                Some(crate::pty::ChildExit { code: Some(7) }),
                "the exit the holder sent before it closed is read, not lost to the failed write"
            );
        }

        #[tokio::test]
        async fn output_that_reached_the_model_before_the_pump_started_is_still_judged() {
            let marker = yagura_test_support::detection::CONTROL_READY_MARKER;
            let (endpoint, listener) = bind_holder_endpoint("pump").await;
            let (session, mut holder) =
                tokio::join!(played_session(&endpoint), Holder::accept(listener));
            holder.expect_ack(1).await;
            // The copy is in the model by the time it is acknowledged, and
            // nothing follows it, so the pump's look at what arrived before
            // it is all that can report the marker.
            let copy = holder.push(true, &[line(0, marker)], 0).await;
            holder.expect_ack(copy).await;

            let definition = parse_definition(
                &yagura_test_support::detection::idle_definition_toml("pump-test", "", ""),
            )
            .expect("the idle definition must parse");
            let engine = DetectionEngine::new(
                Some(definition),
                &DaemonConfig::default(),
                session.model_for_detection(),
            );
            let (handle, mut statuses) = crate::detector::DetectorHandle::spawn(engine);
            let handle = std::sync::Arc::new(handle);
            crate::window::spawn_output_pump(&session, std::sync::Arc::downgrade(&handle));

            // The definition has no time table, so the detector reads the model
            // only when told output arrived.
            let confirmed =
                tokio::time::timeout(std::time::Duration::from_secs(10), statuses.recv())
                    .await
                    .expect("the marker that arrived before the pump must still be judged")
                    .expect("the detector must still be running");
            assert_eq!(confirmed.status, DetectedStatus::Idle);
        }
    }
}
