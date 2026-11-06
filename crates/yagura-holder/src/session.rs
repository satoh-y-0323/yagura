//! The holder's core: the child on its terminal and the one daemon connected
//! at a time. The terminal itself is the terminal thread's (`terminal.rs`);
//! the core hands it the daemon's requests and the child's writer the
//! daemon's writes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use yagura_os::pty::{EnvOp, PtyCommand, PtyKiller, PtySize, PtySpawn, PtySpawnError};
use yagura_os::{IpcTransport, PlatformIpc, ProcessGroup};
use yagura_protocol::holder::{
    HolderEnvOp, HolderExit, HolderFault, HolderFrame, HolderLaunch, HolderSpawnFailure,
    MAX_HELLO_DEADLINE_MS,
};
use yagura_protocol::window_size_within_limit;

use crate::buffer;
use crate::conn::{Conn, Reply, Welcome};
use crate::endpoint::Place;
use crate::peer::{self, Accepted};
use crate::record::{self, RecordFile};
use crate::terminal::{self, Control, Terminal, TerminalStart};

pub(crate) use provisional::{ExitKept, Provisional};

/// How long the output is still read after the child's exit, for an end of
/// output that may never come (something else keeping the terminal open).
const POST_EXIT_DRAIN: Duration = Duration::from_secs(2);

/// How long the child has to act on the first, refusable kill before the OS
/// boundary forces it down (a no-op on Windows, whose kill cannot be refused).
const TERMINATE_ESCALATION_GRACE: Duration = Duration::from_millis(250);

/// The longest the holder waits, on its way out, for its last frames to be
/// taken by a daemon that stopped reading.
const FINAL_SEND_LIMIT: Duration = Duration::from_secs(5);

/// The pause before accepting again after the endpoint failed to offer a
/// connection, so a broken endpoint does not spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// How many connections may be in their handshake at once; the endpoint
/// accepts no more until one of them is let in or refused.
const MAX_HANDSHAKES: usize = 4;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the terminal reader, the exit monitor and the core share.
#[derive(Default)]
struct Shared {
    /// The child's exit, once observed: its code when known.
    exit: Mutex<Option<Option<i32>>>,
    /// The terminal's output has ended.
    reader_done: AtomicBool,
    /// Wakes the core when the child exited or the output ended.
    core_wake: Notify,
}

impl Shared {
    fn exit(&self) -> MutexGuard<'_, Option<Option<i32>>> {
        lock(&self.exit)
    }
}

/// Whether this holder acts out a fault its launch names. Only the
/// fault-injecting binary, built for tests, does; the shipped one refuses
/// such a launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Faults {
    Refused,
    ActedOut,
}

pub(crate) enum Event {
    Accepted(Accepted),
    Frame { conn: u64, frame: HolderFrame },
    Closed { conn: u64 },
    WriteDone { conn: u64, id: u64, written: bool },
}

/// The child and the handles to reach it.
struct Child {
    killer: PtyKiller,
    group: Arc<ProcessGroup>,
    pid: u32,
    exited: Arc<AtomicBool>,
    terminal: Terminal,
    force_kill_scheduled: bool,
    tree_end_started: bool,
}

impl Child {
    /// Kills before letting go of the input: on Windows closing the input is
    /// itself a kill, and the explicit kill is to decide the exit code.
    fn terminate(&mut self, tree: bool) {
        if !self.exited.load(Ordering::SeqCst) {
            let _ = self.killer.kill();
            if !self.force_kill_scheduled {
                self.force_kill_scheduled = true;
                yagura_os::schedule_force_kill(
                    self.pid,
                    TERMINATE_ESCALATION_GRACE,
                    Arc::clone(&self.exited),
                );
            }
        }
        if tree && !self.tree_end_started {
            self.tree_end_started = true;
            // Ending the tree waits out the escalation grace, which the core
            // must not do; the exit monitor waits for this same call.
            let group = Arc::clone(&self.group);
            let started = spawn_thread("holder-tree", move || end_tree(&group));
            if let Err(error) = started {
                crate::log(format_args!("cannot start the tree-ending thread: {error}"));
            }
        }
        self.terminal.input.close();
    }
}

/// Ends the child's process tree, escalating as the group does. Blocks until
/// the signals are sent, and so until any other caller's call has finished.
fn end_tree(group: &ProcessGroup) {
    if let Err(error) = group.terminate() {
        crate::log(format_args!(
            "failed to end the child's process tree: {error}"
        ));
    }
}

