//! The pool against a host that records and a daemon that is scripted: which
//! requests are allowed through, and when a daemon is started.

use yagura_protocol::DaemonMessage;

use super::*;
use crate::host::fake::{FakeHost, Reaction, ScriptedDaemon, error_frame, window_info};

/// A daemon that refuses every approver registration.
fn refusing_daemon() -> ScriptedDaemon {
    ScriptedDaemon::new(|request| match request {
        ClientMessage::RegisterApprover => Reaction::reply(error_frame("auth_failed")),
        _ => None,
    })
}

/// A daemon that carries out every close and interrupt it is asked for.
fn obliging_daemon() -> ScriptedDaemon {
    ScriptedDaemon::new(|request| match request {
        ClientMessage::CloseWindow { window_id, .. } => {
            Reaction::reply(DaemonMessage::WindowClosed {
                window_id: window_id.clone(),
            })
        }
        ClientMessage::InterruptWindow { window_id } => {
            Reaction::reply(DaemonMessage::WindowInterrupted {
                window_id: window_id.clone(),
            })
        }
        _ => None,
    })
}

/// Whether the daemon was sent nothing but what every connection sends of its
/// own accord.
fn heard_only_housekeeping(daemon: &ScriptedDaemon) -> bool {
    daemon.received().iter().all(|request| {
        matches!(
            request,
            ClientMessage::RegisterApprover | ClientMessage::GetProtocolRevision
        )
    })
}

#[tokio::test]
async fn a_close_is_refused_and_nothing_is_sent_while_the_approver_registration_is_refused() {
    let daemon = refusing_daemon();
    let host = FakeHost::served_by("close-refused", &daemon).await;
    let pool = ConnectionPool::new();

    let outcome = pool.close_window(&host, "w1", None).await;

    assert!(
        matches!(outcome, Err(IpcError::ApproverNotRegistered)),
        "got {outcome:?}"
    );
    assert!(heard_only_housekeeping(&daemon), "{:?}", daemon.received());
}

#[tokio::test]
async fn an_interrupt_is_refused_and_nothing_is_sent_while_the_approver_registration_is_refused() {
    let daemon = refusing_daemon();
    let host = FakeHost::served_by("interrupt-refused", &daemon).await;
    let pool = ConnectionPool::new();

    let outcome = pool.interrupt_window(&host, "w1").await;

    assert!(
        matches!(outcome, Err(IpcError::ApproverNotRegistered)),
        "got {outcome:?}"
    );
    assert!(heard_only_housekeeping(&daemon), "{:?}", daemon.received());
}

#[tokio::test]
async fn a_registered_connection_gets_its_close_and_interrupt_through() {
    let daemon = obliging_daemon();
    let host = FakeHost::served_by("registered", &daemon).await;
    let pool = ConnectionPool::new();

    let closed = pool.close_window(&host, "w1", Some(true)).await;
    let interrupted = pool.interrupt_window(&host, "w2").await;

    assert!(matches!(closed, Ok(CloseOutcome::Closed)), "got {closed:?}");
    assert!(interrupted.is_ok(), "got {interrupted:?}");
    let heard = daemon.received();
    assert!(heard.contains(&ClientMessage::CloseWindow {
        window_id: "w1".to_owned(),
        force: Some(true),
    }));
    assert!(heard.contains(&ClientMessage::InterruptWindow {
        window_id: "w2".to_owned(),
    }));
}

