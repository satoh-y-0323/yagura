#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the holder, and this binary as a second client, as processes each test
// owns and ends itself.
#![allow(clippy::disallowed_methods)]

//! Integration test: a real `yagura-holder` process keeping a `holder_child` on
//! its terminal. The test plays the daemon: it holds the daemon endpoint the
//! launch names and talks to the holder over the port the holder's record names.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::sync::mpsc;
use tokio::time::Instant;
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, HolderLaunch, HolderRecord,
    encode_holder_frame, encode_holder_launch, read_holder_exit, read_holder_record,
};
use yagura_test_support::paths::{TempDir, unique_suffix};

/// Upper bound on every wait, so a broken holder fails the test instead of
/// hanging it. Nothing is synchronised on it.
const LIMIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
/// How long the flood's written total must stand still to count as stopped.
/// Unbounded flooding never stands still this long, so a holder that keeps
/// reading fails on [`LIMIT`] rather than passing.
const QUIET: Duration = Duration::from_millis(1000);
/// Long enough that no test meets the first-connection deadline by accident.
const NO_DEADLINE_MS: u64 = 600_000;
const HOLDER: &str = env!("CARGO_BIN_EXE_yagura-holder");
const CHILD: &str = env!("CARGO_BIN_EXE_holder_child");
const INTRUDER: &str = "intruder";
const INTRUDER_ENDPOINT_ENV: &str = "YAGURA_HOLDER_TEST_INTRUDER_ENDPOINT";
const INTRUDER_OUTCOME: &str = "INTRUDER-OUTCOME";

type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

fn short_id(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write(unique_suffix().as_bytes());
    let mut value = hasher.finish();
    let mut out = String::new();
    for _ in 0..len {
        out.push(char::from_digit(u32::try_from(value % 36).unwrap(), 36).unwrap());
        value /= 36;
    }
    out
}

/// Creates `path` owner-only, as a holders directory has to be.
fn private_dir(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    }
    // A directory under the user's temp directory inherits an owner-only ACL.
    #[cfg(windows)]
    std::fs::create_dir_all(path).unwrap();
    path.to_path_buf()
}

/// One window of one isolated instance: its own holders directory and a
/// daemon endpoint the instance's own name would not lead to.
struct Rig {
    window_id: String,
    instance: String,
    holders: PathBuf,
    dir: TempDir,
}

impl Rig {
    fn new(label: &str) -> Self {
        #[cfg(unix)]
        let dir = yagura_test_support::paths::short_tmp_dir("hold", label);
        #[cfg(windows)]
        let dir = TempDir::new("hold", label);
        let instance = format!("ht{}", short_id(10));
        let holders = private_dir(&dir.path().join("holders"));
        Self {
            window_id: format!("w{}", short_id(8)),
            instance,
            holders,
            dir,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn holders_dir(&self) -> PathBuf {
        self.holders.clone()
    }

    fn daemon_endpoint(&self) -> String {
        let user = PlatformIpc::default().current_user_id().unwrap();
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\yagura-test-{}-{user}-daemon", self.instance)
        }
        #[cfg(unix)]
        {
            let instance = std::ffi::OsStr::new(&self.instance);
            let namespace = yagura_os::resolve_instance_from(Some(instance)).unwrap();
            yagura_os::platform_unix::resolve_endpoint_with(
                &user,
                Some(self.dir.path().as_os_str()),
                &std::env::temp_dir(),
                yagura_os::platform_unix::EndpointCreation::CreateIfMissing,
                &namespace,
            )
            .unwrap()
        }
    }

    fn launch(&self, child_args: &[&str], hello_deadline_ms: u64) -> HolderLaunch {
        let mut argv = vec![CHILD.to_owned()];
        argv.extend(child_args.iter().map(|arg| (*arg).to_owned()));
        HolderLaunch {
            argv,
            cwd: Some(self.dir.path().to_path_buf()),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            record: format!("record-of-{}", self.window_id),
            hello_deadline_ms,
            holders_dir: self.holders_dir(),
            daemon_endpoint: self.daemon_endpoint(),
            end_child_on_disconnect: false,
        }
    }

    /// Starts the holder with only the window ID and the instance as
    /// arguments; everything else goes through its stdin.
    fn start(&self, launch: &HolderLaunch) -> Holder {
        self.start_with(&encode_holder_launch(launch), Stdio::null())
    }

    /// Starts the holder with `input` on its stdin and its environment left
    /// as this process's: it is the base of the child's environment.
    fn start_with(&self, input: &[u8], stdout: Stdio) -> Holder {
        let mut child = Command::new(HOLDER)
            .arg(&self.window_id)
            .arg(&self.instance)
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start yagura-holder");
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(input)
            .expect("hand the launch to the holder's stdin");
        drop(stdin);
        Holder { child }
    }

    async fn record_where(&self, satisfied: impl Fn(&HolderRecord) -> bool) -> HolderRecord {
        record_in(&self.holders_dir(), &self.window_id, satisfied).await
    }

    async fn record(&self) -> HolderRecord {
        self.record_where(|_| true).await
    }
}

async fn record_in(
    holders: &Path,
    window_id: &str,
    satisfied: impl Fn(&HolderRecord) -> bool,
) -> HolderRecord {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Ok(record) = read_holder_record(holders, window_id)
            && satisfied(&record)
        {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "no satisfying holder record under {}",
            holders.display()
        );
        tokio::time::sleep(POLL).await;
    }
}

