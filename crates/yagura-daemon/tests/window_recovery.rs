#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The stand-in holders and their children are processes this test starts, waits for
// and kills itself; no window ever adopts them.
#![allow(clippy::disallowed_methods)]

//! A window outlives its daemon: a daemon killed outright leaves every window's holder
//! and child running, and the next daemon of the instance takes each window back under
//! the same id, starting `Unknown`, with the screen, the input modes and the token it had.
//! A window whose holder does not welcome the next daemon stays listed and closable, and
//! its holder and child are not ended for it. A window taken back keeps the hook route
//! its open settled, with its row or from its holder's launch record.
//!
//! Each case runs real daemon processes one after the other in one sandbox instance.

use crate::common;
use crate::common::bins::{self, echo_child_path, holder_child_path, relay_path, term_script_path};

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::real_daemon::{
    ClientCommand, ClientLog, ClientProcess, ClientRead, RunningDaemon, Sandbox,
};
use common::store_fixtures::{insert_window_row_with_layout_value, set_meta};
use common::{read_frame, read_response, send};
use rusqlite::types::Value as SqlValue;
use yagura_daemon::store;
use yagura_os::{Liveness, PeerAuth, PlatformIpc, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderRecord, encode_holder_record, holder_record_path, read_holder_record,
};
use yagura_protocol::{
    ClientMessage, DaemonMessage, DeliveryState, StatusBoot, WindowInfo, WindowStatus,
};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::{Connection, connect_with_retry};
use yagura_test_support::process::{
    COMPOSER_CONTINUATION_PREFIX, COMPOSER_MARKER, COMPOSER_SEPARATOR,
};

/// The only token shape `StatusToken::from_override` accepts: the first daemon gives it
/// to every window it opens, and the next daemon draws its own.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210";
const SCRIPT_MARKER: &str = "HOLDER-CHILD-SCRIPT";
const DA1: &[u8] = b"\x1b[c";
const DA1_REPLY: &[u8] = b"\x1b[?1;2c";

fn holder_child() -> String {
    holder_child_path()
        .to_str()
        .expect("holder_child path must be UTF-8")
        .to_owned()
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("a sandbox path must be UTF-8")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A profile whose child logs every byte it reads to `log`, writes what it is sent
/// between `<` and `>` as hex, and with `gated` writes those bytes once the file exists.
fn write_script_profile(sandbox: &Sandbox, id: &str, log: &Path, gated: Option<(&Path, &[u8])>) {
    let child = holder_child();
    let mut argv = vec![child, "script".to_owned(), utf8(log).to_owned()];
    if let Some((gate, bytes)) = gated {
        argv.push(utf8(gate).to_owned());
        argv.push(hex(bytes));
    }
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    sandbox.write_probe_profile(id, &argv, "");
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Waits until `log` holds `needle`; returns the log. With `holder`, fails as soon as
/// that process has ended instead.
async fn logged(log: &Path, needle: &[u8], holder: Option<&ProcessHandle>) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let bytes = std::fs::read(log).unwrap_or_default();
        if position(&bytes, needle).is_some() {
            return bytes;
        }
        if let Some(holder) = holder {
            assert_ne!(
                holder.liveness(),
                Liveness::Dead,
                "the window's holder ended once its daemon was gone; the child had read {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never read {:?}; it read {:?}",
            log.display(),
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&bytes)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn list_windows(daemon: &mut RunningDaemon) -> Vec<WindowInfo> {
    send(&mut daemon.client, &ClientMessage::ListWindows).await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowList { windows } => windows,
        other => panic!("expected window_list, got {other:?}"),
    }
}

async fn listed(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) -> WindowInfo {
    let windows = list_windows(daemon).await;
    windows
        .iter()
        .find(|info| info.window_id == window_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the next daemon does not list the window {window_id} its predecessor left: \
                 {windows:?}; daemon log:\n{}",
                sandbox.daemon_log()
            )
        })
}

/// The window's own credentials, which a `window_client` reads from its environment.
const OWN_WINDOW: &str = "$YAGURA_WINDOW_ID";
const OWN_TOKEN: &str = "$YAGURA_STATUS_TOKEN";

/// A status event of `window_id` with `token`.
fn status_event(window_id: &str, token: &str) -> ClientMessage {
    ClientMessage::StatusEvent {
        source: "cli".to_owned(),
        status: WindowStatus::Running,
        window_id: Some(window_id.to_owned()),
        token: token.to_owned(),
    }
}

/// Asks `log`'s client for a status event and then a ping: the refusal the daemon answers
/// before the pong, if any, is what `refusal` takes from the responses read.
fn status_event_command(log: &mut ClientLog, window_id: &str, token: &str) -> ClientCommand {
    let conn = log.new_conn();
    log.exchange(&conn, &status_event(window_id, token), ClientRead::Pong)
}

/// The refusal among the responses read before a pong, if any.
fn refusal(mut answers: Vec<DaemonMessage>) -> Option<DaemonMessage> {
    assert!(
        answers.len() <= 1,
        "a status event is refused at most once before its pong: {answers:?}"
    );
    answers.pop()
}

/// An `identify` of `window_id` with [`TOKEN`], declaring `profile` when given.
fn identify_message(window_id: &str, profile: Option<&str>) -> ClientMessage {
    ClientMessage::Identify {
        window_id: window_id.to_owned(),
        token: TOKEN.to_owned(),
        declaration: profile.map(|profile| yagura_protocol::Declaration {
            profile: profile.to_owned(),
        }),
    }
}

/// The screen's text rows that hold anything, right-trimmed.
fn rows(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn record_of(sandbox: &Sandbox, window_id: &str) -> HolderRecord {
    read_holder_record(&sandbox.holders_dir(), window_id).unwrap_or_else(|error| {
        panic!(
            "the window {window_id}'s holder must keep its record: {error}; daemon log:\n{}",
            sandbox.daemon_log()
        )
    })
}

fn handle_of(pid: u32, start: Option<&str>) -> ProcessHandle {
    let start: ProcessStart = start
        .expect("the record carries the process's start")
        .parse()
        .expect("the record's start reads back");
    ProcessHandle::open(pid, &start)
        .expect("open the recorded process")
        .expect("the recorded process must be running")
}

fn start_of(pid: u32) -> String {
    yagura_os::process_start(pid)
        .expect("read the process's start")
        .expect("the process has a start")
        .to_string()
}

/// The `(origin, seq)` of the boot `info` names, which `who` must name.
fn boot_of(info: &WindowInfo, who: &str) -> (u64, u64) {
    let boot = info
        .status_boot
        .as_ref()
        .unwrap_or_else(|| panic!("{who} must name its boot on window {}", info.window_id));
    (boot.origin, boot.seq)
}

/// The largest boot `origin` or `seq` a JavaScript number holds exactly: 2^53 − 1.
const MAX_BOOT_VALUE: u64 = (1 << 53) - 1;

/// Records `(origin, seq)` as the instance's last boot, before a daemon starts.
fn write_boot_file(sandbox: &Sandbox, origin: u64, seq: u64) {
    let root = sandbox.state_root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(yagura_daemon::status_boot::STATUS_BOOT_FILE),
        serde_json::to_vec(&StatusBoot { origin, seq }).unwrap(),
    )
    .unwrap();
}

/// The first boot of a new series: `seq` 1, with an origin a JavaScript number holds.
fn assert_new_series(boot: (u64, u64), why: &str) {
    assert_eq!(
        boot.1, 1,
        "{why}: a new series starts at seq 1, got {boot:?}"
    );
    assert!(
        boot.0 <= MAX_BOOT_VALUE,
        "{why}: a new series' origin must stay within 2^53 - 1, got {boot:?}"
    );
}

/// A boot after `earlier` in the same series: the same origin and a larger seq.
fn assert_later_boot_of_the_same_series(earlier: (u64, u64), later: (u64, u64)) {
    assert_eq!(
        later.0, earlier.0,
        "the next daemon must keep the series of the boot file the window's holder lives beside"
    );
    assert!(
        later.1 > earlier.1,
        "the next daemon's boot must come after the one before it: {earlier:?} then {later:?}"
    );
}

/// A window, killed and taken back, keeps its id, its screen with what the child wrote
/// while no daemon was there, its input modes and its token, and starts `Unknown`. The
/// query the child made meanwhile was answered once, by the holder, before and after the
/// next daemon took the window back.
#[tokio::test]
async fn a_window_outlives_its_daemon_and_the_next_one_takes_it_back_whole() {
    const ABSENT_MARK: &str = "ABSENT-MARK";
    let sandbox = Sandbox::new("recover-whole");
    let log = sandbox.tree.path().join("input.log");
    let gate = sandbox.tree.path().join("gate");
    let absent_output = [DA1, ABSENT_MARK.as_bytes(), b"\r\n"].concat();
    write_script_profile(&sandbox, "script", &log, Some((&gate, &absent_output)));
    write_boot_file(&sandbox, 11, MAX_BOOT_VALUE);

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    first.attach(&window_id).await;
    first
        .output_until(&window_id, "the script's marker", |text| {
            text.contains(SCRIPT_MARKER)
        })
        .await;
    send(
        &mut first.client,
        &ClientMessage::Text {
            text: format!("<{}>", hex(b"\x1b[?2004hBEFORE-MARK\r\n")),
        },
    )
    .await;
    first
        .output_until(&window_id, "the mark before the kill", |text| {
            text.contains("BEFORE-MARK")
        })
        .await;
    send(
        &mut first.client,
        &ClientMessage::Paste {
            text: "PASTE-BEFORE".to_owned(),
        },
    )
    .await;
    // The fixture's precondition: the first daemon already brackets a paste.
    logged(&log, b"\x1b[200~PASTE-BEFORE\x1b[201~", None).await;
    let before = rows(
        &first
            .output_until(&window_id, "the screen before the kill", |text| {
                text.contains("BEFORE-MARK")
            })
            .await,
    );
    let record = record_of(&sandbox, &window_id);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    let first_boot = boot_of(
        &listed(&mut first, &sandbox, &window_id).await,
        "the first daemon",
    );
    assert_new_series(first_boot, "a recorded seq at 2^53 - 1 has no seq after it");
    assert_ne!(
        first_boot.0, 11,
        "a recorded seq at 2^53 - 1 must not be followed in its own series"
    );

    first.kill_keeping_windows();
    std::fs::write(&gate, b"").unwrap();
    // The holder answers the child's query while no daemon is there.
    logged(&log, DA1_REPLY, Some(&holder)).await;

    let mut next = sandbox.start().await;
    let info = listed(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        info.status,
        WindowStatus::Unknown,
        "a window taken back starts Unknown"
    );
    let next_boot = boot_of(&info, "the next daemon");
    assert_later_boot_of_the_same_series(first_boot, next_boot);
    let after = rows(
        &next
            .output_until(&window_id, "the output of the absence", |text| {
                text.contains(ABSENT_MARK)
            })
            .await,
    );
    let mut expected = before.clone();
    expected.push(ABSENT_MARK.to_owned());
    assert_eq!(
        after, expected,
        "the window taken back must show the screen it had, then what the child wrote \
         while no daemon was there"
    );

    next.attach(&window_id).await;
    // Read before any helper below that skips status pushes.
    let snapshot_boot = loop {
        match read_frame(&mut next.client).await {
            DaemonMessage::Frame(_) | DaemonMessage::ChatParticipantsChanged => {}
            DaemonMessage::StatusChanged {
                window_id: changed,
                status_boot,
                ..
            } if changed == window_id => break status_boot.map(|boot| (boot.origin, boot.seq)),
            other => panic!("expected the attach's status snapshot, got {other:?}"),
        }
    };
    assert_eq!(
        snapshot_boot,
        Some(next_boot),
        "the status snapshot after an attach must name the boot the window is listed with"
    );
    send(
        &mut next.client,
        &ClientMessage::Paste {
            text: "INPUT-MARK".to_owned(),
        },
    )
    .await;
    let input = logged(&log, b"\x1b[200~INPUT-MARK\x1b[201~", None).await;
    let before_input = &input[..position(&input, b"INPUT-MARK").unwrap()];
    assert_eq!(
        count(before_input, DA1_REPLY),
        1,
        "the query made while no daemon was there was not answered exactly once: {:?}",
        String::from_utf8_lossy(before_input)
    );

    // The window's own process names it, as its CLI's hooks do; a token that is not the
    // window's is refused before the window's tree is asked, so the test process sends it.
    let mut client = ClientLog::new(&sandbox, "whole");
    let status = status_event_command(&mut client, OWN_WINDOW, OWN_TOKEN);
    next.start_client_once(&window_id, &client, std::slice::from_ref(&status))
        .await;
    assert_eq!(
        refusal(client.messages(&status).await),
        None,
        "the window taken back must accept a status event with the token it had"
    );
    match common::status_event_refusal(&mut next.client, &window_id, OTHER_TOKEN).await {
        Some(DaemonMessage::Error { code, .. }) => assert_eq!(code, "auth_failed"),
        other => panic!("a token that is not the window's must be refused, got {other:?}"),
    }
}

/// A process standing in for a window's holder at the endpoint its real holder had, with
/// an idle child, both named in the window's record. Killed on drop, and its endpoint
/// removed, which a killed holder cannot do.
struct FakeHolder {
    holder: Child,
    child: Child,
    log: PathBuf,
    holders_dir: PathBuf,
    instance: String,
    window_id: String,
}

impl Drop for FakeHolder {
    fn drop(&mut self) {
        for process in [&mut self.holder, &mut self.child] {
            let _ = process.kill();
            let _ = process.wait();
        }
        yagura_test_support::process::remove_killed_holder_endpoint(
            &self.holders_dir,
            Some(OsStr::new(&self.instance)),
            &self.window_id,
        );
    }
}

