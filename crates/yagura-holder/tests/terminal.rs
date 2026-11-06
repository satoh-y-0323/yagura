#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Starts the holder as a process each test owns and ends itself.
#![allow(clippy::disallowed_methods)]

//! Integration test: the terminal kept in the holder. A real `yagura-holder`
//! runs a `holder_child script`; the test plays the daemon and reads what the
//! holder pushes (screen copies, view frames, answers) and what reached the
//! child's input (the script's input log).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::sync::mpsc;
use tokio::time::Instant;
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderFrame, HolderFrameBuffer, HolderLaunch, HolderRecord,
    encode_holder_frame, encode_holder_launch, read_holder_record,
};
use yagura_protocol::{
    CellPosition, InputModes, Modifiers, Palette, PointerButton, PointerInput, PointerKind,
    ScreenCopy, ScreenCursor, ScreenLine, ScreenPush,
};
use yagura_test_support::frame::Grid;
use yagura_test_support::paths::{TempDir, unique_suffix};

const POLL: Duration = Duration::from_millis(20);
const NO_DEADLINE_MS: u64 = 600_000;
const HOLDER: &str = env!("CARGO_BIN_EXE_yagura-holder");
const CHILD: &str = env!("CARGO_BIN_EXE_holder_child");
const SCRIPT_MARKER: &str = "HOLDER-CHILD-SCRIPT";
const HISTORY_ROWS: usize = 1000;

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

struct Rig {
    window_id: String,
    instance: String,
    holders: PathBuf,
    dir: TempDir,
}

