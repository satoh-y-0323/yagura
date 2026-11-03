//! The holder's core: the child on its terminal, the output kept for it, and
//! the one daemon connected at a time.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use yagura_os::pty::{EnvOp, PtyCommand, PtyControl, PtyKiller, PtySize, PtySpawn};
use yagura_os::{IpcTransport, PlatformIpc, ProcessGroup};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderEnvOp, HolderExit, HolderFrame, HolderLaunch, HolderRecord,
    MAX_HELLO_DEADLINE_MS, encode_holder_frame,
};
use yagura_protocol::window_size_within_limit;

use crate::buffer::OutputState;
use crate::endpoint::{self, Place};
use crate::peer::{self, Accepted};
use crate::record;

/// How long the output is still read after the child's exit, for an end of
/// output that may never come (something else keeping the terminal open).
const POST_EXIT_DRAIN: Duration = Duration::from_secs(2);

/// How long the child has to act on the first, refusable kill before the OS
/// boundary forces it down (a no-op on Windows, whose kill cannot be refused).
const TERMINATE_ESCALATION_GRACE: Duration = Duration::from_millis(250);

/// Bytes the reader takes from the terminal at a time.
const READ_CHUNK: usize = 8192;

/// The most output one `Output` frame carries.
const OUTPUT_FRAME_BYTES: u64 = 64 * 1024;

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
    state: Mutex<OutputState>,
    /// Wakes the reader when it may read again.
    reader_wake: Condvar,
    /// Wakes the core when output arrived, the child exited or the output ended.
    core_wake: Notify,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, OutputState> {
        lock(&self.state)
    }

    fn wake_reader(&self) {
        self.reader_wake.notify_all();
    }
}

enum Event {
    Accepted(Accepted),
    Frame { conn: u64, frame: HolderFrame },
    Closed { conn: u64 },
    WriteDone { conn: u64, id: u64, written: bool },
}

/// The connected daemon.
struct Conn {
    id: u64,
    out: mpsc::UnboundedSender<Vec<u8>>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
    /// The end of the replay this connection was offered in its `Welcome`.
    replay_end: u64,
    /// How far output has been sent; `None` until the daemon asks for the replay.
    sent: Option<u64>,
}

impl Conn {
    fn open(
        accepted: Accepted,
        id: u64,
        replay_end: u64,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let (mut read, mut write) = tokio::io::split(accepted.connection);
        let (out, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(bytes) = outgoing.recv().await {
                if write.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            let _ = write.shutdown().await;
        });
        let mut frames = accepted.frames;
        let reader = tokio::spawn(async move {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                loop {
                    match frames.next_frame() {
                        Ok(Some(frame)) => {
                            if events.send(Event::Frame { conn: id, frame }).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            let _ = events.send(Event::Closed { conn: id });
                            return;
                        }
                    }
                }
                match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => {
                        let _ = events.send(Event::Closed { conn: id });
                        return;
                    }
                    Ok(read) => frames.push(&chunk[..read]),
                }
            }
        });
        Self {
            id,
            out,
            writer,
            reader,
            replay_end,
            sent: None,
        }
    }

    fn send(&self, frame: &HolderFrame) {
        let _ = self.out.send(encode_holder_frame(frame));
    }

    /// Stops reading and closes the connection once what was sent so far is
    /// written; returns the task doing that.
    fn close(self) -> JoinHandle<()> {
        self.reader.abort();
        drop(self.out);
        self.writer
    }
}

struct WriteJob {
    conn: u64,
    id: u64,
    bytes: Vec<u8>,
}

/// The child and the handles to reach it.
struct Child {
    killer: PtyKiller,
    group: Arc<ProcessGroup>,
    control: Arc<PtyControl>,
    pid: u32,
    exited: Arc<AtomicBool>,
    /// `None` once the input has been let go of.
    writes: Option<std::sync::mpsc::Sender<WriteJob>>,
    force_kill_scheduled: bool,
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
        if tree && let Err(error) = self.group.terminate() {
            crate::log(format_args!(
                "failed to end the child's process tree: {error}"
            ));
        }
        self.writes = None;
    }
}

fn to_env_op(op: &HolderEnvOp) -> EnvOp {
    match op {
        HolderEnvOp::Set { name, value } => EnvOp::Set(name.clone(), value.clone()),
        HolderEnvOp::Remove { name } => EnvOp::Remove(name.clone()),
    }
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map(drop)
}

