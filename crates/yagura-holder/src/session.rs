//! The holder's core: the child on its terminal and the one daemon connected
//! at a time, for as long as the window lives. The terminal itself is the
//! terminal thread's (`terminal.rs`); the core hands it the daemon's requests
//! and the child's writer the daemon's writes.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use yagura_os::pty::{EnvOp, PtyCommand, PtyControl, PtyKiller, PtySize, PtySpawn, PtySpawnError};
use yagura_os::{IpcTransport, PlatformIpc, ProcessGroup, ReapGate};
use yagura_protocol::holder::{
    HolderEnvOp, HolderFault, HolderFrame, HolderLaunch, HolderSpawnFailure, MAX_HELLO_DEADLINE_MS,
};
use yagura_protocol::window_size_within_limit;

use crate::buffer;
use crate::conn::{Conn, Reply, Welcome};
use crate::endpoint::Place;
use crate::peer::{self, Accepted};
use crate::record::{RecordFile, RecordRemoved};
use crate::terminal::{self, Control, Terminal, TerminalStart};

pub(crate) use succession::Succession;

/// Bytes the end-of-output reader takes from the terminal at a time.
const CUT_READ_CHUNK: usize = 8192;

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
    /// The child has exited; no write is taken from here on.
    child_ended: AtomicBool,
    /// The child's exit code, once reaped: after its tree has ended and the
    /// end of its output is fixed.
    exit: Mutex<Option<Option<i32>>>,
    /// Where the child's output ends.
    cut: Mutex<Cut>,
    /// Everything before the end of output has been handed to the terminal
    /// thread; nothing after it will be.
    output_ended: AtomicBool,
    /// Bytes read after the end of output and dropped.
    dropped_bytes: AtomicU64,
    /// The terminal as the end mark was written to it, open until the end of
    /// output is fixed: closing it earlier can discard output not yet read.
    #[cfg(unix)]
    mark_writer: Mutex<Option<yagura_os::pty::EndMarkWriter>>,
    /// The holder's children have changed: some were reaped.
    #[cfg(target_os = "linux")]
    children_changed: ChildChanges,
    /// The cgroup of the child's scope, once the child was moved into it.
    #[cfg(target_os = "linux")]
    child_cgroup: Mutex<Option<std::path::PathBuf>>,
    /// Wakes the core when the child exited or the output ended.
    core_wake: Notify,
}

impl Shared {
    fn exit(&self) -> MutexGuard<'_, Option<Option<i32>>> {
        lock(&self.exit)
    }

    fn end_output(&self) {
        self.output_ended.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        drop(lock(&self.mark_writer).take());
        self.core_wake.notify_one();
    }

    /// Keeps the written mark's terminal open until the end of output is
    /// fixed; closes it at once when it already is.
    #[cfg(unix)]
    fn hold_mark_writer(&self, writer: yagura_os::pty::EndMarkWriter) {
        let mut slot = lock(&self.mark_writer);
        if self.output_ended.load(Ordering::SeqCst) {
            drop(slot);
            drop(writer);
        } else {
            *slot = Some(writer);
        }
    }
}

/// How the end of the child's output is told.
#[derive(Default)]
enum Cut {
    /// Not yet: everything read is the child's output.
    #[default]
    Open,
    /// The output ends where this mark is read back.
    // Windows ends the output with its end of file, never with a mark.
    #[cfg_attr(windows, allow(dead_code))]
    Mark(Vec<u8>),
    /// No mark could be written: the output ended where it had been read.
    #[cfg_attr(windows, allow(dead_code))]
    Abandoned,
}

/// Counts the changes to the holder's children, for a thread that waits for
/// the next one.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct ChildChanges {
    count: Mutex<u64>,
    changed: std::sync::Condvar,
}

#[cfg(target_os = "linux")]
impl ChildChanges {
    fn count(&self) -> u64 {
        *lock(&self.count)
    }