impl Rig {
    fn new(label: &str) -> Self {
        #[cfg(unix)]
        let dir = yagura_test_support::paths::short_tmp_dir("term", label);
        #[cfg(windows)]
        let dir = TempDir::new("term", label);
        let holders = private_dir(&dir.path().join("holders"));
        Self {
            window_id: format!("w{}", short_id(8)),
            instance: format!("tt{}", short_id(10)),
            holders,
            dir,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
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

    fn launch(&self, child_args: &[&str]) -> HolderLaunch {
        let mut argv = vec![CHILD.to_owned()];
        argv.extend(child_args.iter().map(|arg| (*arg).to_owned()));
        HolderLaunch {
            argv,
            cwd: Some(self.dir.path().to_path_buf()),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            record: format!("record-of-{}", self.window_id),
            hello_deadline_ms: NO_DEADLINE_MS,
            holders_dir: self.holders.clone(),
            daemon_endpoint: self.daemon_endpoint(),
            end_child_on_disconnect: false,
            history_rows: HISTORY_ROWS,
            palette: None,
            fault: None,
        }
    }

    fn start(&self, launch: &HolderLaunch) -> Holder {
        let mut child = Command::new(HOLDER)
            .arg(&self.window_id)
            .arg(&self.instance)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start yagura-holder");
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(&encode_holder_launch(launch))
            .expect("hand the launch to the holder's stdin");
        drop(stdin);
        Holder { child }
    }

    async fn record(&self) -> HolderRecord {
        let deadline = Instant::now() + yagura_test_support::HANG_GUARD;
        loop {
            if let Ok(record) = read_holder_record(&self.holders, &self.window_id) {
                return record;
            }
            assert!(Instant::now() < deadline, "no holder record");
            tokio::time::sleep(POLL).await;
        }
    }
}

struct Holder {
    child: Child,
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

    async fn send(&mut self, frame: HolderFrame) {
        self.writer
            .write_all(&encode_holder_frame(&frame))
            .await
            .expect("write a frame to the holder");
        self.writer.flush().await.expect("flush to the holder");
    }

    async fn frame(&mut self) -> HolderFrame {
        let event = tokio::time::timeout(yagura_test_support::HANG_GUARD, self.events.recv())
            .await
            .expect("nothing from the holder within the limit")
            .expect("the reader ended without saying why");
        match event {
            Event::Frame(frame) => frame,
            Event::Closed => panic!("the holder closed the connection"),
            Event::Broken(error) => panic!("the holder sent a broken frame: {error}"),
        }
    }
}

fn hello() -> HolderFrame {
    HolderFrame::Hello {
        protocol: HOLDER_PROTOCOL,
        daemon_boot: "test-boot".to_owned(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// The script child's command that makes it write `bytes` to its terminal.
fn script_command(bytes: &[u8]) -> Vec<u8> {
    let mut command = b"<".to_vec();
    for byte in bytes {
        command.extend(format!("{byte:02x}").as_bytes());
    }
    command.push(b'>');
    command
}

/// Waits until the child's input log holds `needle`; returns the log.
async fn log_with(log: &Path, needle: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if let Ok(bytes) = std::fs::read(log)
            && contains(&bytes, needle)
        {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "the child never read {:?}",
            String::from_utf8_lossy(needle)
        );
        tokio::time::sleep(POLL).await;
    }
}

fn row_text(line: &ScreenLine) -> String {
    let mut text = String::new();
    let mut col = 0u16;
    for cell in &line.cells {
        while col < cell.col {
            text.push(' ');
            col += 1;
        }
        text.push(cell.c);
        text.extend(&cell.zerowidth);
        col += if cell.wide { 2 } else { 1 };
    }
    text
}

fn blank_lines(cols: u16, rows: u16) -> Vec<ScreenLine> {
    ScreenCopy {
        cols,
        rows,
        lines: Vec::new(),
        cursor: ScreenCursor::default(),
        cursor_hidden: false,
        sync_open: false,
        input_modes: InputModes::default(),
        generation: 0,
        outputs_fed: 0,
    }
    .lines_resized(cols, rows)
}

/// One thing the link saw: a copy it applied, or any other frame.
enum Seen {
    Copy(ScreenPush),
    Other(HolderFrame),
}

fn copies(seen: &[Seen]) -> Vec<&ScreenPush> {
    seen.iter()
        .filter_map(|seen| match seen {
            Seen::Copy(push) => Some(push),
            Seen::Other(_) => None,
        })
        .collect()
}

/// The size of a view's first frame.
struct ViewSize {
    cols: u16,
    rows: u16,
}

/// The test's side of the holder connection, playing the daemon: puts the
/// rows of each copy aside and lays them over the copy before once its
/// `ScreenPush` arrives, and keeps an attached view's grid.
struct Link {
    peer: Peer,
    rows: Vec<ScreenLine>,
    rows_number: Option<u64>,
    screen: Option<ScreenCopy>,
    /// Answers every applied copy with a `PushAck` at once.
    auto_ack: bool,
    /// The attach generation whose frames are applied to `grid` and
    /// acknowledged.
    view: Option<u64>,
    grid: Grid,
    next_id: u64,
}

impl Link {
    /// Connects, is welcomed, and takes the whole copy that must follow the
    /// `Welcome` before any other frame.
    async fn open(endpoint: &str, auto_ack: bool) -> (Self, ScreenPush) {
        let mut peer = Peer::connect(endpoint).await;
        peer.send(hello()).await;
        match peer.frame().await {
            HolderFrame::Welcome { .. } => {}
            other => panic!("expected Welcome, got {other:?}"),
        }
        let mut link = Self {
            peer,
            rows: Vec::new(),
            rows_number: None,
            screen: None,
            auto_ack,
            view: None,
            grid: Grid::new(),
            next_id: 0,
        };
        let first = match link.seen().await {
            Seen::Copy(push) => push,
            Seen::Other(other) => {
                panic!("expected the whole copy right after the Welcome, got {other:?}")
            }
        };
        assert!(first.full, "the first copy after the Welcome is not whole");
        (link, first)
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn screen(&self) -> &ScreenCopy {
        self.screen.as_ref().expect("no copy yet")
    }

    fn text(&self) -> String {
        self.screen()
            .lines
            .iter()
            .map(row_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn seen(&mut self) -> Seen {
        loop {
            match self.peer.frame().await {
                HolderFrame::ScreenRows(rows) => {
                    if let Some(number) = self.rows_number {
                        assert_eq!(number, rows.number, "rows of two copies interleave");
                    }
                    self.rows_number = Some(rows.number);
                    self.rows.extend(rows.lines);
                }
                HolderFrame::ScreenPush(push) => {
                    if let Some(number) = self.rows_number.take() {
                        assert_eq!(number, push.number, "rows came under another copy's number");
                    }
                    self.apply(&push);
                    if self.auto_ack {
                        self.peer
                            .send(HolderFrame::PushAck {
                                number: push.number,
                            })
                            .await;
                    }
                    return Seen::Copy(push);
                }
                HolderFrame::ViewFrame { generation, frame } if Some(generation) == self.view => {
                    let last = self.grid.apply(&frame).expect("a well-formed view frame");
                    if last {
                        self.peer
                            .send(HolderFrame::ViewNote {
                                generation,
                                note: yagura_protocol::holder::HolderViewNote::Ack {
                                    seq: frame.seq,
                                },
                            })
                            .await;
                    }
                    return Seen::Other(HolderFrame::ViewFrame { generation, frame });
                }
                other => return Seen::Other(other),
            }
        }
    }

    fn apply(&mut self, push: &ScreenPush) {
        let mut lines = if push.full {
            blank_lines(push.cols, push.rows)
        } else {
            self.screen
                .as_ref()
                .expect("a difference came before any whole copy")
                .lines_resized(push.cols, push.rows)
        };
        for line in std::mem::take(&mut self.rows) {
            let row = usize::from(line.line);
            assert!(
                row < lines.len(),
                "a row past the copy's {} rows",
                lines.len()
            );
            lines[row] = line;
        }
        self.screen = Some(ScreenCopy {
            cols: push.cols,
            rows: push.rows,
            lines,
            cursor: push.cursor,
            cursor_hidden: push.cursor_hidden,
            sync_open: push.sync_open,
            input_modes: push.input_modes,
            generation: push.generation,
            outputs_fed: push.outputs_fed,
        });
    }

    /// Reads until the next applied copy; returns it and what came before.
    async fn next_copy(&mut self) -> (ScreenPush, Vec<Seen>) {
        let mut before = Vec::new();
        loop {
            match self.seen().await {
                Seen::Copy(push) => return (push, before),
                other => before.push(other),
            }
        }
    }

    /// Reads copies, acknowledged as they come, until one shows `needle`.
    async fn copy_with(&mut self, needle: &str) -> ScreenPush {
        assert!(self.auto_ack, "copies stop without a PushAck");
        loop {
            let (push, _) = self.next_copy().await;
            if self.text().contains(needle) {
                return push;
            }
        }
    }

    /// Reads until a frame `answer` accepts; returns it and what came before.
    async fn until(&mut self, answer: impl Fn(&HolderFrame) -> bool) -> (HolderFrame, Vec<Seen>) {
        let mut before = Vec::new();
        loop {
            match self.seen().await {
                Seen::Other(frame) if answer(&frame) => return (frame, before),
                other => before.push(other),
            }
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> u64 {
        let id = self.id();
        self.peer
            .send(HolderFrame::Write {
                id,
                bytes: bytes.to_vec(),
            })
            .await;
        id
    }

    async fn script(&mut self, bytes: &[u8]) -> u64 {
        self.write(&script_command(bytes)).await
    }

    /// `Settle`, then everything that came before its `Settled`.
    async fn settle(&mut self) -> Vec<Seen> {
        let id = self.id();
        self.peer.send(HolderFrame::Settle { id }).await;
        let (_, before) = self
            .until(|frame| matches!(frame, HolderFrame::Settled { id: settled } if *settled == id))
            .await;
        before
    }

    async fn read_tail(&mut self, max_lines: usize, max_bytes: usize) -> (String, bool, Vec<Seen>) {
        let id = self.id();
        self.peer
            .send(HolderFrame::ReadTail {
                id,
                max_lines,
                max_bytes,
            })
            .await;
        let (tail, before) = self
            .until(
                |frame| matches!(frame, HolderFrame::Tail { id: answered, .. } if *answered == id),
            )
            .await;
        match tail {
            HolderFrame::Tail {
                text, truncated, ..
            } => (text, truncated, before),
            other => panic!("expected Tail, got {other:?}"),
        }
    }

    /// Asks for the holder's text until it shows `needle`: the holder has
    /// taken that output in. Returns what came on the way.
    async fn tail_with(&mut self, needle: &str) -> Vec<Seen> {
        let mut seen = Vec::new();
        let deadline = Instant::now() + yagura_test_support::HANG_GUARD;
        loop {
            let (text, _, before) = self.read_tail(100, 64 * 1024).await;
            seen.extend(before);
            if text.contains(needle) {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "the holder's text never showed {needle:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    async fn view_op(
        &mut self,
        op: yagura_protocol::holder::HolderViewOp,
    ) -> (Option<String>, bool) {
        let id = self.id();
        self.peer.send(HolderFrame::ViewOp { id, op }).await;
        let (done, _) = self
            .until(
                |frame| matches!(frame, HolderFrame::ViewOpDone { id: answered, .. } if *answered == id),
            )
            .await;
        match done {
            HolderFrame::ViewOpDone { text, moved, .. } => (text, moved),
            other => panic!("expected ViewOpDone, got {other:?}"),
        }
    }

    /// Attaches a view as `generation`; returns the size of its first frame.
    async fn attach(&mut self, generation: u64) -> ViewSize {
        self.view = Some(generation);
        self.peer.send(HolderFrame::Attach { generation }).await;
        let (frame, _) = self
            .until(
                |frame| matches!(frame, HolderFrame::ViewFrame { generation: got, .. } if *got == generation),
            )
            .await;
        match frame {
            HolderFrame::ViewFrame { frame, .. } => ViewSize {
                cols: frame.cols,
                rows: frame.rows,
            },
            other => panic!("expected ViewFrame, got {other:?}"),
        }
    }

    /// Reads until the attached view's grid shows `needle`.
    async fn view_with(&mut self, needle: &str) {
        while !self.grid.contains_text(needle) {
            let _ = self.seen().await;
        }
    }
}

async fn script_window(rig: &Rig, auto_ack: bool) -> (Holder, PathBuf, Link, ScreenPush) {
    let log = rig.file("input.log");
    let holder = rig.start(&rig.launch(&["script", path_arg(&log)]));
    let (link, first) = Link::open(&rig.record().await.endpoint, auto_ack).await;
    (holder, log, link, first)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_whole_copy_follows_the_welcome_and_each_push_ack_lets_exactly_the_latest_copy_out() {
    let rig = Rig::new("pushack");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, first) = script_window(&rig, false).await;

    // The output is taken in while the first copy is unacknowledged: the
    // holder's own text shows it, and Settled comes only after it.
    link.script(b"ONE-MARK").await;
    let mut unacknowledged = link.tail_with("ONE-MARK").await;
    unacknowledged.extend(link.settle().await);
    assert!(
        copies(&unacknowledged).is_empty(),
        "a copy came while the first one was unacknowledged"
    );

    link.peer
        .send(HolderFrame::PushAck {
            number: first.number,
        })
        .await;
    let (pushed, _) = link.next_copy().await;
    assert!(!pushed.full, "the copy after the first is not a difference");
    assert_eq!(pushed.number, first.number + 1);
    assert!(
        link.text().contains("ONE-MARK"),
        "the copy let out by the PushAck is not the latest screen: {}",
        link.text()
    );
    assert!(pushed.outputs_fed > first.outputs_fed);

    let after = link.settle().await;
    assert!(
        copies(&after).is_empty(),
        "a second copy came for one PushAck"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_that_leaves_the_screen_as_it_was_still_pushes_a_copy_with_more_outputs_fed() {
    let rig = Rig::new("unchanged");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, _) = script_window(&rig, true).await;
    link.script(b"STEADY").await;
    link.copy_with("STEADY").await;
    link.settle().await;
    let before = link.screen().clone();

    // A title changes nothing on the screen.
    link.script(b"\x1b]0;yagura-title\x07").await;
    let (pushed, _) = link.next_copy().await;
    assert!(
        pushed.outputs_fed > before.outputs_fed,
        "the copy after the output did not count it ({} then {})",
        before.outputs_fed,
        pushed.outputs_fed
    );
    assert_eq!(
        link.screen().lines,
        before.lines,
        "the title changed the screen"
    );
}

const DA1: &[u8] = b"\x1b[c";
const DA1_REPLY: &[u8] = b"\x1b[?1;2c";
const DSR: &[u8] = b"\x1b[5n";
const DSR_REPLY: &[u8] = b"\x1b[0n";
const DECRQM_ALT_SCREEN: &[u8] = b"\x1b[?1049$p";
const DECRQM_ALT_SCREEN_REPLY: &[u8] = b"\x1b[?1049;2$y";
const QUERY_BACKGROUND: &[u8] = b"\x1b]11;?\x1b\\";

fn background_reply(rgb: [u8; 3]) -> Vec<u8> {
    format!(
        "\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
        rgb[0], rgb[1], rgb[2]
    )
    .into_bytes()
}

fn palette() -> Palette {
    Palette {
        foreground: [0x41, 0x42, 0x43],
        background: [0x0a, 0x0b, 0x2c],
        cursor: [0x12, 0x34, 0x56],
        ansi: (0..16u8).map(|i| [i * 16, 255 - i * 8, i]).collect(),
    }
}

/// The holder answers the child's queries from its model, once per query.
/// The holder has one writer, so the answers are written before a write the
/// test sends after `Settled`.
#[tokio::test(flavor = "multi_thread")]
async fn queries_are_answered_by_the_holder_once_each() {
    let rig = Rig::new("queries");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, log, mut link, _) = script_window(&rig, true).await;
    let palette = palette();
    link.peer
        .send(HolderFrame::SetPalette {
            palette: palette.clone(),
        })
        .await;
    link.copy_with(SCRIPT_MARKER).await;

    link.script(&[DA1, DSR, DECRQM_ALT_SCREEN, QUERY_BACKGROUND, b"QUERIED"].concat())
        .await;
    link.copy_with("QUERIED").await;
    link.settle().await;
    link.write(b"AFTER-MARK").await;

    let log = log_with(&log, b"AFTER-MARK").await;
    let at = log
        .windows(b"AFTER-MARK".len())
        .position(|window| window == b"AFTER-MARK")
        .unwrap();
    let before = &log[..at];
    let background = background_reply(palette.background);
    for (answer, what) in [
        (DA1_REPLY, "DA1"),
        (DSR_REPLY, "DSR"),
        (DECRQM_ALT_SCREEN_REPLY, "DECRQM"),
        (background.as_slice(), "the background color"),
    ] {
        assert_eq!(
            count(before, answer),
            1,
            "the answer to {what} did not reach the child exactly once: {:?}",
            String::from_utf8_lossy(before)
        );
    }
}

/// A `Report` is not the user's write: it does not release a synchronized
/// update a view holds back. A `Write` does.
#[tokio::test(flavor = "multi_thread")]
async fn a_report_does_not_release_a_held_update_and_a_write_does() {
    let rig = Rig::new("report");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, log, mut link, _) = script_window(&rig, true).await;
    link.copy_with(SCRIPT_MARKER).await;
    link.attach(1).await;
    link.view_with(SCRIPT_MARKER).await;

    link.script(b"\x1b[?2026hHELD-TEXT").await;
    link.copy_with("HELD-TEXT").await;
    link.peer
        .send(HolderFrame::Report {
            bytes: b"REPORTED".to_vec(),
        })
        .await;
    log_with(&log, b"REPORTED").await;
    link.settle().await;
    assert!(
        !link.grid.contains_text("HELD-TEXT"),
        "a Report released the held update"
    );

    link.write(b"x").await;
    link.view_with("HELD-TEXT").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_focus_report_goes_right_before_the_writes_body() {
    let rig = Rig::new("focus");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, log, mut link, _) = script_window(&rig, true).await;
    link.script(b"\x1b[?1004hFOCUS-ON").await;
    link.copy_with("FOCUS-ON").await;

    link.peer
        .send(HolderFrame::SetViewFocus { focused: false })
        .await;
    link.peer
        .send(HolderFrame::FocusInterval { open: true })
        .await;
    link.write(b"BODY").await;

    let log = log_with(&log, b"BODY").await;
    assert!(
        contains(&log, b"\x1b[IBODY"),
        "the child did not read the focus report right before the body: {:?}",
        String::from_utf8_lossy(&log)
    );
}

/// With no interval open, the write itself tells the child it has the focus,
/// in front of its body.
#[tokio::test(flavor = "multi_thread")]
async fn without_an_interval_the_writes_own_focus_report_goes_right_before_its_body() {
    let rig = Rig::new("focusprefix");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, log, mut link, _) = script_window(&rig, true).await;
    link.script(b"\x1b[?1004hFOCUS-ON").await;
    link.copy_with("FOCUS-ON").await;

    link.peer
        .send(HolderFrame::SetViewFocus { focused: false })
        .await;
    link.write(b"BODY").await;

    let log = log_with(&log, b"BODY").await;
    assert!(
        contains(&log, b"\x1b[O\x1b[IBODY"),
        "the child did not read the write's focus report right before the body: {:?}",
        String::from_utf8_lossy(&log)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resize_sends_a_copy_of_its_size_before_resized_and_an_attach_after_it_draws_that_size() {
    let rig = Rig::new("size");
    let _daemon = hold_daemon_endpoint(&rig).await;
    // No PushAck at all: the copies below are the ones a resize owes.
    let (_holder, _log, mut link, first) = script_window(&rig, false).await;
    assert_eq!((first.cols, first.rows), (80, 24));

    link.peer
        .send(HolderFrame::Resize {
            cols: 100,
            rows: 30,
        })
        .await;
    let (resized, before) = link
        .until(|frame| {
            matches!(
                frame,
                HolderFrame::Resized { .. } | HolderFrame::ResizeFailed { .. }
            )
        })
        .await;
    assert!(
        matches!(
            resized,
            HolderFrame::Resized {
                cols: 100,
                rows: 30
            }
        ),
        "expected Resized, got {resized:?}"
    );
    assert!(
        copies(&before)
            .iter()
            .any(|push| (push.cols, push.rows) == (100, 30)),
        "no copy of the new size came before Resized"
    );

    link.peer
        .send(HolderFrame::Resize { cols: 90, rows: 28 })
        .await;
    let size = link.attach(1).await;
    assert_eq!(
        (size.cols, size.rows),
        (90, 28),
        "the first frame after Resize then Attach is not of the new size"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_childs_last_output_is_in_a_copy_that_comes_before_exited() {
    let rig = Rig::new("exited");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let gate = rig.file("gate");
    let _holder = rig.start(&rig.launch(&["exit", "0", path_arg(&gate)]));
    // No PushAck: the last copy is owed before Exited all the same.
    let (mut link, _) = Link::open(&rig.record().await.endpoint, false).await;

    std::fs::write(&gate, b"").unwrap();
    link.until(|frame| matches!(frame, HolderFrame::Exited { .. }))
        .await;
    assert!(
        link.text().contains("EXITING 0"),
        "the copies before Exited miss the child's last output: {}",
        link.text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_frame_for_output_before_a_settle_comes_before_settled() {
    let rig = Rig::new("settle");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, _) = script_window(&rig, true).await;
    link.copy_with(SCRIPT_MARKER).await;
    link.attach(1).await;

    link.script(b"SETTLE-MARK").await;
    link.copy_with("SETTLE-MARK").await;
    link.settle().await;
    assert!(
        link.grid.contains_text("SETTLE-MARK"),
        "Settled came before the frame for the output before it"
    );
}

fn select_row(row: u16) -> yagura_protocol::holder::HolderViewOp {
    yagura_protocol::holder::HolderViewOp::Select {
        pointer: PointerInput {
            kind: PointerKind::Down,
            button: PointerButton::Left,
            cell: CellPosition { col: 0, row },
            mods: Modifiers::default(),
            clicks: 3,
        },
    }
}

fn numbered_line(text: &str) -> u32 {
    text.trim()
        .strip_prefix('L')
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("not a numbered line: {text:?}"))
}

/// Scrolling, selecting and reading back answer from the holder's model,
/// within the limits asked for; the copy stays the live screen while the
/// view is scrolled back.
#[tokio::test(flavor = "multi_thread")]
async fn view_operations_and_the_tail_answer_from_the_holder_and_the_copy_stays_live() {
    use yagura_protocol::holder::HolderViewOp;

    let rig = Rig::new("viewops");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, _) = script_window(&rig, true).await;
    let lines: String = (1..=40).map(|n| format!("L{n:02}\r\n")).collect();
    link.script(lines.as_bytes()).await;
    link.copy_with("L40").await;

    let (tail, _, _) = link.read_tail(5, 64 * 1024).await;
    assert!(
        tail.lines().count() <= 5,
        "more lines than asked for: {tail:?}"
    );
    assert!(
        tail.contains("L40"),
        "the tail misses the last line: {tail:?}"
    );
    let (tail, truncated, _) = link.read_tail(HISTORY_ROWS, 10).await;
    assert!(tail.len() <= 10, "more bytes than asked for: {tail:?}");
    assert!(truncated, "a cut tail is not reported as cut");
    let (tail, _, _) = link.read_tail(HISTORY_ROWS, 64 * 1024).await;
    assert!(
        tail.contains("L01"),
        "the history scrolled off the screen is not kept: {tail:?}"
    );

    let top = row_text(&link.screen().lines[0]);
    let top_number = numbered_line(&top);
    link.view_op(select_row(0)).await;
    let (selected, _) = link.view_op(HolderViewOp::SelectedText).await;
    assert_eq!(
        selected.as_deref().map(str::trim),
        Some(top.trim()),
        "the selection is not the top row"
    );

    link.view_op(HolderViewOp::ClearSelection).await;
    link.view_op(HolderViewOp::Scroll { lines: 5 }).await;
    link.view_op(select_row(0)).await;
    let (selected, _) = link.view_op(HolderViewOp::SelectedText).await;
    assert_eq!(
        numbered_line(selected.as_deref().unwrap_or_default()),
        top_number - 5,
        "the scroll did not move the view five rows back"
    );

    link.script(b"LIVE-ROW").await;
    link.copy_with("LIVE-ROW").await;
    let (_, moved) = link.view_op(HolderViewOp::ToBottom).await;
    assert!(moved, "the view was not scrolled back");
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_screen_resync_the_next_copy_is_whole() {
    let rig = Rig::new("resync");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, first) = script_window(&rig, false).await;
    link.peer
        .send(HolderFrame::PushAck {
            number: first.number,
        })
        .await;
    link.script(b"R-ONE").await;
    loop {
        let (push, _) = link.next_copy().await;
        assert!(
            !push.full,
            "a copy after the first is whole without a resync"
        );
        if link.text().contains("R-ONE") {
            break;
        }
        link.peer
            .send(HolderFrame::PushAck {
                number: push.number,
            })
            .await;
    }

    link.peer.send(HolderFrame::ScreenResync).await;
    link.script(b"R-TWO").await;
    let (push, _) = link.next_copy().await;
    assert!(push.full, "the copy after ScreenResync is not whole");
}

#[tokio::test(flavor = "multi_thread")]
async fn copies_go_on_while_the_child_holds_a_synchronized_update_open() {
    let rig = Rig::new("syncopen");
    let _daemon = hold_daemon_endpoint(&rig).await;
    let (_holder, _log, mut link, _) = script_window(&rig, true).await;

    link.script(b"\x1b[?2026hSYNC-ONE").await;
    let push = link.copy_with("SYNC-ONE").await;
    assert!(push.sync_open, "the copy does not say the update is open");
    link.script(b"SYNC-TWO").await;
    let push = link.copy_with("SYNC-TWO").await;
    assert!(push.sync_open, "the copy does not say the update is open");
}

/// The shipped holder never runs a fault meant for tests: it refuses the
/// launch as a child that could not start, and the child never runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_holder_refuses_a_launch_that_asks_for_a_fault() {
    use yagura_protocol::holder::HolderFault;

    for (fault, label) in [
        (HolderFault::StallModel, "faultstall"),
        (HolderFault::PoisonModel, "faultpoison"),
    ] {
        let rig = Rig::new(label);
        let _daemon = hold_daemon_endpoint(&rig).await;
        let count = rig.file("count");
        let mut launch = rig.launch(&["flood", path_arg(&count), "8192"]);
        launch.fault = Some(fault);
        let _holder = rig.start(&launch);
        let mut peer = Peer::connect(&rig.record().await.endpoint).await;
        peer.send(hello()).await;

        let mut frames = Vec::new();
        loop {
            let frame = peer.frame().await;
            let exited = matches!(frame, HolderFrame::Exited { .. });
            frames.push(frame);
            if exited {
                break;
            }
        }
        assert!(
            matches!(
                frames.as_slice(),
                [HolderFrame::SpawnFailed { .. }, HolderFrame::Exited { .. }]
            ),
            "{fault:?}: expected SpawnFailed then Exited and no Welcome: {frames:?}"
        );
        assert!(!count.exists(), "{fault:?}: the child ran");
    }
}