fn to_env_op(op: &HolderEnvOp) -> EnvOp {
    match op {
        HolderEnvOp::Set { name, value } => EnvOp::Set(name.clone(), value.clone()),
        HolderEnvOp::Remove { name } => EnvOp::Remove(name.clone()),
    }
}

/// Why the child could not be started, and the detail for the holder's log.
struct SpawnError {
    reason: HolderSpawnFailure,
    detail: String,
}

impl From<PtySpawnError> for SpawnError {
    fn from(error: PtySpawnError) -> Self {
        let reason = match error {
            PtySpawnError::BundledConptyUnavailable(_) => {
                HolderSpawnFailure::BundledConptyUnavailable
            }
            PtySpawnError::Spawn(_) => HolderSpawnFailure::Spawn,
        };
        Self {
            reason,
            detail: error.to_string(),
        }
    }
}

impl SpawnError {
    fn other(detail: String) -> Self {
        Self {
            reason: HolderSpawnFailure::Spawn,
            detail,
        }
    }
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map(drop)
}

/// Starts the child on its terminal, with the threads that keep its
/// terminal, write its input, read its output and wait for its exit. `fault`
/// is the launch's fault when this holder acts faults out.
fn spawn_child(
    launch: &HolderLaunch,
    fault: Option<HolderFault>,
    shared: &Arc<Shared>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<Child, SpawnError> {
    let command = PtyCommand {
        argv: launch.argv.clone(),
        cwd: launch.cwd.clone(),
        env: launch.env.iter().map(to_env_op).collect(),
    };
    let size = PtySize {
        cols: launch.cols,
        rows: launch.rows,
    };
    let PtySpawn {
        child,
        killer,
        group,
        control,
        reader,
        writer,
    } = yagura_os::pty::spawn(&command, size)?;
    let pid = child.process_id();
    let group = Arc::new(group);
    let control = Arc::new(control);
    let exited = Arc::new(AtomicBool::new(false));
    let (terminal, threads) = terminal::terminal(
        TerminalStart {
            cols: launch.cols,
            rows: launch.rows,
            history_rows: launch.history_rows,
            palette: launch.palette.clone(),
            fault,
        },
        Arc::clone(&control),
        writer,
        events.clone(),
    );

    let started = spawn_thread("holder-terminal", threads.model)
        .and_then(|()| spawn_thread("holder-write", threads.writer))
        .and_then(|()| {
            spawn_thread("holder-read", {
                let shared = Arc::clone(shared);
                let intake = Arc::clone(&terminal.intake);
                move || {
                    buffer::read_into(reader, &intake);
                    shared.reader_done.store(true, Ordering::SeqCst);
                    shared.core_wake.notify_one();
                }
            })
        })
        .and_then(|()| {
            spawn_thread("holder-exit", {
                let shared = Arc::clone(shared);
                let group = Arc::clone(&group);
                let exited = Arc::clone(&exited);
                move || {
                    // The monitor is the child's only waiter. After the exit the
                    // order is fixed: end the tree, close the terminal so its
                    // output can end, then publish the exit. A published exit
                    // therefore means the tree has been ended.
                    let code = child.wait().ok();
                    exited.store(true, Ordering::SeqCst);
                    end_tree(&group);
                    control.close();
                    *shared.exit() = Some(code);
                    shared.core_wake.notify_one();
                }
            })
        });
    // Dropping what was started ends the child's tree with it.
    started.map_err(|error| {
        SpawnError::other(format!("cannot start the holder's threads: {error}"))
    })?;
    Ok(Child {
        killer,
        group,
        pid,
        exited,
        terminal,
        force_kill_scheduled: false,
        tree_end_started: false,
    })
}

/// A window's holder before its first accepted `Hello`.
pub(crate) struct Window {
    pub(crate) launch: HolderLaunch,
    pub(crate) place: Place,
    /// Dropped with the core, after its connection is closed.
    pub(crate) record: RecordFile,
    pub(crate) provisional: Provisional,
    pub(crate) faults: Faults,
}

/// How the holder ended.
pub(crate) enum Ending {
    /// No daemon was let in before the deadline; the child never started.
    NoHello,
    /// The child exited (on its own or by `Terminate`).
    ChildExited,
    /// The child could not be started.
    SpawnFailed,
}

struct Core {
    window: Window,
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<Event>,
    child: Option<Child>,
    conn: Option<Conn>,
    next_conn: u64,
    terminating: bool,
    exit_seen: Option<Instant>,
    /// The exit is settled (in the exit file when one is kept) and no daemon
    /// is let in any more; `Exited` has reached the connection.
    exit_recorded: bool,
    /// The writes handed to the child's input and not yet reported done, in
    /// the order the input takes them (connection, id).
    unanswered_writes: VecDeque<(u64, u64)>,
}

/// Serves the window until the holder is to end.
pub(crate) async fn serve(
    window: Window,
    listener: <PlatformIpc as IpcTransport>::Listener,
) -> Ending {
    let (events, mut incoming) = mpsc::unbounded_channel();
    let mut acceptor = Some(Acceptor::spawn(
        listener,
        events.clone(),
        window.place.daemon_endpoint.clone(),
        window.place.user_id.clone(),
    ));
    let hello_wait =
        Duration::from_millis(window.launch.hello_deadline_ms.min(MAX_HELLO_DEADLINE_MS));
    let hello_deadline = Instant::now() + hello_wait;
    let mut core = Core {
        window,
        shared: Arc::new(Shared::default()),
        events,
        child: None,
        conn: None,
        next_conn: 0,
        terminating: false,
        exit_seen: None,
        exit_recorded: false,
        unanswered_writes: VecDeque::new(),
    };
    let ending = loop {
        let output_ended = core.shared.reader_done.load(Ordering::SeqCst);
        if let Some(seen) = core.exit_seen
            && !core.exit_recorded
            && ((output_ended && core.unanswered_writes.is_empty())
                || Instant::now() >= seen + POST_EXIT_DRAIN)
        {
            // An exit file is written while the endpoint is still held, so a
            // next holder of this window starts only after it and clears it.
            core.record_exit();
            core.fail_unanswered_writes();
            core.send_last_copy().await;
            core.send_exited();
            Acceptor::stop(&mut acceptor).await;
            if core.terminating || core.conn.is_none() {
                break Ending::ChildExited;
            }
        }
        let drain_deadline = core
            .exit_seen
            .filter(|_| !core.exit_recorded)
            .map(|seen| seen + POST_EXIT_DRAIN);
        tokio::select! {
            () = tokio::time::sleep_until(hello_deadline), if core.child.is_none() => {
                core.window.record.remove();
                break Ending::NoHello;
            }
            () = tokio::time::sleep_until(drain_deadline.unwrap_or(hello_deadline)),
                if drain_deadline.is_some() => {}
            () = core.shared.core_wake.notified() => core.on_wake(),
            event = incoming.recv() => {
                let Some(event) = event else { break Ending::ChildExited };
                if let Some(ending) = core.handle(event).await {
                    break ending;
                }
            }
        }
    };
    Acceptor::stop(&mut acceptor).await;
    core.close_conn().await;
    // The core, dropped here after the close (or by an unwind), removes a
    // provisional holder's record and ends the terminal's threads.
    ending
}

/// Accepts connections and runs their handshakes, a bounded number at once.
/// Owns the listener, and with it the endpoint, until stopped.
struct Acceptor {
    stop: oneshot::Sender<()>,
    task: JoinHandle<<PlatformIpc as IpcTransport>::Listener>,
}

impl Acceptor {
    fn spawn(
        mut listener: <PlatformIpc as IpcTransport>::Listener,
        events: mpsc::UnboundedSender<Event>,
        daemon_endpoint: String,
        user_id: String,
    ) -> Self {
        let (stop, mut stopped) = oneshot::channel::<()>();
        let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        let task = tokio::spawn(async move {
            let ipc = PlatformIpc::default();
            loop {
                let permit = tokio::select! {
                    _ = &mut stopped => return listener,
                    permit = Arc::clone(&handshakes).acquire_owned() => permit,
                };
                let Ok(permit) = permit else { return listener };
                let accepted = tokio::select! {
                    _ = &mut stopped => return listener,
                    accepted = ipc.accept(&mut listener) => accepted,
                };
                match accepted {
                    Ok(connection) => {
                        let events = events.clone();
                        let daemon_endpoint = daemon_endpoint.clone();
                        let user_id = user_id.clone();
                        tokio::spawn(async move {
                            let accepted =
                                peer::handshake(connection, &daemon_endpoint, &user_id).await;
                            drop(permit);
                            if let Some(accepted) = accepted {
                                let _ = events.send(Event::Accepted(accepted));
                            }
                        });
                    }
                    Err(_) => tokio::select! {
                        _ = &mut stopped => return listener,
                        () = tokio::time::sleep(ACCEPT_RETRY) => {}
                    },
                }
            }
        });
        Self { stop, task }
    }

    /// Stops accepting and gives up the endpoint; the listener removes what
    /// it left on disk as it is dropped.
    async fn stop(acceptor: &mut Option<Self>) {
        let Some(Self { stop, task }) = acceptor.take() else {
            return;
        };
        let _ = stop.send(());
        drop(task.await);
    }
}

impl Core {
    fn on_wake(&mut self) {
        if self.exit_seen.is_none() && self.shared.exit().is_some() {
            self.exit_seen = Some(Instant::now());
        }
    }

    fn terminal(&self) -> Option<&Terminal> {
        self.child.as_ref().map(|child| &child.terminal)
    }

    fn to_terminal(&self, control: Control) {
        if let Some(terminal) = self.terminal() {
            terminal.intake.push(control);
        }
    }

    fn child_has_exited(&self) -> bool {
        self.exit_recorded || self.shared.exit().is_some()
    }

    /// Handles one event; returns how the holder ends when it is to end now.
    async fn handle(&mut self, event: Event) -> Option<Ending> {
        match event {
            Event::Accepted(accepted) => return self.accept(accepted).await,
            Event::Frame { conn, frame } if self.is_current(conn) => {
                return self.on_frame(frame);
            }
            Event::Closed { conn } if self.is_current(conn) => {
                if let Some(conn) = self.conn.take() {
                    self.close_outlet(&conn);
                    drop(conn.close());
                }
                if self.exit_recorded {
                    return Some(Ending::ChildExited);
                }
                if self
                    .window
                    .provisional
                    .on_lost_connection(self.child.as_mut())
                {
                    self.terminating = true;
                }
            }
            Event::WriteDone { conn, id, written } => {
                // Counted whichever connection asked, and only if still owed
                // an answer: a write failed at the settle is answered already.
                if self.unanswered_writes.pop_front().is_some() && self.is_current(conn) {
                    self.answer_write(id, written);
                }
            }
            Event::Frame { .. } | Event::Closed { .. } => {}
        }
        None
    }

    fn is_current(&self, conn: u64) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|current| current.id() == conn)
    }

    fn answer_write(&self, id: u64, written: bool) {
        let Some(conn) = &self.conn else { return };
        if written {
            conn.send(Reply::Written { id });
        } else {
            conn.send(Reply::WriteFailed {
                id,
                reason: "the write to the child's input failed".to_owned(),
            });
        }
    }

    /// Lets a daemon in: the previous one is told it was displaced, the child
    /// starts on the first one, and the newcomer is welcomed and sent a whole
    /// copy of the screen.
    async fn accept(&mut self, accepted: Accepted) -> Option<Ending> {
        if self.exit_recorded {
            return None;
        }
        if let Some(previous) = self.conn.take() {
            previous.send(Reply::Displaced);
            self.close_outlet(&previous);
            drop(previous.close());
        }
        let id = self.next_conn;
        self.next_conn += 1;
        let child_pid = if let Some(child) = &self.child {
            child.pid
        } else {
            let launch = self.window.launch.clone();
            let fault = match (launch.fault, self.window.faults) {
                (None, _) => None,
                (Some(fault), Faults::ActedOut) => Some(fault),
                (Some(_), Faults::Refused) => {
                    let error = SpawnError::other(
                        "the launch asks for a fault, which the shipped holder never acts out"
                            .to_owned(),
                    );
                    return Some(self.spawn_failed(accepted, id, &error));
                }
            };
            let shared = Arc::clone(&self.shared);
            let events = self.events.clone();
            let spawned =
                tokio::task::spawn_blocking(move || spawn_child(&launch, fault, &shared, &events))
                    .await;
            match spawned {
                Ok(Ok(child)) => {
                    let pid = child.pid;
                    self.child = Some(child);
                    if let Err(error) = self.window.record.set_child_pid(pid) {
                        crate::log(format_args!("cannot update the holder record: {error}"));
                    }
                    pid
                }
                Ok(Err(error)) => return Some(self.spawn_failed(accepted, id, &error)),
                Err(error) => {
                    let error = SpawnError::other(error.to_string());
                    return Some(self.spawn_failed(accepted, id, &error));
                }
            }
        };
        let terminal = self.terminal()?;
        let (cols, rows) = terminal.published.size();
        let welcome = Welcome {
            record: self.window.launch.record.clone(),
            cols,
            rows,
            exited: self.shared.exit().is_some(),
            child_pid,
        };
        let conn = Conn::welcome(accepted, id, welcome, self.events.clone());
        terminal.outlet.open(id, conn.sender());
        terminal.intake.push(Control::Connected { conn: id });
        self.conn = Some(conn);
        None
    }

    /// Stops the terminal thread's frames to `conn`, so the connection can
    /// close once what was sent on it is written.
    fn close_outlet(&self, conn: &Conn) {
        if let Some(terminal) = self.terminal() {
            terminal.outlet.close(conn.id());
        }
    }

    /// Answers the first `Hello` with why the child could not start, then
    /// `Exited`, in place of a `Welcome`.
    fn spawn_failed(&mut self, accepted: Accepted, id: u64, error: &SpawnError) -> Ending {
        crate::log(format_args!("cannot start the child: {}", error.detail));
        if let Some(kept) = self.window.provisional.keeps_exit() {
            let exit = HolderExit { code: None };
            if let Err(error) = record::write_exit(&self.window.record, kept, &exit) {
                crate::log(format_args!("cannot write the exit file: {error}"));
            }
        }
        self.conn = Some(Conn::refuse_spawn(
            accepted,
            id,
            error.reason,
            self.events.clone(),
        ));
        Ending::SpawnFailed
    }

    fn on_frame(&mut self, frame: HolderFrame) -> Option<Ending> {
        match frame {
            HolderFrame::Write { id, bytes } => self.write(id, bytes),
            HolderFrame::Report { bytes } => {
                if let Some(terminal) = self.terminal()
                    && !self.child_has_exited()
                {
                    terminal.input.push_report(bytes);
                }
            }
            HolderFrame::SetViewFocus { focused } => {
                if let Some(terminal) = self.terminal() {
                    terminal.input.set_view_focus(focused);
                }
            }
            HolderFrame::FocusInterval { open } => {
                if let Some(terminal) = self.terminal() {
                    terminal.input.focus_interval(open);
                }
            }
            HolderFrame::Resize { cols, rows } => self.resize(cols, rows),
            HolderFrame::PushAck { number } => self.to_terminal(Control::PushAck { number }),
            HolderFrame::ScreenResync => self.to_terminal(Control::ScreenResync),
            HolderFrame::SetPalette { palette } => self.to_terminal(Control::SetPalette(palette)),
            HolderFrame::Attach { generation } => {
                self.to_terminal(Control::Attach { generation });
            }
            HolderFrame::Detach { generation } => {
                self.to_terminal(Control::Detach { generation });
            }
            HolderFrame::ViewNote { generation, note } => {
                self.to_terminal(Control::ViewNote { generation, note });
            }
            HolderFrame::ViewOp { id, op } => self.to_terminal(Control::ViewOp { id, op }),
            HolderFrame::ReadTail {
                id,
                max_lines,
                max_bytes,
            } => self.to_terminal(Control::ReadTail {
                id,
                max_lines,
                max_bytes,
            }),
            HolderFrame::Settle { id } => self.to_terminal(Control::Settle { id }),
            HolderFrame::Terminate { tree } => {
                self.terminating = true;
                if let Some(child) = self.child.as_mut() {
                    child.terminate(tree);
                }
                if self.exit_recorded {
                    return Some(Ending::ChildExited);
                }
            }
            HolderFrame::Hello { .. }
            | HolderFrame::Welcome { .. }
            | HolderFrame::Written { .. }
            | HolderFrame::WriteFailed { .. }
            | HolderFrame::Exited { .. }
            | HolderFrame::Displaced
            | HolderFrame::VersionRejected { .. }
            | HolderFrame::Resized { .. }
            | HolderFrame::ResizeFailed { .. }
            | HolderFrame::SpawnFailed { .. }
            | HolderFrame::ScreenRows(_)
            | HolderFrame::ScreenPush(_)
            | HolderFrame::ViewOpDone { .. }
            | HolderFrame::ViewFrame { .. }
            | HolderFrame::Tail { .. }
            | HolderFrame::Settled { .. } => {}
        }
        None
    }

    fn write(&mut self, id: u64, bytes: Vec<u8>) {
        let Some(conn) = &self.conn else { return };
        let conn_id = conn.id();
        let queued = !self.child_has_exited()
            && self
                .terminal()
                .is_some_and(|terminal| terminal.input.push_write(conn_id, id, bytes));
        if queued {
            self.unanswered_writes.push_back((conn_id, id));
        } else if let Some(conn) = &self.conn {
            conn.send(Reply::WriteFailed {
                id,
                reason: "the child has exited or its input is closed".to_owned(),
            });
        }
    }

    /// Hands the terminal thread a size in range, which answers whether the
    /// terminal took it; answers a size out of range at once.
    fn resize(&mut self, cols: u16, rows: u16) {
        let refused = if !window_size_within_limit(cols, rows) {
            "the size is out of range"
        } else if self.terminal().is_some() {
            self.to_terminal(Control::Resize { cols, rows });
            return;
        } else {
            "the child has not started"
        };
        if let Some(conn) = &self.conn {
            conn.send(Reply::ResizeFailed {
                cols,
                rows,
                reason: refused.to_owned(),
            });
        }
    }

    /// Settles the exit. When it is kept, the exit goes to the exit file as
    /// soon as the exit is known, so a daemon that ends right after `Exited`
    /// still finds it.
    fn record_exit(&mut self) {
        if let Some(kept) = self.window.provisional.keeps_exit() {
            let exit = HolderExit {
                code: self.shared.exit().flatten(),
            };
            if let Err(error) = record::write_exit(&self.window.record, kept, &exit) {
                crate::log(format_args!("cannot write the exit file: {error}"));
            }
        }
        self.exit_recorded = true;
    }

    /// Answers `WriteFailed` to the connection for each write it was owed an
    /// answer for, before `Exited`: they will not be reported done in time.
    fn fail_unanswered_writes(&mut self) {
        let owed = std::mem::take(&mut self.unanswered_writes);
        for (conn, id) in owed {
            if self.is_current(conn) {
                self.answer_write(id, false);
            }
        }
    }

    /// Has the terminal thread take in the output read so far and send the
    /// copy holding it, and waits until it has: that copy goes before
    /// `Exited`. A terminal thread that is gone has nothing more to send.
    async fn send_last_copy(&mut self) {
        let Some(terminal) = self.terminal() else {
            return;
        };
        let (done, sent) = oneshot::channel();
        terminal.intake.push(Control::Final { done });
        let _ = sent.await;
    }

    /// Tells the connection the child exited; no copy of the screen follows.
    fn send_exited(&mut self) {
        let code = self.shared.exit().flatten();
        if let Some(conn) = &self.conn {
            conn.send(Reply::Exited { code });
        }
    }

    /// Closes the connection after what was sent on it, waiting a bounded
    /// time for a daemon that stopped reading.
    async fn close_conn(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.close_outlet(&conn);
            let _ = tokio::time::timeout(FINAL_SEND_LIMIT, conn.close()).await;
        }
    }
}