#[tokio::test]
async fn control_requests_reach_the_daemon_and_return_what_it_answers() {
    let summary = PlanSummary {
        plan_id: "p1".to_owned(),
        title: "release".to_owned(),
        owner_window: None,
        task_count: 2,
        review: None,
        unseen_changes: false,
    };
    let daemon = ScriptedDaemon::new({
        let summary = summary.clone();
        move |request| match request {
            ClientMessage::ListWindows => Reaction::reply(DaemonMessage::WindowList {
                windows: vec![window_info("w1", WindowStatus::Running)],
            }),
            ClientMessage::ListPlans => Reaction::reply(DaemonMessage::PlanList {
                plans: vec![summary.clone()],
            }),
            ClientMessage::DeletePlan { plan_id } => Reaction::reply(DaemonMessage::PlanDeleted {
                plan_id: plan_id.clone(),
            }),
            ClientMessage::RenameWindow { window_id, .. } => {
                Reaction::reply(DaemonMessage::WindowRenamed {
                    window_id: window_id.clone(),
                    name: "build (2)".to_owned(),
                })
            }
            _ => None,
        }
    });
    let host = FakeHost::served_by("control-requests", &daemon).await;
    let pool = ConnectionPool::new();

    assert_eq!(
        pool.list_windows(&host).await.expect("list_windows"),
        vec![window_info("w1", WindowStatus::Running)]
    );
    assert_eq!(
        pool.list_plans(&host).await.expect("list_plans"),
        vec![summary]
    );
    assert_eq!(
        pool.delete_plan(&host, "p1".to_owned())
            .await
            .expect("delete_plan"),
        "p1"
    );
    assert_eq!(
        pool.rename_window(&host, "w1", "build")
            .await
            .expect("rename_window"),
        "build (2)"
    );
    assert_eq!(host.spawn_count(), 0);
}

#[tokio::test]
async fn a_request_that_needs_a_daemon_starts_one_when_none_runs() {
    let daemon = ScriptedDaemon::quiet();
    let host = FakeHost::new("starts-for-request");
    host.starts_daemon(&daemon);
    let pool = ConnectionPool::new();

    pool.ensure_daemon_running(&host)
        .await
        .expect("ensure_daemon_running");

    assert_eq!(host.spawn_count(), 1);
    assert!(pool.has_live_control_connection().await);
}

#[tokio::test]
async fn the_control_connection_is_started_only_when_the_caller_asks_for_a_start() {
    let host = FakeHost::new("control-modes");
    let pool = ConnectionPool::new();

    let existing = pool
        .control_connected(&host, ControlConnect::Existing)
        .await;
    assert!(
        matches!(existing, Err(IpcError::NotConnected)),
        "got {existing:?}"
    );
    assert_eq!(host.spawn_count(), 0);

    let spawning = pool.control_connected(&host, ControlConnect::Spawn).await;
    assert!(
        matches!(spawning, Err(IpcError::DaemonSpawn)),
        "got {spawning:?}"
    );
    assert_eq!(host.spawn_count(), 1);
}

#[tokio::test]
async fn the_control_connection_handed_out_is_the_pools_own_and_connected() {
    let daemon = ScriptedDaemon::quiet();
    let host = FakeHost::served_by("control-handed-out", &daemon).await;
    let pool = ConnectionPool::new();

    let client = pool
        .control_connected(&host, ControlConnect::Existing)
        .await
        .expect("control_connected");

    assert!(Arc::ptr_eq(client, pool.control_connection()));
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn reconcile_attaches_each_window_the_daemon_reports_alive_and_drops_the_rest() {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(DaemonMessage::Attached {
            child_exited: false,
            exit_code: None,
        }),
        _ => None,
    });
    let host = FakeHost::served_by("reconcile", &daemon).await;
    let pool = ConnectionPool::new();
    let failed = pool
        .connect_windows(&["gone".to_owned()], |_id: String| async move {
            Ok::<_, IpcError>(Arc::new(IpcClient::new()))
        })
        .await;
    assert!(failed.is_empty());

    pool.reconcile(
        &host,
        &[
            window_info("w1", WindowStatus::Running),
            window_info("w2", WindowStatus::Exited),
        ],
    )
    .await;

    assert_eq!(pool.window_ids().await, ["w1"]);
    let attached = pool.get_window("w1").await.expect("w1 is tracked");
    assert!(attached.is_connected().await);
    assert_eq!(attached.window_id().await.as_deref(), Some("w1"));
    assert!(
        daemon
            .received()
            .iter()
            .all(|request| !matches!(request, ClientMessage::AttachView { window_id, .. } if window_id == "w2"))
    );
}
