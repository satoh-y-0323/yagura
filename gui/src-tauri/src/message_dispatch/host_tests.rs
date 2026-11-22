//! What reaches the frame mailbox, the WebView and the request in flight when
//! the daemon sends something, against a host that records and a daemon that
//! is scripted.

use serde_json::json;
use yagura_protocol::{
    ClientMessage, CursorShape, CursorState, PROTOCOL_REVISION, ViewState, WindowStatus,
    WithdrawReason,
};
use yagura_test_support::ipc::reply_within;

use super::*;
use crate::events::{EVENT_APPROVAL_RESET, EVENT_CHAT_HISTORY_CLEARED};
use crate::host::fake::{
    FakeHost, Reaction, ScriptedDaemon, SentFrame, announced, attached, error_frame,
    protocol_revision_at,
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

/// A wire revision one behind this build's.
const OLDER_REVISION: u32 = PROTOCOL_REVISION - 1;

/// What a daemon of [`OLDER_REVISION`] answers of its own accord, as such a
/// daemon answers it: under no correlation id.
fn older_housekeeping(request: &ClientMessage) -> Option<Reaction> {
    match request {
        ClientMessage::GetProtocolRevision => Reaction::sends(vec![SentFrame::Unkeyed(
            protocol_revision_at(OLDER_REVISION, None),
        )]),
        ClientMessage::RegisterApprover => Reaction::sends(vec![SentFrame::Unkeyed(
            DaemonMessage::ApproverRegistered {
                pending: Vec::new(),
            },
        )]),
        _ => None,
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
            SentFrame::Unreadable {
                tag: "plan_deleted",
                keyed: false,
            },
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
        daemon.probes(),
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
    assert_eq!(daemon.probes(), 2, "one reconnect");
}

#[tokio::test]
async fn an_error_answers_the_request_of_a_control_connection_under_its_key() {
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
}

// A daemon of this build's revision names the request an error answers, so
// an error under no key answers no request.
#[tokio::test]
async fn an_error_frame_that_names_no_request_does_not_answer_the_attach() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::sends(vec![
            SentFrame::Unkeyed(error_frame("invalid_argument")),
            SentFrame::Answer(attached(false)),
        ]),
        _ => None,
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
        _ => None,
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

/// What a control client did against one daemon: whether its connect opened,
/// the approval resets and the compatibility its announcements carried, whether
/// a window list was refused unwritten, what the daemon heard, and whether a
/// generation is still open at the end.
#[derive(Debug, Clone, PartialEq)]
struct ControlSeen {
    opened: bool,
    resets: Vec<serde_json::Value>,
    compatible: Vec<serde_json::Value>,
    list_refused: bool,
    received: Vec<ClientMessage>,
    open_at_end: bool,
}

/// Connects a control client to `daemon`, asks it for the window list, and
/// reports what happened.
async fn control_against(
    tag: &str,
    daemon: &ScriptedDaemon,
) -> (FakeHost, Arc<IpcClient>, ControlSeen) {
    let host = FakeHost::served_by(tag, daemon).await;
    let client = Arc::new(IpcClient::new());
    let opened = reply_within("the connect", client.connect(&host))
        .await
        .is_ok();
    let listed = reply_within("the window list", client.list_windows()).await;
    let events = host.take_events();
    let seen = ControlSeen {
        opened,
        resets: events
            .iter()
            .filter(|event| event.name == EVENT_APPROVAL_RESET)
            .map(|event| event.payload.clone())
            .collect(),
        compatible: announced(&events)
            .iter()
            .map(|payload| payload["compatible"].clone())
            .collect(),
        list_refused: matches!(listed, Err(IpcError::Unsupported { .. })),
        received: daemon.received(),
        open_at_end: client.protocol_revision().is_some(),
    };
    (host, client, seen)
}

/// A daemon that answers the revision probe with `answer` and lists no
/// windows.
fn probe_answered_with(answer: SentFrame) -> ScriptedDaemon {
    ScriptedDaemon::new(move |request| match request {
        ClientMessage::GetProtocolRevision => Reaction::sends(vec![answer.clone()]),
        ClientMessage::ListWindows => Reaction::reply(DaemonMessage::WindowList {
            windows: Vec::new(),
        }),
        _ => None,
    })
}

#[tokio::test]
async fn a_control_connection_to_a_daemon_of_another_wire_revision_opens_writing_nothing_but_the_probe()
 {
    let older = ScriptedDaemon::new(|request| match request {
        ClientMessage::ListWindows => Reaction::reply(DaemonMessage::WindowList {
            windows: Vec::new(),
        }),
        _ => older_housekeeping(request),
    });
    let (host, _client, older_seen) = control_against("incompatible-older", &older).await;
    let mut seen = vec![("older, unkeyed", older_seen)];
    // A daemon that does not know the probe, or answers it in a shape this
    // build cannot read, is no more one this build can talk to.
    for (label, tag, answer) in [
        (
            "newer, keyed",
            "incompatible-newer",
            SentFrame::Answer(protocol_revision_at(PROTOCOL_REVISION + 1, None)),
        ),
        (
            "probe unknown",
            "incompatible-unknown",
            SentFrame::Unkeyed(error_frame("unknown_type")),
        ),
        (
            "unreadable, unkeyed",
            "incompatible-unreadable",
            SentFrame::Unreadable {
                tag: "protocol_revision",
                keyed: false,
            },
        ),
        (
            "unreadable, keyed",
            "incompatible-unreadable-keyed",
            SentFrame::Unreadable {
                tag: "protocol_revision",
                keyed: true,
            },
        ),
    ] {
        let daemon = probe_answered_with(answer);
        let (_host, _client, outcome) = control_against(tag, &daemon).await;
        seen.push((label, outcome));
    }

    let incompatible = ControlSeen {
        opened: true,
        resets: vec![json!({ "requests": [] })],
        compatible: vec![json!(false)],
        list_refused: true,
        received: vec![ClientMessage::GetProtocolRevision],
        open_at_end: true,
    };
    let expected: Vec<_> = seen
        .iter()
        .map(|(label, _)| (*label, incompatible.clone()))
        .collect();
    assert_eq!(seen, expected);

    // With no daemon left on the endpoint, the end of that generation starts
    // one: a retry alone would find nothing to reach.
    host.set_endpoint(Some(FakeHost::new("incompatible-gone").endpoint_name()));
    older.hang_up();
    host.wait_for_spawns(1).await;
    assert_eq!(host.spawn_count(), 1);
}
