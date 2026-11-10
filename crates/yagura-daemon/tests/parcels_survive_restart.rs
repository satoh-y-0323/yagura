#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The session store is read back with statements naming the tables its own catalogue
// lists, since which table keeps a parcel is not part of what is measured.
#![allow(clippy::disallowed_methods)]

//! Parcels outlive the daemon: what a window's inbox holds (a chat post) and what a
//! parent's report inbox holds are kept once accepted, so the next daemon of the instance
//! hands each one out once, through the same doors, after the first was killed outright.
//! A lost window's kept parcels stay with its row; a closed window's do not. A handoff's
//! envelope is not kept: it is withdrawn with the daemon that held it. A report notice is
//! not kept either; it is derived from the reports a parent still has to read.
//!
//! Each case runs real daemon processes one after the other in one sandbox instance.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox};
use common::store_fixtures::{inbox_parcel_keys, seed_parcels_an_older_daemon_kept, store_holds};
use common::{VALID_TOKEN, identify, read_response, report_status, send};
use yagura_daemon::fault_point::{self, FaultPoint};
use yagura_daemon::store;
use yagura_os::{PeerAuth, PlatformIpc};
use yagura_protocol::holder::read_holder_record;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowInfo, WindowStatus};
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::{Connection, connect_with_retry};
use yagura_test_support::process::holder_path;

/// A profile whose window takes parcels through its doors and leaves no wait registered,
/// so nothing is ever pushed out of its inbox.
const PROFILE: &str = "deliver";

/// The fixed marker a report notice opens with.
const REPORT_NOTICE_MARK: &str = "[yagura report]";

fn write_delivering_profile(sandbox: &Sandbox) {
    let child = holder_path()
        .with_file_name(format!("holder_child{}", std::env::consts::EXE_SUFFIX))
        .to_str()
        .expect("holder_child path must be UTF-8")
        .to_owned();
    sandbox.write_probe_profile(PROFILE, &[&child, "hex"], "\n[delivery]\nwake = \"wait\"\n");
}

/// Starts a daemon whose windows all get [`VALID_TOKEN`] and whose playbook lets a
/// handoff through without a human.
async fn start_first(sandbox: &Sandbox) -> RunningDaemon {
    start_first_with(sandbox, None).await
}

/// [`start_first`] with `point` armed when given: the daemon ends itself there.
async fn start_first_with(sandbox: &Sandbox, point: Option<FaultPoint>) -> RunningDaemon {
    let playbook = sandbox
        .tree
        .make_dir("persist-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]\nauto_approve = [\"open_window\", \"close_window\", \"send_handoff\"]\n",
    )
    .unwrap();
    let playbook = playbook.to_str().expect("the playbook path must be UTF-8");
    let mut env = vec![
        ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
        ("YAGURA_PLAYBOOK", playbook),
    ];
    if let Some(point) = point {
        env.push((fault_point::POINT_ENV, point.name()));
    }
    sandbox.start_from(&env).await
}

/// The profile of a chat participant: [`PROFILE`]'s window under a detection definition
/// of its own, since a window under the default one takes no part in chat.
const CHAT_PROFILE: &str = "deliver-chat";

/// Writes [`CHAT_PROFILE`] and its detection definition; returns the definitions'
/// directory, which every daemon of the sandbox is started with.
fn write_chat_profile(sandbox: &Sandbox) -> PathBuf {
    let detections = sandbox.tree.make_dir("persist-detections");
    common::write_control_definition(&detections);
    let child = holder_path()
        .with_file_name(format!("holder_child{}", std::env::consts::EXE_SUFFIX))
        .to_str()
        .expect("holder_child path must be UTF-8")
        .to_owned();
    common::write_profile(
        &sandbox.profiles_dir,
        CHAT_PROFILE,
        &format!(
            "schema = 1\ndisplay_name = \"{CHAT_PROFILE}\"\ndetection = \"{}\"\n\n[command]\n\
             default = ['{child}', 'hex']\n\n[delivery]\nwake = \"wait\"\n",
            common::CONTROL_DETECTION_NAME
        ),
    );
    detections
}

