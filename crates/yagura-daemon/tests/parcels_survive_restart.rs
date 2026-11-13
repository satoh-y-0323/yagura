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
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use common::real_daemon::{RunningDaemon, Sandbox, daemon_bin};
use common::store_fixtures::{inbox_parcel_keys, seed_parcels_an_older_daemon_kept, store_holds};
use common::{VALID_TOKEN, identify, read_response, report_status, send};
use tokio::io::AsyncWriteExt;
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
    start_chatting_with(sandbox, detections, None).await
}

/// [`start_chatting`] with `point` armed when given: the daemon ends itself there.
async fn start_chatting_with(
    sandbox: &Sandbox,
    detections: &Path,
    point: Option<FaultPoint>,
) -> RunningDaemon {
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
    let mut env = vec![
        ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
        ("YAGURA_PLAYBOOK", playbook),
        ("YAGURA_DETECTION_DIR", detections),
    ];
    if let Some(point) = point {
        env.push((fault_point::POINT_ENV, point.name()));
    }
    sandbox.start_from(&env).await
}

/// The `main` room's messages.
async fn room_messages(client: &mut Connection) -> Vec<yagura_protocol::ChatMessage> {
    send(
        client,
        &ClientMessage::ReadChat {
            room_id: Some("main".to_owned()),
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    match read_response(client).await {
        DaemonMessage::ChatMessages { messages, .. } => messages,
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

/// The delivery state the `main` room records for message `seq` to `window_id`.
async fn chat_delivery_state(client: &mut Connection, seq: u64, window_id: &str) -> Option<String> {
    room_messages(client)
        .await
        .into_iter()
        .find(|message| message.seq == seq)?
        .deliveries
        .into_iter()
        .find(|delivery| delivery.window_id == window_id)
        .map(|delivery| delivery.state)
}

/// The seq of the `main` room's message whose body carries `mark`: a post whose poster was
/// never answered is found by what it said.
async fn chat_seq_of(client: &mut Connection, sandbox: &Sandbox, mark: &str) -> u64 {
    room_messages(client)
        .await
        .into_iter()
        .find(|message| message.body.contains(mark))
        .unwrap_or_else(|| {
            panic!(
                "the room holds no message carrying {mark}; daemon log:
{}",
                sandbox.daemon_log()
            )
        })
        .seq
}

/// Waits until the room records message `seq` as delivered to `window_id`.
async fn until_chat_taken(client: &mut Connection, sandbox: &Sandbox, seq: u64, window_id: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let state = chat_delivery_state(client, seq, window_id).await;
        if state.as_deref() == Some("delivered") {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post taken after the restart must be recorded as delivered, not {state:?}; daemon              log:
{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

/// A door's read of the take door of `window_id`, as a door of revision 34 sends it.
fn door_read_take(window_id: &str) -> ClientMessage {
    let wire = serde_json::json!({
        "type": "door_read",
        "window_id": window_id,
        "token": VALID_TOKEN,
        "door": { "kind": "take" },
    });
    serde_json::from_value(wire.clone())
        .unwrap_or_else(|err| panic!("the wire must know a door's read {wire}: {err}"))
}

/// The notice a door sends once it has printed what its read was answered with.
fn printed() -> ClientMessage {
    serde_json::from_value(serde_json::json!({ "type": "deliver_printed" }))
        .unwrap_or_else(|err| panic!("the wire must know deliver_printed: {err}"))
}

/// Takes through the window's door as a hook does: on a connection of its own, printing
/// what it was answered with and saying so before it ends.
async fn take(_daemon: &mut RunningDaemon, sandbox: &Sandbox, window_id: &str) -> String {
    let mut door = connect(sandbox).await;
    send(&mut door, &door_read_take(window_id)).await;
    match read_response(&mut door).await {
        DaemonMessage::Delivery { output, .. } => {
            send(&mut door, &printed()).await;
            output
        }
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

const CHAT_ENVELOPE_MARKER: &str = "[yagura chat] ";

/// The `id` the chat envelope carrying `mark` in `printed` names.
fn chat_envelope_id(printed: &str, mark: &str) -> String {
    let line = printed
        .lines()
        .find(|line| line.contains(mark))
        .unwrap_or_else(|| panic!("no envelope carrying {mark} in {printed:?}"));
    let start = line
        .find(CHAT_ENVELOPE_MARKER)
        .unwrap_or_else(|| panic!("the post must arrive as a chat envelope: {line:?}"))
        + CHAT_ENVELOPE_MARKER.len();
    let envelope: serde_json::Value = serde_json::from_str(&line[start..])
        .unwrap_or_else(|err| panic!("the envelope must be JSON ({err}): {line:?}"));
    envelope["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("the chat envelope must name its id: {line:?}"))
        .to_string()
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

/// The sandbox daemon's endpoint, which a relay connects to.
fn endpoint(sandbox: &Sandbox) -> String {
    let user = PlatformIpc::default()
        .current_user_id()
        .expect("the current user id must resolve");
    yagura_os::default_endpoint(&user, &sandbox.namespace())
}

/// A report the parent's relay took with `wait_for_report` but never said it received
/// stays kept: the next daemon hands the same report out again, with the same sender and
/// the same `at_ms`.
#[tokio::test]
async fn a_report_taken_through_a_relay_without_a_receipt_is_taken_again_after_the_daemon_is_killed()
 {
    const REPORT_MARK: &str = "PERSIST-RECEIPT-6H3";
    let sandbox = Sandbox::new("persist-receipt");
    write_delivering_profile(&sandbox);

    let mut first = start_first(&sandbox).await;
    let (parent, child) = report_to_opener(&sandbox, &mut first, REPORT_MARK).await;
    let mut relay = crate::relay_session::window_session(&endpoint(&sandbox), &parent).await;
    let took = crate::relay_session::take_reports(&mut relay, 1, 0).await;
    let keys = crate::relay_session::report_keys(&took.reports);
    assert_eq!(
        keys.iter()
            .map(|(from, summary, _)| (from.as_str(), summary.as_str()))
            .collect::<Vec<_>>(),
        [(child.as_str(), REPORT_MARK)],
        "the parent's relay takes the report from the first daemon"
    );
    first.kill_keeping_windows();
    drop(relay);

    let mut next = sandbox.start().await;
    listed(&mut next, &sandbox, &parent).await;
    let mut relay = crate::relay_session::window_session(&endpoint(&sandbox), &parent).await;
    let again = crate::relay_session::take_reports(&mut relay, 1, 0).await;
    assert_eq!(
        crate::relay_session::report_keys(&again.reports),
        keys,
        "the next daemon must hand out the report the relay never received, with the same \
         sender and at_ms; daemon log:\n{}",
        sandbox.daemon_log()
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
    let reports = wait_for_report(&mut as_parent).await;
    assert!(
        reports.is_empty(),
        "a report already read must not be read again: {reports:?}"
    );
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

/// A chat post whose daemon ends once the post is committed, before any inbox shows it,
/// was never answered to its poster; the next daemon still hands it to its destination
/// once, and the room records that delivery as taken rather than leaving it pending.
#[tokio::test]
async fn a_chat_post_committed_as_the_daemon_ends_is_taken_once_by_the_next_daemon() {
    const CHAT_MARK: &str = "PERSIST-COMMITTED-7H3";
    let sandbox = Sandbox::new("persist-chat-committed");
    let detections = write_chat_profile(&sandbox);

    let mut first =
        start_chatting_with(&sandbox, &detections, Some(FaultPoint::ChatPostCommitted)).await;
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
    let ended = first.wait_for_fault_end().await;
    assert!(
        !ended.success(),
        "the daemon must end itself once the post is committed: {ended:?}"
    );

    let mut next = start_chatting(&sandbox, &detections).await;
    listed(&mut next, &sandbox, &window_id).await;
    let seq = chat_seq_of(&mut next.client, &sandbox, CHAT_MARK).await;
    let taken = take(&mut next, &sandbox, &window_id).await;
    assert_eq!(
        count(&taken, CHAT_MARK),
        1,
        "the window's door must hand out the post committed before the daemon ended once: \
         {taken:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    until_chat_taken(&mut next.client, &sandbox, seq, &window_id).await;
}

/// A chat post taken through its window's door as the daemon ends: the parcel leaving the
/// inbox and the room's record of the delivery are one change. Whichever way the next
/// daemon finds it, the delivery is pending while the post is still kept for the window,
/// and taken once it is not; never pending with nothing kept.
#[tokio::test]
async fn a_parcel_ending_as_the_daemon_ends_leaves_its_chat_delivery_and_its_row_in_step() {
    const CHAT_MARK: &str = "PERSIST-ENDING-4R9";
    let sandbox = Sandbox::new("persist-chat-ending");
    let detections = write_chat_profile(&sandbox);

    let mut first = start_chatting_with(
        &sandbox,
        &detections,
        Some(FaultPoint::ParcelEndHalfSettled),
    )
    .await;
    let window_id = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut first, &window_id).await;
    let seq = post_chat(&mut first.client, format!("@{window_id} {CHAT_MARK}")).await;
    // The door's printing ends the parcel; the daemon ends before it could settle.
    let mut door = connect(&sandbox).await;
    send(&mut door, &door_read_take(&window_id)).await;
    match read_response(&mut door).await {
        DaemonMessage::Delivery { output, .. } => assert_eq!(count(&output, CHAT_MARK), 1),
        other => panic!("expected the door's delivery, got {other:?}"),
    }
    send(&mut door, &printed()).await;
    let ended = first.wait_for_fault_end().await;
    assert!(
        !ended.success(),
        "the daemon must end itself as the parcel ends: {ended:?}"
    );

    let mut next = start_chatting(&sandbox, &detections).await;
    listed(&mut next, &sandbox, &window_id).await;
    let state = chat_delivery_state(&mut next.client, seq, &window_id).await;
    // The room keeps the body alone; only the envelope of a kept parcel carries it as JSON.
    let kept = store_holds(&store_db(&sandbox), &format!("{CHAT_MARK}\""));
    assert!(
        matches!(
            (state.as_deref(), kept),
            (Some("queued"), true) | (Some("delivered"), false)
        ),
        "the delivery is {state:?} while the post is {} for the window; daemon log:\n{}",
        if kept { "kept" } else { "not kept" },
        sandbox.daemon_log()
    );
    if kept {
        let taken = take(&mut next, &sandbox, &window_id).await;
        assert_eq!(count(&taken, CHAT_MARK), 1, "taken: {taken:?}");
        until_chat_taken(&mut next.client, &sandbox, seq, &window_id).await;
    }
}

/// A plan task, as the planning window writes it, for a window of [`PROFILE`].
fn report_task(task_id: &str) -> yagura_protocol::PlanTaskSpec {
    yagura_protocol::PlanTaskSpec {
        task_id: task_id.to_owned(),
        title: "do the thing".to_owned(),
        role: None,
        profile: Some(PROFILE.to_owned()),
        model: None,
        effort: None,
        depends_on: Vec::new(),
        writes: vec![format!("{task_id}.txt")],
        instruction: None,
    }
}

/// A task window whose daemon ends while its report is half written: the plan ledger
/// and the parent's reports never disagree afterwards. The next daemon shows the report
/// both in `get_plan` and to the parent's `wait_for_report`, or in neither.
#[tokio::test]
async fn a_task_report_half_written_as_the_daemon_ends_shows_in_both_places_or_neither() {
    const REPORT_MARK: &str = "PERSIST-TASK-REPORT-8N2";
    let sandbox = Sandbox::new("persist-task-report");
    write_delivering_profile(&sandbox);

    // A plan that needs no approval, so its task window opens at once.
    let playbook = sandbox
        .tree
        .make_dir("persist-plan-playbook")
        .join("playbook.toml");
    std::fs::write(
        &playbook,
        "[approval]\nauto_approve = [\"open_window\", \"close_window\"]\n\n[plans]\nrequire_approval = false\n",
    )
    .unwrap();
    let playbook = playbook.to_str().expect("the playbook path must be UTF-8");
    let mut first = sandbox
        .start_from(&[
            ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
            ("YAGURA_PLAYBOOK", playbook),
            (
                fault_point::POINT_ENV,
                FaultPoint::TaskReportHalfWritten.name(),
            ),
        ])
        .await;
    let parent = first.open_profile_window(&sandbox, PROFILE).await;
    let mut as_parent = connect(&sandbox).await;
    identify(&mut as_parent, &parent).await;
    send(
        &mut as_parent,
        &ClientMessage::CreatePlan {
            title: "plan".to_owned(),
            tasks: vec![report_task("t1")],
            ticket: None,
        },
    )
    .await;
    let plan_id = match read_response(&mut as_parent).await {
        DaemonMessage::PlanDetail { plan } => plan.plan_id,
        other => panic!("expected plan_detail, got {other:?}"),
    };
    send(
        &mut as_parent,
        &ClientMessage::OpenTaskWindow {
            plan_id: plan_id.clone(),
            task_id: "t1".to_owned(),
            name: None,
        },
    )
    .await;
    let task_window = match read_response(&mut as_parent).await {
        DaemonMessage::TaskWindowOpened { window_id, .. } => window_id,
        other => panic!("expected task_window_opened, got {other:?}"),
    };
    let mut as_task = connect(&sandbox).await;
    identify(&mut as_task, &task_window).await;
    send(
        &mut as_task,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: REPORT_MARK.to_owned(),
            path: None,
        },
    )
    .await;
    let ended = first.wait_for_fault_end().await;
    assert!(
        !ended.success(),
        "the daemon must end itself while the task report is half written: {ended:?}"
    );

    let mut next = sandbox
        .start_from(&[
            ("YAGURA_STATUS_TOKEN_OVERRIDE", VALID_TOKEN),
            ("YAGURA_PLAYBOOK", playbook),
        ])
        .await;
    listed(&mut next, &sandbox, &parent).await;
    let mut as_parent = connect(&sandbox).await;
    identify(&mut as_parent, &parent).await;
    send(
        &mut as_parent,
        &ClientMessage::GetPlan {
            plan_id: plan_id.clone(),
        },
    )
    .await;
    let plan = match read_response(&mut as_parent).await {
        DaemonMessage::PlanDetail { plan } => plan,
        other => panic!("expected plan_detail, got {other:?}"),
    };
    let in_plan: Vec<String> = plan
        .tasks
        .iter()
        .find(|task| task.task_id == "t1")
        .and_then(|task| task.report.as_ref())
        .map(|report| report.summary.clone())
        .into_iter()
        .collect();
    let reports = wait_for_report(&mut as_parent).await;
    let to_parent: Vec<String> = reports
        .iter()
        .map(|report| report.summary.clone())
        .collect();
    assert_eq!(
        in_plan,
        to_parent,
        "get_plan shows the task report as {in_plan:?} while the parent reads {to_parent:?}; \
         daemon log:\n{}",
        sandbox.daemon_log()
    );
}

/// The door a CLI's turn-start hook runs to take what waits in its window's inbox.
const TAKE_DOOR: &str = "--deliver-take";

/// Runs the daemon executable as the hook door `door` of `window_id`, the way a CLI's
/// hook runs it against the sandbox's instance, and returns how it ended.
async fn run_door(sandbox: &Sandbox, window_id: &str, door: &str) -> Output {
    run_door_with(sandbox, window_id, &[door]).await
}

/// [`run_door`] for a door that takes arguments (`--hook <profile> <event>`).
async fn run_door_with(sandbox: &Sandbox, window_id: &str, args: &[&str]) -> Output {
    let mut command = Command::new(daemon_bin());
    command
        .args(args)
        .env("YAGURA_INSTANCE", &sandbox.instance)
        .env("YAGURA_WINDOW_ID", window_id)
        .env("YAGURA_STATUS_TOKEN", VALID_TOKEN)
        .env_remove("YAGURA_STORE_DB")
        .env_remove(fault_point::POINT_ENV)
        .env_remove(fault_point::SIGNAL_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        command.env("LOCALAPPDATA", &sandbox.state_home);
    } else {
        command
            .env("XDG_STATE_HOME", &sandbox.state_home)
            .env("XDG_DATA_HOME", sandbox.tree.make_dir("data"))
            .env("XDG_CONFIG_HOME", sandbox.tree.make_dir("config"))
            .env("HOME", sandbox.tree.make_dir("home"));
    }
    let child = command.spawn().expect("spawn the door");
    let pid = child.id();
    let ended = tokio::time::timeout(
        HANG_GUARD,
        tokio::task::spawn_blocking(move || child.wait_with_output()),
    )
    .await;
    match ended {
        Ok(joined) => joined
            .expect("the door's waiter must join")
            .expect("the door must be waitable"),
        Err(_) => {
            if let Some(handle) = yagura_os::process_start(pid)
                .ok()
                .flatten()
                .and_then(|start| yagura_os::ProcessHandle::open(pid, &start).ok().flatten())
            {
                let _ = handle.kill();
            }
            panic!("the door {pid} did not end within {HANG_GUARD:?}");
        }
    }
}

/// A door prints a chat post before it says so, and ends without waiting for an answer.
/// When its daemon ends between that notice and settling the take, the door has printed the
/// post and succeeded, and the post is still kept: the next daemon's door hands it out
/// again, once. Once a door has printed it under a daemon that settled the take, the room
/// records it as taken and no later daemon hands it out again.
#[tokio::test]
async fn a_door_whose_daemon_ends_before_settling_has_printed_and_the_next_daemon_hands_it_again() {
    const CHAT_MARK: &str = "PERSIST-DOOR-5C8";
    let sandbox = Sandbox::new("persist-door");
    let detections = write_chat_profile(&sandbox);

    let mut first =
        start_chatting_with(&sandbox, &detections, Some(FaultPoint::DoorBeforeSettle)).await;
    let window_id = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut first, &window_id).await;
    let seq = post_chat(&mut first.client, format!("@{window_id} {CHAT_MARK}")).await;
    let (door, ended) = tokio::join!(
        run_door(&sandbox, &window_id, TAKE_DOOR),
        first.wait_for_fault_end()
    );
    assert!(
        !ended.success(),
        "the daemon must end itself once the door says it printed the post: {ended:?}"
    );

    let mut next = start_chatting(&sandbox, &detections).await;
    listed(&mut next, &sandbox, &window_id).await;
    let again = run_door(&sandbox, &window_id, TAKE_DOOR).await;
    let printed = String::from_utf8_lossy(&door.stdout);
    let printed_again = String::from_utf8_lossy(&again.stdout);
    assert_eq!(
        (
            count(&printed, CHAT_MARK),
            door.status.code(),
            count(&printed_again, CHAT_MARK),
            again.status.success(),
        ),
        (1, Some(0), 1, true),
        "(marks the first door printed, its exit code, marks the next daemon's door printed, \
         whether it succeeded); first door stdout {printed:?} stderr {:?}; next door stdout \
         {printed_again:?} stderr {:?}; daemon log:\n{}",
        String::from_utf8_lossy(&door.stderr),
        String::from_utf8_lossy(&again.stderr),
        sandbox.daemon_log()
    );
    assert_eq!(
        chat_envelope_id(&printed, CHAT_MARK),
        chat_envelope_id(&printed_again, CHAT_MARK),
        "the kept post is handed out again under the same id, so its reader can tell it is          the one it already has"
    );
    // The door ends without waiting for the take to settle, so the record follows it.
    until_chat_taken(&mut next.client, &sandbox, seq, &window_id).await;
    next.kill_keeping_windows();

    let mut last = start_chatting(&sandbox, &detections).await;
    listed(&mut last, &sandbox, &window_id).await;
    let after = run_door(&sandbox, &window_id, TAKE_DOOR).await;
    let printed_after = String::from_utf8_lossy(&after.stdout);
    assert_eq!(
        count(&printed_after, CHAT_MARK),
        0,
        "a post a door printed under a daemon that settled the take must not come out again: \
         {printed_after:?}"
    );
}

/// A door that ends before it printed a take it was handed leaves the post with the same
/// daemon: that daemon's next take hands it out once.
#[tokio::test]
async fn a_take_whose_door_ends_before_printing_is_handed_out_again_by_the_same_daemon() {
    const CHAT_MARK: &str = "PERSIST-DOOR-GONE-2L6";
    let sandbox = Sandbox::new("persist-door-gone");
    let detections = write_chat_profile(&sandbox);

    let mut daemon = start_chatting(&sandbox, &detections).await;
    let window_id = daemon.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut daemon, &window_id).await;
    post_chat(&mut daemon.client, format!("@{window_id} {CHAT_MARK}")).await;

    let mut door = connect(&sandbox).await;
    // Sent as the frame a door of revision 34 sends: it says it printed only once it has.
    let body = serde_json::json!({
        "type": "door_read",
        "window_id": window_id,
        "token": VALID_TOKEN,
        "door": { "kind": "take" },
    });
    door.write_all(&yagura_protocol::frame_encode(
        &serde_json::to_vec(&body).unwrap(),
    ))
    .await
    .expect("write the door's take");
    let handed = read_response(&mut door).await;
    let handed_marks = match &handed {
        DaemonMessage::Delivery { output, .. } => count(output, CHAT_MARK),
        _ => 0,
    };
    assert_eq!(
        handed_marks,
        1,
        "the door's take must hand it the post once: {handed:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
    // The door ends without printing.
    drop(door);

    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    let taken = loop {
        let taken = take(&mut daemon, &sandbox, &window_id).await;
        if count(&taken, CHAT_MARK) > 0 {
            break taken;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post a door never printed must come out of the same daemon again; daemon \
             log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(count(&taken, CHAT_MARK), 1, "taken: {taken:?}");
    let again = take(&mut daemon, &sandbox, &window_id).await;
    assert_eq!(
        count(&again, CHAT_MARK),
        0,
        "a post taken again must not come out a third time: {again:?}"
    );
}

/// The `--hook` door of a profile whose event takes: it runs on the `RunHook` request, not
/// on the `deliver_*` ones.
const HOOK_EVENT: &str = "TurnStart";

/// A hook event's take is a door like the others: when its daemon ends between the hook's
/// notice that it printed a chat post and settling the take, the hook has printed it and
/// succeeded, and the next daemon's hook hands the post out again, once.
#[tokio::test]
async fn a_hook_event_whose_daemon_ends_before_settling_has_printed_and_the_next_daemon_hands_it_again()
 {
    const CHAT_MARK: &str = "PERSIST-HOOK-DOOR-7Q4";
    let sandbox = Sandbox::new("persist-hook-door");
    let detections = write_chat_profile(&sandbox);
    let profile_file = sandbox.profiles_dir.join(format!("{CHAT_PROFILE}.toml"));
    let mut source = std::fs::read_to_string(&profile_file).expect("read the chat profile");
    source.push_str(&format!(
        "
[hook_events]
{HOOK_EVENT} = {{ deliver = \"take\" }}
"
    ));
    std::fs::write(&profile_file, source).expect("write the chat profile");
    let hook = ["--hook", CHAT_PROFILE, HOOK_EVENT];

    let mut first =
        start_chatting_with(&sandbox, &detections, Some(FaultPoint::DoorBeforeSettle)).await;
    let window_id = first.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut first, &window_id).await;
    let seq = post_chat(&mut first.client, format!("@{window_id} {CHAT_MARK}")).await;
    let (door, ended) = tokio::join!(
        run_door_with(&sandbox, &window_id, &hook),
        first.wait_for_fault_end()
    );
    assert!(
        !ended.success(),
        "the daemon must end itself once the hook says it printed the post: {ended:?}"
    );

    let mut next = start_chatting(&sandbox, &detections).await;
    listed(&mut next, &sandbox, &window_id).await;
    let again = run_door_with(&sandbox, &window_id, &hook).await;
    let printed = String::from_utf8_lossy(&door.stdout);
    let printed_again = String::from_utf8_lossy(&again.stdout);
    assert_eq!(
        (
            count(&printed, CHAT_MARK),
            door.status.success(),
            count(&printed_again, CHAT_MARK),
            again.status.success(),
        ),
        (1, true, 1, true),
        "(marks the first hook printed, whether it succeeded, marks the next daemon's hook          printed, whether it succeeded); first hook stdout {printed:?} stderr {:?}; next hook          stdout {printed_again:?} stderr {:?}; daemon log:
{}",
        String::from_utf8_lossy(&door.stderr),
        String::from_utf8_lossy(&again.stderr),
        sandbox.daemon_log()
    );
    // The hook ends without waiting for the take to settle, so the record follows it.
    until_chat_taken(&mut next.client, &sandbox, seq, &window_id).await;
    next.kill_keeping_windows();

    let mut last = start_chatting(&sandbox, &detections).await;
    listed(&mut last, &sandbox, &window_id).await;
    let after = run_door_with(&sandbox, &window_id, &hook).await;
    let printed_after = String::from_utf8_lossy(&after.stdout);
    assert_eq!(
        count(&printed_after, CHAT_MARK),
        0,
        "a post a hook printed under a daemon that settled the take must not come out again:          {printed_after:?}"
    );
}

/// A post a door holds goes on to the CLI even when its room's history goes meanwhile: the
/// door prints it, and settling it once the room no longer has the post updates nothing and
/// fails nothing, so its parcel row goes and it is not handed out again.
#[tokio::test]
async fn a_post_a_door_holds_when_its_rooms_history_goes_is_printed_and_settles_without_its_row() {
    const CHAT_MARK: &str = "PERSIST-CLEARED-HELD-8K3";
    let sandbox = Sandbox::new("persist-cleared-held");
    let detections = write_chat_profile(&sandbox);

    let mut daemon = start_chatting(&sandbox, &detections).await;
    let window_id = daemon.open_profile_window(&sandbox, CHAT_PROFILE).await;
    make_ready(&mut daemon, &window_id).await;
    post_chat(&mut daemon.client, format!("@{window_id} {CHAT_MARK}")).await;
    until_kept(&sandbox, &[&format!("{CHAT_MARK}\"")]).await;

    let mut door = connect(&sandbox).await;
    send(&mut door, &door_read_take(&window_id)).await;
    match read_response(&mut door).await {
        DaemonMessage::Delivery { output, .. } => assert_eq!(
            count(&output, CHAT_MARK),
            1,
            "the door must be handed the post: {output:?}"
        ),
        other => panic!("expected the door's delivery, got {other:?}"),
    }

    // The room's history goes while the door holds the post, as clearing the room removes it.
    let mut chat = yagura_daemon::chat_store::ChatStore::open(&store_db(&sandbox))
        .expect("a second connection to the store must open");
    chat.clear_history("main")
        .expect("clearing the room's history must succeed");
    drop(chat);

    send(&mut door, &printed()).await;
    drop(door);

    let db = store_db(&sandbox);
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while store_holds(&db, &format!("{CHAT_MARK}\"")) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a printed post must leave no parcel row once settled; daemon log:\n{}",
            sandbox.daemon_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let again = take(&mut daemon, &sandbox, &window_id).await;
    assert_eq!(
        count(&again, CHAT_MARK),
        0,
        "a printed post must not come out again: {again:?}; daemon log:\n{}",
        sandbox.daemon_log()
    );
}
