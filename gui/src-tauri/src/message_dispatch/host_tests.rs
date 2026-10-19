//! What reaches the frame mailbox, the WebView and the request in flight when
//! the daemon sends something, against a host that records and a daemon that
//! is scripted.

use serde_json::json;
use yagura_protocol::{ClientMessage, CursorShape, CursorState, ViewState, WithdrawReason};
use yagura_test_support::ipc::reply_within;

use super::*;
use crate::events::{EVENT_CHAT_HISTORY_CLEARED, EVENT_SUPERSEDED};
use crate::host::fake::{FakeHost, Reaction, ScriptedDaemon, WAIT_LIMIT, error_frame};

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

fn attached() -> DaemonMessage {
    DaemonMessage::Attached {
        child_exited: false,
        exit_code: None,
    }
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
    let client = Arc::new(IpcClient::new_window());

    let superseded = dispatch(
        &host,
        &client,
        DaemonMessage::Frame(frame(1)),
        Some("w1".to_owned()),
    )
    .await;

    assert!(!superseded);
    let delivered = reply_within(WAIT_LIMIT, "frames", host.mailbox().take()).await;
    assert_eq!(delivered, [("w1".to_owned(), frame(1))]);
    assert!(host.take_events().is_empty());
}

#[tokio::test]
async fn an_attached_windows_exit_is_emitted_to_the_webview() {
    let host = FakeHost::new("dispatch-exit");
    let client = Arc::new(IpcClient::new_window());

    let superseded = dispatch(
        &host,
        &client,
        DaemonMessage::Exit { exit_code: 3 },
        Some("w1".to_owned()),
    )
    .await;

    assert!(!superseded);
    assert_eq!(
        host.next_event(EVENT_EXIT).await,
        json!({ "window_id": "w1", "exit_code": 3 })
    );
}

#[tokio::test]
async fn pushes_of_the_daemon_become_the_events_the_webview_listens_for() {
    let host = FakeHost::new("dispatch-pushes");
    let client = Arc::new(IpcClient::new());

    let plan_superseded = dispatch(
        &host,
        &client,
        DaemonMessage::PlanChanged {
            plan_id: "p1".to_owned(),
        },
        None,
    )
    .await;
    let withdrawn_superseded = dispatch(
        &host,
        &client,
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
    let client = Arc::new(IpcClient::new());

    let superseded = dispatch(
        &host,
        &client,
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
        ClientMessage::DeletePlan { plan_id } => Reaction::reply(DaemonMessage::PlanDeleted {
            plan_id: plan_id.clone(),
        }),
        _ => None,
    });
    let host = FakeHost::served_by("reader-reply", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let deleted = client.delete_plan("p9".to_owned()).await;

    assert_eq!(deleted.expect("delete_plan"), "p9");
}

#[tokio::test]
async fn any_error_answers_the_request_of_a_control_connection() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::ListWindows => Reaction::reply(error_frame("window_not_found")),
        _ => None,
    });
    let host = FakeHost::served_by("control-error", &daemon).await;
    let client = Arc::new(IpcClient::new());
    client.connect(&host).await.expect("connect");

    let listed = client.list_windows().await;

    assert!(
        is_rejected_with(&listed, "window_not_found"),
        "got {listed:?}"
    );
}

#[tokio::test]
async fn an_internal_error_answers_the_attach_it_arrives_during() {
    let daemon = attaching_with(vec![error_frame(ERROR_CODE_INTERNAL)]);
    let host = FakeHost::served_by("attach-internal", &daemon).await;
    let client = Arc::new(IpcClient::new_window());

    let outcome = client.attach_window(&host, "w1", None).await;

    assert!(
        is_rejected_with(&outcome, ERROR_CODE_INTERNAL),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn an_error_that_may_answer_something_else_does_not_answer_the_attach() {
    let daemon = attaching_with(vec![error_frame("invalid_argument"), attached()]);
    let host = FakeHost::served_by("attach-other-error", &daemon).await;
    let client = Arc::new(IpcClient::new_window());

    let outcome = client.attach_window(&host, "w1", None).await;

    assert!(outcome.is_ok(), "got {outcome:?}");
}

#[tokio::test]
async fn a_superseded_error_ends_the_window_session_and_it_does_not_reconnect() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached()),
        ClientMessage::Resize { .. } => Reaction::reply(error_frame(ERROR_CODE_SUPERSEDED)),
        _ => None,
    });
    let host = FakeHost::served_by("superseded", &daemon).await;
    let client = Arc::new(IpcClient::new_window());
    client
        .attach_window(&host, "w1", None)
        .await
        .expect("attach");

    client.resize(100, 30).await.expect("resize");

    assert_eq!(
        host.next_event(EVENT_SUPERSEDED).await,
        json!({ "window_id": "w1", "reason": ERROR_CODE_SUPERSEDED })
    );
    assert!(!client.is_connected().await);
    assert_eq!(
        daemon.received().len(),
        3,
        "a reconnect would have made a second connection ask again"
    );
}