/// [`start_first`] for a daemon whose playbook also lets a chat post through, reading
/// detection definitions from `detections`.
async fn start_chatting(sandbox: &Sandbox, detections: &Path) -> RunningDaemon {
    let playbook = sandbox
        .tree
        .make_dir("persist-chat-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]\nauto_approve = [\"open_window\", \"close_window\", \"send_chat\"]\n",
    )
    .unwrap();
    let playbook = playbook.to_str().expect("the playbook path must be UTF-8");
    let detections = detections
        .to_str()
        .expect("the detections path must be UTF-8");
    sandbox
        .start_from(&[
            ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
            ("YAGURA_PLAYBOOK", playbook),
            ("YAGURA_DETECTION_DIR", detections),
        ])
        .await
}

/// The delivery state the `main` room records for message `seq` to `window_id`.
async fn chat_delivery_state(client: &mut Connection, seq: u64, window_id: &str) -> Option<String> {
    send(
        client,
        &ClientMessage::ReadChat {
            room_id: Some("main".to_owned()),
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    let messages = match read_response(client).await {
        DaemonMessage::ChatMessages { messages, .. } => messages,
        other => panic!("expected chat_messages, got {other:?}"),
    };
    messages
        .into_iter()
        .find(|message| message.seq == seq)?
        .deliveries
        .into_iter()
        .find(|delivery| delivery.window_id == window_id)
        .map(|delivery| delivery.state)
}

/// Another connection to the sandbox's daemon.
async fn connect(sandbox: &Sandbox) -> Connection {
    let user = PlatformIpc::default()
        .current_user_id()
        .expect("the current user id must resolve");
    connect_with_retry(&yagura_os::default_endpoint(&user, &sandbox.namespace())).await
}

async fn listed(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) -> WindowInfo {
    send(&mut daemon.client, &ClientMessage::ListWindows).await;
    let windows = match read_response(&mut daemon.client).await {
        DaemonMessage::WindowList { windows } => windows,
        other => panic!("expected window_list, got {other:?}"),
    };
    windows
        .iter()
        .find(|info| info.window_id == window_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the next daemon does not list the window {window_id}: {windows:?}; daemon \
                 log:\n{}",
                sandbox.daemon_log()
            )
        })
}

/// Gives the window a status a handoff may land on.
async fn make_ready(daemon: &mut RunningDaemon, window_id: &str) {
    report_status(&mut daemon.client, window_id, WindowStatus::WaitingApproval).await;
    common::wait_until_status(&mut daemon.client, window_id, WindowStatus::WaitingApproval).await;
}

/// Sends a handoff carrying `note` to `window_id` on a connection of its own and returns
/// that connection, without waiting for anything: a daemon armed at the point the handoff
/// reaches ends itself before it could answer.
async fn post_handoff(sandbox: &Sandbox, window_id: &str, note: &str) -> Connection {
    let mut requester = connect(sandbox).await;
    send(
        &mut requester,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.to_owned()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "parcels-survive-restart".to_owned(),
            note: note.to_owned(),
        },
    )
    .await;
    requester
}

/// Posts `body` to the `main` room on `client` and returns the posting's seq.
async fn post_chat(client: &mut Connection, body: String) -> u64 {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: Some("main".to_owned()),
            body,
        },
    )
    .await;
    match read_response(client).await {
        DaemonMessage::ChatPosted { seq, .. } => seq,
        other => panic!("expected chat_posted, got {other:?}"),
    }
}

async fn take(daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) -> String {
    send(
        &mut daemon.client,
        &ClientMessage::DeliverTake {
            window_id: window_id.to_owned(),
            token: VALID_TOKEN.to_owned(),
        },
    )
    .await;
    match read_response(&mut daemon.client).await {
        DaemonMessage::Delivery { output, .. } => output,
        other => panic!(
            "expected a delivery from the door of {window_id}, got {other:?}; daemon log:\n{}",
            sandbox.daemon_log()
        ),
    }
}