impl FakeHolder {
    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn wait_logged(&self, what: &str, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        loop {
            let lines = self.log_lines();
            if done(&lines) {
                return lines;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stand-in holder never logged {what}; it logged {lines:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_gone(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while self.holder.try_wait().unwrap().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stand-in holder outlived {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Hands a `gate` stand-in the line it answers the waiting `Hello` with.
    fn answer(&mut self, line: &str) {
        let stdin = self
            .holder
            .stdin
            .as_mut()
            .expect("the stand-in's stdin is piped");
        writeln!(stdin, "{line}").expect("write to the stand-in's stdin");
        stdin.flush().expect("flush the stand-in's stdin");
    }

    /// The line that has a `gate` stand-in welcome the daemon, naming its idle child.
    fn welcome_own_child(&self) -> String {
        welcome_naming(self.child.id())
    }

    fn running(&mut self) -> (bool, bool) {
        (
            self.holder.try_wait().unwrap().is_none(),
            self.child.try_wait().unwrap().is_none(),
        )
    }
}

fn spawn_term_script(args: &[&str]) -> Child {
    let mut command = Command::new(term_script_path());
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().expect("start term_script")
}

/// Ends the window's real holder and its watcher, which its killed daemon left, before
/// either can clean up, so the endpoint's lock is free; then puts a stand-in in its
/// place: at the endpoint `record` names, in a record naming the stand-in and its child
/// with their starts and no watcher.
async fn replace_holder(
    sandbox: &Sandbox,
    record: &HolderRecord,
    mode: &str,
    log: PathBuf,
) -> FakeHolder {
    replace_holder_with(sandbox, record, &[mode], log).await
}

/// [`replace_holder`] with the stand-in's mode followed by the arguments after its log.
async fn replace_holder_with(
    sandbox: &Sandbox,
    record: &HolderRecord,
    mode: &[&str],
    log: PathBuf,
) -> FakeHolder {
    let holders_dir = sandbox.holders_dir();
    let _ = yagura_test_support::process::end_recorded_holder(
        &holders_dir,
        Some(OsStr::new(&sandbox.instance)),
        record,
    );
    let child = spawn_term_script(&["idle"]);
    let mut args = vec!["fake-holder", mode[0], &record.endpoint, utf8(&log)];
    args.extend_from_slice(&mode[1..]);
    let holder = spawn_term_script(&args);
    let fake = FakeHolder {
        holder,
        child,
        log,
        holders_dir,
        instance: sandbox.instance.clone(),
        window_id: record.window_id.clone(),
    };
    fake.wait_logged("LISTENING", |lines| lines.iter().any(|l| l == "LISTENING"))
        .await;
    let holder_pid = fake.holder.id();
    let child_pid = fake.child.id();
    let replaced = HolderRecord {
        holder_pid,
        child_pid: Some(child_pid),
        protocol: HOLDER_PROTOCOL,
        holder_start: Some(start_of(holder_pid)),
        child_start: Some(start_of(child_pid)),
        watcher_pid: None,
        watcher_start: None,
        ..record.clone()
    };
    std::fs::write(
        holder_record_path(&sandbox.holders_dir(), &record.window_id).unwrap(),
        encode_holder_record(&replaced),
    )
    .unwrap();
    fake
}

fn hello_logged(lines: &[String]) -> bool {
    lines.iter().any(|line| line.starts_with("FRAME Hello"))
}

/// The line that has a `gate` stand-in welcome the daemon, naming the process `pid` and its
/// start as the window's child.
fn welcome_naming(pid: u32) -> String {
    format!("welcome {pid} {}", start_of(pid))
}

/// Another connection to the sandbox's daemon.
async fn connect(sandbox: &Sandbox) -> Connection {
    let user = PlatformIpc::default()
        .current_user_id()
        .expect("the current user id must resolve");
    connect_with_retry(&yagura_os::default_endpoint(&user, &sandbox.namespace())).await
}

fn deliver_take(window_id: &str, token: &str) -> ClientMessage {
    ClientMessage::DoorRead {
        window_id: window_id.to_owned(),
        token: token.to_owned(),
        door: yagura_protocol::Door::Take,
    }
}

fn assert_auth_failed(answer: DaemonMessage, what: &str) {
    match answer {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "auth_failed", "{what}"),
        other => panic!("{what} must be refused, got {other:?}"),
    }
}

/// Waits until the daemon has logged that a request to `window_id` waits for the
/// window's take-back.
async fn until_held(sandbox: &Sandbox, window_id: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let field = format!("window_id={window_id}");
    loop {
        let log = sandbox.daemon_log();
        if log
            .lines()
            .any(|line| line.contains("waits for the window's take-back") && line.contains(&field))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon never held the request to {window_id} for its take-back; daemon log:\n{log}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A launch record of `window_id` carrying `token`, as a holder hands it back.
fn launch_record(window_id: &str, token: &str) -> String {
    serde_json::json!({
        "window_id": window_id,
        "token": token,
        "window": { "name": window_id, "cwd": ".", "command": ["idle"], "credentials": "env" },
    })
    .to_string()
}

/// A request carrying a window's token that arrives while the window is still being
/// taken back is checked once the take-back ends: it passes with the token the holder
/// hands back, and is refused when the take-back fails. A window taken back without a
/// row serves it as the window its launch record's values build, with what it keeps.
#[tokio::test]
async fn a_door_request_during_a_take_back_is_checked_once_the_take_back_ends() {
    const KEPT_MARK: &str = "ROWLESS-KEPT-PARCEL";
    let sandbox = Sandbox::new("recover-gate");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let welcomed = first.open_profile_window(&sandbox, "script").await;
    let refused = first.open_profile_window(&sandbox, "script").await;
    let rowless = first.open_profile_window(&sandbox, "script").await;
    let welcomed_record = record_of(&sandbox, &welcomed);
    let refused_record = record_of(&sandbox, &refused);
    let rowless_record = record_of(&sandbox, &rowless);
    first.kill_keeping_windows();
    // The row goes, and a parcel is kept for the window without it, which only the
    // window built from its launch record's values loads.
    {
        let db = rusqlite::Connection::open(store_db(&sandbox)).unwrap();
        db.execute("DELETE FROM windows WHERE window_id = ?1", [&rowless])
            .unwrap();
        db.execute(
            "INSERT INTO inbox_parcels (window_id, seq, key, notice, body) \
             VALUES (?1, 1, 'seeded:1', 0, ?2)",
            [rowless.as_str(), KEPT_MARK],
        )
        .unwrap();
    }

    let fakes = sandbox.tree.make_dir("fake");
    let welcomed_launch = fakes.join("welcomed.record");
    std::fs::write(&welcomed_launch, launch_record(&welcomed, TOKEN)).unwrap();
    let mut welcomed_fake = replace_holder_with(
        &sandbox,
        &welcomed_record,
        &["gate", utf8(&welcomed_launch)],
        fakes.join("welcomed.log"),
    )
    .await;
    let refused_launch = fakes.join("refused.record");
    std::fs::write(&refused_launch, launch_record(&refused, TOKEN)).unwrap();
    let mut refused_fake = replace_holder_with(
        &sandbox,
        &refused_record,
        &["gate", utf8(&refused_launch)],
        fakes.join("refused.log"),
    )
    .await;
    let rowless_launch = fakes.join("rowless.record");
    std::fs::write(&rowless_launch, launch_record(&rowless, TOKEN)).unwrap();
    let mut rowless_fake = replace_holder_with(
        &sandbox,
        &rowless_record,
        &["gate", utf8(&rowless_launch)],
        fakes.join("rowless.log"),
    )
    .await;

    // The windows that are welcomed ask from a process of their own tree: the child their
    // holder names. The refused one asks as it is, since its token is never settled.
    let mut welcomed_log = ClientLog::new(&sandbox, "welcomed");
    let mut welcomed_client = ClientProcess::spawn(&sandbox, &welcomed_log);
    let mut rowless_log = ClientLog::new(&sandbox, "rowless");
    let mut rowless_client = ClientProcess::spawn(&sandbox, &rowless_log);

    let mut next = sandbox.start().await;
    for (window_id, fake) in [
        (&welcomed, &welcomed_fake),
        (&refused, &refused_fake),
        (&rowless, &rowless_fake),
    ] {
        listed(&mut next, &sandbox, window_id).await;
        fake.wait_logged("the next daemon's Hello", hello_logged)
            .await;
    }
    let welcomed_take =
        welcomed_log.exchange("door", &deliver_take(&welcomed, TOKEN), ClientRead::One);
    welcomed_client.command(&welcomed_take);
    let mut refused_door = connect(&sandbox).await;
    send(&mut refused_door, &deliver_take(&refused, TOKEN)).await;
    let rowless_take =
        rowless_log.exchange("door", &deliver_take(&rowless, TOKEN), ClientRead::One);
    rowless_client.command(&rowless_take);
    for window_id in [&welcomed, &refused, &rowless] {
        until_held(&sandbox, window_id).await;
    }

    welcomed_fake.answer(&welcome_naming(welcomed_client.id()));
    refused_fake.answer("reject");
    rowless_fake.answer(&welcome_naming(rowless_client.id()));
    match welcomed_log.message(&welcomed_take).await {
        DaemonMessage::Delivery { .. } => {}
        other => panic!(
            "a request with the token the holder hands back must pass once the window is \
             taken back, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
    assert_auth_failed(
        read_response(&mut refused_door).await,
        "a request to a window whose take-back failed",
    );
    match rowless_log.message(&rowless_take).await {
        DaemonMessage::Delivery { output, .. } => assert!(
            output.contains(KEPT_MARK),
            "a window taken back without a row must serve the request as the window its \
             values build, with the parcel it keeps: {output:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
        other => panic!(
            "a request to a window taken back without a row must pass, got {other:?}; \
             daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

/// Windows whose holders never welcome the next daemon are listed `Unknown` while it
/// takes them back, and are closed both by `close_window` and by the daemon's shutdown,
/// which end their holders. Another window's status event is not held up by them.
#[tokio::test]
async fn a_window_being_taken_back_is_listed_closable_and_holds_no_other_window_up() {
    let sandbox = Sandbox::new("recover-mute");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);
    write_boot_file(&sandbox, u64::MAX, 1);

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let live = first.open_profile_window(&sandbox, "script").await;
    let closed = first.open_profile_window(&sandbox, "script").await;
    let shut = first.open_profile_window(&sandbox, "script").await;
    let closed_record = record_of(&sandbox, &closed);
    let shut_record = record_of(&sandbox, &shut);
    let first_boot = boot_of(
        &listed(&mut first, &sandbox, &live).await,
        "the first daemon",
    );
    assert_new_series(
        first_boot,
        "a recorded origin past 2^53 - 1 is out of range",
    );
    first.kill_keeping_windows();
    std::fs::remove_file(
        sandbox
            .state_root()
            .join(yagura_daemon::status_boot::STATUS_BOOT_FILE),
    )
    .expect("the first daemon must leave its boot file in the instance's state root");

    let logs = sandbox.tree.make_dir("fake");
    let mut closed_fake =
        replace_holder(&sandbox, &closed_record, "mute", logs.join("closed.log")).await;
    let mut shut_fake = replace_holder(&sandbox, &shut_record, "mute", logs.join("shut.log")).await;

    let mut next = sandbox.start().await;
    for window_id in [&closed, &shut] {
        let info = listed(&mut next, &sandbox, window_id).await;
        assert_eq!(
            info.status,
            WindowStatus::Unknown,
            "a window being taken back is listed Unknown"
        );
    }
    let next_boot = boot_of(&listed(&mut next, &sandbox, &live).await, "the next daemon");
    assert_new_series(next_boot, "a boot file that is gone");
    assert_ne!(
        next_boot.0, first_boot.0,
        "a boot file that is gone must start a new series, so no reader orders the next \
         daemon's observations after the first one's"
    );
    closed_fake
        .wait_logged("the next daemon's Hello", hello_logged)
        .await;
    shut_fake
        .wait_logged("the next daemon's Hello", hello_logged)
        .await;

    // The live window is taken back once a write reaches its child.
    next.attach(&live).await;
    send(
        &mut next.client,
        &ClientMessage::Text {
            text: "LIVE-MARK".to_owned(),
        },
    )
    .await;
    logged(&log, b"LIVE-MARK", None).await;
    let mut client = ClientLog::new(&sandbox, "live");
    let status = status_event_command(&mut client, OWN_WINDOW, OWN_TOKEN);
    next.start_client_once(&live, &client, std::slice::from_ref(&status))
        .await;
    assert_eq!(
        refusal(client.messages(&status).await),
        None,
        "another window's status event must not wait for the windows still being taken back"
    );

    let mut door = connect(&sandbox).await;
    send(&mut door, &deliver_take(&closed, TOKEN)).await;
    until_held(&sandbox, &closed).await;
    send(
        &mut next.client,
        &ClientMessage::CloseWindow {
            window_id: closed.clone(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut next.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("a window being taken back must close, got {other:?}"),
    }
    closed_fake.wait_gone("close_window").await;
    assert_auth_failed(
        read_response(&mut door).await,
        "a door request to a window closed while it was taken back",
    );

    send(&mut next.client, &ClientMessage::Shutdown).await;
    while !matches!(
        yagura_test_support::ipc::read_daemon_frame(&mut next.client).await,
        yagura_test_support::ipc::ReadOutcome::Closed
    ) {}
    shut_fake.wait_gone("the daemon's shutdown").await;
}

/// A window whose holder refuses the next daemon's `Hello` is left as it is: nothing is
/// sent to the holder after its refusal, the window stays listed `Unknown`, and its
/// holder and child keep running.
#[tokio::test]
async fn a_window_whose_take_back_fails_is_kept_with_its_holder_and_child_alive() {
    let sandbox = Sandbox::new("recover-refused");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);
    // Neither daemon can write its boot file: a directory stands where it goes.
    let boot_file = sandbox
        .state_root()
        .join(yagura_daemon::status_boot::STATUS_BOOT_FILE);
    std::fs::create_dir_all(boot_file.join("taken")).unwrap();

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    let first_boot = boot_of(
        &listed(&mut first, &sandbox, &window_id).await,
        "a daemon that cannot write its boot file",
    );
    let record = record_of(&sandbox, &window_id);
    first.kill_keeping_windows();
    let mut fake = replace_holder(
        &sandbox,
        &record,
        "reject",
        sandbox.tree.path().join("fake.log"),
    )
    .await;

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &window_id).await;
    let lines = fake
        .wait_logged("the refused connection closing", |lines| {
            lines
                .iter()
                .skip_while(|line| !line.starts_with("FRAME Hello"))
                .any(|line| line == "CLOSED")
        })
        .await;
    let after_hello: Vec<&String> = lines
        .iter()
        .skip_while(|line| !line.starts_with("FRAME Hello"))
        .skip(1)
        .take_while(|line| *line != "CLOSED")
        .collect();
    assert!(
        after_hello.is_empty(),
        "the daemon sent the holder {after_hello:?} after it refused the Hello"
    );
    let info = listed(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        info.status,
        WindowStatus::Unknown,
        "a window whose take-back failed stays listed Unknown"
    );
    let next_boot = boot_of(&info, "the next daemon that cannot write its boot file");
    assert!(boot_file.is_dir(), "the fixture's directory must stay");
    assert_new_series(first_boot, "a daemon that cannot write its boot file");
    assert_new_series(next_boot, "the next daemon that cannot write its boot file");
    assert_ne!(
        next_boot.0, first_boot.0,
        "two daemons in a row that cannot write their boot file must each start their own \
         series, so a reader does not order one's observations by the other's"
    );
    let candidates = candidate_names(&mut next).await;
    assert!(
        !candidates.contains(&info.name),
        "a window whose holder runs is not lost, so it is no restore candidate: {candidates:?}"
    );
    assert_ne!(
        row_state(&store_db(&sandbox), &window_id).as_deref(),
        Some("lost"),
        "a window whose holder runs must not be recorded lost"
    );

    next.kill_keeping_windows();
    assert_eq!(
        fake.running(),
        (true, true),
        "a failed take-back must leave the window's holder and child running"
    );
}

/// Waits until the next daemon lists `window_id` as exited; returns its listing.
async fn exited_listing(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
) -> WindowInfo {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let info = listed(daemon, sandbox, window_id).await;
        if info.status == WindowStatus::Exited {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the exited window was never taken back as exited: {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Closes `window_id`, then waits for `holder` to end; returns the holder files left.
async fn close_and_wait_for_holder(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
    holder: &ProcessHandle,
) -> Vec<std::ffi::OsString> {
    send(
        &mut daemon.client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_owned(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while holder.liveness() != Liveness::Dead {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the exited window's holder outlived its close"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    holder_files(sandbox, window_id)
}

/// A window whose child exited while its daemon was connected, left unclosed, is taken
/// back by the next daemon as the exited window it was, with its exit code and last
/// screen, and so is a window whose child exited while no daemon was there, whose holder
/// lives on until the window is closed; closing either then ends its holder and leaves
/// none of its files.
#[tokio::test]
async fn an_exited_window_is_taken_back_with_its_last_screen_and_closes_cleanly() {
    let sandbox = Sandbox::new("recover-exited");
    let child = holder_child();
    let gate = sandbox.tree.path().join("exit-gate");
    sandbox.write_probe_profile("exits", &[&child, "exit", "7"], "");
    sandbox.write_probe_profile("exits-later", &[&child, "exit", "9", utf8(&gate)], "");

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "exits").await;
    let later_id = first.open_profile_window(&sandbox, "exits-later").await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let info = listed(&mut first, &sandbox, &window_id).await;
        if info.status == WindowStatus::Exited {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window never exited: {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let record = record_of(&sandbox, &window_id);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    let later_record = record_of(&sandbox, &later_id);
    let later_holder = handle_of(
        later_record.holder_pid,
        later_record.holder_start.as_deref(),
    );
    let later_child = handle_of(
        later_record.child_pid.expect("the record names the child"),
        later_record.child_start.as_deref(),
    );
    first.kill_keeping_windows();
    std::fs::write(&gate, b"").unwrap();
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while later_child.liveness() != Liveness::Dead {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the gated child never exited while no daemon was there"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut next = sandbox.start().await;
    let info = exited_listing(&mut next, &sandbox, &window_id).await;
    assert_eq!(info.exit_code, Some(7), "the exit code must be kept");
    next.output_until(&window_id, "the child's last screen", |text| {
        text.contains("EXITING 7")
    })
    .await;
    let later = exited_listing(&mut next, &sandbox, &later_id).await;
    assert_eq!(
        later.exit_code,
        Some(9),
        "the exit code of a child that ended while no daemon was there must be kept"
    );
    next.output_until(&later_id, "the last screen of the absence", |text| {
        text.contains("EXITING 9")
    })
    .await;
    assert_eq!(
        later_holder.liveness(),
        Liveness::Alive,
        "the holder of a window whose child exited while no daemon was there must live \
         until the window is closed"
    );

    let left = close_and_wait_for_holder(&mut next, &sandbox, &window_id, &holder).await;
    assert!(left.is_empty(), "closing the exited window left {left:?}");
    let left = close_and_wait_for_holder(&mut next, &sandbox, &later_id, &later_holder).await;
    assert!(
        left.is_empty(),
        "closing the window whose child exited while no daemon was there left {left:?}"
    );
}

/// The files the holders folder keeps for `window_id`.
fn holder_files(sandbox: &Sandbox, window_id: &str) -> Vec<std::ffi::OsString> {
    let prefix = format!("{window_id}.");
    std::fs::read_dir(sandbox.holders_dir())
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&prefix))
        .collect()
}

/// A window whose take-back failed is still the user's to close: closing it ends its
/// holder, and none of its files are left.
#[tokio::test]
async fn closing_a_window_whose_take_back_failed_ends_its_holder_and_leaves_no_files() {
    let sandbox = Sandbox::new("recover-refused-close");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    let record = record_of(&sandbox, &window_id);
    first.kill_keeping_windows();
    let mut fake = replace_holder(
        &sandbox,
        &record,
        "reject",
        sandbox.tree.path().join("fake.log"),
    )
    .await;

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &window_id).await;
    fake.wait_logged("the refused connection closing", |lines| {
        lines
            .iter()
            .skip_while(|line| !line.starts_with("FRAME Hello"))
            .any(|line| line == "CLOSED")
    })
    .await;

    send(
        &mut next.client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut next.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("a window whose take-back failed must close, got {other:?}"),
    }
    fake.wait_gone("the close of the window whose take-back failed")
        .await;
    // The window's exit is published before its holder's files are removed: they go
    // under the holder's endpoint lock, which a stopped watcher may still hold.
    send(
        &mut next.client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Exited,
            timeout_sec: HANG_GUARD.as_secs(),
        },
    )
    .await;
    match read_response(&mut next.client).await {
        DaemonMessage::WaitResult { outcome, .. } if outcome == "reached" => {}
        other => panic!(
            "the closed window never read as exited: {other:?}; daemon log:
{}",
            sandbox.daemon_log()
        ),
    }
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let left = holder_files(&sandbox, &window_id);
        if left.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "closing the window whose take-back failed left {left:?}; daemon log:
{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // On Windows the holder's job ends the child's tree; this stand-in has none.
    #[cfg(unix)]
    {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while fake.running().1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "closing the window whose take-back failed left its recorded child running"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The profile of a window whose hooks reach its CLI only through a registration's entry.
const HOOKED_PROFILE: &str = "hooked";

/// Where the registration keeps its hooks entry, under the daemon's home directory.
const HOOKS_FILE: &str = "hc/hooks.json";

/// A registration whose only owned entry is a hooks file: a launch counts on the hooks
/// only while that file holds the entry.
const REGISTRATION: &str = "\n[mcp_registration]\nadd = [\"mcp\", \"add\", \"{server}\"]\n\
     remove = [\"mcp\", \"remove\", \"{server}\"]\n\n\
     [mcp_registration.status]\nfile = \"hc/mcp.json\"\nformat = \"json\"\n\
     command = \"/mcpServers/{server}/command\"\n\n\
     [mcp_registration.json_entry]\nfile = \"hc/hooks.json\"\n\n\
     [mcp_registration.json_entry.entries]\n\
     \"/hooks/{server}\" = { Stop = [{ type = \"command\", command = \"run-hook\" }] }\n";

/// The name the hand-written launch record of an older holder gives its window.
const OLD_RECORD_NAME: &str = "old-record";

/// The home directory every daemon of the sandbox resolves, on every OS.
fn daemon_home(sandbox: &Sandbox) -> PathBuf {
    sandbox.tree.make_dir("home")
}

/// Writes [`HOOKED_PROFILE`]: the script child logging to `log`, under the control
/// definition so its window can take part in chat, with a `[delivery]` table and
/// [`REGISTRATION`]. Returns the definitions' directory the sandbox's daemons read.
fn write_hooked_profile(sandbox: &Sandbox, log: &Path) -> PathBuf {
    write_delivery_profile(sandbox, HOOKED_PROFILE, log, REGISTRATION)
}

/// [`write_hooked_profile`] for the profile `id`, whose `tables` follow its `[delivery]`
/// table in place of [`REGISTRATION`].
fn write_delivery_profile(sandbox: &Sandbox, id: &str, log: &Path, tables: &str) -> PathBuf {
    let detections = sandbox.tree.make_dir("hooked-detections");
    common::write_control_definition(&detections);
    let argv = [holder_child(), "script".to_owned(), utf8(log).to_owned()]
        .iter()
        .map(|arg| format!("'{arg}'"))
        .collect::<Vec<_>>()
        .join(", ");
    common::write_profile(
        &sandbox.profiles_dir,
        id,
        &format!(
            "schema = 1\ndisplay_name = \"{id}\"\ndetection = \"{}\"\n\n\
             [command]\ndefault = [{argv}]\n\n[delivery]\nwake = \"none\"\n{tables}",
            common::CONTROL_DETECTION_NAME
        ),
    );
    detections
}

/// Writes the hooks entry [`REGISTRATION`] owns for the sandbox's instance, as
/// registering the CLI would.
fn write_registered_hooks(sandbox: &Sandbox) {
    let target =
        yagura_daemon::mcp_registration::RegistrationTarget::for_instance(Some(&sandbox.instance));
    let path = daemon_home(sandbox).join(HOOKS_FILE);
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    let entry = serde_json::json!({
        "hooks": { target.server(): { "Stop": [{ "type": "command", "command": "run-hook" }] } }
    });
    std::fs::write(&path, entry.to_string()).unwrap();
}

/// Starts a daemon of the sandbox whose home is [`daemon_home`], which reads the
/// definitions in `detections`, gives the windows it opens [`TOKEN`] and lets a handoff
/// through without a human.
async fn start_hooked(sandbox: &Sandbox, detections: &Path) -> RunningDaemon {
    start_hooked_with(sandbox, detections, &[]).await
}

/// [`start_hooked`] with `extra` added to the daemon's environment.
async fn start_hooked_with(
    sandbox: &Sandbox,
    detections: &Path,
    extra: &[(&str, &str)],
) -> RunningDaemon {
    let playbook = sandbox
        .tree
        .make_dir("hooked-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]\nauto_approve = [\"open_window\", \"close_window\", \"send_handoff\"]\n",
    )
    .unwrap();
    let home = daemon_home(sandbox);
    let mut env = vec![
        ("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN),
        ("YAGURA_PLAYBOOK", utf8(&playbook)),
        ("YAGURA_DETECTION_DIR", utf8(detections)),
        (
            if cfg!(windows) { "USERPROFILE" } else { "HOME" },
            utf8(&home),
        ),
    ];
    env.extend_from_slice(extra);
    sandbox.start_from(&env).await
}

/// The answer to a handoff sent to `window_id`.
async fn handoff_answer(daemon: &mut RunningDaemon, window_id: &str) -> DaemonMessage {
    send(
        &mut daemon.client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.to_owned()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "window-recovery".to_owned(),
            note: "HOOK-ROUTE-NOTE".to_owned(),
        },
    )
    .await;
    read_response(&mut daemon.client).await
}

/// Asserts a handoff to `window_id` is answered at once as one nothing could deliver.
async fn assert_undeliverable(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
    what: &str,
) {
    match handoff_answer(daemon, window_id).await {
        DaemonMessage::HandoffSent {
            state: DeliveryState::UndeliveredNoDelivery,
            opened: false,
            ..
        } => {}
        other => panic!(
            "{what}: a handoff to a window whose hooks were left out must be answered at once \
             as undelivered_no_delivery, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

/// The ids of the windows the `main` room lists as its participants.
async fn main_roster(daemon: &mut RunningDaemon) -> Vec<String> {
    send(
        &mut daemon.client,
        &ClientMessage::ReadChat {
            room_id: Some("main".to_owned()),
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::ChatMessages { roster, .. } => roster
            .into_iter()
            .map(|participant| participant.window_id)
            .collect(),
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

/// Asserts the `main` room lists `takes_part` and none of `left_out`.
async fn assert_roster(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    takes_part: &[&str],
    left_out: &[&str],
    what: &str,
) {
    let roster = main_roster(daemon).await;
    for window_id in takes_part {
        assert!(
            roster.iter().any(|id| id == window_id),
            "{what}: the window {window_id} whose hooks reach its CLI takes part in chat: \
             {roster:?}; daemon log:\n{}",
            sandbox.daemon_log()
        );
    }
    for window_id in left_out {
        assert!(
            !roster.iter().any(|id| id == window_id),
            "{what}: the window {window_id} whose hooks were left out takes no part in chat: \
             {roster:?}"
        );
    }
}

/// Waits until the window taken back without a row has the name its launch record keeps,
/// so it is the window built from the record's values.
async fn until_named(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str, name: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let mut info = listed(daemon, sandbox, window_id).await;
    while info.name != name {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window never took the name its launch record keeps ({name:?}): {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        info = listed(daemon, sandbox, window_id).await;
    }
}

/// Opens a window of `profile`; returns its id and the name the answer gives it.
async fn open_answered(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    profile: &str,
) -> (String, String) {
    send(
        &mut daemon.client,
        &ClientMessage::OpenWindowFromProfile {
            profile: profile.to_owned(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowOpened {
            window_id,
            name: Some(name),
            ..
        } => (window_id, name),
        other => panic!(
            "expected window_opened with its name, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

/// [`launch_record`] as a holder older than the hook route a record carries hands it
/// back: its window's values name [`HOOKED_PROFILE`], and no route.
fn old_launch_record(window_id: &str, token: &str) -> String {
    let mut record: serde_json::Value = serde_json::from_str(&launch_record(window_id, token))
        .expect("the hand-written launch record is JSON");
    record["window"]["profile"] = HOOKED_PROFILE.into();
    record["window"]["name"] = OLD_RECORD_NAME.into();
    record.to_string()
}

/// A window whose holder runs but whose row is gone (here, with the whole database) is
/// taken back all the same: listed under its id at once, and given its values from the
/// launch record its holder keeps once the holder lets the next daemon in, with its
/// token and the hook route its open settled. A record of an older holder, which names
/// no route, is read as hooks left out. Closing it ends its holder and leaves none of
/// its files.
#[tokio::test]
async fn a_window_without_a_row_is_taken_back_from_its_holders_launch_record() {
    let sandbox = Sandbox::new("recover-rowless");
    let log = sandbox.tree.path().join("input.log");
    let detections = write_hooked_profile(&sandbox, &log);
    write_boot_file(&sandbox, MAX_BOOT_VALUE, MAX_BOOT_VALUE - 2);

    let mut first = start_hooked(&sandbox, &detections).await;
    // Opened before the hooks entry is there, so its hooks are left out.
    let window_id = first.open_profile_window(&sandbox, HOOKED_PROFILE).await;
    write_registered_hooks(&sandbox);
    let registered = first.open_profile_window(&sandbox, HOOKED_PROFILE).await;
    let older = first.open_profile_window(&sandbox, HOOKED_PROFILE).await;
    assert_roster(
        &mut first,
        &sandbox,
        &[&registered],
        &[&window_id],
        "the fixture, as opened",
    )
    .await;
    let first_listed = listed(&mut first, &sandbox, &window_id).await;
    let first_boot = boot_of(&first_listed, "the first daemon");
    assert_eq!(
        first_boot,
        (MAX_BOOT_VALUE, MAX_BOOT_VALUE - 1),
        "a recorded boot within 2^53 - 1 must be followed in its own series"
    );
    let name = first_listed.name;
    let registered_name = listed(&mut first, &sandbox, &registered).await.name;
    let record = record_of(&sandbox, &window_id);
    let older_record = record_of(&sandbox, &older);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    first.kill_keeping_windows();
    for file in ["yagura.db", "yagura.db-wal", "yagura.db-shm"] {
        let _ = std::fs::remove_file(sandbox.data_root().join(file));
    }
    let fakes = sandbox.tree.make_dir("fake");
    let older_launch = fakes.join("older.record");
    std::fs::write(&older_launch, old_launch_record(&older, TOKEN)).unwrap();
    let mut older_fake = replace_holder_with(
        &sandbox,
        &older_record,
        &["gate", utf8(&older_launch)],
        fakes.join("older.log"),
    )
    .await;

    let mut next = start_hooked(&sandbox, &detections).await;
    let info = listed(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        info.status,
        WindowStatus::Unknown,
        "a window taken back starts Unknown"
    );
    // The database went with the row: the boot must not go with it.
    let next_boot = boot_of(&info, "the next daemon");
    assert_later_boot_of_the_same_series(first_boot, next_boot);
    assert_eq!(
        next_boot,
        (MAX_BOOT_VALUE, MAX_BOOT_VALUE),
        "the series must be followed up to its last seq, 2^53 - 1"
    );
    until_named(&mut next, &sandbox, &window_id, &name).await;
    until_named(&mut next, &sandbox, &registered, &registered_name).await;
    listed(&mut next, &sandbox, &older).await;
    older_fake
        .wait_logged("the next daemon's Hello", hello_logged)
        .await;
    older_fake.answer(&older_fake.welcome_own_child());
    until_named(&mut next, &sandbox, &older, OLD_RECORD_NAME).await;
    assert_undeliverable(
        &mut next,
        &sandbox,
        &window_id,
        "taken back from its launch record",
    )
    .await;
    assert_undeliverable(
        &mut next,
        &sandbox,
        &older,
        "taken back from an older holder's launch record",
    )
    .await;
    assert_roster(
        &mut next,
        &sandbox,
        &[&registered],
        &[&window_id, &older],
        "taken back from launch records",
    )
    .await;
    let mut client = ClientLog::new(&sandbox, "rowless");
    let status = status_event_command(&mut client, OWN_WINDOW, OWN_TOKEN);
    next.start_client_once(&window_id, &client, std::slice::from_ref(&status))
        .await;
    assert_eq!(
        refusal(client.messages(&status).await),
        None,
        "the window taken back from its launch record accepts the token it had"
    );

    send(
        &mut next.client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut next.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while holder.liveness() != Liveness::Dead {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's holder outlived its close"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let left = holder_files(&sandbox, &window_id);
    assert!(left.is_empty(), "closing the window left {left:?}");
}

/// A window taken back with its row keeps the hook route its open settled, not the one
/// its profile plans or its registration holds by then: one opened while the hooks entry
/// was not there is still answered at once as one nothing reaches and takes no part in
/// chat, though the entry is there when the next daemon takes it back, and one opened
/// with the entry still takes part. So do one whose profile arranges no hooks and one
/// whose launch injects them: every route that reaches a CLI is kept.
#[tokio::test]
async fn a_window_taken_back_keeps_the_hook_route_its_open_settled() {
    const UNARRANGED_PROFILE: &str = "unarranged";
    const INJECTED_PROFILE: &str = "injected";
    let sandbox = Sandbox::new("recover-hooks");
    let log = sandbox.tree.path().join("input.log");
    let detections = write_hooked_profile(&sandbox, &log);
    write_delivery_profile(&sandbox, UNARRANGED_PROFILE, &log, "");
    // The injected arguments land where the script child takes a gate file and its
    // bytes: a gate that never exists, and bytes it never writes.
    let never = sandbox.tree.path().join("never-gate");
    write_delivery_profile(
        &sandbox,
        INJECTED_PROFILE,
        &log,
        &format!("\n[hooks]\nargs = ['{}', '00']\n", utf8(&never)),
    );

    let mut first = start_hooked(&sandbox, &detections).await;
    let (left_out, left_out_name) = open_answered(&mut first, &sandbox, HOOKED_PROFILE).await;
    write_registered_hooks(&sandbox);
    let (registered, registered_name) = open_answered(&mut first, &sandbox, HOOKED_PROFILE).await;
    let (unarranged, unarranged_name) =
        open_answered(&mut first, &sandbox, UNARRANGED_PROFILE).await;
    let (injected, injected_name) = open_answered(&mut first, &sandbox, INJECTED_PROFILE).await;
    let reach = [registered.as_str(), unarranged.as_str(), injected.as_str()];
    assert_undeliverable(&mut first, &sandbox, &left_out, "the fixture, as opened").await;
    assert_roster(
        &mut first,
        &sandbox,
        &reach,
        &[&left_out],
        "the fixture, as opened",
    )
    .await;
    first.kill_keeping_windows();

    let mut next = start_hooked(&sandbox, &detections).await;
    // A window whose row the killed daemon left without its route is listed under its
    // id until its launch record rebuilds it; the name its open gave marks the rebuilt one.
    for (window_id, name) in [
        (&left_out, &left_out_name),
        (&registered, &registered_name),
        (&unarranged, &unarranged_name),
        (&injected, &injected_name),
    ] {
        until_named(&mut next, &sandbox, window_id, name).await;
    }
    assert_undeliverable(&mut next, &sandbox, &left_out, "taken back with its row").await;
    assert_roster(
        &mut next,
        &sandbox,
        &reach,
        &[&left_out],
        "taken back with their rows",
    )
    .await;
}

/// The window's row holds the hook route its open settled from the moment its holder
/// runs: a daemon that ends once the holder has started the child, before the window is
/// registered, leaves a row the next daemon builds the whole window from. So the window
/// is listed from the first time under the name its open asked for, and takes part in
/// chat as one whose launch injects its hooks does.
#[tokio::test]
async fn a_window_whose_daemon_ended_once_its_holder_started_is_taken_back_from_its_row() {
    const INJECTED_PROFILE: &str = "injected";
    const NAME: &str = "opened-row";
    let sandbox = Sandbox::new("recover-openrow");
    let log = sandbox.tree.path().join("input.log");
    let detections = write_hooked_profile(&sandbox, &log);
    // As in `a_window_taken_back_keeps_the_hook_route_its_open_settled`.
    let never = sandbox.tree.path().join("never-gate");
    write_delivery_profile(
        &sandbox,
        INJECTED_PROFILE,
        &log,
        &format!("\n[hooks]\nargs = ['{}', '00']\n", utf8(&never)),
    );

    let point = yagura_daemon::fault_point::FaultPoint::WindowHolderStarted;
    let mut first = start_hooked_with(
        &sandbox,
        &detections,
        &[(yagura_daemon::fault_point::POINT_ENV, point.name())],
    )
    .await;
    send(
        &mut first.client,
        &ClientMessage::OpenWindowFromProfile {
            profile: INJECTED_PROFILE.to_owned(),
            cwd: None,
            name: Some(NAME.to_owned()),
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    first.wait_for_fault_end().await;

    let (window_id, stored_hooks): (String, Option<String>) = {
        let conn = store::open_readonly(&store_db(&sandbox)).unwrap();
        conn.query_row(
            "SELECT window_id, hooks FROM windows WHERE name = ?1",
            [NAME],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or_else(|error| {
            panic!(
                "the open reserves the window's row before its holder starts: {error}; \
                 daemon log:\n{}",
                sandbox.daemon_log()
            )
        })
    };
    let record = record_of(&sandbox, &window_id);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    assert_ne!(
        holder.liveness(),
        Liveness::Dead,
        "the window's holder outlives the daemon that ended at {}",
        point.name()
    );

    let mut next = start_hooked(&sandbox, &detections).await;
    let info = listed(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        info.name,
        NAME,
        "the next daemon lists the window from the first time under the name its open \
         asked for, built from its row; daemon log:\n{}",
        sandbox.daemon_log()
    );
    assert_roster(
        &mut next,
        &sandbox,
        &[&window_id],
        &[],
        "listed first, built from its row",
    )
    .await;
    assert_eq!(
        stored_hooks.as_deref(),
        Some("injection"),
        "the window's row holds the hook route its open settled once its holder runs"
    );
}

/// How many launch records the holders folder keeps.
fn launch_records(sandbox: &Sandbox) -> usize {
    std::fs::read_dir(sandbox.holders_dir())
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        })
        .count()
}

/// A window whose row cannot take its values is refused before its holder starts: the
/// open leaves no holder's launch record behind. Once the row takes values again, the
/// same open starts a holder whose record is kept there.
#[tokio::test]
async fn a_window_whose_row_cannot_be_written_is_refused_before_its_holder_starts() {
    let sandbox = Sandbox::new("recover-row-unwritable");
    let signal = sandbox.tree.path().join("row-update-fails");
    std::fs::write(&signal, b"").unwrap();
    let mut daemon = sandbox
        .start_at_fault(
            yagura_daemon::fault_point::FaultPoint::WindowRowUpdateFails,
            Some(&signal),
        )
        .await;
    let open = ClientMessage::OpenWindow {
        command: bins::echo_child_command(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        on_name_taken: Default::default(),
    };

    send(&mut daemon.client, &open).await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::Error { code, .. } if code == "spawn_failed" => {}
        other => panic!(
            "a window whose row cannot be written must be refused as spawn_failed, got \
             {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
    assert_eq!(
        launch_records(&sandbox),
        0,
        "the refused open must start no holder, yet the holders folder keeps a launch record"
    );

    std::fs::remove_file(&signal).unwrap();
    send(&mut daemon.client, &open).await;
    let window_id = match read_response(&mut daemon.client).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!(
            "once its row takes values the window must open, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    };
    record_of(&sandbox, &window_id);
    assert_eq!(
        launch_records(&sandbox),
        1,
        "the holders folder keeps the record of the one holder started"
    );
}

/// Has `child`, a window running `window_client serve` for `log`, report `summary` to the
/// window that opened it; returns the window the report was delivered to.
async fn report_from(
    daemon: &mut RunningDaemon,
    log: &mut ClientLog,
    child: &str,
    summary: &str,
) -> Option<String> {
    let conn = log.new_conn();
    let identify = log.exchange(&conn, &identify_message(child, None), ClientRead::One);
    let report = log.exchange(
        &conn,
        &ClientMessage::SendReport {
            outcome: yagura_protocol::ReportOutcome::Succeeded,
            summary: summary.to_owned(),
            path: None,
        },
        ClientRead::One,
    );
    daemon.type_command(child, &identify).await;
    daemon.type_command(child, &report).await;
    expect_identified(log.message(&identify).await, child);
    match log.message(&report).await {
        DaemonMessage::ReportSent { delivered_to, .. } => delivered_to,
        other => panic!("expected report_sent, got {other:?}"),
    }
}

fn expect_identified(answer: DaemonMessage, window_id: &str) {
    match answer {
        DaemonMessage::Identified { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected identified, got {other:?}"),
    }
}

/// The commands that, from the window `opener` (declaring `profile` the way the bridge of
/// a window opened from it does), open a window running `window_client serve` for
/// `opened`.
fn open_as(
    log: &mut ClientLog,
    opener: &str,
    profile: Option<&str>,
    opened: &ClientLog,
) -> [ClientCommand; 2] {
    let conn = log.new_conn();
    let identify = log.exchange(&conn, &identify_message(opener, profile), ClientRead::One);
    let open = log.exchange(
        &conn,
        &ClientMessage::OpenWindow {
            command: bins::window_client_command(opened.path()),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
        ClientRead::One,
    );
    [identify, open]
}

/// The id of the window the commands of [`open_as`] opened, once they were answered.
async fn opened_window(log: &mut ClientLog, commands: &[ClientCommand; 2], opener: &str) -> String {
    expect_identified(log.message(&commands[0]).await, opener);
    match log.message(&commands[1]).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    }
}

/// The `(seq, summary)` of each report the store keeps for `window_id`, by place.
fn kept_reports(db: &Path, window_id: &str) -> Vec<(i64, String)> {
    let conn = store::open_readonly(db).expect("open the store");
    let mut statement = conn
        .prepare("SELECT seq, report_json FROM report_parcels WHERE window_id = ?1 ORDER BY seq")
        .unwrap();
    statement
        .query_map([window_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (seq, json) = row.unwrap();
            let report: serde_json::Value = serde_json::from_str(&json).unwrap();
            (
                seq,
                report["summary"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// The text a hook door takes from `window_id`'s inbox now, run by a client started in the
/// window; it says it printed once it has, which ends what it took.
async fn door_output(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    log: &mut ClientLog,
    window_id: &str,
) -> String {
    let conn = log.new_conn();
    let take = log.exchange(&conn, &deliver_take(window_id, TOKEN), ClientRead::Delivery);
    daemon
        .start_client_once(window_id, log, std::slice::from_ref(&take))
        .await;
    match log.message(&take).await {
        DaemonMessage::Delivery { output, .. } => output,
        other => panic!(
            "expected delivery, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

/// A window rebuilt from its launch record, over the placeholder it was taken back as,
/// numbers its reports on from the places the store kept, even once another window is
/// numbered for the first time; its notice of what was kept comes once, before that of
/// the next stretch of reports.
#[tokio::test]
async fn a_window_rebuilt_from_its_launch_record_numbers_its_reports_on_and_is_noticed_once() {
    const NOTICE: &str = "[yagura report]";
    let sandbox = Sandbox::new("recover-report-seq");
    let log = sandbox.tree.path().join("input.log");
    let detections = write_hooked_profile(&sandbox, &log);
    write_registered_hooks(&sandbox);

    let mut first = start_hooked(&sandbox, &detections).await;
    let parent = first.open_profile_window(&sandbox, HOOKED_PROFILE).await;
    let parent_name = listed(&mut first, &sandbox, &parent).await.name;
    // The parent's script child starts the client that opens and names the others; the
    // windows it opens run the client themselves.
    let mut parent_log = ClientLog::new(&sandbox, "parent");
    let mut child_log = ClientLog::new(&sandbox, "child");
    let opening = open_as(&mut parent_log, &parent, Some(HOOKED_PROFILE), &child_log);
    first
        .start_client_once(&parent, &parent_log, &opening)
        .await;
    let child = opened_window(&mut parent_log, &opening, &parent).await;
    assert_eq!(
        report_from(&mut first, &mut child_log, &child, "KEPT-REPORT")
            .await
            .as_deref(),
        Some(parent.as_str())
    );
    first.kill_keeping_windows();
    // The row goes, and its trigger with the reports it kept; they are kept again,
    // which only the window its launch record's values build reads.
    {
        let db = rusqlite::Connection::open(store_db(&sandbox)).unwrap();
        let mut statement = db
            .prepare("SELECT seq, report_json FROM report_parcels WHERE window_id = ?1")
            .unwrap();
        let kept: Vec<(i64, String)> = statement
            .query_map([&parent], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        drop(statement);
        assert_eq!(kept.len(), 1, "the fixture keeps the parent's one report");
        db.execute("DELETE FROM windows WHERE window_id = ?1", [&parent])
            .unwrap();
        for (seq, json) in &kept {
            db.execute(
                "INSERT INTO report_parcels (window_id, seq, report_json) VALUES (?1, ?2, ?3)",
                rusqlite::params![parent, seq, json],
            )
            .unwrap();
        }
    }

    let mut next = start_hooked(&sandbox, &detections).await;
    until_named(&mut next, &sandbox, &parent, &parent_name).await;
    let kept_notice = door_output(&mut next, &sandbox, &mut parent_log, &parent).await;
    assert_eq!(
        count(kept_notice.as_bytes(), NOTICE.as_bytes()),
        1,
        "the rebuilt window is noticed once of the report kept for it: {kept_notice:?}"
    );

    // Another window numbered for the first time.
    let mut other_parent_log = ClientLog::new(&sandbox, "other-parent");
    let mut other_child_log = ClientLog::new(&sandbox, "other-child");
    let other_parent = common::call_open_window(
        &mut next.client,
        bins::window_client_command(other_parent_log.path()),
    )
    .await;
    let opening = open_as(&mut other_parent_log, &other_parent, None, &other_child_log);
    for command in &opening {
        next.type_command(&other_parent, command).await;
    }
    let other_child = opened_window(&mut other_parent_log, &opening, &other_parent).await;
    assert_eq!(
        report_from(
            &mut next,
            &mut other_child_log,
            &other_child,
            "OTHER-REPORT"
        )
        .await
        .as_deref(),
        Some(other_parent.as_str())
    );
    assert_eq!(
        report_from(&mut next, &mut child_log, &child, "NEXT-REPORT")
            .await
            .as_deref(),
        Some(parent.as_str())
    );
    let kept = kept_reports(&store_db(&sandbox), &parent);
    let summaries: Vec<&str> = kept.iter().map(|(_, summary)| summary.as_str()).collect();
    assert_eq!(
        summaries,
        ["KEPT-REPORT", "NEXT-REPORT"],
        "the next report takes a place of its own behind the kept one: {kept:?}"
    );

    let conn = parent_log.new_conn();
    let identify = parent_log.exchange(
        &conn,
        &identify_message(&parent, Some(HOOKED_PROFILE)),
        ClientRead::One,
    );
    let wait = parent_log.exchange(
        &conn,
        &ClientMessage::WaitForReport { timeout_sec: 0 },
        ClientRead::One,
    );
    let waiting = [identify, wait];
    next.start_client_once(&parent, &parent_log, &waiting).await;
    expect_identified(parent_log.message(&waiting[0]).await, &parent);
    match parent_log.message(&waiting[1]).await {
        DaemonMessage::Reports { reports, .. } => assert_eq!(
            reports
                .iter()
                .map(|report| report.summary.as_str())
                .collect::<Vec<_>>(),
            ["KEPT-REPORT", "NEXT-REPORT"]
        ),
        other => panic!("expected reports, got {other:?}"),
    }
    // The next stretch's notice bounds what came of the first.
    assert_eq!(
        report_from(&mut next, &mut child_log, &child, "LATER-REPORT")
            .await
            .as_deref(),
        Some(parent.as_str())
    );
    let later_notice = door_output(&mut next, &sandbox, &mut parent_log, &parent).await;
    assert_eq!(
        count(later_notice.as_bytes(), NOTICE.as_bytes()),
        1,
        "only the next stretch's notice follows the first: {later_notice:?}"
    );
    assert!(
        later_notice.contains("1 pending report(s)"),
        "the notice that follows is the next stretch's: {later_notice:?}"
    );
}

/// The session store of the sandbox's instance.
fn store_db(sandbox: &Sandbox) -> PathBuf {
    sandbox.data_root().join("yagura.db")
}

/// The state the window table at `db` records for `window_id`; `None` without a row.
fn row_state(db: &Path, window_id: &str) -> Option<String> {
    let conn = store::open_readonly(db).ok()?;
    conn.query_row(
        "SELECT state FROM windows WHERE window_id = ?1",
        [window_id],
        |row| row.get(0),
    )
    .ok()
}

/// The names of the window table's rows at `db` in `state`, in the order they opened.
fn names_in_state(db: &Path, state: &str) -> Vec<String> {
    let Ok(conn) = store::open_readonly(db) else {
        return Vec::new();
    };
    let Ok(mut statement) =
        conn.prepare("SELECT name FROM windows WHERE state = ?1 ORDER BY opened_seq")
    else {
        return Vec::new();
    };
    statement
        .query_map([state], |row| row.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

async fn candidate_names(daemon: &mut RunningDaemon) -> Vec<String> {
    send(&mut daemon.client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::RestoreCandidateList { windows, .. } => windows
            .into_iter()
            .map(|candidate| candidate.name)
            .collect(),
        other => panic!("expected restore_candidate_list, got {other:?}"),
    }
}

/// Polls `check` until it reports no problem; fails with every problem it last reported
/// once [`HANG_GUARD`] has passed.
macro_rules! settle {
    ($sandbox:expr, $what:expr, $check:block) => {{
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        loop {
            let problems: Vec<String> = $check;
            if problems.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}:\n- {}\ndaemon log:\n{}",
                $what,
                problems.join("\n- "),
                $sandbox.daemon_log()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

/// Removes, as a test ends, the endpoints (Unix) of holders it killed itself, which a
/// killed holder cannot remove and the next daemon may have left.
struct KilledEndpoints {
    holders_dir: PathBuf,
    instance: String,
    window_ids: Vec<String>,
}

impl KilledEndpoints {
    fn new(sandbox: &Sandbox, window_ids: &[&str]) -> Self {
        Self {
            holders_dir: sandbox.holders_dir(),
            instance: sandbox.instance.clone(),
            window_ids: window_ids.iter().map(|id| (*id).to_owned()).collect(),
        }
    }
}

impl Drop for KilledEndpoints {
    fn drop(&mut self) {
        for window_id in &self.window_ids {
            yagura_test_support::process::remove_killed_holder_endpoint(
                &self.holders_dir,
                Some(OsStr::new(&self.instance)),
                window_id,
            );
        }
    }
}

/// Ends a process the test left running, as the test ends.
struct EndOnDrop(Option<ProcessHandle>);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        if let Some(process) = self.0.take() {
            let _ = process.kill();
        }
    }
}

/// Ends the window's holder and its watcher, the holder stopped first, so that neither
/// ends the window's tree or removes its files: what is left is the next daemon's.
fn end_holder_and_watcher(record: &HolderRecord) {
    assert_eq!(
        yagura_test_support::process::end_holder_and_watcher(record),
        Some(record.holder_pid),
        "the window's holder must still run once its daemon was killed"
    );
}

/// Kills the process `handle` names and waits until it has ended.
async fn kill_and_wait(handle: &ProcessHandle, what: &str) {
    handle.kill().unwrap();
    wait_dead(handle, what).await;
}

async fn wait_dead(handle: &ProcessHandle, what: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while handle.liveness() != Liveness::Dead {
        assert!(tokio::time::Instant::now() < deadline, "{what} never ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A window whose holder was ended from outside while no daemon was there is found gone
/// by the next daemon from its record's pid and start: it is not taken back but becomes
/// `lost`, a restore candidate, and its holder's files are removed. A window taken back is
/// no candidate. On Windows the holder's job had ended the child's whole tree.
#[tokio::test]
async fn a_window_whose_holder_died_while_no_daemon_was_there_is_lost_and_the_only_candidate() {
    let sandbox = Sandbox::new("reconcile-lost");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);
    let pid_file = sandbox.tree.path().join("grandchild.pid");
    sandbox.write_probe_profile("tree", &[&holder_child(), "tree", utf8(&pid_file)], "");

    let mut first = sandbox.start().await;
    let kept = first.open_profile_window(&sandbox, "script").await;
    let lost = first.open_profile_window(&sandbox, "tree").await;
    first
        .output_until(&lost, "the tree's marker", |text| {
            text.contains("HOLDER-CHILD-TREE")
        })
        .await;
    let kept_name = listed(&mut first, &sandbox, &kept).await.name;
    let lost_name = listed(&mut first, &sandbox, &lost).await.name;
    let record = record_of(&sandbox, &lost);
    let _endpoints = KilledEndpoints::new(&sandbox, &[&lost]);
    let child = handle_of(
        record.child_pid.expect("the record names the child"),
        record.child_start.as_deref(),
    );
    let grandchild_pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let grandchild = EndOnDrop(Some(handle_of(
        grandchild_pid,
        Some(&start_of(grandchild_pid)),
    )));
    first.kill_keeping_windows();
    end_holder_and_watcher(&record);
    if cfg!(windows) {
        wait_dead(&child, "the child of the killed holder's job").await;
        wait_dead(
            grandchild.0.as_ref().unwrap(),
            "the grandchild of the killed holder's job",
        )
        .await;
    }

    let mut next = sandbox.start().await;
    let db = store_db(&sandbox);
    settle!(sandbox, "the window whose holder died is not lost", {
        let mut problems = Vec::new();
        let ids: Vec<String> = list_windows(&mut next)
            .await
            .into_iter()
            .map(|info| info.window_id)
            .collect();
        if !ids.contains(&kept) {
            problems.push(format!("the live window {kept} is not listed: {ids:?}"));
        }
        if ids.contains(&lost) {
            problems.push(format!("the window whose holder died is listed: {ids:?}"));
        }
        let names = candidate_names(&mut next).await;
        if !names.contains(&lost_name) {
            problems.push(format!(
                "the window whose holder died ({lost_name}) is no candidate: {names:?}"
            ));
        }
        if names.contains(&kept_name) {
            problems.push(format!(
                "the window taken back ({kept_name}) is a candidate: {names:?}"
            ));
        }
        let state = row_state(&db, &lost);
        if state.as_deref() != Some("lost") {
            problems.push(format!(
                "the dead holder's window row is {state:?}, not lost"
            ));
        }
        let left = holder_files(&sandbox, &lost);
        if !left.is_empty() {
            problems.push(format!("the dead holder's files are left: {left:?}"));
        }
        problems
    });
}

/// What the start sweeps away is decided by the windows that stay: the credentials of a
/// window taken back and of one whose holder runs but does not let the daemon in are kept,
/// and so are the plan owners, task bindings and pending chat deliveries of a window taken
/// back and of a lost one. What names no such window is cleared as before.
#[tokio::test]
async fn the_start_keeps_what_belongs_to_windows_that_stay_and_clears_the_rest() {
    const STRAY: &str = "wstray001";
    let sandbox = Sandbox::new("reconcile-sweep");
    // `hex` reads no argument, so the credentials file named on its command line is inert.
    sandbox.write_probe_profile(
        "launch",
        &[&holder_child(), "hex"],
        "\n[credentials]\nmode = \"launch\"\n\n[hooks]\nargs = [\"--creds\", \"{credentials_file}\"]\n",
    );
    let mcp_dir = sandbox.state_root().join("mcp");
    let folders = || -> BTreeSet<PathBuf> {
        std::fs::read_dir(&mcp_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("credentials-")
                    && entry.file_type().is_ok_and(|kind| kind.is_dir())
            })
            .map(|entry| entry.path())
            .collect()
    };

    let mut first = sandbox.start().await;
    let mut opened = Vec::new();
    for _ in 0..3 {
        let before = folders();
        let window_id = first.open_profile_window(&sandbox, "launch").await;
        let added: Vec<PathBuf> = folders().difference(&before).cloned().collect();
        assert_eq!(
            added.len(),
            1,
            "a launch window must have one credentials folder of its own: {added:?}; daemon \
             log:\n{}",
            sandbox.daemon_log()
        );
        opened.push((window_id, added[0].clone()));
    }
    let [
        (taken, taken_folder),
        (mute, mute_folder),
        (lost, lost_folder),
    ] = <[(String, PathBuf); 3]>::try_from(opened).unwrap();
    let mute_record = record_of(&sandbox, &mute);
    let lost_record = record_of(&sandbox, &lost);
    let _endpoints = KilledEndpoints::new(&sandbox, &[&lost]);
    first.kill_keeping_windows();
    let mut mute_fake = replace_holder(
        &sandbox,
        &mute_record,
        "reject",
        sandbox.tree.path().join("fake.log"),
    )
    .await;
    end_holder_and_watcher(&lost_record);

    let stray_folder = mcp_dir.join(format!("credentials-{}", "0".repeat(32)));
    std::fs::create_dir_all(&stray_folder).unwrap();
    std::fs::write(
        stray_folder.join("credentials.json"),
        format!("{{\"window_id\":\"{STRAY}\",\"token\":\"{OTHER_TOKEN}\"}}"),
    )
    .unwrap();
    let db = store_db(&sandbox);
    let owners = [
        ("plan-taken", taken.as_str()),
        ("plan-lost", lost.as_str()),
        ("plan-stray", STRAY),
    ];
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for (seq, (plan_id, owner)) in (1i64..).zip(owners) {
            conn.execute(
                "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
                 VALUES (?1, ?1, 'C:/work', ?2, 0, 0)",
                rusqlite::params![plan_id, owner],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plan_task (plan_id, task_id, seq, title, window_id) \
                 VALUES (?1, 'task', 0, 'task', ?2)",
                rusqlite::params![plan_id, owner],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_message (room_id, seq, at_ms, from_kind, from_name, body, \
                 mentions_json) VALUES ('main', ?1, 0, 'user', 'user', 'body', '[]')",
                [seq],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms) \
                 VALUES ('main', ?1, ?2, 'pending', 0)",
                rusqlite::params![seq, owner],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO chat_seq (room_id, last_seq) VALUES ('main', 3) \
             ON CONFLICT(room_id) DO UPDATE SET last_seq = 3",
            [],
        )
        .unwrap();
    }

    let _next = sandbox.start().await;
    settle!(sandbox, "the start's sweep did not keep what stays", {
        let mut problems = Vec::new();
        for (folder, kept, what) in [
            (&taken_folder, true, "the window taken back"),
            (
                &mute_folder,
                true,
                "the window that does not let the daemon in",
            ),
            (&lost_folder, false, "the lost window"),
            (&stray_folder, false, "no window"),
        ] {
            if folder.exists() != kept {
                problems.push(format!(
                    "the credentials folder of {what} must {}: {}",
                    if kept { "stay" } else { "be removed" },
                    folder.display()
                ));
            }
        }
        let conn = store::open_readonly(&db).unwrap();
        for (plan_id, owner) in owners {
            let expected = (owner != STRAY).then(|| owner.to_owned());
            let (plan_owner, task_window): (Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT plan.owner_window, plan_task.window_id FROM plan \
                     JOIN plan_task ON plan_task.plan_id = plan.plan_id WHERE plan.plan_id = ?1",
                    [plan_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            if plan_owner != expected || task_window != expected {
                problems.push(format!(
                    "{plan_id}: owner {plan_owner:?} and task binding {task_window:?}, \
                     expected {expected:?}"
                ));
            }
            let state: String = conn
                .query_row(
                    "SELECT state FROM chat_delivery WHERE window_id = ?1",
                    [owner],
                    |row| row.get(0),
                )
                .unwrap();
            let expected = if owner == STRAY {
                "undelivered_restart"
            } else {
                "pending"
            };
            if state != expected {
                problems.push(format!(
                    "the delivery to {owner} is {state}, expected {expected}"
                ));
            }
        }
        problems
    });

    // A credentials folder lives as long as its window: when the holder of the window that
    // was kept without a connection ends, the folder goes with the window.
    let _ = mute_fake.holder.kill();
    mute_fake.wait_gone("its kill").await;
    settle!(sandbox, "the credentials folder outlived its window", {
        let mut problems = Vec::new();
        if mute_folder.exists() {
            problems.push(format!(
                "the credentials folder of the window whose holder ended stayed: {}",
                mute_folder.display()
            ));
        }
        if !taken_folder.exists() {
            problems.push(format!(
                "the credentials folder of the window still running went: {}",
                taken_folder.display()
            ));
        }
        problems
    });
}

/// Records without a row (as an earlier version leaves them) whose holder and child are
/// gone are removed with the holder's other files, whether or not they carry the
/// processes' starts, and are no restore candidates.
#[tokio::test]
async fn records_of_dead_holders_without_rows_are_removed_and_are_no_candidates() {
    let sandbox = Sandbox::new("reconcile-rowless");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut first = sandbox.start().await;
    let with_starts = first.open_profile_window(&sandbox, "script").await;
    let without_starts = first.open_profile_window(&sandbox, "script").await;
    let mut names = Vec::new();
    for window_id in [&with_starts, &without_starts] {
        names.push(listed(&mut first, &sandbox, window_id).await.name);
    }
    let records = [
        record_of(&sandbox, &with_starts),
        record_of(&sandbox, &without_starts),
    ];
    let _endpoints = KilledEndpoints::new(&sandbox, &[&with_starts, &without_starts]);
    first.kill_keeping_windows();
    for record in &records {
        let child = handle_of(record.child_pid.unwrap(), record.child_start.as_deref());
        // A watcher would end the tree once the child ends, so it goes first;
        // without one, the child goes before its holder takes it down.
        if record.watcher_pid.is_none() {
            kill_and_wait(&child, "the killed child").await;
        }
        end_holder_and_watcher(record);
        if record.watcher_pid.is_some() {
            kill_and_wait(&child, "the killed child").await;
        }
    }
    let earlier = HolderRecord {
        holder_start: None,
        child_start: None,
        ..records[1].clone()
    };
    std::fs::write(
        holder_record_path(&sandbox.holders_dir(), &without_starts).unwrap(),
        encode_holder_record(&earlier),
    )
    .unwrap();
    let db = store_db(&sandbox);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for window_id in [&with_starts, &without_starts] {
            conn.execute("DELETE FROM windows WHERE window_id = ?1", [window_id])
                .unwrap();
        }
    }

    let mut next = sandbox.start().await;
    settle!(sandbox, "the dead holders' records are not cleared", {
        let mut problems = Vec::new();
        let candidates = candidate_names(&mut next).await;
        let ids: Vec<String> = list_windows(&mut next)
            .await
            .into_iter()
            .map(|info| info.window_id)
            .collect();
        for (window_id, name) in [&with_starts, &without_starts].into_iter().zip(&names) {
            let left = holder_files(&sandbox, window_id);
            if !left.is_empty() {
                problems.push(format!("{window_id}'s files are left: {left:?}"));
            }
            if candidates.contains(name) {
                problems.push(format!(
                    "{window_id} ({name}) is a candidate: {candidates:?}"
                ));
            }
            if ids.contains(window_id) {
                problems.push(format!("{window_id} is listed: {ids:?}"));
            }
            if let Some(state) = row_state(&db, window_id) {
                problems.push(format!("{window_id} has a row again: {state}"));
            }
        }
        problems
    });
}

/// On Unix, a lost window's child that outlived its killed holder and watcher (it ignores
/// the terminal's hangup) and is still the process its record names is ended with its
/// whole process group before the window becomes `lost`. The daemon is killed, then the
/// holder stopped, the watcher killed and seen to end, and the holder killed: neither of
/// them ends the tree, so only the next daemon can.
#[cfg(unix)]
#[tokio::test]
async fn a_lost_windows_child_that_outlived_its_holder_is_ended_with_its_group() {
    let sandbox = Sandbox::new("reconcile-group");
    sandbox.write_probe_profile("stubborn", &[&holder_child(), "ignore-end"], "");

    // On Linux a child's scope bound to its holder's would end the child with the holder,
    // so the window opens where no user bus is reached and its child gets no scope.
    #[cfg(target_os = "linux")]
    let no_bus = format!("unix:path={}", sandbox.tree.path().join("no-bus").display());
    #[cfg(target_os = "linux")]
    let launcher_env = [("DBUS_SESSION_BUS_ADDRESS", no_bus.as_str())];
    #[cfg(not(target_os = "linux"))]
    let launcher_env: [(&str, &str); 0] = [];
    let mut first = sandbox.start_from(&launcher_env).await;
    let window_id = first.open_profile_window(&sandbox, "stubborn").await;
    first
        .output_until(&window_id, "the child ignoring every end", |text| {
            text.contains("HOLDER-CHILD-IGNORING")
        })
        .await;
    let record = record_of(&sandbox, &window_id);
    let _endpoints = KilledEndpoints::new(&sandbox, &[&window_id]);
    let child = EndOnDrop(Some(handle_of(
        record.child_pid.expect("the record names the child"),
        record.child_start.as_deref(),
    )));
    first.kill_keeping_windows();
    end_holder_and_watcher(&record);
    assert_eq!(
        child.0.as_ref().unwrap().liveness(),
        Liveness::Alive,
        "the fixture's child must outlive its killed holder and watcher"
    );

    let _next = sandbox.start().await;
    let db = store_db(&sandbox);
    settle!(
        sandbox,
        "the child that outlived its holder was not ended",
        {
            let mut problems = Vec::new();
            if child.0.as_ref().unwrap().liveness() != Liveness::Dead {
                problems.push("the child that outlived its holder still runs".to_owned());
            }
            let state = row_state(&db, &window_id);
            if state.as_deref() != Some("lost") {
                problems.push(format!("the window's row is {state:?}, not lost"));
            }
            if !holder_files(&sandbox, &window_id).is_empty() {
                problems.push("the dead holder's files are left".to_owned());
            }
            problems
        }
    );
}

/// On Unix, when the child id a dead holder's record names now belongs to another process
/// (its start differs), that process and its group are left running; the window still
/// becomes `lost`. The watcher has ended before the record is rewritten, and the
/// rewritten record names none, so the next daemon reaches the child only by the record.
#[cfg(unix)]
#[tokio::test]
async fn a_lost_windows_child_id_taken_by_another_process_is_left_running() {
    let sandbox = Sandbox::new("reconcile-reused");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);
    let mut other = spawn_term_script(&["idle"]);

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    let record = record_of(&sandbox, &window_id);
    first.kill_keeping_windows();
    let _ = yagura_test_support::process::end_recorded_holder(
        &sandbox.holders_dir(),
        Some(OsStr::new(&sandbox.instance)),
        &record,
    );
    let reused = HolderRecord {
        child_pid: Some(other.id()),
        watcher_pid: None,
        watcher_start: None,
        ..record.clone()
    };
    std::fs::write(
        holder_record_path(&sandbox.holders_dir(), &window_id).unwrap(),
        encode_holder_record(&reused),
    )
    .unwrap();

    let _next = sandbox.start().await;
    let db = store_db(&sandbox);
    settle!(
        sandbox,
        "the window whose child id was taken is not lost",
        {
            let state = row_state(&db, &window_id);
            if state.as_deref() == Some("lost") {
                Vec::new()
            } else {
                vec![format!("the window's row is {state:?}, not lost")]
            }
        }
    );
    let running = other.try_wait().unwrap().is_none();
    let _ = other.kill();
    let _ = other.wait();
    assert!(
        running,
        "a process that took over the recorded child's id must not be ended"
    );
}

/// The marker the `ignore-end` child prints once it ignores SIGTERM and SIGHUP.
#[cfg(unix)]
const IGNORING_MARKER: &str = "HOLDER-CHILD-IGNORING";

/// A daemon of the sandbox whose windows reach no user bus on Linux, so no window gets a
/// child's scope bound to its holder's: the only writers left to end a window's tree are
/// its watcher, its holder and the daemon.
#[cfg(unix)]
async fn start_without_bus(sandbox: &Sandbox) -> RunningDaemon {
    #[cfg(target_os = "linux")]
    let no_bus = format!("unix:path={}", sandbox.tree.path().join("no-bus").display());
    #[cfg(target_os = "linux")]
    let launcher_env = [("DBUS_SESSION_BUS_ADDRESS", no_bus.as_str())];
    #[cfg(not(target_os = "linux"))]
    let launcher_env: [(&str, &str); 0] = [];
    sandbox.start_from(&launcher_env).await
}

/// Opens a window whose child ignores SIGTERM and the terminal's hangup, and waits until
/// it does.
#[cfg(unix)]
async fn open_stubborn(daemon: &mut RunningDaemon, sandbox: &Sandbox) -> String {
    let window_id = daemon.open_profile_window(sandbox, "stubborn").await;
    daemon
        .output_until(&window_id, "the child ignoring every end", |text| {
            text.contains(IGNORING_MARKER)
        })
        .await;
    window_id
}

/// The processes of one window as its record names them, each killed as the test ends.
#[cfg(unix)]
struct Tree {
    record: HolderRecord,
    leader: EndOnDrop,
    watcher: EndOnDrop,
    holder: EndOnDrop,
}

#[cfg(unix)]
impl Tree {
    fn of(sandbox: &Sandbox, window_id: &str) -> Self {
        let record = record_of(sandbox, window_id);
        let leader = handle_of(
            record.child_pid.expect("the record names the child"),
            record.child_start.as_deref(),
        );
        let watcher = handle_of(
            record
                .watcher_pid
                .expect("the record names the window's watcher"),
            record.watcher_start.as_deref(),
        );
        let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
        Self {
            record,
            leader: EndOnDrop(Some(leader)),
            watcher: EndOnDrop(Some(watcher)),
            holder: EndOnDrop(Some(holder)),
        }
    }

    fn leader(&self) -> &ProcessHandle {
        self.leader.0.as_ref().unwrap()
    }

    fn watcher(&self) -> &ProcessHandle {
        self.watcher.0.as_ref().unwrap()
    }

    fn holder(&self) -> &ProcessHandle {
        self.holder.0.as_ref().unwrap()
    }

    fn watcher_pid(&self) -> u32 {
        self.record.watcher_pid.unwrap()
    }

    /// Stops the holder and then the watcher, each seen stopped: a running holder resumes
    /// a stopped watcher, so the holder goes first.
    async fn stop_holder_then_watcher(&self) {
        stop(self.record.holder_pid, "the window's holder").await;
        stop(self.watcher_pid(), "the window's watcher").await;
    }
}

/// Sends `signal`, a name `kill -s` takes, to `pid`.
#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args(["-s", signal, &pid.to_string()])
        .stdin(Stdio::null())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -s {signal} {pid} failed: {status}");
}

/// The state `ps` reports for `pid`; empty once it is gone.
#[cfg(unix)]
fn process_state(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Stops `pid` and waits until it reads stopped.
#[cfg(unix)]
async fn stop(pid: u32, what: &str) {
    send_signal(pid, "STOP");
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let state = process_state(pid);
        if state.starts_with('T') {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never read stopped: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the daemon reads `window_id` as exited, which it publishes once it has
/// handled the end of the window's holder.
#[cfg(unix)]
async fn wait_exited(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) {
    send(
        &mut daemon.client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.to_owned(),
            target_status: WindowStatus::Exited,
            timeout_sec: HANG_GUARD.as_secs(),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WaitResult { outcome, .. } if outcome == "reached" => {}
        other => panic!(
            "the window {window_id} never read as exited: {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

#[cfg(unix)]
async fn close(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) {
    send(
        &mut daemon.client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_owned(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!(
            "the window {window_id} must close, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

/// On Unix, once a window's watcher and then its holder have died while its daemon runs
/// (the holder stopped, the watcher killed and seen to end, then the holder killed, so
/// neither ends the tree or removes the files), the daemon ends the window's tree -- a
/// child that ignores SIGTERM and the hangup -- and removes the holder's files: for a
/// window it took back, as it learns of the holder's end and marks the window lost; for a
/// window it opened, whose holder it can no longer name, once the window is closed.
#[cfg(unix)]
#[tokio::test]
async fn a_windows_tree_is_ended_once_its_watcher_and_then_its_holder_died_under_a_daemon() {
    let sandbox = Sandbox::new("end-after-watcher");
    sandbox.write_probe_profile("stubborn", &[&holder_child(), "ignore-end"], "");

    let mut first = start_without_bus(&sandbox).await;
    let taken = open_stubborn(&mut first, &sandbox).await;
    first.kill_keeping_windows();
    let mut next = start_without_bus(&sandbox).await;
    next.output_until(&taken, "the taken-back window's screen", |text| {
        text.contains(IGNORING_MARKER)
    })
    .await;
    let opened = open_stubborn(&mut next, &sandbox).await;
    let _endpoints = KilledEndpoints::new(&sandbox, &[&taken, &opened]);
    let taken_tree = Tree::of(&sandbox, &taken);
    let opened_tree = Tree::of(&sandbox, &opened);
    for tree in [&taken_tree, &opened_tree] {
        end_holder_and_watcher(&tree.record);
        assert_eq!(
            tree.leader().liveness(),
            Liveness::Alive,
            "the fixture's child must outlive its killed holder and watcher"
        );
    }

    wait_exited(&mut next, &sandbox, &taken).await;
    let db = store_db(&sandbox);
    settle!(
        sandbox,
        "the taken-back window's tree was not ended once its holder ended",
        {
            let mut problems = Vec::new();
            if taken_tree.leader().liveness() != Liveness::Dead {
                problems
                    .push("the child that outlived its holder and watcher still runs".to_owned());
            }
            let left = holder_files(&sandbox, &taken);
            if !left.is_empty() {
                problems.push(format!("the dead holder's files are left: {left:?}"));
            }
            let state = row_state(&db, &taken);
            if state.as_deref() != Some("lost") {
                problems.push(format!("the window's row is {state:?}, not lost"));
            }
            problems
        }
    );

    close(&mut next, &sandbox, &opened).await;
    settle!(
        sandbox,
        "the closed window's tree was not ended after its holder had died",
        {
            let mut problems = Vec::new();
            if opened_tree.leader().liveness() != Liveness::Dead {
                problems
                    .push("the child that outlived its holder and watcher still runs".to_owned());
            }
            let left = holder_files(&sandbox, &opened);
            if !left.is_empty() {
                problems.push(format!("the dead holder's files are left: {left:?}"));
            }
            problems
        }
    );
}

/// On Unix, a window whose holder was killed while its watcher was stopped (the holder
/// stopped first) keeps its tree and its holder's files while the watcher holds the
/// holder's endpoint lock: the daemon sends nothing to the tree, even once it has handled
/// the holder's end. Resumed, the watcher ends the tree and removes the files; killed
/// instead, it leaves both to the daemon, which then ends the tree and removes the files.
#[cfg(unix)]
#[tokio::test]
async fn a_window_whose_holder_died_while_its_watcher_was_stopped_is_ended_once_the_watcher_goes_on_or_dies()
 {
    let sandbox = Sandbox::new("end-stopped-watcher");
    sandbox.write_probe_profile("stubborn", &[&holder_child(), "ignore-end"], "");

    let mut first = start_without_bus(&sandbox).await;
    let resumed = open_stubborn(&mut first, &sandbox).await;
    let killed = open_stubborn(&mut first, &sandbox).await;
    first.kill_keeping_windows();
    let mut next = start_without_bus(&sandbox).await;
    for window_id in [&resumed, &killed] {
        next.output_until(window_id, "the taken-back window's screen", |text| {
            text.contains(IGNORING_MARKER)
        })
        .await;
    }
    let _endpoints = KilledEndpoints::new(&sandbox, &[&resumed, &killed]);
    let resumed_tree = Tree::of(&sandbox, &resumed);
    let killed_tree = Tree::of(&sandbox, &killed);
    for tree in [&resumed_tree, &killed_tree] {
        tree.stop_holder_then_watcher().await;
        kill_and_wait(tree.holder(), "the stopped holder").await;
    }
    for window_id in [&resumed, &killed] {
        wait_exited(&mut next, &sandbox, window_id).await;
    }
    for (tree, window_id) in [(&resumed_tree, &resumed), (&killed_tree, &killed)] {
        assert_eq!(
            tree.leader().liveness(),
            Liveness::Alive,
            "the window's tree was sent to while its stopped watcher held the endpoint lock; \
             daemon log:\n{}",
            sandbox.daemon_log()
        );
        assert!(
            !holder_files(&sandbox, window_id).is_empty(),
            "the holder's files were removed while its stopped watcher held the endpoint lock"
        );
    }

    send_signal(resumed_tree.watcher_pid(), "CONT");
    settle!(sandbox, "the resumed watcher did not end the window", {
        let mut problems = Vec::new();
        if resumed_tree.leader().liveness() != Liveness::Dead {
            problems.push("the child still runs".to_owned());
        }
        let left = holder_files(&sandbox, &resumed);
        if !left.is_empty() {
            problems.push(format!("the holder's files are left: {left:?}"));
        }
        if resumed_tree.watcher().liveness() != Liveness::Dead {
            problems.push("the watcher still runs".to_owned());
        }
        problems
    });

    assert_eq!(
        killed_tree.leader().liveness(),
        Liveness::Alive,
        "the window's tree was sent to while its stopped watcher held the endpoint lock"
    );
    kill_and_wait(killed_tree.watcher(), "the stopped watcher").await;
    settle!(
        sandbox,
        "the window's tree was not ended once its stopped watcher was killed",
        {
            let mut problems = Vec::new();
            if killed_tree.leader().liveness() != Liveness::Dead {
                problems
                    .push("the child that outlived its holder and watcher still runs".to_owned());
            }
            let left = holder_files(&sandbox, &killed);
            if !left.is_empty() {
                problems.push(format!("the dead holder's files are left: {left:?}"));
            }
            problems
        }
    );
}

/// On Unix, closing a window the daemon is not connected to (its take-back waits on a
/// stopped holder) leaves the window's tree to the watcher its holder's record names: the
/// daemon kills the holder and sends nothing to the tree while that watcher lives, here
/// stopped, and the watcher, resumed, ends the tree and removes the files. A record that
/// names no watcher (a holder from before the watcher) has the daemon end the tree first,
/// with the window's real watcher still stopped.
#[cfg(unix)]
#[tokio::test]
async fn closing_an_unconnected_window_leaves_its_tree_to_the_watcher_its_record_names() {
    let sandbox = Sandbox::new("close-unconnected");
    sandbox.write_probe_profile("stubborn", &[&holder_child(), "ignore-end"], "");

    let mut first = start_without_bus(&sandbox).await;
    let watched = open_stubborn(&mut first, &sandbox).await;
    let unwatched = open_stubborn(&mut first, &sandbox).await;
    first.kill_keeping_windows();
    let _endpoints = KilledEndpoints::new(&sandbox, &[&watched, &unwatched]);
    let watched_tree = Tree::of(&sandbox, &watched);
    let unwatched_tree = Tree::of(&sandbox, &unwatched);
    for tree in [&watched_tree, &unwatched_tree] {
        tree.stop_holder_then_watcher().await;
    }
    let without_watcher = HolderRecord {
        watcher_pid: None,
        watcher_start: None,
        ..unwatched_tree.record.clone()
    };
    std::fs::write(
        holder_record_path(&sandbox.holders_dir(), &unwatched).unwrap(),
        encode_holder_record(&without_watcher),
    )
    .unwrap();

    let mut next = start_without_bus(&sandbox).await;
    listed(&mut next, &sandbox, &watched).await;
    listed(&mut next, &sandbox, &unwatched).await;

    close(&mut next, &sandbox, &unwatched).await;
    wait_dead(unwatched_tree.holder(), "the closed window's holder").await;
    wait_exited(&mut next, &sandbox, &unwatched).await;
    settle!(
        sandbox,
        "closing a window whose record names no watcher did not end its tree",
        {
            if unwatched_tree.leader().liveness() == Liveness::Dead {
                Vec::new()
            } else {
                vec!["the child still runs".to_owned()]
            }
        }
    );
    assert!(
        process_state(unwatched_tree.watcher_pid()).starts_with('T'),
        "the window's real watcher must still be stopped, so it ended nothing"
    );

    close(&mut next, &sandbox, &watched).await;
    wait_dead(watched_tree.holder(), "the closed window's holder").await;
    wait_exited(&mut next, &sandbox, &watched).await;
    assert_eq!(
        watched_tree.leader().liveness(),
        Liveness::Alive,
        "the daemon sent to the tree of a window whose record names a living watcher; daemon \
         log:\n{}",
        sandbox.daemon_log()
    );
    assert!(
        !holder_files(&sandbox, &watched).is_empty(),
        "the holder's files were removed while its stopped watcher held the endpoint lock"
    );
    send_signal(watched_tree.watcher_pid(), "CONT");
    settle!(
        sandbox,
        "the resumed watcher did not end the closed window",
        {
            let mut problems = Vec::new();
            if watched_tree.leader().liveness() != Liveness::Dead {
                problems.push("the child still runs".to_owned());
            }
            let left = holder_files(&sandbox, &watched);
            if !left.is_empty() {
                problems.push(format!("the holder's files are left: {left:?}"));
            }
            problems
        }
    );
}

/// The holders folder a window registry made of its own in this test process for
/// `window_id`, found by the record the window's holder keeps there.
#[cfg(target_os = "macos")]
fn private_holders_dir(window_id: &str) -> PathBuf {
    let prefix = format!("yagura-holders-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                && dir.join(format!("{window_id}.json")).exists()
        })
        .unwrap_or_else(|| panic!("no private holders folder keeps the record of {window_id}"))
}

/// On macOS, a window served by a registry with a place of its own, whose watcher and then
/// holder died while it was connected (the holder stopped first, so neither ends the tree
/// or removes the files), reads as exited, and then has its tree ended and its holder's
/// files removed. On Linux the holder there may get a scope its child's is bound to, which
/// the test process's environment decides, so the daemon's part cannot be told apart.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_served_window_whose_watcher_and_then_holder_died_has_its_tree_ended() {
    let harness =
        common::Harness::start("served-holder-died", yagura_daemon::DaemonConfig::default()).await;
    let window_id = harness
        .open(vec![holder_child(), "ignore-end".to_owned()], None)
        .await;
    harness
        .wait_for_scrollback(&window_id, IGNORING_MARKER.as_bytes())
        .await;
    let holders_dir = private_holders_dir(&window_id);
    let record = read_holder_record(&holders_dir, &window_id).unwrap();
    let leader = EndOnDrop(Some(handle_of(
        record.child_pid.expect("the record names the child"),
        record.child_start.as_deref(),
    )));
    let _watcher = EndOnDrop(Some(handle_of(
        record
            .watcher_pid
            .expect("the record names the window's watcher"),
        record.watcher_start.as_deref(),
    )));
    end_holder_and_watcher(&record);
    assert_eq!(
        leader.0.as_ref().unwrap().liveness(),
        Liveness::Alive,
        "the fixture's child must outlive its killed holder and watcher"
    );

    common::wait_for_registry_window_exited(&harness.registry, &window_id).await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let runs = leader.0.as_ref().unwrap().liveness() != Liveness::Dead;
        let left: Vec<_> = std::fs::read_dir(&holders_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with(&format!("{window_id}.")))
            .collect();
        if !runs && left.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the served window's tree was not ended after its holder died: the child runs \
             {runs}, the holder's files left {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether `name` is a unit name of the form a window's scope has.
#[cfg(target_os = "linux")]
fn is_scope_name(name: &str) -> bool {
    name.strip_suffix(".scope").is_some_and(|stem| {
        !stem.is_empty()
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

/// The scope names `window_id`'s holder record carries, read without naming its fields.
#[cfg(target_os = "linux")]
fn recorded_scope_names(sandbox: &Sandbox, window_id: &str) -> Vec<String> {
    fn leaves(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => out.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|item| leaves(item, out)),
            serde_json::Value::Object(map) => map.values().for_each(|item| leaves(item, out)),
            _ => {}
        }
    }
    let mut names = Vec::new();
    if let Ok(bytes) = std::fs::read(holder_record_path(&sandbox.holders_dir(), window_id).unwrap())
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        leaves(&value, &mut names);
    }
    names.retain(|name| is_scope_name(name));
    names
}

/// The cgroup path `/proc/<pid>/cgroup` names; empty once the process is gone.
#[cfg(target_os = "linux")]
fn cgroup_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or_default()
        .to_owned()
}

/// Stops, as a test ends, the scopes it saw.
#[cfg(target_os = "linux")]
struct StopScopes(Vec<String>);

#[cfg(target_os = "linux")]
impl Drop for StopScopes {
    fn drop(&mut self) {
        for name in &self.0 {
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "--quiet", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// On Linux with the user's user manager, a window's child is in the scope its holder's
/// record names. When the holder and its watcher are killed while no daemon is there (the
/// holder stopped first, so neither ends the tree), the whole child's scope ends with the
/// holder's -- the child ignoring SIGTERM and SIGHUP and a grandchild it started in a
/// session of its own -- and the next daemon makes the window `lost`.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs the user's systemd user manager; run with --include-ignored where one runs"]
async fn a_linux_window_whose_holder_was_killed_while_no_daemon_was_there_ends_its_scope_whole() {
    let sandbox = Sandbox::new("reconcile-scope");
    let gate = sandbox.tree.path().join("gate");
    let pid_file = sandbox.tree.path().join("grandchild.pid");
    sandbox.write_probe_profile(
        "late",
        &[&holder_child(), "late-tree", utf8(&gate), utf8(&pid_file)],
        "",
    );

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "late").await;
    first
        .output_until(&window_id, "the child waiting for its gate", |text| {
            text.contains("HOLDER-CHILD-LATE-TREE-WAITING")
        })
        .await;
    let record = record_of(&sandbox, &window_id);
    let _endpoints = KilledEndpoints::new(&sandbox, &[&window_id]);
    let child_pid = record.child_pid.expect("the record names the child");
    let child = EndOnDrop(Some(handle_of(child_pid, record.child_start.as_deref())));
    let mut scope: Option<String>;
    settle!(sandbox, "the child is not in a scope its record names", {
        let names = recorded_scope_names(&sandbox, &window_id);
        let cgroup = cgroup_of(child_pid);
        scope = names
            .iter()
            .find(|name| cgroup.ends_with(&format!("/{name}")))
            .cloned();
        if scope.is_some() {
            Vec::new()
        } else {
            vec![format!(
                "the child's cgroup {cgroup:?}, the record's scopes {names:?}"
            )]
        }
    });
    let scope = scope.unwrap();
    let _scopes = StopScopes(recorded_scope_names(&sandbox, &window_id));

    std::fs::write(&gate, b"").unwrap();
    first
        .output_until(&window_id, "the grandchild started", |text| {
            text.contains("HOLDER-CHILD-LATE-TREE-STARTED")
        })
        .await;
    let grandchild_pid: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let grandchild = EndOnDrop(Some(handle_of(
        grandchild_pid,
        Some(&start_of(grandchild_pid)),
    )));
    assert!(
        cgroup_of(grandchild_pid).ends_with(&format!("/{scope}")),
        "the fixture's grandchild must start in the child's scope: {}",
        cgroup_of(grandchild_pid)
    );
    first.kill_keeping_windows();
    end_holder_and_watcher(&record);
    wait_dead(
        child.0.as_ref().unwrap(),
        "the child in the killed holder's scope",
    )
    .await;
    wait_dead(
        grandchild.0.as_ref().unwrap(),
        "the grandchild in the killed holder's scope",
    )
    .await;

    let _next = sandbox.start().await;
    let db = store_db(&sandbox);
    settle!(sandbox, "the window whose holder died is not lost", {
        let state = row_state(&db, &window_id);
        if state.as_deref() == Some("lost") {
            Vec::new()
        } else {
            vec![format!("the window's row is {state:?}, not lost")]
        }
    });
}

/// The version that shipped the snapshot slots: migrations 001 to 016.
const V0_3_SCHEMA_VERSION: usize = 16;
/// The slots of a 0.3 store: the windows open at its last write, and the ones rotated
/// out of them at the start before.
const V0_3_SLOT_CURRENT: i64 = 0;
const V0_3_SLOT_PREV: i64 = 1;

/// Writes a session store as version 0.3 left it in the sandbox's data root: `current`
/// and `prev` slot rows as `(name, layout)`.
fn seed_0_3_store(
    sandbox: &Sandbox,
    current: &[(&str, Option<&str>)],
    prev: &[(&str, Option<&str>)],
) {
    std::fs::create_dir_all(sandbox.data_root()).unwrap();
    let mut conn = rusqlite::Connection::open(store_db(sandbox)).unwrap();
    store::apply_migrations(&mut conn, &store::MIGRATIONS[..V0_3_SCHEMA_VERSION]).unwrap();
    for (slot, rows) in [(V0_3_SLOT_CURRENT, current), (V0_3_SLOT_PREV, prev)] {
        for (position, (name, layout)) in (0i64..).zip(rows) {
            insert_window_row_with_layout_value(
                &conn,
                slot,
                position,
                name,
                layout.map_or(SqlValue::Null, |layout| SqlValue::Text(layout.to_owned())),
            );
        }
    }
    set_meta(&conn, 2000, Some(1000));
}

fn has_slot_table(db: &Path) -> bool {
    let conn = store::open_readonly(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'snapshot_windows'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// The first start over a 0.3 store turns the slot the 0.3 daemon would have offered
/// next (`current` when it holds a window, `prev` otherwise) into `lost` rows, which are
/// the restore candidates with their layouts, and drops the slot table; a second start
/// offers the same candidates once.
#[tokio::test]
async fn the_first_start_over_a_0_3_store_moves_its_last_windows_into_lost_rows() {
    const LAYOUT: &str = "{\"v\":3}";
    let with_current = Sandbox::new("reconcile-0-3-current");
    seed_0_3_store(
        &with_current,
        &[("c-zero", Some(LAYOUT)), ("c-one", None)],
        &[("p-zero", None)],
    );
    let only_prev = Sandbox::new("reconcile-0-3-prev");
    seed_0_3_store(&only_prev, &[], &[("p-only", Some(LAYOUT))]);

    for (sandbox, expected, layout_of_first) in [
        (&with_current, vec!["c-zero", "c-one"], Some(LAYOUT)),
        (&only_prev, vec!["p-only"], Some(LAYOUT)),
    ] {
        let db = store_db(sandbox);
        for start in ["first", "second"] {
            let mut daemon = sandbox.start().await;
            send(&mut daemon.client, &ClientMessage::ListRestoreCandidates).await;
            let candidates = match read_response(&mut daemon.client).await {
                DaemonMessage::RestoreCandidateList { windows, .. } => windows,
                other => panic!("expected restore_candidate_list, got {other:?}"),
            };
            let mut problems = Vec::new();
            let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
            if names != expected {
                problems.push(format!("the candidates are {names:?}"));
            }
            let layouts: Vec<Option<&str>> = candidates
                .iter()
                .map(|c| c.layout_json.as_deref())
                .collect();
            if layouts.first().copied().flatten() != layout_of_first
                || layouts.iter().skip(1).any(Option::is_some)
            {
                problems.push(format!("the candidates' layouts are {layouts:?}"));
            }
            let lost = names_in_state(&db, "lost");
            if lost != expected {
                problems.push(format!("the lost rows are {lost:?}"));
            }
            if has_slot_table(&db) {
                problems.push("the slot table is still there".to_owned());
            }
            assert!(
                problems.is_empty(),
                "after the {start} start over a 0.3 store, expected the candidates \
                 {expected:?}:\n- {}\ndaemon log:\n{}",
                problems.join("\n- "),
                sandbox.daemon_log()
            );
            drop(daemon);
        }
    }
}

/// A window whose holder is killed from outside while its daemon is connected reads as
/// exited with no exit code wherever it is read, its holder's files are removed, and where
/// holders are the daemon's children the holder is reaped, not left as a zombie.
#[tokio::test]
async fn a_connected_windows_holder_killed_from_outside_ends_the_window_and_is_reaped() {
    let sandbox = Sandbox::new("holder-killed-connected");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "script").await;
    daemon.attach(&window_id).await;
    daemon
        .output_until(&window_id, "the script's marker", |text| {
            text.contains(SCRIPT_MARKER)
        })
        .await;
    let _endpoint = KilledEndpoints::new(&sandbox, &[&window_id]);
    let record = record_of(&sandbox, &window_id);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    holder.kill().unwrap();

    // Asked for less than the read's own guard, so a window that never exits is answered
    // `timeout` and fails with the daemon log instead of a bare read timeout.
    send(
        &mut daemon.client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Exited,
            timeout_sec: HANG_GUARD.as_secs() / 2,
        },
    )
    .await;
    // The attached view is pushed the exit too, before or after the wait's answer.
    let mut reached = false;
    let mut exited_push = None;
    while !reached || exited_push.is_none() {
        match read_frame(&mut daemon.client).await {
            DaemonMessage::WaitResult { outcome, .. } if outcome == "reached" => reached = true,
            DaemonMessage::StatusChanged {
                window_id: id,
                status: WindowStatus::Exited,
                exit_code,
                ..
            } if id == window_id => exited_push = Some(exit_code),
            DaemonMessage::Frame(_)
            | DaemonMessage::StatusChanged { .. }
            | DaemonMessage::ChatParticipantsChanged => {}
            other => panic!(
                "the window {window_id} never read as exited once its holder was killed: \
                 {other:?}; daemon log:\n{}",
                sandbox.daemon_log()
            ),
        }
    }
    settle!(
        sandbox,
        "the killed holder was not cleaned up after the window read as exited",
        {
            let mut problems = Vec::new();
            let left = holder_files(&sandbox, &window_id);
            if !left.is_empty() {
                problems.push(format!("the dead holder's files are left: {left:?}"));
            }
            #[cfg(unix)]
            if process_state(record.holder_pid).starts_with('Z') {
                problems.push("the killed holder is a zombie, never reaped".to_owned());
            }
            problems
        }
    );

    // The holder died with the window, so its code is unknown: `null`, never a code.
    assert_eq!(
        exited_push,
        Some(None),
        "the Exited push of a window whose code is unknown"
    );
    let info = listed(&mut daemon, &sandbox, &window_id).await;
    assert_eq!(
        (info.status, info.exit_code),
        (WindowStatus::Exited, None),
        "the listing of a window whose code is unknown"
    );
    send(
        &mut daemon.client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.clone(),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowStatus {
            status, exit_code, ..
        } => assert_eq!(
            (status, exit_code),
            (WindowStatus::Exited, None),
            "the status answer of a window whose code is unknown"
        ),
        other => panic!("expected window_status, got {other:?}"),
    }
    send(
        &mut daemon.client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::Attached {
            child_exited,
            exit_code,
        } => assert_eq!(
            (child_exited, exit_code),
            (true, None),
            "the attach answer of a window whose code is unknown"
        ),
        other => panic!("expected attached, got {other:?}"),
    }
}

/// Reads the daemon's window list until `window_id`'s `mcp_connected` is
/// `Some(expected)`. No event tells a change of the mark, so its level is read;
/// how often carries no meaning.
async fn until_mcp_connected(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
    expected: bool,
    what: &str,
) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let info = listed(daemon, sandbox, window_id).await;
        if info.mcp_connected == Some(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: mcp_connected of {window_id} never became Some({expected}), it is {:?}; \
             daemon log:\n{}",
            info.mcp_connected,
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The answer to the request `id` among the lines the relay `kept` by a window client
/// writes, skipping every line before it.
async fn relay_answer(
    log: &mut ClientLog,
    kept: &ClientCommand,
    id: &serde_json::Value,
) -> serde_json::Value {
    loop {
        let result = log.result(kept).await;
        let line = result["line"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay's stdout ended: {result}"));
        let line: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("the relay writes JSON-RPC lines ({err}): {line}"));
        if line.get("method").is_none() && line.get("id") == Some(id) {
            return line;
        }
    }
}

/// A window's relay names its window to the next daemon after the kill and the
/// restart without the window typing anything or calling any tool, and a CLI
/// line it then carries is answered by that daemon's MCP. Killing the relay
/// clears the window's mark.
#[tokio::test]
async fn a_windows_relay_names_its_window_to_the_next_daemon_without_any_call_and_its_end_clears_the_mark()
 {
    let sandbox = Sandbox::new("relay-names");
    // The window's process keeps the relay as its child and carries the CLI's lines to it.
    let mut log = ClientLog::new(&sandbox, "relay");
    let client: Vec<String> = bins::window_client_command(log.path());
    let client: Vec<&str> = client.iter().map(String::as_str).collect();
    sandbox.write_probe_profile("relay-window", &client, "");
    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let window_id = first.open_profile_window(&sandbox, "relay-window").await;

    let relay_exe = relay_path();
    let relay = log.keep(
        &[utf8(&relay_exe)],
        &[
            (yagura_protocol::INSTANCE_ENV, sandbox.instance.as_str()),
            (yagura_protocol::WINDOW_ID_ENV, window_id.as_str()),
            (yagura_protocol::STATUS_TOKEN_ENV, TOKEN),
        ],
        &["YAGURA_STORE_DB"],
    );
    first.type_command(&window_id, &relay).await;
    log.result(&relay).await;
    until_mcp_connected(&mut first, &sandbox, &window_id, true, "the relay's start").await;

    first.kill_keeping_windows();
    let mut next = sandbox.start().await;
    until_mcp_connected(&mut next, &sandbox, &window_id, true, "the next daemon").await;

    let write = log.write(&relay, &common::relay::initialize_line(1).to_string());
    next.type_command(&window_id, &write).await;
    let opened = relay_answer(&mut log, &relay, &serde_json::json!(1)).await;
    assert!(
        opened["result"]["protocolVersion"].is_string(),
        "the next daemon's MCP answers the initialize the relay carries: {opened}"
    );
    for line in [
        common::relay::initialized_line(),
        common::relay::tool_call_line(2, "list_windows", serde_json::json!({}), false),
    ] {
        let write = log.write(&relay, &line.to_string());
        next.type_command(&window_id, &write).await;
    }
    let listing =
        common::relay::tool_json(&relay_answer(&mut log, &relay, &serde_json::json!(2)).await);
    let listed_ids: Vec<&str> = listing["windows"]
        .as_array()
        .unwrap_or_else(|| panic!("list_windows answers a windows array: {listing}"))
        .iter()
        .filter_map(|window| window["window_id"].as_str())
        .collect();
    assert!(
        listed_ids.contains(&window_id.as_str()),
        "the next daemon's list_windows, carried by the relay, lists the window: {listing}"
    );

    let end = log.end(&relay);
    next.type_command(&window_id, &end).await;
    until_mcp_connected(&mut next, &sandbox, &window_id, false, "the relay's end").await;
}

/// A definition under which a daemon that typed into windows took a body it had marked
/// back out of the composer: a composer with a marker it reads, and `[input]` keys
/// naming a clear key and a probe.
fn left_mark_definition() -> String {
    format!(
        r#"schema_version = 1
name = "left-mark"
bottom_line_count = 5

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_input"
pattern = '{COMPOSER_MARKER}'
priority = 10
region = "bottom"

[input]
bracketed_paste = true
submit_delay_ms = 120
clear = "\u0015"
delivery_confirm_ms = 2000
resend_max = 0
probe = "x"

[composer]
marker = '{COMPOSER_MARKER}'
separator = "{COMPOSER_SEPARATOR}"
continuation_prefix = "{COMPOSER_CONTINUATION_PREFIX}"
"#
    )
}

/// The text the composer `echo_child` paints on `screen` holds: what follows its marker
/// and separator on the last row carrying the marker.
fn painted_composer(screen: &str) -> Option<String> {
    let row = screen
        .lines()
        .rfind(|line| line.trim_start().starts_with(COMPOSER_MARKER))?;
    let rest = row.trim_start()[COMPOSER_MARKER.len()..].trim_end();
    Some(
        rest.strip_prefix(COMPOSER_SEPARATOR)
            .unwrap_or(rest)
            .to_owned(),
    )
}

/// The WARN lines of `log`, without the timestamp each line begins with.
fn warn_lines(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains(" WARN "))
        .map(|line| {
            line.split_once(' ')
                .map_or(line, |(_, rest)| rest)
                .trim_start()
                .to_owned()
        })
        .collect()
}

/// Upgrading over a daemon that typed into windows, once: the mark such a daemon kept
/// for a body it was typing (a `delivery_marks` row) is discarded by the next daemon with
/// a warning, and nothing is taken back out of the composer for it. The composer here
/// holds the very text the mark names, as the user typed it, and keeps it.
#[tokio::test]
async fn a_typing_mark_an_older_daemon_left_is_dropped_with_a_warning_and_nothing_is_taken_back() {
    const BODY: &str = "LEFT-MARK-BODY-5Q8";
    const AFTER: &str = "AFTER-MARK.";
    let sandbox = Sandbox::new("left-mark");
    let detections = sandbox.tree.make_dir("left-mark-detections");
    std::fs::write(detections.join("left-mark.toml"), left_mark_definition()).unwrap();
    let child = echo_child_path();
    common::write_profile(
        &sandbox.profiles_dir,
        "left-mark",
        &format!(
            "schema = 1\ndisplay_name = \"left-mark\"\ndetection = \"left-mark\"\n\n[command]\n\
             default = ['{}', '{COMPOSER_MARKER}', '{COMPOSER_SEPARATOR}', \
             '{COMPOSER_CONTINUATION_PREFIX}', '0']\n",
            utf8(&child)
        ),
    );
    let env = [("YAGURA_DETECTION_DIR", utf8(&detections))];

    let mut first = sandbox.start_from(&env).await;
    let window_id = first.open_profile_window(&sandbox, "left-mark").await;
    first.attach(&window_id).await;
    first
        .output_until(&window_id, "the empty composer", |text| {
            painted_composer(text).is_some()
        })
        .await;
    send(
        &mut first.client,
        &ClientMessage::Text {
            text: BODY.to_owned(),
        },
    )
    .await;
    first
        .output_until(&window_id, "the typed text in the composer", |text| {
            painted_composer(text).as_deref() == Some(BODY)
        })
        .await;
    first.kill_keeping_windows();

    let db = store_db(&sandbox);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO delivery_marks (window_id, body) VALUES (?1, ?2)",
            rusqlite::params![window_id, BODY],
        )
        .unwrap();
    }
    assert!(
        common::store_fixtures::store_holds(&db, BODY),
        "the precondition: the store holds the mark the older daemon left"
    );
    let log_before = sandbox.daemon_log();

    let mut next = sandbox.start_from(&env).await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while listed(&mut next, &sandbox, &window_id).await.status != WindowStatus::WaitingInput
        || common::store_fixtures::store_holds(&db, BODY)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the next daemon must take the window back to waiting_input and drop the mark; \
             daemon log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    next.attach(&window_id).await;
    send(
        &mut next.client,
        &ClientMessage::Text {
            text: AFTER.to_owned(),
        },
    )
    .await;
    let screen = next
        .output_until(&window_id, "the text typed after the restart", |text| {
            painted_composer(text).is_some_and(|composer| composer.ends_with(AFTER))
        })
        .await;
    assert_eq!(
        painted_composer(&screen).as_deref(),
        Some(format!("{BODY}{AFTER}").as_str()),
        "nothing may be taken back out of the composer for a mark an older daemon left; \
         screen:\n{screen}\ndaemon log:\n{}",
        sandbox.daemon_log()
    );

    let log = sandbox.daemon_log();
    let earlier = warn_lines(&log_before);
    let new_warns: Vec<String> = warn_lines(log.get(log_before.len()..).unwrap_or_default())
        .into_iter()
        .filter(|line| !earlier.contains(line))
        .collect();
    assert!(
        !new_warns.is_empty(),
        "the next daemon must warn that it dropped the mark the older daemon left; \
         daemon log:\n{log}"
    );
    assert!(
        !log.contains(BODY),
        "the daemon log must not carry the marked text {BODY:?}:\n{log}"
    );
}

// --- what the take-back cases below share ------------------------------------------------

/// Keeps `mark` as a parcel for `window_id` while no daemon runs, for the next one to load.
fn seed_parcel(sandbox: &Sandbox, window_id: &str, mark: &str) {
    let db = rusqlite::Connection::open(store_db(sandbox)).unwrap();
    db.execute(
        "INSERT INTO inbox_parcels (window_id, seq, key, notice, body) VALUES (?1, 1, ?2, 0, ?3)",
        [window_id, &format!("seeded:{mark}"), mark],
    )
    .unwrap();
}

/// Waits until the row of each of `window_ids` holds the hook route its open settled, so
/// the next daemon builds each window from its row rather than from its holder's launch
/// record.
async fn until_rows_settled(sandbox: &Sandbox, window_ids: &[&str]) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let settled = {
            let conn = store::open_readonly(&store_db(sandbox)).expect("open the store");
            window_ids.iter().all(|window_id| {
                conn.query_row(
                    "SELECT hooks FROM windows WHERE window_id = ?1",
                    [window_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .is_some()
            })
        };
        if settled {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rows of {window_ids:?} never held their hook routes; daemon log:
{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Starts a daemon of the sandbox that gives the windows it opens [`TOKEN`].
async fn start_with_token(sandbox: &Sandbox) -> RunningDaemon {
    sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await
}

/// The migration that keeps whether a window's overlay was made, and every later one, are
/// numbered from here.
const OVERLAY_FACT_MIGRATION: u32 = 23;

/// Puts the sandbox's store back as it was before the migration numbered `number`: the
/// column changes of it and the later ones are undone, and the schema version set back, so
/// the next daemon migrates rows written before.
fn store_before_migration(sandbox: &Sandbox, number: u32) {
    let version = number - 1;
    let db = rusqlite::Connection::open(store_db(sandbox)).unwrap();
    common::store_fixtures::undo_column_migrations_after(&db, version as usize);
    db.pragma_update(None, "user_version", version).unwrap();
}

// --- whether a window's overlay was made is a fact of the window -------------------------

/// The argv the window's own CLI logged under `logs`. The bridge scan before a window
/// opens runs the same `[command]` (`mcp list`) into the same log, so its line is skipped.
async fn window_cli_args_logged_in(logs: &std::path::Path) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        let events = std::fs::read_to_string(logs.join("events.jsonl")).unwrap_or_default();
        let started = events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|line| line["event"] == "argv")
            .map(|line| {
                line["args"]
                    .as_array()
                    .map(|args| {
                        args.iter()
                            .filter_map(|arg| arg.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .find(|args| !(args.iter().any(|a| a == "mcp") && args.iter().any(|a| a == "list")));
        if let Some(started) = started {
            return started;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hang guard: the window's CLI never logged its argv under {}",
            logs.display()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// What the daemon answers a claim of `window_id` of `profile` with [`TOKEN`], made by the
/// window's own `window_client`: whether it is answered as its window's.
async fn spoken_for(
    daemon: &mut RunningDaemon,
    log: &mut ClientLog,
    window_id: &str,
    profile: &str,
) -> DaemonMessage {
    let conn = log.new_conn();
    let claim = log.exchange(
        &conn,
        &identify_message(window_id, Some(profile)),
        ClientRead::One,
    );
    daemon.type_command(window_id, &claim).await;
    log.message(&claim).await
}

/// Writes the profile `id`: the shipped profile `shipped` whose CLI keeps its window open
/// as a `window_client` writing to `results`, run in another environment with `guest`.
fn write_overlay_profile(sandbox: &Sandbox, id: &str, shipped: &str, guest: bool, results: &Path) {
    use crate::credentials_launch::{shipped_with_command, unchanged_convert};
    let convert = unchanged_convert();
    let cli = bins::window_client_command(results);
    common::write_profile(
        &sandbox.profiles_dir,
        id,
        &shipped_with_command(id, shipped, &cli, guest.then_some(convert.as_slice()), &[]),
    );
}

/// The time the daemon names a context read in the name of `window_id` with [`TOKEN`], made
/// by the window's own `window_client`. A hook asked for values of its input reports them
/// all absent.
async fn context_time(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    log: &mut ClientLog,
    window_id: &str,
) -> Option<serde_json::Value> {
    let read = |input: Option<Vec<serde_json::Value>>| -> ClientMessage {
        let mut door = serde_json::json!({ "kind": "context" });
        if let Some(input) = input {
            door["input"] = serde_json::Value::Array(input);
        }
        let wire = serde_json::json!({
            "type": "door_read",
            "window_id": window_id,
            "token": TOKEN,
            "door": door,
        });
        serde_json::from_value(wire.clone())
            .unwrap_or_else(|err| panic!("the wire must know a door's read {wire}: {err}"))
    };
    let conn = log.new_conn();
    let first = log.exchange(&conn, &read(None), ClientRead::One);
    daemon.type_command(window_id, &first).await;
    let mut answer = log.message(&first).await;
    if let DaemonMessage::HookInputWanted { pointers } = &answer {
        let input = vec![serde_json::json!("absent"); pointers.len()];
        let second = log.exchange(&conn, &read(Some(input)), ClientRead::One);
        daemon.type_command(window_id, &second).await;
        answer = log.message(&second).await;
    }
    let answer = serde_json::to_value(&answer).expect("a daemon message serializes");
    assert_eq!(
        answer["type"],
        "delivery",
        "{window_id}: the context read is answered: {answer}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    answer.get("hook_timeout_sec").cloned()
}

/// Rewrites the profile `id` of the sandbox so its `[hook_timeout]` gives every door 7
/// seconds more; returns the time it gave a context door before.
fn lengthen_hook_timeouts(sandbox: &Sandbox, id: &str) -> u64 {
    let path = sandbox.profiles_dir.join(format!("{id}.toml"));
    let source = std::fs::read_to_string(&path).unwrap();
    let before = yagura_daemon::profile::parse_profile(id, &source)
        .unwrap_or_else(|err| panic!("{id} parses: {err}"))
        .hook_timeouts
        .for_door(&yagura_protocol::Door::Context)
        .unwrap_or_else(|| panic!("{id} gives a context door a time"));
    let mut document: toml::Table = source.parse().unwrap();
    let mut table = toml::Table::new();
    table.insert(
        "default".into(),
        toml::Value::Integer(i64::try_from(before + 7).unwrap()),
    );
    document.insert("hook_timeout".into(), toml::Value::Table(table));
    std::fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    before
}

/// A window taken back by the next daemon keeps whether its overlay was made, with its row
/// or from its holder's launch record when the row is gone: one run directly made it at its
/// open and is spoken for. One whose CLI runs in another environment and whose daemon ended
/// before anything asked for its overlay never made it, and is still refused. The time its
/// doors are named is the one its CLI was given at its open, kept the same two ways, though
/// its profile was rewritten after.
#[tokio::test]
async fn a_taken_back_window_keeps_whether_its_overlay_was_made() {
    const DIRECT: &str = "overlay-kept-direct";
    const DIRECT_ROWLESS: &str = "overlay-kept-direct-rowless";
    const GUEST: &str = "overlay-kept-guest";
    const GUEST_ROWLESS: &str = "overlay-kept-guest-rowless";
    let sandbox = Sandbox::new("recover-overlay");
    let mut logs = Vec::new();
    for (id, shipped, guest) in [
        (DIRECT, "grok-build", false),
        (DIRECT_ROWLESS, "grok-build", false),
        (GUEST, "wsl-grok-build", true),
        (GUEST_ROWLESS, "wsl-grok-build", true),
    ] {
        let log = ClientLog::new(&sandbox, id);
        write_overlay_profile(&sandbox, id, shipped, guest, log.path());
        logs.push(log);
    }

    let mut first = start_with_token(&sandbox).await;
    let mut ids = Vec::new();
    for profile in [DIRECT, DIRECT_ROWLESS, GUEST, GUEST_ROWLESS] {
        ids.push(first.open_profile_window(&sandbox, profile).await);
    }
    let settled: Vec<&str> = ids.iter().map(String::as_str).collect();
    until_rows_settled(&sandbox, &settled).await;
    first.kill_keeping_windows();
    {
        let db = rusqlite::Connection::open(store_db(&sandbox)).unwrap();
        for window_id in [&ids[1], &ids[3]] {
            db.execute("DELETE FROM windows WHERE window_id = ?1", [window_id])
                .unwrap();
        }
    }
    let opened = [DIRECT, DIRECT_ROWLESS].map(|id| lengthen_hook_timeouts(&sandbox, id));

    let mut next = start_with_token(&sandbox).await;
    for window_id in &ids {
        listed(&mut next, &sandbox, window_id).await;
    }
    for (at, ((window_id, profile), opened)) in [(&ids[0], DIRECT), (&ids[1], DIRECT_ROWLESS)]
        .into_iter()
        .zip(opened)
        .enumerate()
    {
        let answer = spoken_for(&mut next, &mut logs[at], window_id, profile).await;
        assert!(
            matches!(answer, DaemonMessage::Identified { .. }),
            "{profile}: a window run directly made its overlay at its open, got {answer:?}; \
             daemon log:\n{}",
            sandbox.daemon_log()
        );
        assert_eq!(
            context_time(&mut next, &sandbox, &mut logs[at], window_id).await,
            Some(serde_json::json!(opened)),
            "{profile}: a window taken back is named the time its CLI was given at its open, \
             not its rewritten profile's; daemon log:\n{}",
            sandbox.daemon_log()
        );
    }
    for (at, (window_id, profile)) in [(&ids[2], GUEST), (&ids[3], GUEST_ROWLESS)]
        .into_iter()
        .enumerate()
    {
        let answer = spoken_for(&mut next, &mut logs[at + 2], window_id, profile).await;
        assert!(
            crate::credentials_launch::refused_as_pending(&answer),
            "{profile}: a guest window whose overlay was never made is still refused after \
             the take-back, got {answer:?}; daemon log:\n{}",
            sandbox.daemon_log()
        );
    }
}

/// A window whose row was written before whether its overlay was made was kept is refused
/// when the profile its row names declares an overlay, and spoken for as before when it
/// declares none. Such a row keeps no time its CLI was given either, so the window's doors
/// are named none.
#[tokio::test]
async fn a_window_whose_row_predates_the_overlay_fact_is_refused_when_its_profile_declares_one() {
    const DECLARED: &str = "overlay-old-declared";
    const PLAIN: &str = "overlay-old-plain";
    let sandbox = Sandbox::new("recover-overlay-old");
    let mut declared_log = ClientLog::new(&sandbox, DECLARED);
    let mut plain_log = ClientLog::new(&sandbox, PLAIN);
    write_overlay_profile(&sandbox, DECLARED, "grok-build", false, declared_log.path());
    let cli = bins::window_client_command(plain_log.path());
    let cli: Vec<&str> = cli.iter().map(String::as_str).collect();
    sandbox.write_probe_profile(PLAIN, &cli, "\n[hook_timeout]\ndefault = 9\n");

    let mut first = start_with_token(&sandbox).await;
    let declared = first.open_profile_window(&sandbox, DECLARED).await;
    let plain = first.open_profile_window(&sandbox, PLAIN).await;
    until_rows_settled(&sandbox, &[&declared, &plain]).await;
    first.kill_keeping_windows();
    store_before_migration(&sandbox, OVERLAY_FACT_MIGRATION);

    let mut next = start_with_token(&sandbox).await;
    listed(&mut next, &sandbox, &declared).await;
    listed(&mut next, &sandbox, &plain).await;
    let answer = spoken_for(&mut next, &mut plain_log, &plain, PLAIN).await;
    assert!(
        matches!(answer, DaemonMessage::Identified { .. }),
        "control: an older row whose profile declares no overlay is spoken for, got {answer:?}"
    );
    assert_eq!(
        context_time(&mut next, &sandbox, &mut plain_log, &plain).await,
        None,
        "a row older than the time its CLI was given keeps none, so its doors are named none \
         whatever its profile gives now; daemon log:\n{}",
        sandbox.daemon_log()
    );
    let answer = spoken_for(&mut next, &mut declared_log, &declared, DECLARED).await;
    assert!(
        crate::credentials_launch::refused_as_pending(&answer),
        "an older row whose profile declares an overlay is refused, got {answer:?}; daemon \
         log:\n{}",
        sandbox.daemon_log()
    );
}

/// A WSL grok window makes its overlay in its guest through the shipped launch statement:
/// the statement runs the daemon named by the whole `{daemon}` element as a door, which
/// hands it the overlay the daemon makes from the guest's facts with the same rule as for a
/// window run directly, and the CLI starts with it. The window is then spoken for, and still
/// is once the next daemon has taken it back. The guest is this machine when it runs Linux;
/// elsewhere there is no bash to run the statement with, and the case returns without a run.
#[tokio::test]
async fn a_guest_window_makes_its_overlay_through_its_door_as_a_direct_one_would_and_keeps_it() {
    use crate::credentials_launch::{
        OVERLAY_VAR, drops_with, overlay_drops, reported_overlay, shipped_with_command,
        unchanged_convert,
    };
    if !cfg!(target_os = "linux") {
        return;
    }
    const DIRECT: &str = "overlay-door-direct";
    const GUEST_PROBE: &str = "overlay-door-probe";
    const GUEST: &str = "overlay-door-guest";
    const INLINE: &str = r#"{"models":{"default_reasoning_effort":"high"},"shell_environment_policy":{"exclude":["BETA_*"]}}"#;
    let shipped: toml::Table =
        common::docs::read_doc_verbatim("../../profiles/wsl-grok-build.toml")
            .parse()
            .expect("the shipped WSL grok profile is TOML");
    let argv: Vec<String> = shipped["command"]["windows"]
        .as_array()
        .expect("the shipped WSL grok profile has a Windows argv")
        .iter()
        .map(|arg| arg.as_str().expect("an argument").to_string())
        .collect();
    let at = argv
        .iter()
        .position(|arg| arg == "-lic")
        .expect("the shipped profile starts grok through `bash -lic <statement>`");
    assert_eq!(
        argv[at + 2..],
        ["grok", "{daemon}"],
        "the shipped statement takes the program, then the door it runs as a whole \
         `{{daemon}}` element: {argv:?}"
    );
    let statement = argv[at + 1].clone();
    let guest_argv = |program: Vec<String>| -> Vec<String> {
        let mut argv = vec![
            crate::hook_events_door::bash_path(),
            "-c".to_string(),
            statement.clone(),
            program[0].clone(),
            "{daemon}".to_string(),
        ];
        argv.extend(program[1..].iter().cloned());
        argv
    };

    let sandbox = Sandbox::new("recover-overlay-door");
    let grok = sandbox.tree.make_dir("home").join(".grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(
        grok.join("config.toml"),
        "[shell_environment_policy]\nexclude = [\"ACME_*\"]\n",
    )
    .unwrap();
    let probe = vec![
        bins::env_probe_path().display().to_string(),
        OVERLAY_VAR.to_string(),
    ];
    let convert = unchanged_convert();
    let guest_env = [("GROK_CONFIG", INLINE)];
    common::write_profile(
        &sandbox.profiles_dir,
        DIRECT,
        &shipped_with_command(DIRECT, "grok-build", &probe, None, &[]),
    );
    common::write_profile(
        &sandbox.profiles_dir,
        GUEST_PROBE,
        &shipped_with_command(
            GUEST_PROBE,
            "wsl-grok-build",
            &guest_argv(probe.clone()),
            Some(&convert),
            &guest_env,
        ),
    );
    let logs = sandbox.tree.make_dir("logs-guest");
    let mut guest_log = ClientLog::new(&sandbox, GUEST);
    let mut guest_cli = bins::window_client_command(guest_log.path());
    guest_cli.extend(["--log".to_string(), logs.display().to_string()]);
    common::write_profile(
        &sandbox.profiles_dir,
        GUEST,
        &shipped_with_command(
            GUEST,
            "wsl-grok-build",
            &guest_argv(guest_cli),
            Some(&convert),
            &guest_env,
        ),
    );
    // The statement runs what a guest's shell finds on its own `PATH`.
    let path = std::env::join_paths(
        common::real_daemon::daemon_base_dirs()
            .into_iter()
            .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]),
    )
    .unwrap();
    let path = path.to_str().unwrap().to_string();
    let mut first = sandbox
        .start_from(&[
            ("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN),
            ("GROK_CONFIG", INLINE),
            ("PATH", &path),
        ])
        .await;

    let direct = first.open_profile_window(&sandbox, DIRECT).await;
    let direct_overlay = reported_overlay(&mut first, &sandbox, &direct).await;
    let probe_window = first.open_profile_window(&sandbox, GUEST_PROBE).await;
    let guest_overlay = reported_overlay(&mut first, &sandbox, &probe_window).await;
    assert_eq!(
        guest_overlay, direct_overlay,
        "the door hands the overlay a window run directly gets for the same facts"
    );
    assert_eq!(
        overlay_drops(&guest_overlay),
        drops_with(&["ACME_*", "BETA_*"]),
        "{guest_overlay}"
    );

    let guest = first.open_profile_window(&sandbox, GUEST).await;
    // The CLI logs its argv once the statement has started it, after the door answered.
    let started = window_cli_args_logged_in(&logs).await;
    assert!(
        !started.iter().any(|arg| arg.contains("yagura-daemon")),
        "the door's path is not handed on to the CLI: {started:?}"
    );
    let answer = spoken_for(&mut first, &mut guest_log, &guest, GUEST).await;
    assert!(
        matches!(answer, DaemonMessage::Identified { .. }),
        "a guest window whose overlay was made is spoken for, got {answer:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    until_rows_settled(&sandbox, &[&guest]).await;
    first.kill_keeping_windows();

    let mut next = start_with_token(&sandbox).await;
    listed(&mut next, &sandbox, &guest).await;
    let answer = spoken_for(&mut next, &mut guest_log, &guest, GUEST).await;
    assert!(
        matches!(answer, DaemonMessage::Identified { .. }),
        "the guest window is still spoken for once taken back, got {answer:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
}

// --- a guest window's wake slot is a fact of the window ---------------------------------

/// The migration that keeps a window's wake record, and every later one, are numbered from
/// here.
#[cfg(windows)]
const WAKE_RECORD_MIGRATION: u32 = 24;

/// Writes the profile `id`: a `launch` window whose CLI keeps it open and runs in another
/// environment, with a `{wake_paths}` answer and a `state_base` reading the WSL base `base`.
#[cfg(windows)]
fn write_wake_profile(sandbox: &Sandbox, id: &str, base: &crate::hooks_injection::WslBase) {
    use crate::hooks_injection::{WAKE_ANSWER, wsl_wake_profile};
    let cli = crate::credentials_launch::quiet_cli(&sandbox.tree.make_dir(&format!("logs-{id}")));
    common::write_profile(
        &sandbox.profiles_dir,
        id,
        &wsl_wake_profile(id, &cli, &[("SessionStart", WAKE_ANSWER)], base.tool(false)),
    );
}

/// The wake paths the window `window_id` of the sandbox was handed at its open.
fn sandbox_wake_paths(sandbox: &Sandbox, window_id: &str) -> Option<Vec<String>> {
    crate::hooks_injection::wake_paths_of(sandbox.tree.path(), window_id)
}

/// The wake paths of the slot `slot` under the WSL base `base`, for the sandbox's instance.
#[cfg(windows)]
fn wake_slot(
    sandbox: &Sandbox,
    base: &crate::hooks_injection::WslBase,
    slot: u32,
) -> Option<Vec<String>> {
    Some(vec![crate::hooks_injection::guest_wake_path(
        &base.linux,
        Some(&sandbox.instance),
        slot,
    )])
}

/// Closes the window `window_id` and waits until its credentials folder is gone.
#[cfg(windows)]
async fn close_and_wait_for_credentials(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    window_id: &str,
) {
    let folder = crate::hooks_injection::credentials_folder_of(sandbox.tree.path(), window_id)
        .expect("a launch window has its credentials folder");
    send(
        &mut daemon.client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_owned(),
            force: None,
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    crate::hooks_injection::until_gone(&folder).await;
}

/// A guest window taken back by the next daemon keeps the wake slot its open took, with its
/// row or from its holder's launch record when the row is gone: a window the next daemon
/// opens takes a slot neither holds, and takes each one's again once that window has ended.
#[cfg(windows)]
#[tokio::test]
#[ignore = "needs a WSL distribution; run with --include-ignored on a Windows host with one"]
async fn a_taken_back_guest_window_keeps_its_wake_slot_until_it_ends() {
    const WAKE: &str = "wake-kept";
    let base = crate::hooks_injection::WslBase::new("recover-wake");
    let sandbox = Sandbox::new("recover-wake");
    write_wake_profile(&sandbox, WAKE, &base);

    let mut first = start_with_token(&sandbox).await;
    let with_row = first.open_profile_window(&sandbox, WAKE).await;
    let rowless = first.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &with_row),
        wake_slot(&sandbox, &base, 0)
    );
    assert_eq!(
        sandbox_wake_paths(&sandbox, &rowless),
        wake_slot(&sandbox, &base, 1)
    );
    until_rows_settled(&sandbox, &[&with_row, &rowless]).await;
    first.kill_keeping_windows();
    {
        let db = rusqlite::Connection::open(store_db(&sandbox)).unwrap();
        db.execute("DELETE FROM windows WHERE window_id = ?1", [&rowless])
            .unwrap();
    }

    let mut next = start_with_token(&sandbox).await;
    listed(&mut next, &sandbox, &with_row).await;
    listed(&mut next, &sandbox, &rowless).await;
    let third = next.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &third),
        wake_slot(&sandbox, &base, 2),
        "the windows taken back hold their slots; daemon log:\n{}",
        sandbox.daemon_log()
    );
    close_and_wait_for_credentials(&mut next, &sandbox, &with_row).await;
    let fourth = next.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &fourth),
        wake_slot(&sandbox, &base, 0),
        "a window taken back with its row gives its slot back at its end"
    );
    close_and_wait_for_credentials(&mut next, &sandbox, &rowless).await;
    let fifth = next.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &fifth),
        wake_slot(&sandbox, &base, 1),
        "a window taken back from its launch record gives its slot back at its end"
    );
}

/// A window whose row was written before its wake record was kept is taken back with none,
/// so it holds no slot.
#[cfg(windows)]
#[tokio::test]
#[ignore = "needs a WSL distribution; run with --include-ignored on a Windows host with one"]
async fn a_window_whose_row_predates_the_wake_record_holds_no_wake_slot() {
    const WAKE: &str = "wake-old";
    let base = crate::hooks_injection::WslBase::new("recover-wake-old");
    let sandbox = Sandbox::new("recover-wake-old");
    write_wake_profile(&sandbox, WAKE, &base);

    let mut first = start_with_token(&sandbox).await;
    let older = first.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &older),
        wake_slot(&sandbox, &base, 0)
    );
    until_rows_settled(&sandbox, &[&older]).await;
    first.kill_keeping_windows();
    store_before_migration(&sandbox, WAKE_RECORD_MIGRATION);

    let mut next = start_with_token(&sandbox).await;
    listed(&mut next, &sandbox, &older).await;
    let later = next.open_profile_window(&sandbox, WAKE).await;
    assert_eq!(
        sandbox_wake_paths(&sandbox, &later),
        wake_slot(&sandbox, &base, 0),
        "an older row holds no wake record and so no slot; daemon log:\n{}",
        sandbox.daemon_log()
    );
}

// --- a taken-back window whose CLI has no wait is asked to wait again --------------------

/// Writes the profile `id`: a `launch` window whose CLI keeps it open on this host, with a
/// `{wake_paths}` answer, and whose `[delivery]` wakes its CLI by answering the wait the CLI
/// registers, on stderr with exit code 2.
fn write_wait_wake_profile(sandbox: &Sandbox, id: &str, results: &Path) {
    use crate::hooks_injection::{WAKE_ANSWER, wake_profile};
    let cli = bins::window_client_command(results);
    let source = wake_profile(id, &cli, &[("SessionStart", WAKE_ANSWER)], None);
    common::write_profile(
        &sandbox.profiles_dir,
        id,
        &format!(
            "{source}\n[delivery]\nwake = \"wait\"\nwake_stream = \"stderr\"\nwake_exit = 2\n"
        ),
    );
}

/// Reports that the CLI of `window_id` waits for input, as its hooks do (from the window's
/// own `window_client`), and waits until the window can be pushed to (its status is neither
/// `Unknown` nor `Exited`).
async fn until_pushable(
    daemon: &mut RunningDaemon,
    sandbox: &Sandbox,
    log: &mut ClientLog,
    window_id: &str,
) {
    let conn = log.new_conn();
    let report = log.exchange(
        &conn,
        &ClientMessage::StatusEvent {
            source: "cli".to_owned(),
            status: WindowStatus::WaitingInput,
            window_id: Some(window_id.to_owned()),
            token: TOKEN.to_owned(),
        },
        ClientRead::Pong,
    );
    daemon.type_command(window_id, &report).await;
    let refused = log.messages(&report).await;
    assert!(
        refused.is_empty(),
        "the status report must be taken, got {refused:?}"
    );
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while matches!(
        listed(daemon, sandbox, window_id).await.status,
        WindowStatus::Unknown | WindowStatus::Exited
    ) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{window_id} could never be pushed to; daemon log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Sends `text` to `window_id`, which must leave it waiting.
async fn send_waiting(daemon: &mut RunningDaemon, window_id: &str, text: &str) {
    send(
        &mut daemon.client,
        &ClientMessage::SendText {
            window_id: window_id.to_owned(),
            text: text.to_owned(),
        },
    )
    .await;
    let answer = read_response(&mut daemon.client).await;
    assert_eq!(
        crate::send_message_lifecycle::state_of(&answer).as_deref(),
        Some("queued"),
        "{text} must be left waiting: {answer:?}"
    );
}

/// A `wake = "wait"` window taken back by the next daemon is asked to wait again once it can
/// be pushed to and its CLI has no wait registered: the next daemon writes the wake file
/// the window's open named (so it holds something other than what the first daemon's ask
/// left), and the wait its CLI registers on that change is handed the parcel the store kept
/// from before the first daemon ended and the one sent after, in the profile's stream and
/// exit code.
#[tokio::test]
async fn a_taken_back_wait_window_with_no_wait_is_asked_again_and_its_next_wait_takes_every_parcel()
{
    const WAKE: &str = "wake-ask";
    let sandbox = Sandbox::new("recover-wake-ask");
    let mut log = ClientLog::new(&sandbox, WAKE);
    write_wait_wake_profile(&sandbox, WAKE, log.path());
    let playbook = sandbox
        .tree
        .make_dir("wake-ask-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]\nauto_approve = [\"open_window\", \"close_window\", \"send_message\"]\n",
    )
    .unwrap();
    let env = [
        ("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN),
        ("YAGURA_PLAYBOOK", utf8(&playbook)),
    ];

    let mut first = sandbox.start_from(&env).await;
    let window_id = first.open_profile_window(&sandbox, WAKE).await;
    let paths = sandbox_wake_paths(&sandbox, &window_id)
        .unwrap_or_else(|| panic!("the open of {window_id} names no wake file"));
    let wake = PathBuf::from(&paths[0]);
    until_pushable(&mut first, &sandbox, &mut log, &window_id).await;
    send_waiting(&mut first, &window_id, "SENT-BEFORE-RESTART").await;
    crate::inbox::until_rewritten(&wake, None).await;
    until_rows_settled(&sandbox, &[&window_id]).await;
    first.kill_keeping_windows();
    let before = std::fs::read(&wake).expect("the first daemon's ask stays");
    // A message's envelope goes with its daemon; a parcel the store keeps stays.
    seed_parcel(&sandbox, &window_id, "KEPT-BEFORE-RESTART");

    let mut next = sandbox.start_from(&env).await;
    listed(&mut next, &sandbox, &window_id).await;
    until_pushable(&mut next, &sandbox, &mut log, &window_id).await;
    crate::inbox::until_rewritten(&wake, Some(&before)).await;
    send_waiting(&mut next, &window_id, "AFTER-RESTART").await;

    // The window's own process asks, and says it printed once it has.
    let conn = log.new_conn();
    let wait = log.exchange(
        &conn,
        &ClientMessage::DoorRead {
            window_id: window_id.clone(),
            token: TOKEN.to_owned(),
            door: yagura_protocol::Door::Wait,
        },
        ClientRead::Delivery,
    );
    next.type_command(&window_id, &wait).await;
    let (output, stream, code) = common::expect_delivery(log.message(&wait).await);
    for text in ["KEPT-BEFORE-RESTART", "AFTER-RESTART"] {
        assert!(
            output.contains(text),
            "the wait is handed {text}: {output:?}"
        );
    }
    assert_eq!(stream, yagura_protocol::DeliveryStream::Stderr);
    assert_eq!(code, 2);
}
