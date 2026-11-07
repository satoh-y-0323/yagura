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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox};
use common::{read_response, send};
use yagura_os::{Liveness, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{
    HOLDER_PROTOCOL, HolderRecord, encode_holder_record, holder_record_path, read_holder_record,
};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowInfo, WindowStatus};
use yagura_test_support::HANG_GUARD;
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().expect("start term_script")
}

/// Ends the window's real holder, which its killed daemon left, and puts a stand-in in
/// its place: at the endpoint `record` names, in a record naming the stand-in and its
/// child with their starts.
async fn replace_holder(
    sandbox: &Sandbox,
    record: &HolderRecord,
    mode: &str,
    log: PathBuf,
) -> FakeHolder {
    let holders_dir = sandbox.holders_dir();
    let _ = yagura_test_support::process::end_recorded_holder(
        &holders_dir,
        Some(OsStr::new(&sandbox.instance)),
        record,
    );
    let child = spawn_term_script(&["idle"]);
    let holder = spawn_term_script(&["fake-holder", mode, &record.endpoint, utf8(&log)]);
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
        window_id: record.window_id.clone(),
        endpoint: record.endpoint.clone(),
        holder_pid,
        child_pid: Some(child_pid),
        protocol: HOLDER_PROTOCOL,
        holder_start: Some(start_of(holder_pid)),
        child_start: Some(start_of(child_pid)),
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

    next.kill_keeping_windows();
    assert_eq!(
        fake.running(),
        (true, true),
        "a failed take-back must leave the window's holder and child running"
    );
}

/// A window whose child exited while its daemon was connected, left unclosed, is taken
/// back by the next daemon as the exited window it was, with its exit code and last
/// screen; closing it then ends its holder and leaves none of its files.
#[tokio::test]
async fn an_exited_window_is_taken_back_with_its_last_screen_and_closes_cleanly() {
    let sandbox = Sandbox::new("recover-exited");
    let child = holder_child();
    sandbox.write_probe_profile("exits", &[&child, "exit", "7"], "");

    let mut first = sandbox.start().await;
    let window_id = first.open_profile_window(&sandbox, "exits").await;
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
    first.kill_keeping_windows();

    let mut next = sandbox.start().await;
    let info = listed(&mut next, &sandbox, &window_id).await;
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let mut info = info;
    while info.status != WindowStatus::Exited {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the exited window was never taken back as exited: {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        info = listed(&mut next, &sandbox, &window_id).await;
    }
    assert_eq!(info.exit_code, Some(7), "the exit code must be kept");
    next.output_until(&window_id, "the child's last screen", |text| {
        text.contains("EXITING 7")
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
    let prefix = format!("{window_id}.");
    let left: Vec<_> = std::fs::read_dir(sandbox.holders_dir())
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&prefix))
        .collect();
    assert!(left.is_empty(), "closing the exited window left {left:?}");
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
    // The reply may come before the holder's files are removed; the window's exit is
    // published only after.
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
    let left = holder_files(&sandbox, &window_id);
    assert!(
        left.is_empty(),
        "closing the window whose take-back failed left {left:?}"
    );
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
