//! What reaches the frame mailbox, the WebView and the request in flight when
//! the daemon sends something, against a host that records and a daemon that
//! is scripted.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use yagura_protocol::{
    ClientMessage, CursorShape, CursorState, ViewState, WindowStatus, WithdrawReason,
};
use yagura_test_support::ipc::reply_within;

use super::*;
use crate::events::EVENT_CHAT_HISTORY_CLEARED;
use crate::host::fake::{
    FakeHost, Reaction, ScriptedDaemon, SentFrame, attached, error_frame, protocol_revision_at,
};
use crate::window_record::WindowRecord;

fn frame(seq: u64) -> Frame {
    Frame {
        seq,
        cols: 2,
        rows: 1,
        reset: true,
        last: true,
        attrs: Vec::new(),
        lines: Vec::new(),
        cursor: CursorState {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
        },
        view: ViewState {
            display_offset: 0,
            history_rows: 0,
            mouse_reporting: false,
            alt_screen: false,
        },
        overrides: Vec::new(),
    }
}

/// A generation over an in-memory stream that nobody answers on, with the
/// far end and the reader's ticket kept alive beside it: what one frame does
/// is measured apart from how frames are read.
fn idle_generation() -> (
    Arc<crate::ipc_client::Generation>,
    tokio::io::DuplexStream,
    impl Sized,
) {
    let (near, far) = tokio::io::duplex(64 * 1024);
    let (generation, reader) = crate::ipc_client::Generation::wired(near);
    (generation, far, reader)
}

/// The last wire revision whose daemon answers without a correlation id.
const BEFORE_CORRELATION_IDS: u32 = yagura_protocol::CORRELATION_ID_MIN_REVISION - 1;

/// What a daemon of [`BEFORE_CORRELATION_IDS`] answers of its own accord, as
/// such a daemon answers it: under no correlation id.
fn older_housekeeping(request: &ClientMessage) -> Option<Reaction> {
    match request {
        ClientMessage::GetProtocolRevision => Reaction::sends(vec![SentFrame::Unkeyed(
            protocol_revision_at(BEFORE_CORRELATION_IDS, None),
        )]),
        ClientMessage::RegisterApprover => Reaction::sends(vec![SentFrame::Unkeyed(
            DaemonMessage::ApproverRegistered {
                pending: Vec::new(),
            },
        )]),
        _ => None,
    }
}

/// How many connections asked `daemon` for its revision.
fn probes(daemon: &ScriptedDaemon) -> usize {
    daemon
        .received()
        .iter()
        .filter(|request| matches!(request, ClientMessage::GetProtocolRevision))
        .count()
}

/// A daemon that answers every attach with `answer`.
fn attaching_with(answer: Vec<DaemonMessage>) -> ScriptedDaemon {
    ScriptedDaemon::new(move |request| match request {
        ClientMessage::AttachView { .. } => Reaction::replies(answer.clone()),
        _ => None,
    })
}

fn is_rejected_with<T>(outcome: &Result<T, IpcError>, wanted: &str) -> bool {
    matches!(outcome, Err(IpcError::Rejected { code, .. }) if code == wanted)
}

#[tokio::test]
async fn an_attached_windows_frame_goes_to_the_mailbox() {
    let host = FakeHost::new("dispatch-frame");
    let (generation, _peer, _reader) = idle_generation();

    let superseded = dispatch(
        &host,
        &generation,
        None,
        DaemonMessage::Frame(frame(1)),
        Some("w1".to_owned()),
    )
    .await;

    assert!(!superseded);
    let delivered = reply_within("frames", host.mailbox().take()).await;
    assert_eq!(delivered, [("w1".to_owned(), frame(1))]);
    assert!(host.take_events().is_empty());
}