/// Starts the child on its terminal, with the threads that read its output,
/// wait for its exit and write its input.
fn spawn_child(
    launch: &HolderLaunch,
    shared: &Arc<Shared>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<Child, String> {
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
    } = yagura_os::pty::spawn(&command, size).map_err(|error| error.to_string())?;
    let pid = child.process_id();
    let group = Arc::new(group);
    let control = Arc::new(control);
    let exited = Arc::new(AtomicBool::new(false));
    let (writes, jobs) = std::sync::mpsc::channel::<WriteJob>();

    let started = spawn_thread("holder-read", {
        let shared = Arc::clone(shared);
        move || read_output(&shared, reader)
    })
    .and_then(|()| {
        spawn_thread("holder-exit", {
            let shared = Arc::clone(shared);
            let group = Arc::clone(&group);
            let control = Arc::clone(&control);
            let exited = Arc::clone(&exited);
            move || {
                // The monitor is the child's only waiter. After the exit the
                // order is fixed: publish it, end the tree, then close the
                // terminal so its output can end.
                let code = child.wait().ok();
                exited.store(true, Ordering::SeqCst);
                shared.state().exit = Some(code);
                shared.wake_reader();
                shared.core_wake.notify_one();
                if let Err(error) = group.terminate() {
                    crate::log(format_args!(
                        "failed to end the exited child's tree: {error}"
                    ));
                }
                control.close();
            }
        })
    })
    .and_then(|()| {
        spawn_thread("holder-write", {
            let events = events.clone();
            move || write_input(writer, &jobs, &events)
        })
    });
    // Dropping what was started ends the child's tree with it.
    started.map_err(|error| format!("cannot start the holder's threads: {error}"))?;
    Ok(Child {
        killer,
        group,
        control,
        pid,
        exited,
        writes: Some(writes),
        force_kill_scheduled: false,
    })
}

/// Reads the terminal until its output ends, never dropping a byte: when the
/// kept output is full, or the connected daemon is a window behind, it stops
/// reading and the child is held up in its write instead.
fn read_output(shared: &Shared, mut reader: Box<dyn Read + Send>) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        {
            let mut state = shared.state();
            while state.reader_must_wait() {
                state = shared
                    .reader_wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                shared.state().buffer.append(&buf[..read]);
                shared.core_wake.notify_one();
            }
        }
    }
    shared.state().reader_done = true;
    shared.core_wake.notify_one();
}

/// Writes each job to the child's input in order, then lets go of the input
/// when the core does.
fn write_input(
    mut writer: Box<dyn Write + Send>,
    jobs: &std::sync::mpsc::Receiver<WriteJob>,
    events: &mpsc::UnboundedSender<Event>,
) {
    for WriteJob { conn, id, bytes } in jobs {
        let written = writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_ok();
        let _ = events.send(Event::WriteDone { conn, id, written });
    }
}

/// A window's holder before its first accepted `Hello`.
pub(crate) struct Window {
    pub(crate) window_id: String,
    pub(crate) launch: HolderLaunch,
    pub(crate) place: Place,
    pub(crate) record: HolderRecord,
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
    size: (u16, u16),
    terminating: bool,
    exit_seen: Option<Instant>,
    exit_reported: bool,
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
    let size = (window.launch.cols, window.launch.rows);
    let mut core = Core {
        window,
        shared: Arc::new(Shared::default()),
        events,
        child: None,
        conn: None,
        next_conn: 0,
        size,
        terminating: false,
        exit_seen: None,
        exit_reported: false,
    };
    let ending = loop {
        let output_ended = core.shared.state().reader_done;
        if let Some(seen) = core.exit_seen
            && !core.exit_reported
            && (output_ended || Instant::now() >= seen + POST_EXIT_DRAIN)
        {
            // The exit file is written while the endpoint is still held, so a
            // next holder of this window starts only after it and clears it.
            core.report_exit();
            Acceptor::stop(&mut acceptor, &core.window.place).await;
            if core.terminating || core.conn.is_none() {
                break Ending::ChildExited;
            }
        }
        let drain_deadline = core
            .exit_seen
            .filter(|_| !core.exit_reported)
            .map(|seen| seen + POST_EXIT_DRAIN);
        tokio::select! {
            () = tokio::time::sleep_until(hello_deadline), if core.child.is_none() => {
                record::remove_record(&core.window.place, &core.window.window_id);
                break Ending::NoHello;
            }
            () = tokio::time::sleep_until(drain_deadline.unwrap_or(hello_deadline)),
                if drain_deadline.is_some() => {}
            () = core.shared.core_wake.notified() => core.on_output(),
            event = incoming.recv() => {
                let Some(event) = event else { break Ending::ChildExited };
                if let Some(ending) = core.handle(event).await {
                    break ending;
                }
            }
        }
    };
    Acceptor::stop(&mut acceptor, &core.window.place).await;
    core.close_conn().await;
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