    fn note(&self) {
        *lock(&self.count) += 1;
        self.changed.notify_all();
    }

    /// Waits until a change after `seen` has been noted.
    fn wait_past(&self, seen: u64) {
        let mut count = lock(&self.count);
        while *count == seen {
            count = self
                .changed
                .wait(count)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The terminal's output up to its end: the end of file, or the end mark
/// read back, whichever comes first. The mark is never passed on, and as
/// many bytes as a mark could start with are held back until they are known
/// not to start it. Past the end it reads on and drops what it reads, so
/// whoever still writes to the terminal is never held up.
struct CutReader {
    inner: Box<dyn Read + Send>,
    shared: Arc<Shared>,
    chunk: Vec<u8>,
    held: Vec<u8>,
    ready: Vec<u8>,
    ready_at: usize,
    end_found: bool,
    inner_ended: bool,
}

impl CutReader {
    fn new(inner: Box<dyn Read + Send>, shared: Arc<Shared>) -> Self {
        Self {
            inner,
            shared,
            chunk: vec![0; CUT_READ_CHUNK],
            held: Vec::new(),
            ready: Vec::new(),
            ready_at: 0,
            end_found: false,
            inner_ended: false,
        }
    }

    /// Moves what is known to come before the end from `held` to `ready`.
    fn release(&mut self) {
        let cut = lock(&self.shared.cut);
        match &*cut {
            Cut::Open => self.ready = std::mem::take(&mut self.held),
            Cut::Mark(mark) => {
                let found = if mark.is_empty() {
                    None
                } else {
                    self.held
                        .windows(mark.len())
                        .position(|bytes| bytes == mark.as_slice())
                };
                if let Some(at) = found {
                    let after = self.held.len() - at - mark.len();
                    self.held.truncate(at);
                    self.ready = std::mem::take(&mut self.held);
                    self.end_found = true;
                    drop(cut);
                    self.drop_bytes(after);
                } else {
                    let keep = mark.len().saturating_sub(1);
                    if self.held.len() > keep {
                        self.ready = self.held.drain(..self.held.len() - keep).collect();
                    }
                }
            }
            Cut::Abandoned => {
                let held = std::mem::take(&mut self.held);
                self.end_found = true;
                drop(cut);
                self.drop_bytes(held.len());
            }
        }
    }

    fn drop_bytes(&self, count: usize) {
        self.shared
            .dropped_bytes
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    /// The output ended: what was held is the last of it.
    fn end_here(&mut self) {
        self.inner_ended = true;
        self.ready = std::mem::take(&mut self.held);
        self.end_found = true;
    }

    /// Called once everything before the end has been passed on: tells the
    /// core, then reads and drops until the terminal's output ends.
    fn read_past_end(&mut self) {
        self.shared.end_output();
        while !self.inner_ended {
            match self.inner.read(&mut self.chunk) {
                Ok(0) => self.inner_ended = true,
                Ok(read) => self.drop_bytes(read),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.inner_ended = true,
            }
        }
    }
}

impl Read for CutReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.ready_at < self.ready.len() {
                let count = buf.len().min(self.ready.len() - self.ready_at);
                buf[..count].copy_from_slice(&self.ready[self.ready_at..self.ready_at + count]);
                self.ready_at += count;
                return Ok(count);
            }
            self.ready.clear();
            self.ready_at = 0;
            if self.end_found {
                self.read_past_end();
                return Ok(0);
            }
            match self.inner.read(&mut self.chunk) {
                // Linux ends the output (EIO) once nothing has the terminal
                // open: an end before the mark is the end.
                Ok(0) => self.end_here(),
                Ok(read) => {
                    self.held.extend_from_slice(&self.chunk[..read]);
                    self.release();
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.end_here(),
            }
        }
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
    WriterDone,
}

/// The child and the handles to reach it.
struct Child {
    killer: PtyKiller,
    group: Arc<ProcessGroup>,
    pid: u32,
    gate: Arc<ReapGate>,
    control: Arc<PtyControl>,
    terminal: Terminal,
    force_kill_scheduled: bool,
    tree_end_started: bool,
}

impl Child {
    /// Kills before letting go of the input: on Windows closing the input is
    /// itself a kill, and the explicit kill is to decide the exit code.
    fn terminate(&mut self, tree: bool) {
        if !self.gate.is_reaped() {
            let _ = self.killer.kill(&self.gate);
            if !self.force_kill_scheduled {
                self.force_kill_scheduled = true;
                yagura_os::schedule_force_kill(
                    self.pid,
                    TERMINATE_ESCALATION_GRACE,
                    Arc::clone(&self.gate),
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

/// Ends the tree of the exited `leader`, still unreaped, and returns once its
/// every process has died. The holder is their reaper, so the tree has ended
/// once the zombie leader is the holder's only child; each reap is a change
/// to look again at.
#[cfg(target_os = "linux")]
fn end_tree_after_exit(group: &ProcessGroup, leader: u32, gate: &ReapGate, shared: &Shared) {
    use yagura_os::platform_unix::{child_pids, kill_orphans};

    let only_leader = child_pids().is_ok_and(|children| children.iter().all(|&pid| pid == leader));
    if only_leader {
        // Nobody is left to be given the grace.
        if let Err(error) = group.kill_now() {
            crate::log(format_args!(
                "failed to end the child's process tree: {error}"
            ));
        }
    } else {
        end_tree(group);
    }
    loop {
        let seen = shared.children_changed.count();
        let cgroup = lock(&shared.child_cgroup).clone();
        if let Some(cgroup) = cgroup
            && let Err(error) = yagura_os::platform_unix::kill_cgroup(&cgroup)
        {
            crate::log(format_args!(
                "cannot kill the child's scope through its cgroup: {error}"
            ));
        }
        if let Err(error) = group.kill_now() {
            crate::log(format_args!(
                "failed to end the child's process group: {error}"
            ));
        }
        match kill_orphans(leader, gate) {
            Ok(0) => return,
            Ok(_) => shared.children_changed.wait_past(seen),
            Err(error) => {
                crate::log(format_args!(
                    "cannot end the rest of the child's process tree: {error}"
                ));
                return;
            }
        }
    }
}

/// Ends the tree of the exited `leader` and returns once every member of its
/// group has exited, or, when the members cannot be watched, once the last
/// signal is sent.
#[cfg(target_os = "macos")]
fn end_tree_after_exit(group: &ProcessGroup, leader: u32, _gate: &ReapGate, _shared: &Shared) {
    end_tree(group);
    if let Err(error) = yagura_os::platform_unix::wait_group_exit(leader) {
        crate::log(format_args!(
            "cannot watch the child's process group end: {error}"
        ));
    }
}

/// Ends the tree of the exited child. Its end is the end of the output that
/// follows (Windows), or is not known apart from the signals sent.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn end_tree_after_exit(group: &ProcessGroup, _leader: u32, _gate: &ReapGate, _shared: &Shared) {
    end_tree(group);
}

/// Fixes where the child's output ends, once its whole tree has died.
/// Closing the pseudoconsole ends the output once the console host is done.
#[cfg(windows)]
fn end_output(control: &PtyControl, _shared: &Shared) {
    control.close();
}

/// Fixes where the child's output ends, once its whole tree has died: at a
/// mark nobody could know before, written after every writer of the tree,
/// which the reader is told of before it can arrive.
#[cfg(unix)]
fn end_output(control: &PtyControl, shared: &Shared) {
    let written = PtyControl::new_end_mark().and_then(|mark| {
        *lock(&shared.cut) = Cut::Mark(mark.clone());
        control.write_mark(&mark)
    });
    let written = written.map(|writer| shared.hold_mark_writer(writer));
    if let Err(error) = written {
        crate::log(format_args!(
            "cannot mark the end of the child's output, which ends where it was read: {error}"
        ));
        *lock(&shared.cut) = Cut::Abandoned;
        shared.end_output();
    }
}

/// Reaps the holder's children other than the leader as they exit, for as
/// long as the window lives, and tells whoever waits for the tree's end.
#[cfg(target_os = "linux")]
async fn reap_orphans_as_they_end(
    mut signals: yagura_os::platform_unix::ChildSignals,
    leader: u32,
    gate: Arc<ReapGate>,
    shared: Arc<Shared>,
) {
    while signals.next().await {
        if let Err(error) = yagura_os::platform_unix::reap_orphans(leader, &gate) {
            crate::log(format_args!("cannot reap the child's orphans: {error}"));
        }
        shared.children_changed.note();
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
    // Every process the child's tree leaves behind becomes the holder's, so
    // the tree's end can be known from the holder's children.
    #[cfg(target_os = "linux")]
    yagura_os::platform_unix::set_child_subreaper().map_err(|error| {
        SpawnError::other(format!(
            "cannot become the reaper of the child's tree: {error}"
        ))
    })?;
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
    let gate = Arc::new(ReapGate::new());
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
                move || buffer::read_into(Box::new(CutReader::new(reader, shared)), &intake)
            })
        })
        .and_then(|()| {
            spawn_thread("holder-exit", {
                let shared = Arc::clone(shared);
                let group = Arc::clone(&group);
                let gate = Arc::clone(&gate);
                let control = Arc::clone(&control);
                move || {
                    // The monitor is the child's only waiter, and each step
                    // starts on the one before: the exit, known without
                    // reaping so the leader's id keeps holding its group;
                    // the tree's end; the end of its output; the reap.
                    if let Err(error) = child.wait_for_exit() {
                        crate::log(format_args!("cannot wait for the child's exit: {error}"));
                    }
                    shared.child_ended.store(true, Ordering::SeqCst);
                    shared.core_wake.notify_one();
                    end_tree_after_exit(&group, pid, &gate, &shared);
                    end_output(&control, &shared);
                    let code = gate.reap_with(|| child.wait().ok());
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
        gate,
        control,
        terminal,
        force_kill_scheduled: false,
        tree_end_started: false,
    })
}

/// A window's holder before its first accepted `Hello`.
pub(crate) struct Window {
    pub(crate) launch: HolderLaunch,
    pub(crate) place: Place,
    /// Removed after the connection is closed and before the endpoint goes.
    pub(crate) record: RecordFile,
    pub(crate) succession: Succession,
    pub(crate) faults: Faults,
}

/// How the holder ended.
pub(crate) enum Ending {
    /// No daemon was let in before the deadline; the child never started.
    NoHello,
    /// The window was closed, by `Terminate` or by a lost connection no next
    /// daemon comes after, and its child has exited.
    WindowClosed,
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
    /// The child's tree and output have ended and its last copy is sent.
    exit_settled: bool,
    /// The writer thread has answered its last write.
    writer_done: bool,
    /// Lets the writes in hand finish while the writer thread is waited for.
    input_drain: Option<yagura_os::pty::InputDrain>,
    /// `Exited` has been told; every later connection is told it after its
    /// whole copy of the last screen.
    exit_told: bool,
    /// Focus intervals the current connection opened and has not closed.
    focus_intervals: usize,
}

/// Serves the window until the holder is to end.
pub(crate) async fn serve(
    window: Window,
    listener: <PlatformIpc as IpcTransport>::Listener,
) -> Ending {
    let (events, mut incoming) = mpsc::unbounded_channel();
    let acceptor = Acceptor::spawn(
        listener,
        events.clone(),
        window.place.daemon_endpoint.clone(),
        window.place.user_id.clone(),
    );
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
        exit_settled: false,
        writer_done: false,
        input_drain: None,
        exit_told: false,
        focus_intervals: 0,
    };
    let ending = loop {
        if !core.exit_settled
            && core.shared.exit().is_some()
            && core.shared.output_ended.load(Ordering::SeqCst)
        {
            core.exit_settled = true;
            core.send_last_copy().await;
            core.let_go_of_input();
        }
        // Every write is answered by the writer thread alone, and all its
        // answers are handled before its end is: none is guessed here.
        if core.exit_settled && core.writer_done && !core.exit_told {
            core.send_exited();
            core.release_terminal();
        }
        // The holder outlives the child: the window ends only once closed.
        if core.exit_told && core.terminating {
            break Ending::WindowClosed;
        }
        tokio::select! {
            () = tokio::time::sleep_until(hello_deadline), if core.child.is_none() => {
                break Ending::NoHello;
            }
            () = core.shared.core_wake.notified() => {}
            event = incoming.recv() => {
                let Some(event) = event else { break Ending::WindowClosed };
                if let Some(ending) = core.handle(event).await {
                    break ending;
                }
            }
        }
    };
    if core.child.is_some() {
        crate::log(format_args!(
            "dropped {} bytes of terminal output read after the child's exit",
            core.shared.dropped_bytes.load(Ordering::SeqCst)
        ));
    }
    // The connection closes, then the record goes, then the endpoint: a
    // daemon that finds the endpoint free finds nothing of the window left.
    let removed = core.finish().await;
    acceptor.stop(removed).await;
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

    /// Stops accepting and gives up the endpoint once the record is gone;
    /// the listener removes what it left on disk as it is dropped.
    async fn stop(self, _removed: RecordRemoved) {
        let _ = self.stop.send(());
        drop(self.task.await);
    }
}

impl Core {
    fn terminal(&self) -> Option<&Terminal> {
        self.child.as_ref().map(|child| &child.terminal)
    }

    fn to_terminal(&self, control: Control) {
        if let Some(terminal) = self.terminal() {
            terminal.intake.push(control);
        }
    }

    fn child_has_exited(&self) -> bool {
        self.exit_settled || self.shared.child_ended.load(Ordering::SeqCst)
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
                    self.lose(conn);
                }
                let running = !self.child_has_exited();
                let child = self.child.as_mut().filter(|_| running);
                if self.window.succession.on_lost_connection(child) {
                    self.terminating = true;
                }
            }
            Event::WriteDone { conn, id, written } => {
                if self.is_current(conn) {
                    self.answer_write(id, written);
                }
            }
            Event::WriterDone => {
                self.writer_done = true;
                self.input_drain = None;
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
    /// copy of the screen, followed by `Exited` once the child's exit is told.
    async fn accept(&mut self, accepted: Accepted) -> Option<Ending> {
        if let Some(previous) = self.conn.take() {
            previous.send(Reply::Displaced);
            self.lose(previous);
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
            #[cfg(target_os = "linux")]
            let signals = match yagura_os::platform_unix::ChildSignals::new() {
                Ok(signals) => signals,
                Err(error) => {
                    let error = SpawnError::other(format!(
                        "cannot learn of the exits of the child's tree: {error}"
                    ));
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
                    #[cfg(target_os = "linux")]
                    tokio::spawn(reap_orphans_as_they_end(
                        signals,
                        pid,
                        Arc::clone(&child.gate),
                        Arc::clone(&self.shared),
                    ));
                    #[cfg(target_os = "linux")]
                    if let Some(scopes) = self.window.record.written_scopes() {
                        scopes::ask_for(scopes, pid, &child.gate, &self.shared);
                    }
                    self.child = Some(child);
                    if let Err(error) = self.window.record.set_child(pid) {
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
            exited: self.exit_told,
            child_pid,
        };
        let conn = Conn::welcome(accepted, id, welcome, self.events.clone());
        terminal.outlet.open(id, conn.sender());
        let (done, sent) = oneshot::channel();
        terminal.intake.push(Control::Connected { conn: id, done });
        // The model is still once the exit is settled, so this copy is its
        // last screen, and it goes before `Exited`, told now or later.
        if self.exit_settled {
            let _ = sent.await;
        }
        if self.exit_told {
            conn.send(Reply::Exited {
                code: self.shared.exit().flatten(),
            });
        }
        self.conn = Some(conn);
        None
    }

    /// Lets go of a connection that is lost or displaced: what was opened on
    /// it is closed (its focus intervals, its view, the copies it was sent),
    /// so the next connection starts from a whole copy.
    fn lose(&mut self, conn: Conn) {
        self.close_outlet(&conn);
        self.to_terminal(Control::Disconnected);
        let intervals = std::mem::take(&mut self.focus_intervals);
        if let Some(terminal) = self.terminal() {
            terminal.input.close_intervals(intervals);
        }
        drop(conn.close());
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
                self.focus_intervals = if open {
                    self.focus_intervals.saturating_add(1)
                } else {
                    self.focus_intervals.saturating_sub(1)
                };
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
                let running = !self.child_has_exited();
                if let Some(child) = self.child.as_mut().filter(|_| running) {
                    child.terminate(tree);
                }
                if self.exit_told {
                    return Some(Ending::WindowClosed);
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
        if !queued && let Some(conn) = &self.conn {
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

    /// Lets go of the child's input, so the writer thread answers what it
    /// still holds and ends, and lets a write in hand that nobody reads any
    /// more finish its piece meanwhile.
    fn let_go_of_input(&mut self) {
        let Some(child) = &self.child else { return };
        child.terminal.input.close();
        if self.writer_done {
            return;
        }
        match child.control.drain_input() {
            Ok(drain) => self.input_drain = Some(drain),
            Err(error) => crate::log(format_args!(
                "cannot read the child's input in its place, so a write nobody reads may never end: {error}"
            )),
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

    /// Tells the connection the child exited; no copy of the screen follows
    /// on it.
    fn send_exited(&mut self) {
        self.exit_told = true;
        let code = self.shared.exit().flatten();
        if let Some(conn) = &self.conn {
            conn.send(Reply::Exited { code });
        }
    }

    /// Lets go of the handle kept for resizing. On Unix it only resizes, so
    /// it goes once the exit is told; on Windows closing it ended the output
    /// and was done before.
    fn release_terminal(&self) {
        if let Some(child) = &self.child {
            child.control.close();
        }
    }

    /// Closes the connection after what was sent on it, waiting a bounded
    /// time for a daemon that stopped reading, then removes the record.
    async fn finish(&mut self) -> RecordRemoved {
        if let Some(conn) = self.conn.take() {
            self.close_outlet(&conn);
            let _ = tokio::time::timeout(FINAL_SEND_LIMIT, conn.close()).await;
        }
        self.window.record.remove()
    }
}

/// The systemd user scopes of the holder and its child (Linux), asked for once
/// the child has started. The window never waits for the answer: the child
/// runs, and its tree ends, under the holder's own care either way, and a
/// scope only adds that the tree ends with the holder.
#[cfg(target_os = "linux")]
mod scopes {
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use yagura_os::ReapGate;
    use yagura_os::platform_unix::{
        UserManager, cgroup_dir_of, descendants_outside, orphan_pidfds, pidfd_open,
    };

    use super::{Shared, lock};
    use crate::record::WrittenScopes;

    /// Opens the holder's and the leader's pidfds now, the leader's under its
    /// reap gate so its id still names it, and asks for the scopes on a task
    /// of their own. A failure is logged once and nothing more is asked.
    pub(super) fn ask_for(
        scopes: WrittenScopes,
        leader: u32,
        gate: &Arc<ReapGate>,
        shared: &Arc<Shared>,
    ) {
        let opened = pidfd_open(std::process::id()).and_then(|holder| {
            let leader = gate
                .signal_unless_reaped(|| pidfd_open(leader))
                .unwrap_or_else(|| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "the child has been reaped",
                    ))
                })?;
            Ok((holder, leader))
        });
        let (holder_fd, leader_fd) = match opened {
            Ok(fds) => fds,
            Err(error) => {
                crate::log(format_args!(
                    "cannot name the processes to move into the window's scopes: {error}"
                ));
                return;
            }
        };
        tokio::spawn(request(
            scopes,
            holder_fd,
            leader_fd,
            leader,
            Arc::clone(gate),
            Arc::clone(shared),
        ));
    }

    async fn request(
        scopes: WrittenScopes,
        holder_fd: OwnedFd,
        leader_fd: OwnedFd,
        leader: u32,
        gate: Arc<ReapGate>,
        shared: Arc<Shared>,
    ) {
        let manager = match UserManager::connect().await {
            Ok(manager) => manager,
            Err(error) => {
                crate::log(format_args!(
                    "cannot ask the user manager for the window's scopes: {error}"
                ));
                return;
            }
        };
        if let Err(error) = manager
            .start_holder_scope(scopes.holder(), holder_fd.as_fd())
            .await
        {
            crate::log(format_args!(
                "cannot move the holder into its scope: {error}"
            ));
            return;
        }
        // A child that has exited is ending its tree already.
        if shared.child_ended.load(Ordering::SeqCst) {
            return;
        }
        let orphans = match orphan_pidfds(leader, &gate) {
            Ok(orphans) => orphans,
            Err(error) => {
                crate::log(format_args!(
                    "cannot name the child's tree to move into its scope: {error}"
                ));
                return;
            }
        };
        let mut members = vec![leader_fd.as_fd()];
        members.extend(orphans.iter().map(|fd| fd.as_fd()));
        if let Err(error) = manager
            .start_child_scope(scopes.child(), scopes.holder(), &members)
            .await
        {
            crate::log(format_args!(
                "cannot move the child into its scope: {error}"
            ));
            return;
        }
        match gate.signal_unless_reaped(|| cgroup_dir_of(leader)) {
            Some(Ok(Some(cgroup))) => {
                // Not acted on: how often a child's tree grows before its scope is
                // made, for whoever reads the log.
                match descendants_outside(&cgroup) {
                    Ok(outside) => crate::log(format_args!(
                        "the child's scope is made; {outside} of the holder's descendants are outside it"
                    )),
                    Err(error) => crate::log(format_args!(
                        "the child's scope is made; its outsiders cannot be counted: {error}"
                    )),
                }
                *lock(&shared.child_cgroup) = Some(cgroup);
            }
            // The child was reaped meanwhile: its tree has ended.
            None | Some(Ok(None)) => {}
            Some(Err(error)) => crate::log(format_args!(
                "cannot read the cgroup of the child's scope: {error}"
            )),
        }
    }
}

/// Whether a next daemon comes for the window: the one difference between a
/// window that outlives its daemon and one that does not.
mod succession {
    use yagura_protocol::holder::HolderLaunch;

    use super::Child;

    pub(crate) struct Succession {
        awaited: bool,
    }

    impl Succession {
        pub(crate) fn new(launch: &HolderLaunch) -> Self {
            Self {
                awaited: launch.awaits_successor,
            }
        }

        /// The connection was lost other than by this holder closing it on a
        /// displacement. With a next daemon to come, the child runs on and
        /// the holder waits for it; with none, the child's tree (`child`,
        /// while it runs) is ended. Returns whether the window ends.
        pub(super) fn on_lost_connection(&self, child: Option<&mut Child>) -> bool {
            if self.awaited {
                return false;
            }
            if let Some(child) = child {
                child.terminate(true);
            }
            true
        }
    }
}
