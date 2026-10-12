//! The connection logic against a host that records and a daemon that is
//! scripted: connecting, losing the connection, and registering as an approver.

use std::sync::atomic::AtomicUsize;

use serde_json::json;
use yagura_protocol::{
    ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, DaemonMessage, WindowStatus,
};

use super::*;
use crate::host::fake::{FakeHost, Reaction, ScriptedDaemon, error_frame};

fn attached(child_exited: bool, exit_code: Option<i32>) -> DaemonMessage {
    DaemonMessage::Attached {
        child_exited,
        exit_code,
    }
}

/// A daemon that lets every window attach.
fn attaching_daemon() -> ScriptedDaemon {
    ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached(false, None)),
        _ => None,
    })
}

const ATTACH_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

/// A host served by `daemon` and a window client attached to `w1` through it.
async fn attached_window(daemon: &ScriptedDaemon, tag: &str) -> (FakeHost, Arc<IpcClient>) {
    let host = FakeHost::served_by(tag, daemon).await;
    let client = Arc::new(IpcClient::new_window());
    client
        .attach_window(&host, "w1", Some(ATTACH_SIZE))
        .await
        .expect("attach");
    (host, client)
}

fn approval_request(request_id: &str) -> ApprovalRequest {
    ApprovalRequest {
        request_id: request_id.to_owned(),
        tool: "close_window".to_owned(),
        requester: ApprovalRequester {
            kind: ApprovalRequesterKind::External,
            window_id: None,
            name: None,
        },
        target: Some(ApprovalTarget {
            window_id: "w1".to_owned(),
            name: "build".to_owned(),
            status: WindowStatus::Running,
        }),
        text: None,
        room: None,
    }
}

fn refused_with(outcome: &Result<Vec<ApprovalRequest>, IpcError>, wanted: &str) -> bool {
    matches!(outcome, Err(IpcError::Rejected { code, .. }) if code == wanted)
}

#[tokio::test]
async fn connecting_a_control_client_registers_it_and_settles_the_revision() {
    let daemon = ScriptedDaemon::quiet();
    let host = FakeHost::served_by("connect", &daemon).await;
    let client = Arc::new(IpcClient::new());

    client.connect(&host).await.expect("connect");

    assert!(client.is_connected().await);
    assert!(client.is_approver_registered());
    assert_eq!(
        client.protocol_revision(),
        Some(yagura_protocol::PROTOCOL_REVISION)
    );
    assert_eq!(
        daemon.received(),
        [
            ClientMessage::RegisterApprover,
            ClientMessage::GetProtocolRevision
        ]
    );
    assert_eq!(host.spawn_count(), 0);
}

#[tokio::test]
async fn a_daemon_that_is_not_running_is_not_started_by_connecting_to_an_existing_one() {
    let host = FakeHost::new("existing");
    let client = Arc::new(IpcClient::new());

    let outcome = client.connect_existing(&host).await;

    assert!(
        matches!(outcome, Err(IpcError::NotConnected)),
        "got {outcome:?}"
    );
    assert_eq!(host.spawn_count(), 0);
}

#[tokio::test]
async fn a_daemon_that_is_not_running_is_started_by_connecting_and_a_failed_start_is_reported() {
    let host = FakeHost::new("spawn-fails");
    let client = Arc::new(IpcClient::new());

    let outcome = client.connect(&host).await;

    assert!(
        matches!(outcome, Err(IpcError::DaemonSpawn)),
        "got {outcome:?}"
    );
    assert_eq!(host.spawn_count(), 1);
}