#[tokio::test]
async fn a_status_change_carries_its_exit_code_to_the_webview_and_names_none_without_one() {
    let host = FakeHost::new("dispatch-exit");
    let (generation, _peer, _reader) = idle_generation();
    let status_changed = |status, exit_code| DaemonMessage::StatusChanged {
        window_id: "w1".to_owned(),
        status,
        unknown_reason: None,
        status_rev: None,
        status_boot: None,
        task_report: None,
        exit_code,
    };

    let running = dispatch(
        &host,
        &generation,
        None,
        status_changed(WindowStatus::Running, None),
        None,
    )
    .await;
    let exited = dispatch(
        &host,
        &generation,
        None,
        status_changed(WindowStatus::Exited, Some(3)),
        None,
    )
    .await;

    assert!(!running && !exited);
    assert_eq!(
        host.next_event(EVENT_STATUS).await,
        json!({ "window_id": "w1", "status": "running" })
    );
    assert_eq!(
        host.next_event(EVENT_STATUS).await,
        json!({ "window_id": "w1", "status": "exited", "exit_code": 3 })
    );
}

#[tokio::test]
async fn pushes_of_the_daemon_become_the_events_the_webview_listens_for() {
    let host = FakeHost::new("dispatch-pushes");
    let (generation, _peer, _reader) = idle_generation();

    let plan_superseded = dispatch(
        &host,
        &generation,
        None,
        DaemonMessage::PlanChanged {
            plan_id: "p1".to_owned(),
        },
        None,
    )
    .await;
    let withdrawn_superseded = dispatch(
        &host,
        &generation,
        None,
        DaemonMessage::ApprovalWithdrawn {
            request_id: "r1".to_owned(),
            reason: WithdrawReason::Resolved,
        },
        None,
    )
    .await;

    assert!(!plan_superseded && !withdrawn_superseded);
    let events = host.take_events();
    assert_eq!(
        events.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        [EVENT_PLAN_CHANGED, EVENT_APPROVAL_WITHDRAWN]
    );
    assert_eq!(events[0].payload, json!({ "plan_id": "p1" }));
    assert_eq!(events[1].payload["request_id"], "r1");
}

#[tokio::test]
async fn a_cleared_chat_history_becomes_the_event_naming_its_room() {
    let host = FakeHost::new("dispatch-history-cleared");
    let (generation, _peer, _reader) = idle_generation();

    let superseded = dispatch(
        &host,
        &generation,
        None,
        DaemonMessage::ChatHistoryCleared {
            room_id: "r-1".to_owned(),
        },
        None,
    )
    .await;

    assert!(!superseded);
    assert_eq!(
        host.next_event(EVENT_CHAT_HISTORY_CLEARED).await,
        json!({ "room_id": "r-1" })
    );
}

#[tokio::test]
async fn clearing_and_deleting_a_chat_room_reach_the_daemon_and_return_the_room_it_answered() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::ClearChatRoom { room_id } => {
            Reaction::reply(DaemonMessage::ChatRoomCleared {
                room_id: room_id.clone(),
            })
        }
        ClientMessage::DeleteChatRoom { room_id } => {
            Reaction::reply(DaemonMessage::ChatRoomDeleted {
                room_id: room_id.clone(),
            })
        }
        _ => None,
    });
    let host = FakeHost::served_by("reader-room-removal", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    assert_eq!(
        client
            .clear_chat_room("r-1".to_owned())
            .await
            .expect("clear_chat_room"),
        "r-1"
    );
    assert_eq!(
        client
            .delete_chat_room("r-2".to_owned())
            .await
            .expect("delete_chat_room"),
        "r-2"
    );
}

#[tokio::test]
async fn a_room_in_use_rejection_reaches_the_caller_of_delete_chat_room() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::DeleteChatRoom { .. } => Reaction::reply(error_frame("room_in_use")),
        _ => None,
    });
    let host = FakeHost::served_by("reader-room-in-use", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let outcome = client.delete_chat_room("r-1".to_owned()).await;

    assert!(is_rejected_with(&outcome, "room_in_use"), "got {outcome:?}");
}