    /// Stops accepting and gives up the endpoint. A Unix socket file is
    /// removed before the listener, and the lock with it, is let go of, so it
    /// is never a next holder's socket that gets removed.
    async fn stop(acceptor: &mut Option<Self>, place: &Place) {
        let Some(Self { stop, task }) = acceptor.take() else {
            return;
        };
        let _ = stop.send(());
        // A task that did not hand the listener back has already dropped it,
        // so the file may be someone else's by now and is left alone.
        if let Ok(listener) = task.await {
            endpoint::remove_endpoint_file(place);
            drop(listener);
        }
    }
}

impl Core {
    fn on_output(&mut self) {
        self.flush();
        if self.exit_seen.is_none() && self.shared.state().exit.is_some() {
            self.exit_seen = Some(Instant::now());
        }
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
                    drop(conn.close());
                }
                self.shared.state().ack_floor = None;
                self.shared.wake_reader();
                if self.exit_reported {
                    return Some(Ending::ChildExited);
                }
            }
            Event::WriteDone { conn, id, written } if self.is_current(conn) => {
                self.answer_write(id, written);
            }
            Event::Frame { .. } | Event::Closed { .. } | Event::WriteDone { .. } => {}
        }
        None
    }

    fn is_current(&self, conn: u64) -> bool {
        self.conn.as_ref().is_some_and(|current| current.id == conn)
    }

    fn answer_write(&self, id: u64, written: bool) {
        let Some(conn) = &self.conn else { return };
        if written {
            conn.send(&HolderFrame::Written { id });
        } else {
            conn.send(&HolderFrame::WriteFailed {
                id,
                reason: "the write to the child's input failed".to_owned(),
            });
        }
    }

    /// Lets a daemon in: the previous one is told it was displaced, the child
    /// starts on the first one, and the newcomer is welcomed.
    async fn accept(&mut self, accepted: Accepted) -> Option<Ending> {
        if self.exit_reported {
            return None;
        }
        if let Some(previous) = self.conn.take() {
            previous.send(&HolderFrame::Displaced);
            drop(previous.close());
        }
        let id = self.next_conn;
        self.next_conn += 1;
        if self.child.is_none() {
            let launch = self.window.launch.clone();
            let shared = Arc::clone(&self.shared);
            let events = self.events.clone();
            let spawned =
                tokio::task::spawn_blocking(move || spawn_child(&launch, &shared, &events)).await;
            match spawned {
                Ok(Ok(child)) => {
                    self.window.record.child_pid = Some(child.pid);
                    self.child = Some(child);
                    if let Err(error) =
                        record::write_record(&self.window.place, &self.window.record)
                    {
                        crate::log(format_args!("cannot update the holder record: {error}"));
                    }
                }
                Ok(Err(error)) => return Some(self.spawn_failed(accepted, id, &error)),
                Err(error) => return Some(self.spawn_failed(accepted, id, &error.to_string())),
            }
        }
        let (welcome, replay_end) = {
            let mut state = self.shared.state();
            state.ack_floor = Some(state.buffer.start());
            let replay_end = state.buffer.end();
            let welcome = HolderFrame::Welcome {
                protocol: HOLDER_PROTOCOL,
                record: self.window.launch.record.clone(),
                cols: self.size.0,
                rows: self.size.1,
                exited: state.exit.is_some(),
                checkpoint_seq: state.buffer.checkpoint().map(|checkpoint| checkpoint.seq),
                replay_end_seq: replay_end,
            };
            (welcome, replay_end)
        };
        let conn = Conn::open(accepted, id, replay_end, self.events.clone());
        conn.send(&welcome);
        self.conn = Some(conn);
        self.shared.wake_reader();
        None
    }

    fn spawn_failed(&mut self, accepted: Accepted, id: u64, error: &str) -> Ending {
        crate::log(format_args!("cannot start the child: {error}"));
        let exit = HolderExit {
            code: None,
            checkpoint: None,
            output: Vec::new(),
            end_seq: 0,
        };
        if let Err(error) = record::write_exit(&self.window.place, &self.window.window_id, &exit) {
            crate::log(format_args!("cannot write the exit file: {error}"));
        }
        let conn = Conn::open(accepted, id, 0, self.events.clone());
        conn.send(&HolderFrame::Exited { code: None });
        self.conn = Some(conn);
        Ending::SpawnFailed
    }

    fn on_frame(&mut self, frame: HolderFrame) -> Option<Ending> {
        match frame {
            HolderFrame::Replay => self.replay(),
            HolderFrame::Ack { seq } => {
                self.shared.state().acknowledge(seq);
                self.shared.wake_reader();
            }
            HolderFrame::StoreCheckpoint { seq, bytes } => {
                self.shared.state().buffer.store_checkpoint(seq, bytes);
                self.shared.wake_reader();
            }
            HolderFrame::Write { id, bytes } => self.write(id, bytes),
            HolderFrame::Resize { cols, rows } => self.resize(cols, rows),
            HolderFrame::Terminate { tree } => {
                self.terminating = true;
                if let Some(child) = self.child.as_mut() {
                    child.terminate(tree);
                }
                if self.exit_reported {
                    return Some(Ending::ChildExited);
                }
            }
            HolderFrame::Hello { .. }
            | HolderFrame::Welcome { .. }
            | HolderFrame::Checkpoint { .. }
            | HolderFrame::Output { .. }
            | HolderFrame::Written { .. }
            | HolderFrame::WriteFailed { .. }
            | HolderFrame::Exited { .. }
            | HolderFrame::Displaced
            | HolderFrame::VersionRejected { .. } => {}
        }
        None
    }

    fn write(&self, id: u64, bytes: Vec<u8>) {
        let Some(conn) = &self.conn else { return };
        let exited = self.exit_reported || self.shared.state().exit.is_some();
        let queued = !exited
            && self
                .child
                .as_ref()
                .and_then(|child| child.writes.as_ref())
                .is_some_and(|writes| {
                    writes
                        .send(WriteJob {
                            conn: conn.id,
                            id,
                            bytes,
                        })
                        .is_ok()
                });
        if !queued {
            conn.send(&HolderFrame::WriteFailed {
                id,
                reason: "the child has exited or its input is closed".to_owned(),
            });
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let fits = window_size_within_limit(cols, rows);
        let Some(child) = self.child.as_ref().filter(|_| fits) else {
            return;
        };
        if child.control.resize(cols, rows).is_ok() {
            self.size = (cols, rows);
        }
    }

    /// Sends the checkpoint and the output up to the `Welcome`'s
    /// `replay_end_seq`, then everything read since, once per connection.
    fn replay(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        if conn.sent.is_some() {
            return;
        }
        {
            let state = self.shared.state();
            if let Some(checkpoint) = state.buffer.checkpoint() {
                conn.send(&HolderFrame::Checkpoint {
                    seq: checkpoint.seq,
                    bytes: checkpoint.bytes.clone(),
                });
            }
            let from = state.buffer.start();
            let to = conn.replay_end.max(from);
            send_output(conn, &state, from, to);
            conn.sent = Some(to);
        }
        self.flush();
    }

    /// Sends what was read after what has been sent, once the replay is done.
    fn flush(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let Some(sent) = conn.sent else {
            return;
        };
        let state = self.shared.state();
        let from = sent.max(state.buffer.start());
        let to = state.buffer.end();
        send_output(conn, &state, from, to);
        conn.sent = Some(to.max(from));
    }

    /// Writes the exit and the kept output to the exit file first, so a daemon
    /// that ends right after `Exited` loses nothing, then tells the daemon.
    fn report_exit(&mut self) {
        self.flush();
        let exit = {
            let state = self.shared.state();
            HolderExit {
                code: state.exit.flatten(),
                checkpoint: state.buffer.checkpoint().cloned(),
                output: state
                    .buffer
                    .range(state.buffer.start(), state.buffer.end())
                    .to_vec(),
                end_seq: state.buffer.end(),
            }
        };
        if let Err(error) = record::write_exit(&self.window.place, &self.window.window_id, &exit) {
            crate::log(format_args!("cannot write the exit file: {error}"));
        }
        if let Some(conn) = &self.conn {
            conn.send(&HolderFrame::Exited { code: exit.code });
        }
        self.exit_reported = true;
    }

    /// Closes the connection after what was sent on it, waiting a bounded
    /// time for a daemon that stopped reading.
    async fn close_conn(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = tokio::time::timeout(FINAL_SEND_LIMIT, conn.close()).await;
        }
    }
}

/// Sends the kept output from `from` to `to` as `Output` frames.
fn send_output(conn: &Conn, state: &OutputState, from: u64, to: u64) {
    let mut at = from;
    while at < to {
        let bytes = state
            .buffer
            .range(at, to.min(at.saturating_add(OUTPUT_FRAME_BYTES)));
        if bytes.is_empty() {
            return;
        }
        at += bytes.len() as u64;
        conn.send(&HolderFrame::Output {
            seq: at,
            bytes: bytes.to_vec(),
        });
    }
}