#[tokio::test]
async fn connecting_reaches_the_daemon_it_started() {
    let daemon = ScriptedDaemon::quiet();
    let host = FakeHost::new("spawned");
    host.starts_daemon(&daemon);
    let client = Arc::new(IpcClient::new());

    client.connect(&host).await.expect("connect");

    assert_eq!(host.spawn_count(), 1);
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn a_connection_the_daemon_hangs_up_is_reported_as_disconnected_when_it_cannot_be_restored() {
    let daemon = attaching_daemon();
    let (host, client) = attached_window(&daemon, "lost").await;
    host.set_endpoint(None);

    daemon.hang_up();

    assert_eq!(
        host.next_event(EVENT_DISCONNECTED).await,
        json!({ "window_id": "w1", "reason": "reconnect_failed" })
    );
    assert!(!client.is_connected().await);
    assert_eq!(client.window_id().await, None);
}

#[tokio::test]
async fn a_lost_connection_is_restored_and_the_window_attached_again_as_it_was() {
    let daemon = attaching_daemon();
    let (host, client) = attached_window(&daemon, "restore").await;
    client
        .send_attached(&ClientMessage::SetVisible { visible: false })
        .await
        .expect("hide");
    let first_connection = daemon.received_at_least(3).await;
    assert_eq!(first_connection.len(), 3);

    daemon.hang_up();
    let all = daemon.received_at_least(6).await;

    assert_eq!(
        all[3..],
        [
            ClientMessage::GetProtocolRevision,
            ClientMessage::AttachView {
                window_id: "w1".to_owned(),
                size: Some(ATTACH_SIZE),
            },
            ClientMessage::SetVisible { visible: false },
        ]
    );
    assert!(client.is_connected().await);
    assert_eq!(client.window_id().await.as_deref(), Some("w1"));
    assert!(host.take_events().is_empty());
}

#[tokio::test]
async fn reattaching_to_a_child_that_exited_without_a_code_reports_the_exit_as_minus_one() {
    let attaches = Arc::new(AtomicUsize::new(0));
    let daemon = ScriptedDaemon::new({
        let attaches = Arc::clone(&attaches);
        move |request| match request {
            ClientMessage::AttachView { .. } => {
                let earlier = attaches.fetch_add(1, Ordering::SeqCst);
                Reaction::reply(attached(earlier > 0, None))
            }
            _ => None,
        }
    });
    let (host, _client) = attached_window(&daemon, "exited").await;

    daemon.hang_up();

    assert_eq!(
        host.next_event(EVENT_EXIT).await,
        json!({ "window_id": "w1", "exit_code": -1 })
    );
}

#[tokio::test]
async fn a_stale_readers_disconnect_leaves_the_current_connection_alone() {
    let daemon = attaching_daemon();
    let (host, client) = attached_window(&daemon, "stale").await;
    let stale = client.generation.load(Ordering::SeqCst) - 1;

    client.on_disconnect(&host, false, stale).await;

    assert!(client.is_connected().await);
    assert_eq!(client.window_id().await.as_deref(), Some("w1"));
    assert!(host.take_events().is_empty());
}

#[tokio::test]
async fn a_superseded_window_is_reported_and_never_reconnects() {
    let daemon = attaching_daemon();
    let (host, client) = attached_window(&daemon, "superseded").await;
    let generation = client.generation.load(Ordering::SeqCst);

    client.on_disconnect(&host, true, generation).await;

    assert_eq!(
        host.next_event(EVENT_SUPERSEDED).await,
        json!({ "window_id": "w1", "reason": ERROR_CODE_SUPERSEDED })
    );
    assert!(!client.is_connected().await);
    assert_eq!(
        daemon.received().len(),
        2,
        "a reconnect would have made a second connection ask again"
    );
}

#[tokio::test]
async fn a_registration_the_daemon_accepts_hands_the_pending_requests_to_the_webview() {
    let pending = vec![approval_request("a"), approval_request("b")];
    let daemon = ScriptedDaemon::new({
        let pending = pending.clone();
        move |request| match request {
            ClientMessage::RegisterApprover => Reaction::reply(DaemonMessage::ApproverRegistered {
                pending: pending.clone(),
            }),
            _ => None,
        }
    });
    let host = FakeHost::served_by("accepted", &daemon).await;
    let client = Arc::new(IpcClient::new());

    client.connect(&host).await.expect("connect");

    let events: Vec<_> = host
        .take_events()
        .into_iter()
        .map(|event| (event.name, event.payload))
        .collect();
    assert_eq!(
        events,
        [
            (
                EVENT_APPROVAL_RESET.to_owned(),
                json!({ "requests": pending })
            ),
            (
                EVENT_APPROVAL_REQUESTED.to_owned(),
                json!({ "request": pending[0] })
            ),
            (
                EVENT_APPROVAL_REQUESTED.to_owned(),
                json!({ "request": pending[1] })
            ),
        ]
    );
    assert!(client.is_approver_registered());
    assert_eq!(client.sync_approvals(&host).await.expect("sync"), pending);
    assert!(host.take_events().is_empty(), "a pull emits nothing");
}

#[tokio::test]
async fn a_refused_registration_is_reported_once_per_generation() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::RegisterApprover => Reaction::reply(error_frame("auth_failed")),
        _ => None,
    });
    let host = FakeHost::served_by("refused", &daemon).await;
    let client = Arc::new(IpcClient::new());

    client.connect(&host).await.expect("connect");
    let first = client.sync_approvals(&host).await;
    let second = client.sync_approvals(&host).await;

    assert!(refused_with(&first, "auth_failed"), "got {first:?}");
    assert!(refused_with(&second, "auth_failed"), "got {second:?}");
    let refusals: Vec<_> = host
        .take_events()
        .into_iter()
        .filter(|event| event.name == EVENT_APPROVAL_REGISTRATION_REFUSED)
        .collect();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].payload, json!({}));
    assert_eq!(client.approver_registration_due(), None);
    assert!(!client.is_approver_registered());
}

#[tokio::test]
async fn a_registration_refused_for_another_reason_stays_owed_and_is_not_reported() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::RegisterApprover => Reaction::reply(error_frame(ERROR_CODE_INTERNAL)),
        _ => None,
    });
    let host = FakeHost::served_by("internal", &daemon).await;
    let client = Arc::new(IpcClient::new());

    client.connect(&host).await.expect("connect");
    let pulled = client.sync_approvals(&host).await;

    assert!(refused_with(&pulled, ERROR_CODE_INTERNAL), "got {pulled:?}");
    assert!(host.take_events().is_empty());
    assert_eq!(client.approver_registration_due(), Some(1));
}
