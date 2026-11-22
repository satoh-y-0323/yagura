//! The owner, driven through the wiring it has in production: its stages
//! write to and read from real generations over in-memory streams, and the
//! news a generation ends with comes from its reader alone. Nothing
//! here reads a clock, opens a socket, spawns a task or sleeps: the order
//! in which results reach the owner is the order the test hands them over.

use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, Join, duplex};
use yagura_protocol::{
    ClientMessage, DaemonMessage, decode_correlated_client_message, encode_daemon_message,
    frame_encode,
};

use super::super::generation::{Settled, WithLateBytes};
use super::super::reader_link::ReaderLink;
use super::super::transition::EndKind;
use super::*;
use crate::events::{AttachResponse, LinkLostReason};
use crate::host::fake::{FakeHost, error_frame};
use crate::ipc_client::{ERROR_CODE_SUPERSEDED, Reply};

const PIPE: usize = 64 * 1024;
/// A daemon of the current wire revision.
const REVISION: u32 = yagura_protocol::PROTOCOL_REVISION;
/// Older than the first revision that carries frames.
const BEFORE_FRAMES: u32 = 18;
const WINDOW: &str = "w1";

type Outcome = <Stage as Future>::Output;

/// The write half of a connection: it writes until told to fail, and then
/// fails as an allocation failure does, with an error that does not say the
/// peer closed.
struct FlakyWriter {
    inner: DuplexStream,
    failing: Arc<AtomicBool>,
}

impl AsyncWrite for FlakyWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.failing.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::ErrorKind::OutOfMemory.into()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The daemon's end of a connection.
struct Peer {
    to_client: Option<DuplexStream>,
    from_client: Option<DuplexStream>,
    failing: Arc<AtomicBool>,
}

impl Peer {
    /// What the client wrote, as one request.
    async fn read_request(&mut self) -> (Option<u64>, ClientMessage) {
        let from_client = self.from_client.as_mut().expect("the daemon still reads");
        let mut length = [0u8; 4];
        from_client.read_exact(&mut length).await.expect("a length");
        let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
        from_client.read_exact(&mut body).await.expect("a body");
        decode_correlated_client_message(&body).expect("a request the client wrote")
    }

    async fn send(&mut self, message: &DaemonMessage) {
        let to_client = self.to_client.as_mut().expect("the daemon still writes");
        to_client
            .write_all(&frame_encode(&encode_daemon_message(message)))
            .await
            .expect("the client's end is open");
    }

    /// The daemon closes: whatever the client writes next fails as a write
    /// to a closed peer does.
    fn stop_reading(&mut self) {
        self.from_client = None;
    }

    /// The daemon lets go of the connection it was writing to.
    fn stop_writing(&mut self) {
        self.to_client = None;
    }

    /// From now on a write fails in a way that does not say the peer closed.
    fn make_writes_fail(&self) {
        self.failing.store(true, Ordering::SeqCst);
    }
}

type Stream = Join<DuplexStream, FlakyWriter>;

fn connection() -> (Stream, Peer) {
    let (client_read, to_client) = duplex(PIPE);
    let (client_write, from_client) = duplex(PIPE);
    let failing = Arc::new(AtomicBool::new(false));
    let stream = tokio::io::join(
        client_read,
        FlakyWriter {
            inner: client_write,
            failing: Arc::clone(&failing),
        },
    );
    let peer = Peer {
        to_client: Some(to_client),
        from_client: Some(from_client),
        failing,
    };
    (stream, peer)
}

/// A generation's reader, polled by hand.
struct Reader {
    task: Pin<Box<dyn Future<Output = ()> + Send>>,
    done: bool,
}

/// One generation of a scene, with the daemon's end and either its own
/// reader (polled by the test) or the link its reader would report through.
struct Conn {
    generation: Arc<Generation>,
    peer: Peer,
    reader: Option<Reader>,
    link: Option<ReaderLink>,
}

impl Conn {
    /// Polls the reader once, and tells whether it has ended.
    async fn reader_ended(&mut self) -> bool {
        let reader = self.reader.as_mut().expect("this connection has a reader");
        if !reader.done {
            let ready =
                std::future::poll_fn(|cx| Poll::Ready(reader.task.as_mut().poll(cx).is_ready()))
                    .await;
            reader.done = ready;
        }
        reader.done
    }