#[tokio::test]
async fn a_reply_reaches_the_request_waiting_for_it_through_the_reader() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::DeletePlan { plan_id } => Reaction::sends(vec![
            // Answers of the same kind, but not to this request.
            SentFrame::OtherKey(DaemonMessage::PlanDeleted {
                plan_id: "other".to_owned(),
            }),
            SentFrame::Unkeyed(DaemonMessage::PlanDeleted {
                plan_id: "unkeyed".to_owned(),
            }),
            // Unreadable, and under no key or another one: no answer at all.
            SentFrame::Raw(br#"{"type":"plan_deleted"}"#.to_vec()),
            SentFrame::Raw(
                br#"{"type":"plan_deleted","correlation_id":18446744073709551615}"#.to_vec(),
            ),
            SentFrame::Answer(DaemonMessage::PlanDeleted {
                plan_id: plan_id.clone(),
            }),
        ]),
        // Under the request's key, but of another kind: the daemon broke its
        // word, so the connection is not trusted further.
        ClientMessage::ListWindows => Reaction::reply(DaemonMessage::PlanDeleted {
            plan_id: "p9".to_owned(),
        }),
        _ => None,
    });
    let host = FakeHost::served_by("reader-reply", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let deleted = client.delete_plan("p9".to_owned()).await;
    let again = client.delete_plan("p10".to_owned()).await;

    assert_eq!(deleted.expect("delete_plan"), "p9");
    assert_eq!(again.expect("the same connection answers again"), "p10");
    assert_eq!(
        probes(&daemon),
        1,
        "what answers nothing keeps the connection"
    );

    let listed = reply_within("an answer of another kind", client.list_windows()).await;
    assert!(
        matches!(listed, Err(IpcError::ConnectionLost)),
        "got {listed:?}"
    );
    client
        .connect_existing(&host)
        .await
        .expect("the reconnect opens");
    assert_eq!(probes(&daemon), 2, "one reconnect");
}

#[tokio::test]
async fn an_error_answers_the_request_of_a_control_connection_under_its_key_or_from_an_older_daemon()
 {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::ListWindows => Reaction::sends(vec![
            SentFrame::Unkeyed(error_frame("window_not_found")),
            SentFrame::Answer(DaemonMessage::WindowList {
                windows: Vec::new(),
            }),
        ]),
        ClientMessage::DeletePlan { .. } => Reaction::reply(error_frame("plan_not_found")),
        _ => None,
    });
    let host = FakeHost::served_by("control-error", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let listed = client.list_windows().await;
    let deleted = client.delete_plan("p1".to_owned()).await;

    assert_eq!(
        listed.expect("an error under no key answers nothing"),
        Vec::new()
    );
    assert!(
        is_rejected_with(&deleted, "plan_not_found"),
        "got {deleted:?}"
    );

    // A daemon that returns no key: the one request in flight is what any
    // error answers.
    let older = ScriptedDaemon::new(|request| match request {
        ClientMessage::ListWindows => {
            Reaction::sends(vec![SentFrame::Unkeyed(error_frame("window_not_found"))])
        }
        _ => older_housekeeping(request),
    });
    let host = FakeHost::served_by("control-error-older", &older).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let listed = client.list_windows().await;

    assert_eq!(client.protocol_revision(), Some(BEFORE_CORRELATION_IDS));
    assert!(
        is_rejected_with(&listed, "window_not_found"),
        "got {listed:?}"
    );
}

#[tokio::test]
async fn an_internal_error_answers_the_attach_it_arrives_during() {
    let daemon = attaching_with(vec![error_frame(ERROR_CODE_INTERNAL)]);
    let host = FakeHost::served_by("attach-internal", &daemon).await;
    let client = Arc::new(IpcClient::new_window(Arc::new(WindowRecord::new())));

    let outcome = client.attach_window(&host, "w1", None).await;

    assert!(
        is_rejected_with(&outcome, ERROR_CODE_INTERNAL),
        "got {outcome:?}"
    );

    // A daemon that returns no key leaves the error to be weighed by its
    // code, and `internal` is one the attach itself can produce.
    let older = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => {
            Reaction::sends(vec![SentFrame::Unkeyed(error_frame(ERROR_CODE_INTERNAL))])
        }
        _ => older_housekeeping(request),
    });
    let host = FakeHost::served_by("attach-internal-older", &older).await;
    let client = Arc::new(IpcClient::new_window(Arc::new(WindowRecord::new())));

    let outcome = reply_within("the attach", client.attach_window(&host, "w1", None)).await;

    assert!(
        is_rejected_with(&outcome, ERROR_CODE_INTERNAL),
        "an older daemon: got {outcome:?}"
    );
}

