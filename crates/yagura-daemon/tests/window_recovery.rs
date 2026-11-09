#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The stand-in holders and their children are processes this test starts, waits for
// and kills itself; no window ever adopts them.
#![allow(clippy::disallowed_methods)]

//! A window outlives its daemon: a daemon killed outright leaves every window's holder
//! and child running, and the next daemon of the instance takes each window back under
//! the same id, starting `Unknown`, with the screen, the input modes and the token it had.
//! A window whose holder does not welcome the next daemon stays listed and closable, and
//! its holder and child are not ended for it.
//!
//! Each case runs real daemon processes one after the other in one sandbox instance.

use crate::common;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox};
use common::store_fixtures::{insert_window_row_with_layout_value, set_meta};
use common::{read_response, send};
use rusqlite::types::Value as SqlValue;
use yagura_daemon::store;
use yagura_os::{Liveness, PeerAuth, PlatformIpc, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderRecord, encode_holder_record, holder_record_path, read_holder_record,
};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowInfo, WindowStatus};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::{Connection, connect_with_retry};
use yagura_test_support::process::{holder_path, term_script_path};

/// The only token shape `StatusToken::from_override` accepts: the first daemon gives it
/// to every window it opens, and the next daemon draws its own.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210";
const SCRIPT_MARKER: &str = "HOLDER-CHILD-SCRIPT";
const DA1: &[u8] = b"\x1b[c";
const DA1_REPLY: &[u8] = b"\x1b[?1;2c";

fn holder_child() -> String {
    holder_path()
        .with_file_name(format!("holder_child{}", std::env::consts::EXE_SUFFIX))
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

async fn attach(daemon: &mut RunningDaemon, window_id: &str) {
    send(
        &mut daemon.client,
        &ClientMessage::AttachView {
            window_id: window_id.to_owned(),
            size: None,
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::Attached { .. } => {}
        other => panic!("expected attached, got {other:?}"),
    }
}

/// Sends a status event and then a ping: the refusal answered before the pong, if any.
async fn status_event_refusal(
    daemon: &mut RunningDaemon,
    window_id: &str,
    token: &str,
) -> Option<DaemonMessage> {
    send(
        &mut daemon.client,
        &ClientMessage::StatusEvent {
            source: "cli".to_owned(),
            status: WindowStatus::Running,
            window_id: Some(window_id.to_owned()),
            token: token.to_owned(),
        },
    )
    .await;
    send(&mut daemon.client, &ClientMessage::Ping).await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::Pong => None,
        refusal => {
            match read_response(&mut daemon.client).await {
                DaemonMessage::Pong => {}
                other => panic!("expected pong after the refusal, got {other:?}"),
            }
            Some(refusal)
        }
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

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    attach(&mut first, &window_id).await;
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

    attach(&mut next, &window_id).await;
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

    assert_eq!(
        status_event_refusal(&mut next, &window_id, TOKEN).await,
        None,
        "the window taken back must accept a status event with the token it had"
    );
    match status_event_refusal(&mut next, &window_id, OTHER_TOKEN).await {
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

/// Another connection to the sandbox's daemon.
async fn connect(sandbox: &Sandbox) -> Connection {
    let user = PlatformIpc::default()
        .current_user_id()
        .expect("the current user id must resolve");
    connect_with_retry(&yagura_os::default_endpoint(&user, &sandbox.namespace())).await
}

fn deliver_take(window_id: &str, token: &str) -> ClientMessage {
    ClientMessage::DeliverTake {
        window_id: window_id.to_owned(),
        token: token.to_owned(),
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
    let mut welcomed_door = connect(&sandbox).await;
    send(&mut welcomed_door, &deliver_take(&welcomed, TOKEN)).await;
    let mut refused_door = connect(&sandbox).await;
    send(&mut refused_door, &deliver_take(&refused, TOKEN)).await;
    let mut rowless_door = connect(&sandbox).await;
    send(&mut rowless_door, &deliver_take(&rowless, TOKEN)).await;
    for window_id in [&welcomed, &refused, &rowless] {
        until_held(&sandbox, window_id).await;
    }

    welcomed_fake.answer("welcome");
    refused_fake.answer("reject");
    rowless_fake.answer("welcome");
    match read_response(&mut welcomed_door).await {
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
    match read_response(&mut rowless_door).await {
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

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let live = first.open_profile_window(&sandbox, "script").await;
    let closed = first.open_profile_window(&sandbox, "script").await;
    let shut = first.open_profile_window(&sandbox, "script").await;
    let closed_record = record_of(&sandbox, &closed);
    let shut_record = record_of(&sandbox, &shut);
    first.kill_keeping_windows();

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
    closed_fake
        .wait_logged("the next daemon's Hello", hello_logged)
        .await;
    shut_fake
        .wait_logged("the next daemon's Hello", hello_logged)
        .await;

    // The live window is taken back once a write reaches its child.
    attach(&mut next, &live).await;
    send(
        &mut next.client,
        &ClientMessage::Text {
            text: "LIVE-MARK".to_owned(),
        },
    )
    .await;
    logged(&log, b"LIVE-MARK", None).await;
    assert_eq!(
        status_event_refusal(&mut next, &live, TOKEN).await,
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

/// A window whose holder runs but whose row is gone (here, with the whole database) is
/// taken back all the same: listed under its id at once, and given its values from the
/// launch record its holder keeps once the holder lets the next daemon in, with its
/// token. Closing it ends its holder and leaves none of its files.
#[tokio::test]
async fn a_window_without_a_row_is_taken_back_from_its_holders_launch_record() {
    let sandbox = Sandbox::new("recover-rowless");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut first = sandbox
        .start_from(&[("YAGURA_STATUS_TOKEN_OVERRIDE", TOKEN)])
        .await;
    let window_id = first.open_profile_window(&sandbox, "script").await;
    let name = listed(&mut first, &sandbox, &window_id).await.name;
    let record = record_of(&sandbox, &window_id);
    let holder = handle_of(record.holder_pid, record.holder_start.as_deref());
    first.kill_keeping_windows();
    for file in ["yagura.db", "yagura.db-wal", "yagura.db-shm"] {
        let _ = std::fs::remove_file(sandbox.data_root().join(file));
    }

    let mut next = sandbox.start().await;
    let info = listed(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        info.status,
        WindowStatus::Unknown,
        "a window taken back starts Unknown"
    );
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let mut info = info;
    while info.name != name {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window never took the name its launch record keeps ({name:?}): {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        info = listed(&mut next, &sandbox, &window_id).await;
    }
    assert_eq!(
        status_event_refusal(&mut next, &window_id, TOKEN).await,
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
/// exited, its holder's files are removed, and where holders are the daemon's children the
/// holder is reaped, not left as a zombie.
#[tokio::test]
async fn a_connected_windows_holder_killed_from_outside_ends_the_window_and_is_reaped() {
    let sandbox = Sandbox::new("holder-killed-connected");
    let log = sandbox.tree.path().join("input.log");
    write_script_profile(&sandbox, "script", &log, None);

    let mut daemon = sandbox.start().await;
    let window_id = daemon.open_profile_window(&sandbox, "script").await;
    attach(&mut daemon, &window_id).await;
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
    match read_response(&mut daemon.client).await {
        DaemonMessage::WaitResult { outcome, .. } if outcome == "reached" => {}
        other => panic!(
            "the window {window_id} never read as exited once its holder was killed: \
             {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
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
}