    /// The reader reports that the connection ended as `kind`.
    fn report(&mut self, kind: EndKind) {
        self.link
            .take()
            .expect("this connection's reader is reported for")
            .report(kind);
    }
}

/// Reads the request the generation wrote and answers it under the key it
/// carried.
async fn answer_next(
    generation: &Generation,
    peer: &mut Peer,
    kind: PendingKind,
    answer: Result<Reply, IpcError>,
) {
    let (key, _request) = peer.read_request().await;
    let awaiting = generation
        .awaiting()
        .expect("the request written is the one awaiting");
    assert_eq!(key, Some(awaiting.correlation_id));
    assert_eq!(
        generation.settle(Some(awaiting.correlation_id), kind, answer),
        Settled::Answered
    );
}

fn describe(step: &Step<IpcError>) -> String {
    match step {
        Step::Do(action) => format!("{action:?}"),
        Step::Answer { id, result, .. } => format!("Answer({id:?}, {result:?})"),
    }
}

enum Item {
    Result(Outcome),
    End(OwnerEvent),
}

/// The `index`th arrangement of `pool`, in the order of the permutations'
/// factorial number system.
fn arrangement<T>(mut pool: Vec<T>, mut index: usize) -> Vec<T> {
    let mut arranged = Vec::with_capacity(pool.len());
    while !pool.is_empty() {
        let size = factorial(pool.len() - 1);
        arranged.push(pool.remove(index / size));
        index %= size;
    }
    arranged
}

fn factorial(n: usize) -> usize {
    (1..=n).product()
}

/// A client's owner with the inbox its generations report to.
struct Rig {
    owner: Owner<FakeHost>,
    shared: Arc<Shared>,
    app: FakeHost,
    events: mpsc::UnboundedSender<OwnerEvent>,
    inbox: mpsc::UnboundedReceiver<OwnerEvent>,
    /// What reached the inbox and was not handed to the owner yet.
    news: Vec<OwnerEvent>,
}

impl Rig {
    fn new(kind: ClientKind) -> Self {
        let app = FakeHost::new("owner-scenes");
        let record = (kind == ClientKind::Window).then(|| Arc::new(WindowRecord::new()));
        let shared = Arc::new(Shared::new(kind, record, 0));
        let (events, inbox) = mpsc::unbounded_channel();
        let OwnerSlot::Unborn(publisher) = std::mem::replace(
            &mut *lock_blocking(&shared.slot),
            OwnerSlot::Running(events.clone()),
        ) else {
            panic!("a new client has no owner yet");
        };
        let owner = Owner::new(
            app.clone(),
            Arc::clone(&shared),
            publisher,
            events.downgrade(),
        );
        Self {
            owner,
            shared,
            app,
            events,
            inbox,
            news: Vec::new(),
        }
    }

    fn want(&mut self, mode: ConnectMode, until: Until) {
        let (reply, _answered) = oneshot::channel();
        self.owner.receive(OwnerEvent::Want { mode, until, reply });
    }

    fn pop(&mut self, what: &str) -> Step<IpcError> {
        self.owner
            .queue
            .pop_front()
            .unwrap_or_else(|| panic!("{what} is queued"))
    }

    /// The number of the attempt the owner started, which the scene does
    /// not run.
    fn next_attempt(&mut self) -> u64 {
        attempt_of(self.pop("an attempt"))
    }

    /// Hands the owner the connection of `attempt` and builds the
    /// generation it begins over a fresh stream. With a real reader the
    /// test polls it; without one the test reports its ends.
    fn begin(&mut self, attempt: u64, real_reader: bool) -> Conn {
        self.begin_with_late_bytes(attempt, real_reader, Vec::new())
    }