// Only a daemon that returns no key leaves an error to be weighed by its
// code; a newer one names the request an error answers.
#[tokio::test]
async fn an_error_frame_that_names_no_request_does_not_answer_the_attach() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::sends(vec![
            SentFrame::Unkeyed(error_frame("invalid_argument")),
            SentFrame::Unkeyed(attached(false)),
        ]),
        _ => older_housekeeping(request),
    });
    let host = FakeHost::served_by("attach-other-error", &daemon).await;
    let client = Arc::new(IpcClient::new_window(Arc::new(WindowRecord::new())));

    let outcome = client.attach_window(&host, "w1", None).await;

    assert!(outcome.is_ok(), "got {outcome:?}");

    // A takeover is the connection's end, not the attach's answer: the attach
    // ends with the connection, and the window is reported as taken over.
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => {
            Reaction::sends(vec![SentFrame::Unkeyed(error_frame(ERROR_CODE_SUPERSEDED))])
        }
        _ => older_housekeeping(request),
    });
    let host = FakeHost::served_by("attach-superseded", &daemon).await;
    let client = Arc::new(IpcClient::new_window(Arc::new(WindowRecord::new())));

    let outcome = reply_within("the attach", client.attach_window(&host, "w1", None)).await;

    assert!(
        matches!(outcome, Err(IpcError::ConnectionLost)),
        "got {outcome:?}"
    );
    let link = loop {
        let link = host.next_link("w1").await;
        if link.state == "superseded" {
            break link;
        }
    };
    assert_eq!(link.reason, None);
}

#[tokio::test]
async fn a_superseded_error_ends_the_window_session_and_it_does_not_reconnect() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
        ClientMessage::Resize { .. } => Reaction::reply(error_frame(ERROR_CODE_SUPERSEDED)),
        _ => None,
    });
    let host = FakeHost::served_by("superseded", &daemon).await;
    let client = Arc::new(IpcClient::new_window(Arc::new(WindowRecord::new())));
    client
        .attach_window(&host, "w1", None)
        .await
        .expect("attach");

    client.resize(100, 30).await.expect("resize");

    let superseded = loop {
        let link = host.next_link("w1").await;
        if link.state == "superseded" {
            break link;
        }
    };
    assert_eq!(superseded.reason, None);
    assert!(!client.is_connected().await);
    assert_eq!(
        daemon.received().len(),
        3,
        "a reconnect would have made a second connection ask again"
    );
}

// A daemon before the interrupt outcome existed answers an interrupt in a
// shape this build cannot read. The answer still ends the request it answers,
// by its tag, and the connection goes on.
#[tokio::test]
async fn an_answer_an_older_daemon_sends_in_a_shape_this_build_cannot_read_ends_the_request_it_answers()
 {
    let lists = AtomicUsize::new(0);
    let daemon = ScriptedDaemon::new(move |request| match request {
        ClientMessage::InterruptWindow { .. } => Reaction::sends(vec![
            // Unreadable, and not the kind awaited: no answer at all.
            SentFrame::Raw(br#"{"type":"plan_deleted"}"#.to_vec()),
            SentFrame::Raw(br#"{"type":"window_interrupted","window_id":"w1"}"#.to_vec()),
        ]),
        ClientMessage::ListWindows => match lists.fetch_add(1, Ordering::SeqCst) {
            // The list is what the caller reads, and it is missing.
            0 => Reaction::sends(vec![SentFrame::Raw(br#"{"type":"window_list"}"#.to_vec())]),
            _ => Reaction::sends(vec![SentFrame::Unkeyed(DaemonMessage::WindowList {
                windows: Vec::new(),
            })]),
        },
        _ => older_housekeeping(request),
    });
    let host = FakeHost::served_by("older-unreadable-answer", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");
    assert_eq!(client.protocol_revision(), Some(BEFORE_CORRELATION_IDS));

    let interrupted = reply_within("the interrupt", client.interrupt_window("w1")).await;
    let unreadable = reply_within("the unreadable list", client.list_windows()).await;
    let listed = reply_within("the next list", client.list_windows()).await;

    assert!(interrupted.is_ok(), "got {interrupted:?}");
    assert!(
        matches!(unreadable, Err(IpcError::UnexpectedReply)),
        "got {unreadable:?}"
    );
    assert_eq!(listed.expect("the same connection answers"), Vec::new());
    assert_eq!(probes(&daemon), 1, "the connection goes on");
}