/// Provisional, removed with [`HolderLaunch::end_child_on_disconnect`] once
/// holders outlive the daemon: the one place that flag takes effect. A daemon
/// that left will not come back for the window, so nobody reads an exit file
/// or the record once the holder ends: none is written, and the holder
/// removes its record itself.
mod provisional {
    use yagura_protocol::holder::{HolderLaunch, HolderRecord};

    use super::Child;
    use crate::endpoint::Place;
    use crate::record::RecordFile;

    pub(crate) struct Provisional {
        end_child_on_disconnect: bool,
    }

    /// Leave to write an exit file. Only [`Provisional::keeps_exit`] makes
    /// one, so a provisional holder has no way to write it.
    pub(crate) struct ExitKept(());

    impl Provisional {
        pub(crate) fn new(launch: &HolderLaunch) -> Self {
            Self {
                end_child_on_disconnect: launch.end_child_on_disconnect,
            }
        }

        /// The holder's record, removed when dropped while provisional.
        pub(crate) fn own_record(&self, place: &Place, record: HolderRecord) -> RecordFile {
            RecordFile::new(place, record, self.end_child_on_disconnect)
        }

        /// Ends the child's tree when the connection was lost other than by
        /// this holder closing it on a displacement; returns whether it did.
        pub(super) fn on_lost_connection(&self, child: Option<&mut Child>) -> bool {
            if !self.end_child_on_disconnect {
                return false;
            }
            if let Some(child) = child {
                child.terminate(true);
            }
            true
        }

        /// Leave to write the exit file for a next daemon; `None` while
        /// provisional.
        pub(super) fn keeps_exit(&self) -> Option<ExitKept> {
            (!self.end_child_on_disconnect).then_some(ExitKept(()))
        }
    }
}