struct Holder {
    child: Child,
}

impl Holder {
    async fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the holder is still running");
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The test process holding the instance's daemon endpoint, as the daemon does.
struct DaemonEndpoint {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DaemonEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn hold_daemon_endpoint(rig: &Rig) -> DaemonEndpoint {
    let ipc = PlatformIpc::default();
    let security = ipc.endpoint_security().unwrap();
    let mut listener = ipc
        .bind(&rig.daemon_endpoint(), security)
        .await
        .expect("hold the daemon endpoint");
    let task = tokio::spawn(async move {
        let ipc = PlatformIpc::default();
        // Kept open: the holder asks the OS who serves the other end.
        let mut held = Vec::new();
        loop {
            match ipc.accept(&mut listener).await {
                Ok(connection) => held.push(connection),
                Err(_) => tokio::time::sleep(POLL).await,
            }
        }
    });
    DaemonEndpoint { task }
}

enum Event {
    Frame(HolderFrame),
    Closed,
    Broken(String),
}

struct Peer {
    events: mpsc::UnboundedReceiver<Event>,
    writer: WriteHalf<Connection>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Peer {
    async fn connect(endpoint: &str) -> Self {
        let connection = PlatformIpc::default()
            .connect(endpoint)
            .await
            .expect("connect to the holder's port");
        let (mut read, writer) = tokio::io::split(connection);
        let (tx, events) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            let mut buf = HolderFrameBuffer::new();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                loop {
                    match buf.next_frame() {
                        Ok(Some(frame)) => {
                            if tx.send(Event::Frame(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = tx.send(Event::Broken(format!("{error:?}")));
                            return;
                        }
                    }
                }
                match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Event::Closed);
                        return;
                    }
                    Ok(n) => buf.push(&chunk[..n]),
                }
            }
        });
        Self {
            events,
            writer,
            reader,
        }
    }

    /// Connects and is welcomed; returns the record and `replay_end_seq`.
    async fn welcomed(endpoint: &str) -> (Self, String, u64) {
        let mut peer = Self::connect(endpoint).await;
        peer.send(hello(HOLDER_PROTOCOL)).await;
        match peer.frame().await {
            HolderFrame::Welcome {
                record,
                replay_end_seq,
                ..
            } => (peer, record, replay_end_seq),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    async fn send(&mut self, frame: HolderFrame) {
        self.writer
            .write_all(&encode_holder_frame(&frame))
            .await
            .expect("write a frame to the holder");
        self.writer.flush().await.expect("flush to the holder");
    }

    async fn next(&mut self) -> Event {
        tokio::time::timeout(LIMIT, self.events.recv())
            .await
            .expect("nothing from the holder within the limit")
            .expect("the reader ended without saying why")
    }

    async fn frame(&mut self) -> HolderFrame {
        match self.next().await {
            Event::Frame(frame) => frame,
            Event::Closed => panic!("the holder closed the connection"),
            Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
        }
    }

    /// Reads until the output so far contains `needle`; returns the other
    /// frames seen on the way.
    async fn output_until(&mut self, needle: &[u8]) -> Vec<HolderFrame> {
        let mut output = Vec::new();
        let mut others = Vec::new();
        while !contains(&output, needle) {
            match self.frame().await {
                HolderFrame::Output { bytes, .. } => output.extend_from_slice(&bytes),
                other => others.push(other),
            }
        }
        others
    }
}

fn hello(protocol: u32) -> HolderFrame {
    HolderFrame::Hello {
        protocol,
        daemon_boot: "test-boot".to_owned(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// The flood's last fully written total; `0` before it starts.
fn flood_total(count_file: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(count_file) else {
        return 0;
    };
    let complete = &text[..text.rfind('\n').map_or(0, |end| end + 1)];
    complete
        .lines()
        .next_back()
        .and_then(|line| line.parse().ok())
        .unwrap_or(0)
}

async fn wait_until(what: &str, satisfied: impl Fn() -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !satisfied() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        tokio::time::sleep(POLL).await;
    }
}

/// Output frames checked to follow one another with no bytes missing: an
/// `Output`'s `seq` is the stream position just past its bytes.
struct Stream {
    end: u64,
}

impl Stream {
    fn take(&mut self, seq: u64, len: usize) {
        let start = seq
            .checked_sub(len as u64)
            .expect("seq is before the frame's own start");
        assert_eq!(
            start, self.end,
            "bytes are missing or repeated before seq {seq}"
        );
        self.end = seq;
    }
}

/// Drains the holder's output without acknowledging it until the flood's
/// written total stands still; returns that total.
async fn drain_until_flood_stops(peer: &mut Peer, stream: &mut Stream, count_file: &Path) -> u64 {
    let deadline = Instant::now() + LIMIT;
    let mut total = flood_total(count_file);
    let mut since = Instant::now();
    loop {
        tokio::select! {
            event = peer.next() => match event {
                Event::Frame(HolderFrame::Output { seq, bytes }) => stream.take(seq, bytes.len()),
                Event::Frame(other) => panic!("unexpected frame while flooding: {other:?}"),
                Event::Closed => panic!("the holder closed the connection"),
                Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
            },
            () = tokio::time::sleep(POLL) => {}
        }
        let now = flood_total(count_file);
        if now != total {
            total = now;
            since = Instant::now();
        } else if total > 0 && since.elapsed() >= QUIET {
            return total;
        }
        assert!(
            Instant::now() < deadline,
            "the flood never stopped while unacknowledged ({total} bytes written)"
        );
    }
}

/// Keeps draining until the flood writes past `stalled`.
async fn drain_until_flood_passes(
    peer: &mut Peer,
    stream: &mut Stream,
    count_file: &Path,
    stalled: u64,
) {
    let deadline = Instant::now() + LIMIT;
    while flood_total(count_file) <= stalled {
        tokio::select! {
            event = peer.next() => match event {
                Event::Frame(HolderFrame::Output { seq, bytes }) => stream.take(seq, bytes.len()),
                Event::Frame(other) => panic!("unexpected frame while flooding: {other:?}"),
                Event::Closed => panic!("the holder closed the connection"),
                Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
            },
            () = tokio::time::sleep(POLL) => {}
        }
        assert!(
            Instant::now() < deadline,
            "the flood did not move on from {stalled} bytes"
        );
    }
}

fn running_as(name: &str) -> bool {
    let args: Vec<String> = std::env::args().collect();
    args.iter().any(|arg| arg == "--exact") && args.iter().any(|arg| arg == name)
}

/// Runs [`intruder`] in a process of its own, which does not hold the daemon
/// endpoint, and returns what it printed.
async fn run_intruder(endpoint: &str) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            INTRUDER,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INTRUDER_ENDPOINT_ENV, endpoint)
        .stdin(Stdio::null());
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .expect("run the intruder");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the second client of hello_is_accepted_only_from_the_process_holding_the_daemon_endpoint"]
async fn intruder() {
    let Ok(endpoint) = std::env::var(INTRUDER_ENDPOINT_ENV) else {
        return;
    };
    if !running_as(INTRUDER) {
        return;
    }
    let mut peer = Peer::connect(&endpoint).await;
    let _ = peer
        .writer
        .write_all(&encode_holder_frame(&hello(HOLDER_PROTOCOL)))
        .await;
    let outcome = match peer.next().await {
        Event::Frame(frame) => format!("{frame:?}"),
        Event::Closed => "closed".to_owned(),
        Event::Broken(error) => format!("broken {error}"),
    };
    println!("{INTRUDER_OUTCOME} {outcome}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_child_starts_only_after_the_first_valid_hello() {
    let rig = Rig::new("start");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let count = rig.file("count");
    let _holder = rig.start(&rig.launch(&["flood", path_arg(&count), "8192"], NO_DEADLINE_MS));

    let before = rig.record().await;
    assert_eq!(before.child_pid, None, "the child started before any Hello");
    assert!(!count.exists(), "holder_child ran before any Hello");

    let (_peer, _, _) = Peer::welcomed(&before.endpoint).await;
    rig.record_where(|record| record.child_pid.is_some()).await;
    wait_until("holder_child runs after the Hello", || count.exists()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_is_accepted_only_from_the_process_holding_the_daemon_endpoint() {
    let rig = Rig::new("peer");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let launch = rig.launch(&["hex"], NO_DEADLINE_MS);
    let _holder = rig.start(&launch);
    let record = rig.record().await;

    let printed = run_intruder(&record.endpoint).await;
    let outcome = printed
        .lines()
        .find_map(|line| {
            line.split_once(INTRUDER_OUTCOME)
                .map(|(_, outcome)| outcome)
        })
        .unwrap_or_else(|| panic!("the intruder reported nothing: {printed}"));
    assert!(
        !outcome.contains("Welcome"),
        "a process not holding the daemon endpoint was welcomed: {outcome}"
    );
    assert_eq!(rig.record().await.child_pid, None);

    let (_peer, returned, _) = Peer::welcomed(&record.endpoint).await;
    assert_eq!(returned, launch.record);
}

#[tokio::test(flavor = "multi_thread")]
async fn connected_output_stops_at_the_unacknowledged_window_and_resumes_on_ack() {
    let rig = Rig::new("ack");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let count = rig.file("count");
    let _holder = rig.start(&rig.launch(&["flood", path_arg(&count)], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;

    let mut stream = Stream { end: 0 };
    let stalled = drain_until_flood_stops(&mut peer, &mut stream, &count).await;
    peer.send(HolderFrame::Ack { seq: stream.end }).await;
    drain_until_flood_passes(&mut peer, &mut stream, &count, stalled).await;
}

/// The holder keeps the checkpoint and every byte after it while no daemon is
/// connected, stops reading at its bound instead of dropping, and hands it all
/// to the next daemon before the live output.
#[tokio::test(flavor = "multi_thread")]
async fn replay_hands_over_the_checkpoint_and_every_byte_after_it_then_live_output() {
    let rig = Rig::new("replay");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let count = rig.file("count");
    let _holder = rig.start(&rig.launch(&["flood", path_arg(&count)], NO_DEADLINE_MS));
    let endpoint = rig.record().await.endpoint;

    let (mut first, _, _) = Peer::welcomed(&endpoint).await;
    first.send(HolderFrame::Replay).await;
    let checkpoint_seq = loop {
        if let HolderFrame::Output { seq, .. } = first.frame().await {
            break seq;
        }
    };
    first
        .send(HolderFrame::StoreCheckpoint {
            seq: checkpoint_seq,
            bytes: b"CHECKPOINT-1".to_vec(),
        })
        .await;
    first
        .send(HolderFrame::Ack {
            seq: checkpoint_seq,
        })
        .await;
    drop(first);

    // No daemon: the flood must come to a stop at the holder's bound.
    let deadline = Instant::now() + LIMIT;
    let mut stalled = flood_total(&count);
    let mut since = Instant::now();
    while since.elapsed() < QUIET {
        tokio::time::sleep(POLL).await;
        let now = flood_total(&count);
        if now != stalled {
            stalled = now;
            since = Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "the flood never stopped with no daemon connected ({stalled} bytes written)"
        );
    }

    let (mut second, _, replay_end_seq) = Peer::welcomed(&endpoint).await;
    second.send(HolderFrame::Replay).await;
    assert_eq!(
        second.frame().await,
        HolderFrame::Checkpoint {
            seq: checkpoint_seq,
            bytes: b"CHECKPOINT-1".to_vec(),
        }
    );
    let mut stream = Stream {
        end: checkpoint_seq,
    };
    while stream.end < replay_end_seq {
        match second.frame().await {
            HolderFrame::Output { seq, bytes } => stream.take(seq, bytes.len()),
            other => panic!("unexpected frame during replay: {other:?}"),
        }
    }
    assert_eq!(
        stream.end, replay_end_seq,
        "replay did not end on replay_end_seq"
    );

    second
        .send(HolderFrame::StoreCheckpoint {
            seq: replay_end_seq,
            bytes: b"CHECKPOINT-2".to_vec(),
        })
        .await;
    second
        .send(HolderFrame::Ack {
            seq: replay_end_seq,
        })
        .await;
    drain_until_flood_passes(&mut second, &mut stream, &count, stalled).await;
    // The child's writes moving on and its output reaching us race, so read until it arrives.
    tokio::time::timeout(LIMIT, async {
        while stream.end <= replay_end_seq {
            match second.frame().await {
                HolderFrame::Output { seq, bytes } => stream.take(seq, bytes.len()),
                other => panic!("unexpected frame after the replay: {other:?}"),
            }
        }
    })
    .await
    .expect("no live output followed the replay");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_valid_hello_displaces_the_first_connection() {
    let rig = Rig::new("displace");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let _holder = rig.start(&rig.launch(&["hex"], NO_DEADLINE_MS));
    let endpoint = rig.record().await.endpoint;

    let (mut first, _, _) = Peer::welcomed(&endpoint).await;
    let (_second, _, _) = Peer::welcomed(&endpoint).await;
    loop {
        match first.next().await {
            Event::Frame(HolderFrame::Displaced) => break,
            Event::Frame(HolderFrame::Output { .. }) => {}
            Event::Frame(other) => panic!("expected Displaced, got {other:?}"),
            Event::Closed => panic!("closed without Displaced"),
            Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
        }
    }
    assert!(
        matches!(first.next().await, Event::Closed),
        "the displaced connection was not closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_reach_the_child_in_order_and_are_acknowledged() {
    let rig = Rig::new("write");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let _holder = rig.start(&rig.launch(&["hex"], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"HOLDER-CHILD-READY").await;

    peer.send(HolderFrame::Write {
        id: 1,
        bytes: b"ab".to_vec(),
    })
    .await;
    peer.send(HolderFrame::Write {
        id: 2,
        bytes: b"cd".to_vec(),
    })
    .await;
    let others = peer.output_until(b"61626364").await;
    let mut written: Vec<HolderFrame> = others;
    while written.len() < 2 {
        match peer.frame().await {
            HolderFrame::Output { .. } => {}
            other => written.push(other),
        }
    }
    assert_eq!(
        written,
        [
            HolderFrame::Written { id: 1 },
            HolderFrame::Written { id: 2 }
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_after_the_child_exited_fails() {
    let rig = Rig::new("wfail");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let gate = rig.file("gate");
    let _holder = rig.start(&rig.launch(&["exit", "0", path_arg(&gate)], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;

    std::fs::write(&gate, b"").unwrap();
    loop {
        match peer.frame().await {
            HolderFrame::Exited { .. } => break,
            HolderFrame::Output { .. } => {}
            other => panic!("unexpected frame before Exited: {other:?}"),
        }
    }
    // The exit file is written before `Exited` is sent, so it is there now.
    let exit = read_holder_exit(&rig.holders_dir(), &rig.window_id)
        .expect("the exit file is readable as soon as Exited arrives");
    assert_eq!(exit.code, Some(0));
    assert!(
        contains(&exit.output, b"EXITING 0"),
        "the child's last output is missing: {:?}",
        String::from_utf8_lossy(&exit.output)
    );
    peer.send(HolderFrame::Write {
        id: 9,
        bytes: b"x".to_vec(),
    })
    .await;
    let reply = peer.frame().await;
    assert!(
        matches!(reply, HolderFrame::WriteFailed { id: 9, .. }),
        "expected WriteFailed for id 9, got {reply:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_reaches_the_child() {
    let rig = Rig::new("resize");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let _holder = rig.start(&rig.launch(&["size"], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"SIZE 80x24").await;

    peer.send(HolderFrame::Resize {
        cols: 100,
        rows: 30,
    })
    .await;
    peer.output_until(b"SIZE 100x30").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_hello_by_the_deadline_the_holder_ends_and_never_starts_the_child() {
    let rig = Rig::new("deadline");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let count = rig.file("count");
    let mut holder = rig.start(&rig.launch(&["flood", path_arg(&count), "8192"], 2_000));
    // The holder was up and waiting for a daemon, not ending at once.
    rig.record().await;

    holder.wait_exit().await;
    assert!(!count.exists(), "holder_child ran although no Hello came");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_exiting_while_disconnected_leaves_its_result_and_output() {
    let rig = Rig::new("exit");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let gate = rig.file("gate");
    let mut holder = rig.start(&rig.launch(&["exit", "7", path_arg(&gate)], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    drop(peer);

    std::fs::write(&gate, b"").unwrap();
    holder.wait_exit().await;
    let exit = read_holder_exit(&rig.holders_dir(), &rig.window_id).expect("read the exit");
    assert_eq!(exit.code, Some(7));
    assert!(
        contains(&exit.output, b"EXITING 7"),
        "the child's last output is missing: {:?}",
        String::from_utf8_lossy(&exit.output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn terminate_ends_the_child_tree_and_the_holder() {
    let rig = Rig::new("tree");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let pid_file = rig.file("grandchild");
    let mut holder = rig.start(&rig.launch(&["tree", path_arg(&pid_file)], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"HOLDER-CHILD-TREE").await;
    let grandchild: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let watch = GrandchildWatch::open(grandchild);

    peer.send(HolderFrame::Terminate { tree: true }).await;
    holder.wait_exit().await;
    watch.wait_gone().await;
}

/// Without `tree`, only the kill of the child itself, escalated where the
/// first one can be refused, can end a child that ignores requests to end.
#[tokio::test(flavor = "multi_thread")]
async fn terminate_ends_a_child_ignoring_requests_to_end_and_keeps_its_exit() {
    let rig = Rig::new("ignore");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let mut holder = rig.start(&rig.launch(&["ignore-end"], NO_DEADLINE_MS));
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"HOLDER-CHILD-IGNORING").await;

    peer.send(HolderFrame::Terminate { tree: false }).await;
    let code = loop {
        match peer.frame().await {
            HolderFrame::Exited { code } => break code,
            HolderFrame::Output { .. } => {}
            other => panic!("unexpected frame before Exited: {other:?}"),
        }
    };
    holder.wait_exit().await;
    // 1 is TerminateProcess's code on Windows and a kill by signal on Unix.
    assert_eq!(code, Some(1), "the child was not ended by the kill");
    let exit = read_holder_exit(&rig.holders_dir(), &rig.window_id).expect("read the exit");
    assert_eq!(exit.code, Some(1));
    assert!(
        contains(&exit.output, b"HOLDER-CHILD-IGNORING"),
        "the child's output is missing: {:?}",
        String::from_utf8_lossy(&exit.output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hello_of_another_protocol_version_is_rejected() {
    let rig = Rig::new("version");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let _holder = rig.start(&rig.launch(&["hex"], NO_DEADLINE_MS));
    let mut peer = Peer::connect(&rig.record().await.endpoint).await;

    peer.send(hello(HOLDER_PROTOCOL + 1)).await;
    assert_eq!(
        peer.frame().await,
        HolderFrame::VersionRejected {
            protocol: HOLDER_PROTOCOL,
        }
    );
    assert!(
        matches!(peer.next().await, Event::Closed),
        "the rejected connection was not closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_holder_keeps_its_files_in_the_launch_directory_and_serves_the_launch_endpoint() {
    let rig = Rig::new("place");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let launch = rig.launch(&["hex"], NO_DEADLINE_MS);
    let _holder = rig.start(&launch);

    let record = rig.record().await;
    // The rig's directory is short enough for the socket to fit in it.
    #[cfg(unix)]
    assert!(
        Path::new(&record.endpoint).starts_with(rig.holders_dir()),
        "the socket {} is not in the launch's directory",
        record.endpoint
    );
    let (_peer, returned, _) = Peer::welcomed(&record.endpoint).await;
    assert_eq!(returned, launch.record);
}

/// The same window of the same instance in two holders directories: on Unix
/// both directories are too long for a socket, so both use the fallback.
#[tokio::test(flavor = "multi_thread")]
async fn holders_in_different_directories_have_different_endpoints() {
    let rig = Rig::new("twodirs");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let long = "l".repeat(100);
    let dirs = [
        private_dir(&rig.dir.path().join(&long).join("a")),
        private_dir(&rig.dir.path().join(&long).join("b")),
    ];
    let mut holders = Vec::new();
    let mut peers = Vec::new();
    for dir in &dirs {
        let mut launch = rig.launch(&["hex"], NO_DEADLINE_MS);
        launch.holders_dir = dir.clone();
        holders.push(rig.start(&launch));
        let record = record_in(dir, &rig.window_id, |_| true).await;
        let (peer, returned, _) = Peer::welcomed(&record.endpoint).await;
        assert_eq!(returned, launch.record);
        peers.push(peer);
    }

    // Ended through the holders, so they take their sockets with them.
    for (peer, holder) in peers.iter_mut().zip(&mut holders) {
        peer.send(HolderFrame::Terminate { tree: true }).await;
        holder.wait_exit().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_holder_tells_on_stdout_when_it_is_ready_and_a_failed_one_does_not() {
    use std::io::{BufRead as _, Read as _};

    let rig = Rig::new("ready");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let launch = rig.launch(&["hex"], NO_DEADLINE_MS);
    let mut holder = rig.start_with(&encode_holder_launch(&launch), Stdio::piped());
    let stdout = std::io::BufReader::new(holder.child.stdout.take().unwrap());
    let (line, mut stdout) = tokio::time::timeout(
        LIMIT,
        tokio::task::spawn_blocking(move || {
            let mut stdout = stdout;
            let mut line = Vec::new();
            stdout.read_until(b'\n', &mut line).unwrap();
            (line, stdout)
        }),
    )
    .await
    .expect("no notice within the limit")
    .unwrap();
    assert!(
        line.len() > 1 && line.ends_with(b"\n"),
        "the notice is not one line: {line:?}"
    );

    // Once the notice is out, the record is there and the endpoint answers.
    let record = read_holder_record(&rig.holders_dir(), &rig.window_id)
        .expect("the record is written before the notice");
    let (_peer, _, _) = Peer::welcomed(&record.endpoint).await;
    let rest = tokio::time::timeout(
        LIMIT,
        tokio::task::spawn_blocking(move || {
            let mut rest = Vec::new();
            stdout.read_to_end(&mut rest).unwrap();
            rest
        }),
    )
    .await
    .expect("stdout was not closed after the notice")
    .unwrap();
    assert!(rest.is_empty(), "more than one line: {rest:?}");
    assert!(
        holder.child.try_wait().unwrap().is_none(),
        "stdout closed only because the holder ended"
    );

    let unready = Rig::new("unready");
    let mut failed = unready.start_with(b"not a launch", Stdio::piped());
    let mut stdout = failed.child.stdout.take().unwrap();
    let printed = tokio::time::timeout(
        LIMIT,
        tokio::task::spawn_blocking(move || {
            let mut printed = Vec::new();
            stdout.read_to_end(&mut printed).unwrap();
            printed
        }),
    )
    .await
    .expect("a failed holder's stdout stayed open")
    .unwrap();
    assert!(printed.is_empty(), "a failed holder printed {printed:?}");
    assert!(
        !failed.wait_exit().await.success(),
        "a failed holder ended with success"
    );
}

/// What the holder keeps after the checkpoint before it stops reading:
/// `MAX_PENDING` (`buffer.rs`), eight bytes per cell of the largest screen.
fn kept_output_bound() -> u64 {
    u64::from(yagura_protocol::MAX_WINDOW_DIMENSION).pow(2) * 8
}

/// Provisional: goes with the end-child-on-disconnect flag once holders
/// outlive the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn provisional_flag_a_lost_connection_ends_the_tree_and_the_holder_and_leaves_no_file() {
    let rig = Rig::new("flagend");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let pid_file = rig.file("grandchild");
    let mut launch = rig.launch(&["tree", path_arg(&pid_file)], NO_DEADLINE_MS);
    launch.end_child_on_disconnect = true;
    let mut holder = rig.start(&launch);
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"HOLDER-CHILD-TREE").await;
    let grandchild: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let watch = GrandchildWatch::open(grandchild);

    drop(peer);
    holder.wait_exit().await;
    watch.wait_gone().await;
    // The exit file and the record hold the child's raw output and nobody
    // would read them.
    let left: Vec<_> = std::fs::read_dir(rig.holders_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "the holder left {left:?}");
}

/// Provisional: goes with the end-child-on-disconnect flag, after which only a
/// stored checkpoint frees what the holder keeps.
#[tokio::test(flavor = "multi_thread")]
async fn provisional_flag_acks_free_the_kept_output_so_it_never_stops() {
    let rig = Rig::new("flagack");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let count = rig.file("count");
    let mut launch = rig.launch(&["flood", path_arg(&count)], NO_DEADLINE_MS);
    launch.end_child_on_disconnect = true;
    let _holder = rig.start(&launch);
    let (mut peer, _, _) = Peer::welcomed(&rig.record().await.endpoint).await;
    peer.send(HolderFrame::Replay).await;

    let past = kept_output_bound() + kept_output_bound() / 4;
    let mut stream = Stream { end: 0 };
    while stream.end <= past {
        match peer.frame().await {
            HolderFrame::Output { seq, bytes } => {
                stream.take(seq, bytes.len());
                peer.send(HolderFrame::Ack { seq }).await;
            }
            other => panic!("unexpected frame while flooding: {other:?}"),
        }
    }
}

/// The next frame other than output.
async fn next_answer(peer: &mut Peer) -> HolderFrame {
    loop {
        match peer.frame().await {
            HolderFrame::Output { .. } => {}
            other => return other,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_is_answered_and_a_refused_one_leaves_the_size() {
    let rig = Rig::new("resizeans");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let _holder = rig.start(&rig.launch(&["size"], NO_DEADLINE_MS));
    let endpoint = rig.record().await.endpoint;
    let (mut peer, _, _) = Peer::welcomed(&endpoint).await;
    peer.send(HolderFrame::Replay).await;
    peer.output_until(b"SIZE 80x24").await;

    peer.send(HolderFrame::Resize {
        cols: 100,
        rows: 30,
    })
    .await;
    let others = peer.output_until(b"SIZE 100x30").await;
    let answer = match others.as_slice() {
        [] => next_answer(&mut peer).await,
        [answer] => answer.clone(),
        more => panic!("more than one answer: {more:?}"),
    };
    assert!(
        matches!(answer, HolderFrame::Resized { .. }),
        "expected Resized, got {answer:?}"
    );

    let too_wide = yagura_protocol::MAX_WINDOW_DIMENSION + 1;
    peer.send(HolderFrame::Resize {
        cols: too_wide,
        rows: 30,
    })
    .await;
    let answer = next_answer(&mut peer).await;
    assert!(
        matches!(answer, HolderFrame::ResizeFailed { .. }),
        "expected ResizeFailed, got {answer:?}"
    );

    let mut next = Peer::connect(&endpoint).await;
    next.send(hello(HOLDER_PROTOCOL)).await;
    match next.frame().await {
        HolderFrame::Welcome { cols, rows, .. } => assert_eq!((cols, rows), (100, 30)),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_cannot_start_is_reported_with_its_reason_before_any_welcome() {
    let rig = Rig::new("nospawn");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let mut launch = rig.launch(&[], NO_DEADLINE_MS);
    launch.argv = vec![path_arg(&rig.file("no-such-program")).to_owned()];
    let _holder = rig.start(&launch);
    let mut peer = Peer::connect(&rig.record().await.endpoint).await;
    peer.send(hello(HOLDER_PROTOCOL)).await;

    let mut frames = Vec::new();
    loop {
        match peer.next().await {
            Event::Frame(frame @ HolderFrame::Exited { .. }) => {
                frames.push(frame);
                break;
            }
            Event::Frame(frame) => frames.push(frame),
            Event::Closed => panic!("closed without Exited after {frames:?}"),
            Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
        }
    }
    // The daemon chooses its error by the reason when `Exited` arrives.
    assert!(
        matches!(
            frames.as_slice(),
            [
                HolderFrame::SpawnFailed {
                    reason: yagura_protocol::holder::HolderSpawnFailure::Spawn,
                },
                HolderFrame::Exited { .. },
            ]
        ),
        "expected the reason, then Exited, and no Welcome: {frames:?}"
    );
}

/// A handle on the grandchild taken while it is known to be alive, so its
/// exit is observed on the process itself rather than on a reused pid.
#[cfg(windows)]
struct GrandchildWatch {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl GrandchildWatch {
    fn open(pid: u32) -> Self {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: OpenProcess takes plain values; the handle is closed in `wait_gone`.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
            .expect("the grandchild is alive before Terminate");
        Self { handle }
    }

    async fn wait_gone(self) {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        let handle = self.handle;
        let millis = u32::try_from(LIMIT.as_millis()).unwrap();
        // SAFETY: `handle` is a process handle this watch owns until here.
        let waited = tokio::task::spawn_blocking(move || unsafe {
            let waited = WaitForSingleObject(handle, millis);
            let _ = CloseHandle(handle);
            waited
        })
        .await
        .unwrap();
        assert_eq!(waited, WAIT_OBJECT_0, "the grandchild outlived Terminate");
    }
}

#[cfg(unix)]
struct GrandchildWatch {
    pid: libc::pid_t,
}

#[cfg(unix)]
impl GrandchildWatch {
    fn open(pid: u32) -> Self {
        let pid = libc::pid_t::try_from(pid).unwrap();
        assert!(alive(pid), "the grandchild is alive before Terminate");
        Self { pid }
    }

    async fn wait_gone(self) {
        let pid = self.pid;
        wait_until("the grandchild ends with the tree", || !alive(pid)).await;
    }
}

#[cfg(unix)]
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}