    /// `begin`, with `late` bytes that reach the reader's end only after a
    /// stopped reader has counted what was delivered.
    fn begin_with_late_bytes(&mut self, attempt: u64, real_reader: bool, late: Vec<u8>) -> Conn {
        self.owner.feed(Event::Connected { attempt }, false, None);
        let step = self.pop("the begin of a generation");
        let Step::Do(Action::Begin { generation, .. }) = step else {
            panic!("expected a begin, got {}", describe(&step));
        };
        let (stream, peer) = connection();
        let (begun, reader, link) = Generation::begin(
            generation,
            WithLateBytes {
                connection: stream,
                late,
            },
            Arc::clone(&self.shared.correlation_ids),
            Some(end_sink(self.events.downgrade())),
        );
        self.owner
            .generations
            .insert(generation, Arc::clone(&begun));
        if real_reader {
            let task = Box::pin(read_loop(
                reader,
                self.app.clone(),
                Arc::clone(&begun),
                link,
            ));
            Conn {
                generation: begun,
                peer,
                reader: Some(Reader { task, done: false }),
                link: None,
            }
        } else {
            drop(reader);
            Conn {
                generation: begun,
                peer,
                reader: None,
                link: Some(link),
            }
        }
    }

    /// Runs the revision round trip the generation's opening starts, with
    /// the daemon answering `revision`.
    async fn probe(&mut self, conn: &mut Conn, revision: u32) {
        let step = self.pop("the revision round trip");
        assert!(
            matches!(step, Step::Do(Action::ProbeRevision { .. })),
            "{}",
            describe(&step)
        );
        let stage = self.owner.execute(step).expect("the round trip is a stage");
        let answer = Ok(Reply::ProtocolRevision {
            revision,
            boot: None,
        });
        let (outcome, ()) = tokio::join!(
            stage,
            answer_next(
                &conn.generation,
                &mut conn.peer,
                PendingKind::GetProtocolRevision,
                answer
            )
        );
        self.owner.finish(outcome);
    }

    /// Carries out what is queued, except the steps that start something
    /// the scene runs itself: those are returned.
    async fn carry_out(&mut self) -> Vec<Step<IpcError>> {
        let mut held = Vec::new();
        while let Some(step) = self.owner.queue.pop_front() {
            if matches!(
                step,
                Step::Do(
                    Action::Attempt { .. } | Action::Register { .. } | Action::RestoreAttach { .. }
                )
            ) {
                held.push(step);
            } else if let Some(stage) = self.owner.execute(step) {
                let outcome = stage.await;
                self.owner.finish(outcome);
            }
        }
        held
    }

    fn collect(&mut self) {
        while let Ok(event) = self.inbox.try_recv() {
            self.news.push(event);
        }
    }

    /// The reader of `conn` reports `kind` and the owner hears it.
    fn lose(&mut self, conn: &mut Conn, kind: EndKind) {
        conn.report(kind);
        self.collect();
        for event in std::mem::take(&mut self.news) {
            self.owner.receive(event);
        }
    }

    /// How many ends of a generation reached the inbox, and how many of
    /// them are `kind`. The inbox holds one generation's news at a time in
    /// these scenes, and an end is told apart by how it prints, so that
    /// the test does not depend on how the news is built.
    fn ends(&mut self, kind: EndKind) -> (usize, usize) {
        self.collect();
        let ended: Vec<String> = self
            .news
            .iter()
            .map(|event| format!("{event:?}"))
            .filter(|printed| printed.starts_with("Ended"))
            .collect();
        let named = format!("{kind:?}");
        let of_kind = ended
            .iter()
            .filter(|printed| printed.contains(&named))
            .count();
        (ended.len(), of_kind)
    }

    /// Hands the owner the items in order, after emptying the queue so
    /// that only what they bring is read.
    fn deliver(&mut self, items: Vec<Item>) {
        self.owner.queue.clear();
        for item in items {
            match item {
                Item::Result(outcome) => self.owner.finish(outcome),
                Item::End(event) => self.owner.receive(event),
            }
        }
    }

    /// What the delivery left. A step that ends a generation is not read:
    /// an urgent event runs it at once and a result queues it.
    fn observe(&self, items: usize, notes: Vec<String>) -> Observed {
        let mut actions = Vec::new();
        let mut steps = Vec::new();
        for step in &self.owner.queue {
            match step {
                Step::Do(Action::End { .. }) => {}
                Step::Do(action) => {
                    actions.push(action.clone());
                    steps.push(describe(step));
                }
                Step::Answer { .. } => steps.push(describe(step)),
            }
        }
        Observed {
            items,
            actions,
            steps,
            public: format!("{:?}", self.owner.public),
            notes,
        }
    }
}