async fn wait_for_report(client: &mut Connection) -> Vec<yagura_protocol::ReceivedReport> {
    send(client, &ClientMessage::WaitForReport { timeout_sec: 0 }).await;
    match read_response(client).await {
        DaemonMessage::Reports { reports, .. } => reports,
        other => panic!("expected reports, got {other:?}"),
    }
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn store_db(sandbox: &Sandbox) -> PathBuf {
    sandbox.data_root().join("yagura.db")
}

/// The state the window table records for `window_id`; `None` without a row.
fn row_state(db: &Path, window_id: &str) -> Option<String> {
    let conn = store::open_readonly(db).ok()?;
    conn.query_row(
        "SELECT state FROM windows WHERE window_id = ?1",
        [window_id],
        |row| row.get(0),
    )
    .ok()
}

/// Waits until the session store holds every one of `needles`: a chat post is accepted
/// once its envelope is kept, and no answer tells its poster so before the window's CLI
/// takes it.
async fn until_kept(sandbox: &Sandbox, needles: &[&str]) {
    let db = store_db(sandbox);
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let missing: Vec<&str> = needles
            .iter()
            .copied()
            .filter(|needle| !store_holds(&db, needle))
            .collect();
        if missing.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon never kept the parcels carrying {missing:?} it accepted; daemon \
             log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A handoff's envelope that reached a window's inbox is withdrawn when its daemon dies
/// before the window's CLI took it: the next daemon's door hands the window nothing, and
/// no row of the session store keeps the envelope.
#[tokio::test]
async fn a_handoff_waiting_in_an_inbox_is_withdrawn_when_the_daemon_is_killed() {
    const HANDOFF_MARK: &str = "PERSIST-HANDOFF-9K4";
    let sandbox = Sandbox::new("persist-handoff");
    write_delivering_profile(&sandbox);

    let mut first = start_first_with(&sandbox, Some(FaultPoint::HandoffDeposited)).await;
    let window_id = first.open_profile_window(&sandbox, PROFILE).await;
    make_ready(&mut first, &window_id).await;
    let _requester = post_handoff(&sandbox, &window_id, HANDOFF_MARK).await;
    let ended = first.wait_for_fault_end().await;
    assert!(
        !ended.success(),
        "the daemon must end itself once the envelope is in the inbox: {ended:?}"
    );

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &window_id).await;
    let taken = take(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        count(&taken, HANDOFF_MARK),
        0,
        "the window's door must not hand out an envelope the killed daemon held: {taken:?}; \
         daemon log:\n{}",
        sandbox.daemon_log()
    );
    let kept = inbox_parcel_keys(&store_db(&sandbox), &window_id).expect("the store must read");
    assert!(
        kept.iter().all(|key| !key.starts_with("handoff:")),
        "no envelope may stay in the store: {kept:?}"
    );
}

/// Opens a parent window and a child of it on `first`, and has the child send the parent a
/// succeeded report carrying `summary`; returns the parent's and the child's ids.
async fn report_to_opener(
    sandbox: &Sandbox,
    first: &mut RunningDaemon,
    summary: &str,
) -> (String, String) {
    let parent = first.open_profile_window(sandbox, PROFILE).await;
    let mut as_parent = connect(sandbox).await;
    identify(&mut as_parent, &parent).await;
    send(
        &mut as_parent,
        &ClientMessage::OpenWindowFromProfile {
            profile: PROFILE.to_owned(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    let child = match read_response(&mut as_parent).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    };
    let mut as_child = connect(sandbox).await;
    identify(&mut as_child, &child).await;
    send(
        &mut as_child,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_owned(),
            path: None,
        },
    )
    .await;
    match read_response(&mut as_child).await {
        DaemonMessage::ReportSent { delivered_to, .. } => {
            assert_eq!(delivered_to.as_deref(), Some(parent.as_str()));
        }
        other => panic!("expected report_sent, got {other:?}"),
    }
    (parent, child)
}

/// A report a child sent to its opener before the daemon was killed is read by the
/// parent from the next daemon with `wait_for_report` exactly once, and the notice it put
/// in the parent's own inbox is taken through the parent's door exactly once.
#[tokio::test]
async fn a_report_and_its_notice_are_taken_once_after_the_daemon_is_killed() {
    const REPORT_MARK: &str = "PERSIST-REPORT-3W8";
    let sandbox = Sandbox::new("persist-report");
    write_delivering_profile(&sandbox);

    let mut first = start_first(&sandbox).await;
    let (parent, child) = report_to_opener(&sandbox, &mut first, REPORT_MARK).await;
    first.kill_keeping_windows();

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &parent).await;
    let taken = take(&mut next, &sandbox, &parent).await;
    assert_eq!(
        count(&taken, REPORT_NOTICE_MARK),
        1,
        "the parent's door must hand out the report notice it held before the kill once: \
         {taken:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    let again = take(&mut next, &sandbox, &parent).await;
    assert_eq!(
        count(&again, REPORT_NOTICE_MARK),
        0,
        "a notice already taken must not be handed out again: {again:?}"
    );

    let mut as_parent = connect(&sandbox).await;
    identify(&mut as_parent, &parent).await;
    let reports = wait_for_report(&mut as_parent).await;
    let summaries: Vec<(&str, &str)> = reports
        .iter()
        .map(|report| (report.from_window_id.as_str(), report.summary.as_str()))
        .collect();
    assert_eq!(
        summaries,
        [(child.as_str(), REPORT_MARK)],
        "the parent must read the report sent before the kill once; daemon log:\n{}",
        sandbox.daemon_log()
    );
    let reports = wait_for_report(&mut as_parent).await;
    assert!(
        reports.is_empty(),
        "a report already read must not be read again: {reports:?}"
    );
}

/// A store an older daemon left, keeping a handoff's envelope and a report notice in a
/// window's inbox rows, is cleared of both by the next daemon: its door hands out neither,
/// the only notice it hands out is the one derived from the report the parent still has to
/// read, and that report is still read once.
#[tokio::test]
async fn a_handoff_envelope_and_a_report_notice_an_older_daemon_kept_are_dropped() {
    const STALE_HANDOFF_MARK: &str = "PERSIST-STALE-HANDOFF-6D2";
    const STALE_NOTICE_MARK: &str = "PERSIST-STALE-NOTICE-4J1";
    const REPORT_MARK: &str = "PERSIST-REPORT-6B5";
    let sandbox = Sandbox::new("persist-older-parcels");
    write_delivering_profile(&sandbox);

    let mut first = start_first(&sandbox).await;
    let (parent, child) = report_to_opener(&sandbox, &mut first, REPORT_MARK).await;
    first.kill_keeping_windows();
    seed_parcels_an_older_daemon_kept(
        &store_db(&sandbox),
        &parent,
        STALE_HANDOFF_MARK,
        &format!("{REPORT_NOTICE_MARK} 7 pending report(s). {STALE_NOTICE_MARK}"),
    );

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &parent).await;
    let taken = take(&mut next, &sandbox, &parent).await;
    assert_eq!(
        (
            count(&taken, STALE_HANDOFF_MARK),
            count(&taken, STALE_NOTICE_MARK),
            count(&taken, REPORT_NOTICE_MARK),
        ),
        (0, 0, 1),
        "the door must hand out no kept envelope and no kept notice, and one notice derived \
         from the report: {taken:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    let again = take(&mut next, &sandbox, &parent).await;
    assert_eq!(
        count(&again, REPORT_NOTICE_MARK),
        0,
        "a notice already taken must not be handed out again: {again:?}"
    );
    let kept = inbox_parcel_keys(&store_db(&sandbox), &parent).expect("the store must read");
    assert!(
        kept.iter()
            .all(|key| !key.starts_with("handoff:") && key != "report"),
        "no envelope or notice may stay in the store: {kept:?}"
    );

    let mut as_parent = connect(&sandbox).await;
    identify(&mut as_parent, &parent).await;
    let reports = wait_for_report(&mut as_parent).await;
    let summaries: Vec<(&str, &str)> = reports
        .iter()
        .map(|report| (report.from_window_id.as_str(), report.summary.as_str()))
        .collect();
    assert_eq!(summaries, [(child.as_str(), REPORT_MARK)]);
}

/// A window whose holder died while no daemon was there becomes `lost`, and the parcel
/// kept for it stays with its row. A window closed while a parcel waited for it leaves
/// that parcel nowhere.
#[tokio::test]
async fn a_lost_windows_kept_parcel_stays_with_its_row_and_a_closed_windows_does_not() {
    const LOST_MARK: &str = "PERSIST-LOST-5T1";
    const CLOSED_MARK: &str = "PERSIST-CLOSED-8M6";
    let sandbox = Sandbox::new("persist-lost");
    let detections = write_chat_profile(&sandbox);

    let mut first = start_chatting(&sandbox, &detections).await;
    let lost = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    let closed = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut first, &lost).await;
    make_ready(&mut first, &closed).await;
    post_chat(&mut first.client, format!("@{lost} {LOST_MARK}")).await;
    post_chat(&mut first.client, format!("@{closed} {CLOSED_MARK}")).await;
    // The room keeps the body alone; only the envelope in the inbox carries it as JSON.
    let lost_envelope = format!("{LOST_MARK}\"");
    let closed_envelope = format!("{CLOSED_MARK}\"");
    until_kept(
        &sandbox,
        &[lost_envelope.as_str(), closed_envelope.as_str()],
    )
    .await;

    send(
        &mut first.client,
        &ClientMessage::CloseWindow {
            window_id: closed.clone(),
            force: Some(false),
        },
    )
    .await;
    match read_response(&mut first.client).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    let record = read_holder_record(&sandbox.holders_dir(), &lost)
        .expect("the lost window's holder must keep its record");
    first.kill_keeping_windows();
    assert!(
        yagura_test_support::process::end_recorded_holder(
            &sandbox.holders_dir(),
            Some(std::ffi::OsStr::new(&sandbox.instance)),
            &record,
        )
        .is_some(),
        "the lost window's holder must still run once its daemon was killed"
    );

    let mut next = start_chatting(&sandbox, &detections).await;
    let db = store_db(&sandbox);
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        // Asked after a request is answered, so the next daemon's start has run.
        send(&mut next.client, &ClientMessage::Ping).await;
        assert!(matches!(
            read_response(&mut next.client).await,
            DaemonMessage::Pong
        ));
        let mut problems = Vec::new();
        let state = row_state(&db, &lost);
        if state.as_deref() != Some("lost") {
            problems.push(format!(
                "the dead holder's window row is {state:?}, not lost"
            ));
        }
        if !store_holds(&db, &lost_envelope) {
            problems.push("the lost window's kept parcel is gone".to_owned());
        }
        if let Some(state) = row_state(&db, &closed) {
            problems.push(format!("the closed window's row is left as {state:?}"));
        }
        if store_holds(&db, &closed_envelope) {
            problems.push("the closed window's parcel is still kept".to_owned());
        }
        if problems.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "after the restart:\n- {}\ndaemon log:\n{}",
            problems.join("\n- "),
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A chat post waiting in a window's inbox when its daemon is killed is taken through the
/// window's door from the next daemon once, and the room records that delivery as taken,
/// as it does for a post taken from the daemon it was sent through.
#[tokio::test]
async fn a_chat_post_taken_after_the_daemon_is_killed_is_recorded_as_taken() {
    const CHAT_MARK: &str = "PERSIST-CHAT-2Q7";
    let sandbox = Sandbox::new("persist-chat");
    let detections = write_chat_profile(&sandbox);

    let mut first = start_chatting(&sandbox, &detections).await;
    let window_id = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut first, &window_id).await;
    send(
        &mut first.client,
        &ClientMessage::SendChat {
            room_id: Some("main".to_owned()),
            body: format!("@{window_id} {CHAT_MARK}"),
        },
    )
    .await;
    let seq = match read_response(&mut first.client).await {
        DaemonMessage::ChatPosted { seq, targets, .. } => {
            assert_eq!(targets, std::slice::from_ref(&window_id));
            seq
        }
        other => panic!("expected chat_posted, got {other:?}"),
    };
    // The room keeps the body alone; only the envelope in the inbox carries it as JSON.
    let in_envelope = format!("{CHAT_MARK}\"");
    until_kept(&sandbox, &[in_envelope.as_str()]).await;
    first.kill_keeping_windows();

    let mut next = start_chatting(&sandbox, &detections).await;
    listed(&mut next, &sandbox, &window_id).await;
    let taken = take(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        count(&taken, CHAT_MARK),
        1,
        "the window's door must hand out the post it held before the kill once: {taken:?}; \
         daemon log:\n{}",
        sandbox.daemon_log()
    );
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let state = chat_delivery_state(&mut next.client, seq, &window_id).await;
        if state.as_deref() == Some("taken") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post taken after the restart must be recorded as taken, not {state:?}; daemon \
             log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