fn attempt_of(step: Step<IpcError>) -> u64 {
    match step {
        Step::Do(Action::Attempt { attempt, .. }) => attempt,
        other => panic!("expected an attempt, got {}", describe(&other)),
    }
}

/// What one scene left, in one order of delivery.
struct Observed {
    /// How many things were handed to the owner.
    items: usize,
    actions: Vec<Action>,
    steps: Vec<String>,
    public: String,
    /// What went against the scene before anything was handed over.
    notes: Vec<String>,
}

impl Observed {
    fn moves_to(&self, to: LinkState) -> bool {
        self.actions.contains(&Action::MoveLink {
            window_id: WINDOW.to_owned(),
            to,
        })
    }

    fn lost(&self) -> bool {
        self.moves_to(LinkState::Lost(LinkLostReason::ReconnectFailed))
    }

    fn tries(&self) -> bool {
        self.actions
            .iter()
            .any(|action| matches!(action, Action::Attempt { .. }))
    }

    fn tries_in_retry(&self) -> bool {
        self.actions.iter().any(|action| {
            matches!(
                action,
                Action::Attempt {
                    mode: ConnectMode::Retry,
                    ..
                }
            )
        })
    }
}

fn check(notes: &mut Vec<String>, held: bool, what: &str) {
    if !held {
        notes.push(what.to_owned());
    }
}

fn require(held: bool, what: &str) -> Result<(), String> {
    if held { Ok(()) } else { Err(what.to_owned()) }
}

fn all(checks: Vec<Result<(), String>>) -> Result<(), String> {
    let problems: Vec<String> = checks.into_iter().filter_map(Result::err).collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n  "))
    }
}

/// Runs `scene` in every order its news and results can reach the owner,
/// and tells whether every order met `expect` and left the same.
async fn across_orders<F, Fut>(
    scene: F,
    expect: fn(&Observed) -> Result<(), String>,
) -> Result<(), String>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Observed>,
{
    let first = scene(0).await;
    let mut problems = first.notes.clone();
    let mut add = |problem: String| {
        if !problems.contains(&problem) {
            problems.push(problem);
        }
    };
    if let Err(problem) = expect(&first) {
        add(problem);
    }
    for order in 1..factorial(first.items) {
        let other = scene(order).await;
        if other.steps != first.steps || other.public != first.public {
            add(format!(
                "order {order} of {} leaves {:?} and {}, order 0 leaves {:?} and {}",
                factorial(first.items),
                other.steps,
                other.public,
                first.steps,
                first.public
            ));
        }
        if let Err(problem) = expect(&other) {
            add(problem);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n  "))
    }
}

/// A window client whose generation 0 was open on window `w1` and ended
/// on its reader, and whose chain's generation 1 is restoring: the step
/// that attaches it again is returned, not yet run.
async fn chain_restoring(real_reader: bool, revision: u32) -> (Rig, Conn, Stage) {
    let mut rig = Rig::new(ClientKind::Window);
    rig.want(ConnectMode::Retry, Until::Open);
    let attempt = rig.next_attempt();
    let mut lost = rig.begin(attempt, false);
    rig.probe(&mut lost, REVISION).await;
    assert!(rig.carry_out().await.is_empty());
    lost.generation.set_window_id(Some(WINDOW.to_owned()));
    rig.lose(&mut lost, EndKind::ReadError);
    let mut held = rig.carry_out().await;
    let attempt = attempt_of(held.pop().expect("the chain's first attempt"));
    assert!(held.is_empty());

    let mut restoring = rig.begin(attempt, real_reader);
    rig.probe(&mut restoring, revision).await;
    let mut held = rig.carry_out().await;
    let step = held.pop().expect("the restoring attach");
    assert!(held.is_empty());
    assert!(
        matches!(step, Step::Do(Action::RestoreAttach { .. })),
        "{}",
        describe(&step)
    );
    let stage = rig.owner.execute(step).expect("the attach is a stage");
    (rig, restoring, stage)
}

/// A window client whose generation 0 is open on window `w1`, with its
/// own reader.
/// Its reader's end holds `late` bytes.
async fn open_window(late: Vec<u8>) -> (Rig, Conn) {
    let mut rig = Rig::new(ClientKind::Window);
    rig.want(ConnectMode::Retry, Until::Open);
    let attempt = rig.next_attempt();
    let mut open = rig.begin_with_late_bytes(attempt, true, late);
    rig.probe(&mut open, REVISION).await;
    assert!(rig.carry_out().await.is_empty());
    open.generation.set_window_id(Some(WINDOW.to_owned()));
    (rig, open)
}

fn superseded_frame() -> DaemonMessage {
    error_frame(ERROR_CODE_SUPERSEDED)
}

fn attached_reply() -> Result<Reply, IpcError> {
    Ok(Reply::Attached(AttachResponse {
        child_exited: false,
        exit_code: None,
    }))
}

/// (a) The attach was answered before the connection ended.
async fn answered_before_the_end() -> Observed {
    let (mut rig, mut restoring, stage) = chain_restoring(false, REVISION).await;
    let (outcome, ()) = tokio::join!(
        stage,
        answer_next(
            &restoring.generation,
            &mut restoring.peer,
            PendingKind::Attach,
            attached_reply()
        )
    );
    rig.owner.queue.clear();
    rig.lose(&mut restoring, EndKind::ReadError);
    rig.owner.finish(outcome);
    rig.observe(2, Vec::new())
}

/// (b) An answer to a request that is not the step does not count, and the
/// connection ended before the step was answered.
async fn ended_before_the_step_was_answered() -> (Observed, bool) {
    let (mut rig, mut restoring, mut stage) = chain_restoring(false, REVISION).await;
    let attach = ClientMessage::AttachView {
        window_id: WINDOW.to_owned(),
        size: None,
    };
    let user_attach = restoring.generation.request(&attach, PendingKind::Attach);
    let (answered, ()) = tokio::join!(
        user_attach,
        answer_next(
            &restoring.generation,
            &mut restoring.peer,
            PendingKind::Attach,
            attached_reply()
        )
    );
    assert!(answered.is_ok(), "{answered:?}");

    // The step's request is written and not answered.
    tokio::select! {
        _ = &mut stage => panic!("the step ended before its request was answered"),
        _ = restoring.peer.read_request() => {}
    }
    let awaiting = restoring
        .generation
        .awaiting()
        .expect("the step's request awaits");
    rig.owner.queue.clear();
    rig.lose(&mut restoring, EndKind::ReadError);
    let outcome = stage.await;
    rig.owner.finish(outcome);
    let late = restoring.generation.settle(
        Some(awaiting.correlation_id),
        PendingKind::Attach,
        attached_reply(),
    );
    (rig.observe(2, Vec::new()), late == Settled::Unanswered)
}

/// G: the control connection's approver registration was answered before
/// the connection ended.
async fn registered_before_the_end() -> Observed {
    let mut rig = Rig::new(ClientKind::Control);
    rig.want(ConnectMode::Once, Until::Open);
    let attempt = rig.next_attempt();
    let mut conn = rig.begin(attempt, false);
    rig.probe(&mut conn, REVISION).await;
    let mut held = rig.carry_out().await;
    let step = held.pop().expect("the registration");
    assert!(
        matches!(step, Step::Do(Action::Register { .. })),
        "{}",
        describe(&step)
    );
    assert!(held.is_empty());
    let stage = rig
        .owner
        .execute(step)
        .expect("the registration is a stage");
    let (outcome, ()) = tokio::join!(
        stage,
        answer_next(
            &conn.generation,
            &mut conn.peer,
            PendingKind::RegisterApprover,
            Ok(Reply::ApproverRegistered(Vec::new()))
        )
    );
    rig.owner.queue.clear();
    rig.lose(&mut conn, EndKind::ReadError);
    rig.owner.finish(outcome);
    rig.observe(2, Vec::new())
}

/// (c)/(d) The peer closed, so the step's write failed, and the daemon
/// had sent `superseded` before it closed. With `earlier_write`, an input
/// that is not the step failed first.
async fn closed_peer_then_superseded(order: usize, earlier_write: bool) -> Observed {
    let (mut rig, mut restoring, stage) = chain_restoring(true, REVISION).await;
    restoring.peer.stop_reading();
    if earlier_write {
        let write = restoring
            .generation
            .write(&ClientMessage::Resize { cols: 80, rows: 24 })
            .await;
        assert!(write.is_err(), "the peer is closed");
    }
    let outcome = stage.await;
    let mut notes = Vec::new();
    check(
        &mut notes,
        rig.ends(EndKind::WriteFailed).0 == 0,
        "a write that failed because the peer closed told the owner the generation ended, before its reader saw an end",
    );
    check(
        &mut notes,
        !restoring.reader_ended().await,
        "the reader ended before the daemon sent or closed anything",
    );
    restoring.peer.send(&superseded_frame()).await;
    restoring.peer.stop_writing();
    check(
        &mut notes,
        restoring.reader_ended().await,
        "the reader did not end on the superseded frame and the close",
    );
    let (ended, superseded) = rig.ends(EndKind::Superseded);
    check(
        &mut notes,
        ended == 1 && superseded == 1,
        &format!(
            "{ended} ends reached the inbox, {superseded} of them superseded; wanted exactly one, superseded"
        ),
    );
    let mut items: Vec<Item> = vec![Item::Result(outcome)];
    items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
    let count = items.len();
    rig.deliver(arrangement(items, order));
    rig.observe(count, notes)
}

/// (e)/(f) The step was refused (by the daemon, or by the revision's
/// gate) and the connection was taken over.
async fn refused_then_superseded(order: usize, gate: bool) -> Observed {
    let revision = if gate { BEFORE_FRAMES } else { REVISION };
    let (mut rig, mut restoring, stage) = chain_restoring(false, revision).await;
    let outcome = if gate {
        stage.await
    } else {
        let refusal = Err(IpcError::Rejected {
            code: "window_not_found".to_owned(),
            message: String::new(),
            rejected_argument: None,
        });
        let (outcome, ()) = tokio::join!(
            stage,
            answer_next(
                &restoring.generation,
                &mut restoring.peer,
                PendingKind::Attach,
                refusal
            )
        );
        outcome
    };
    restoring.report(EndKind::Superseded);
    rig.collect();
    let mut items: Vec<Item> = vec![Item::Result(outcome)];
    items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
    let count = items.len();
    rig.deliver(arrangement(items, order));
    rig.observe(count, Vec::new())
}

/// (f) alone: the revision gate's refusal is the step's answer, so the
/// attach fails on it with no news of the connection's end.
async fn refused_by_the_gate_and_never_ended() -> Observed {
    let (mut rig, _restoring, stage) = chain_restoring(false, BEFORE_FRAMES).await;
    let outcome = stage.await;
    rig.deliver(vec![Item::Result(outcome)]);
    rig.observe(1, Vec::new())
}

/// (g) The step's write failed on this side while the daemon is alive.
async fn failed_write_with_a_live_peer(order: usize) -> Observed {
    let (mut rig, mut restoring, stage) = chain_restoring(true, REVISION).await;
    restoring.peer.make_writes_fail();
    let outcome = stage.await;
    let mut notes = Vec::new();
    check(
        &mut notes,
        rig.ends(EndKind::WriteFailed).0 == 0,
        "a write that failed here told the owner the generation ended, before its reader saw an end",
    );
    check(
        &mut notes,
        restoring.reader_ended().await,
        "the reader did not end after a write that failed in a way that does not say the peer closed",
    );
    let (ended, write_failed) = rig.ends(EndKind::WriteFailed);
    check(
        &mut notes,
        ended == 1 && write_failed == 1,
        &format!(
            "{ended} ends reached the inbox, {write_failed} of them a failed write; wanted exactly one"
        ),
    );
    let mut items: Vec<Item> = vec![Item::Result(outcome)];
    items.extend(std::mem::take(&mut rig.news).into_iter().map(Item::End));
    let count = items.len();
    rig.deliver(arrangement(items, order));
    rig.observe(count, notes)
}

/// Where the daemon's `superseded` frame is when an input write fails.
#[derive(Clone, Copy, PartialEq)]
enum SupersededFrame {
    /// (h) The peer closed; the frame follows, then the end of the stream.
    AfterAClosedPeer,
    /// (i) The write failed here and the frame was delivered before it,
    /// beyond the first chunk the stopped reader takes.
    AlreadyDelivered,
    /// (j) The write failed here; the frame reaches this end only after the
    /// stopped reader has counted what was delivered, so it is not read.
    AfterTheStoppedReadBegan,
}

/// An open window whose input failed to write, with the daemon's `superseded`
/// frame placed as `frame` says.
async fn input_failed_then_superseded(order: usize, frame: SupersededFrame) -> Observed {
    let closed_peer = frame == SupersededFrame::AfterAClosedPeer;
    let late = if frame == SupersededFrame::AfterTheStoppedReadBegan {
        frame_encode(&encode_daemon_message(&superseded_frame()))
    } else {
        Vec::new()
    };
    let (mut rig, mut open) = open_window(late).await;
    let mut notes = Vec::new();
    if closed_peer {
        open.peer.stop_reading();
    } else {
        check(
            &mut notes,
            !open.reader_ended().await,
            "the reader ended before the daemon sent or closed anything",
        );
        // Pushes that end nothing, together longer than one read chunk,
        // so a frame beyond them lies beyond the first chunk the stopped
        // reader takes.
        let filler = "x".repeat(crate::constants::READ_CHUNK_LEN);
        for plan in ["p1", "p2"] {
            let plan_id = format!("{plan}{filler}");
            open.peer
                .send(&DaemonMessage::PlanChanged { plan_id })
                .await;
        }
        if frame == SupersededFrame::AlreadyDelivered {
            open.peer.send(&superseded_frame()).await;
        }
        open.peer.make_writes_fail();
    }
    let write = open
        .generation
        .write(&ClientMessage::Resize { cols: 80, rows: 24 })
        .await;
    assert!(write.is_err(), "the write was made to fail");
    check(
        &mut notes,
        rig.ends(EndKind::WriteFailed).0 == 0,
        "a failed input write told the owner the generation ended, before its reader saw an end",
    );
    if closed_peer {
        check(
            &mut notes,
            !open.reader_ended().await,
            "the reader ended before the daemon sent or closed anything",
        );
        open.peer.send(&superseded_frame()).await;
        open.peer.stop_writing();
    }
    let (end, what) = if frame == SupersededFrame::AfterTheStoppedReadBegan {
        (EndKind::WriteFailed, "a failed write")
    } else {
        (EndKind::Superseded, "superseded")
    };
    check(
        &mut notes,
        open.reader_ended().await,
        "the reader did not end after the input write failed",
    );
    let (ended, of_kind) = rig.ends(end);
    check(
        &mut notes,
        ended == 1 && of_kind == 1,
        &format!(
            "{ended} ends reached the inbox, {of_kind} of them {what}; wanted exactly one, {what}"
        ),
    );
    let items: Vec<Item> = std::mem::take(&mut rig.news)
        .into_iter()
        .map(Item::End)
        .collect();
    let count = items.len();
    rig.deliver(arrangement(items, order));
    rig.observe(count, notes)
}

fn superseded_without_a_chain(observed: &Observed) -> Result<(), String> {
    all(vec![
        require(
            observed.moves_to(LinkState::Superseded),
            "the window was not reported as taken over by another GUI",
        ),
        require(
            !observed.moves_to(LinkState::Connecting),
            "the window was reported as reconnecting after it was taken over",
        ),
        require(
            !observed.actions.iter().any(|action| {
                matches!(
                    action,
                    Action::MoveLink {
                        to: LinkState::Lost(_),
                        ..
                    }
                )
            }),
            "the window was reported as lost after it was taken over",
        ),
        require(
            !observed.tries(),
            "a reconnect started after the window was taken over",
        ),
    ])
}

fn reconnects_after_a_failed_write(observed: &Observed) -> Result<(), String> {
    all(vec![
        require(
            observed.moves_to(LinkState::Connecting) && observed.tries_in_retry(),
            "a window whose connection ended on a failed write did not start a new chain",
        ),
        require(
            !observed.moves_to(LinkState::Superseded),
            "a frame the stopped reader did not read was reported as a takeover",
        ),
        require(
            !observed.lost(),
            "a window whose connection ended on a failed write was reported as lost",
        ),
    ])
}

fn lost_to_a_failed_reconnect(observed: &Observed) -> Result<(), String> {
    all(vec![
        require(
            observed.lost(),
            "the window was not reported as lost to a failed reconnect",
        ),
        require(
            !observed.moves_to(LinkState::Superseded),
            "a refused attach was reported as a takeover",
        ),
        require(
            !observed.tries(),
            "a reconnect started after the chain's budget was spent",
        ),
    ])
}

#[tokio::test]
async fn the_owner_settles_a_generation_the_same_whichever_news_of_it_arrives_first() {
    let a = answered_before_the_end().await;
    let (b, late_answer_dropped) = ended_before_the_step_was_answered().await;
    let g = registered_before_the_end().await;
    let results: Vec<(&str, Result<(), String>)> = vec![
        (
            "(a) answered before the end",
            all(vec![
                require(
                    a.moves_to(LinkState::Connecting) && a.tries_in_retry(),
                    "a window whose restoring attach was answered before the connection ended did not start a new chain",
                ),
                require(
                    !a.actions.iter().any(|action| {
                        matches!(
                            action,
                            Action::MoveLink {
                                to: LinkState::Lost(_),
                                ..
                            }
                        )
                    }),
                    "a window whose restoring attach was answered was reported as lost",
                ),
            ]),
        ),
        (
            "(b) ended before the step was answered",
            all(vec![
                require(
                    b.lost() && !b.tries(),
                    "an attach that no answer reached did not spend the chain's budget",
                ),
                require(
                    late_answer_dropped,
                    "an answer after the generation ended was not dropped",
                ),
            ]),
        ),
        (
            "(G) registered before the end",
            require(
                g.tries(),
                "a control connection that registered before it ended did not start a new chain",
            ),
        ),
        (
            "(c) closed peer, then superseded",
            across_orders(
                |order| closed_peer_then_superseded(order, false),
                superseded_without_a_chain,
            )
            .await,
        ),
        (
            "(d) earlier failed write, then superseded",
            across_orders(
                |order| closed_peer_then_superseded(order, true),
                superseded_without_a_chain,
            )
            .await,
        ),
        (
            "(e) refused by the daemon, then superseded",
            across_orders(
                |order| refused_then_superseded(order, false),
                lost_to_a_failed_reconnect,
            )
            .await,
        ),
        (
            "(f) refused by the revision gate, then superseded",
            all(vec![
                across_orders(
                    |order| refused_then_superseded(order, true),
                    lost_to_a_failed_reconnect,
                )
                .await,
                lost_to_a_failed_reconnect(&refused_by_the_gate_and_never_ended().await),
            ]),
        ),
        (
            "(g) failed write with a live peer",
            across_orders(failed_write_with_a_live_peer, lost_to_a_failed_reconnect).await,
        ),
        (
            "(h) input failed, peer closed, then superseded",
            across_orders(
                |order| input_failed_then_superseded(order, SupersededFrame::AfterAClosedPeer),
                superseded_without_a_chain,
            )
            .await,
        ),
        (
            "(i) input failed here, superseded already sent",
            across_orders(
                |order| input_failed_then_superseded(order, SupersededFrame::AlreadyDelivered),
                superseded_without_a_chain,
            )
            .await,
        ),
        (
            "(j) input failed here, superseded reaching this end only after the stopped read began",
            across_orders(
                |order| {
                    input_failed_then_superseded(order, SupersededFrame::AfterTheStoppedReadBegan)
                },
                reconnects_after_a_failed_write,
            )
            .await,
        ),
    ];
    let failed: Vec<String> = results
        .iter()
        .filter_map(|(name, result)| {
            result
                .as_ref()
                .err()
                .map(|problem| format!("{name}\n  {problem}"))
        })
        .collect();
    assert!(
        failed.is_empty(),
        "scenes that failed:\n{}",
        failed.join("\n")
    );
}
