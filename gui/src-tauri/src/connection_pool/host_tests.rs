//! The pool against a host that records and a daemon that is scripted: which
//! requests are allowed through, and when a daemon is started.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio::sync::Notify;
use yagura_protocol::{DaemonMessage, MAX_PASTE_BYTES, StatusBoot};
use yagura_test_support::ipc::reply_within;

use super::*;
use crate::constants::RECONNECT_ATTEMPT_LIMIT;
use crate::events::{EVENT_DAEMON_CONNECTED, EVENT_EXIT};
use crate::host::fake::{
    FakeHost, Link, Reaction, ScriptedDaemon, announced, attach_pane, attached, brief, connected,
    error_frame, probes_answered_in, sample_palette, text, window_info,
};
use crate::ipc_client::ERROR_CODE_INTERNAL;
use crate::window_record::MAX_PENDING_INPUT_BYTES;

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
                outcome: yagura_protocol::InterruptOutcome::Interrupted,
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
    pool.track_window_list(&[window_info("w1", WindowStatus::Running)])
        .await;

    let closed = pool.close_window(&host, "w1", Some(true)).await;
    let interrupted = pool.interrupt_window(&host, "w2").await;
    let after_close = pool.set_window_visible("w1", false).await;

    assert!(matches!(closed, Ok(CloseOutcome::Closed)), "got {closed:?}");
    assert!(interrupted.is_ok(), "got {interrupted:?}");
    assert!(
        after_close.is_ok(),
        "a close keeps the record until a list no longer names the window: {after_close:?}"
    );
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
    pool.set_window_visible("w1", false)
        .await
        .expect("a window the list named has a record, so what is asked of it is recorded");
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
async fn a_request_that_needs_a_daemon_starts_one_when_none_runs_and_keeps_what_its_boot_announced()
{
    const BOOT_A: StatusBoot = StatusBoot { origin: 7, seq: 1 };
    const BOOT_B: StatusBoot = StatusBoot { origin: 7, seq: 2 };
    const WEAK_START: &str = "started without breaking away";
    let daemon = ScriptedDaemon::new(probes_answered_in(vec![
        Some(BOOT_A),
        Some(BOOT_A),
        Some(BOOT_B),
    ]));
    let host = FakeHost::new("starts-for-request");
    host.starts_daemon_with_warning(&daemon, WEAK_START);
    let pool = ConnectionPool::new();
    let last_announced = |pool: &ConnectionPool| {
        serde_json::to_value(pool.daemon_connection()).expect("the announcement serializes")
    };
    assert_eq!(last_announced(&pool), json!(null), "nothing announced yet");

    pool.ensure_daemon_running(&host)
        .await
        .expect("ensure_daemon_running");

    assert_eq!(host.spawn_count(), 1);
    assert!(pool.has_live_control_connection().await);
    // Read before any event is taken, as a WebView that subscribes late does.
    let weak = connected(Some(BOOT_A), Some(WEAK_START));
    assert_eq!(last_announced(&pool), weak);
    assert_eq!(announced(&host.take_events()), std::slice::from_ref(&weak));

    // The pull is a round trip behind the second connection's revision
    // probe, so that connection has settled on the same boot.
    daemon.hang_up();
    daemon.received_at_least(4).await;
    pool.control_connection()
        .sync_approvals(&host)
        .await
        .expect("sync");
    assert_eq!(
        last_announced(&pool),
        weak,
        "the same boot keeps what it announced"
    );

    daemon.hang_up();
    let started_elsewhere = connected(Some(BOOT_B), None);
    assert_eq!(
        host.next_event(EVENT_DAEMON_CONNECTED).await,
        started_elsewhere
    );
    assert_eq!(last_announced(&pool), started_elsewhere);
    assert_eq!(host.spawn_count(), 1);
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
async fn applying_a_list_retires_what_it_no_longer_names_and_reconcile_attaches_what_it_names_unless_superseded()
 {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(DaemonMessage::Attached {
            child_exited: false,
            exit_code: None,
        }),
        ClientMessage::Resize { .. } => Reaction::reply(error_frame(ERROR_CODE_SUPERSEDED)),
        ClientMessage::CloseWindow { window_id, .. } => {
            Reaction::reply(DaemonMessage::WindowClosed {
                window_id: window_id.clone(),
            })
        }
        _ => None,
    });
    let host = FakeHost::served_by("reconcile", &daemon).await;
    let pool = ConnectionPool::new();
    pool.track_window_list(&[window_info("gone", WindowStatus::Running)])
        .await;
    let failed = pool
        .connect_windows(
            &["gone".to_owned()],
            |_id: String, _client: Arc<IpcClient>| async move { Ok::<_, IpcError>(()) },
        )
        .await;
    assert!(failed.is_empty());
    let listed = [
        window_info("w1", WindowStatus::Running),
        window_info("w2", WindowStatus::Exited),
    ];

    pool.track_window_list(&listed).await;
    let after_the_list = pool.window_ids().await;
    pool.reconcile(&host, &listed).await;

    assert!(
        !after_the_list.iter().any(|id| id == "gone"),
        "applying a list retires the window it no longer names: {after_the_list:?}"
    );
    for (window_id, why) in [
        (
            "gone",
            "a list drops the record of a window it no longer names",
        ),
        ("w2", "a list makes no record for a window that has exited"),
    ] {
        let recorded = pool.set_window_visible(window_id, false).await;
        assert!(
            matches!(&recorded, Err(IpcError::WindowNotAttached { window_id: id }) if id == window_id),
            "{why}: {recorded:?}"
        );
    }
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

    // w3 is superseded by a resize the daemon answers so, then closed, which
    // lets go of its client and keeps its record.
    let with_w3 = [
        window_info("w1", WindowStatus::Running),
        window_info("w3", WindowStatus::Running),
    ];
    pool.track_window_list(&with_w3).await;
    pool.attach_window(&host, "w3", None)
        .await
        .expect("w3 attaches");
    assert_eq!(settled_link(&host, "w3").await.state, "attached");
    pool.resize("w3", SIZE_B.cols, SIZE_B.rows)
        .await
        .expect("the resize is written");
    assert_eq!(settled_link(&host, "w3").await.state, "superseded");
    pool.close_window(&host, "w3", None).await.expect("close");
    let paste = |fill: &str| ClientMessage::Paste {
        text: fill.repeat(MAX_PASTE_BYTES),
    };
    let typed = [
        pool.send_window_input("w3", &paste("p")).await,
        pool.send_window_input("w3", &paste("o")).await,
    ];
    pool.reconcile(&host, &with_w3).await;
    let explicit = pool.attach_window(&host, "w3", None).await;

    assert!(
        typed.iter().all(Result::is_ok),
        "input to a superseded window is dropped, not held up to a bound: {typed:?}"
    );
    assert!(
        matches!(&explicit, Err(IpcError::Rejected { code, .. }) if code == ERROR_CODE_SUPERSEDED),
        "an explicit attach of a superseded window is refused: {explicit:?}"
    );
    let w3_attaches = daemon
        .received()
        .iter()
        .filter(|request| matches!(request, ClientMessage::AttachView { window_id, .. } if window_id == "w3"))
        .count();
    assert_eq!(
        w3_attaches, 1,
        "neither reconcile nor an explicit attach takes a superseded window back"
    );
}

/// The next link report about `window_id` that is not `connecting`.
async fn settled_link(host: &FakeHost, window_id: &str) -> Link {
    loop {
        let link = host.next_link(window_id).await;
        if link.state != "connecting" {
            return link;
        }
    }
}

fn no_text_reached(daemon: &ScriptedDaemon) -> bool {
    !daemon
        .received()
        .iter()
        .any(|request| matches!(request, ClientMessage::Text { .. }))
}

/// Starts attaching `window_id` through `pool` as a hidden pane, without
/// waiting for the answer.
fn attach_in_background(
    pool: &Arc<ConnectionPool>,
    host: &FakeHost,
    window_id: &'static str,
) -> tokio::task::JoinHandle<Result<(), IpcError>> {
    let pool = Arc::clone(pool);
    let host = host.clone();
    tokio::spawn(async move { attach_pane(&pool, &host, window_id, None, false).await })
}

const SIZE_A: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const SIZE_B: TerminalSize = TerminalSize {
    cols: 120,
    rows: 40,
};

#[tokio::test]
async fn a_window_whose_reconnect_gave_up_is_attached_again_by_the_next_reconcile_with_what_was_asked_meanwhile()
 {
    let daemon = ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
        ClientMessage::CopySelection => {
            Reaction::reply(DaemonMessage::SelectionText { text: None })
        }
        _ => None,
    });
    let host = FakeHost::served_by("reconcile-restores", &daemon).await;
    let pool = ConnectionPool::new();
    attach_pane(&pool, &host, "w1", Some(SIZE_A), true)
        .await
        .expect("attach");
    assert_eq!(settled_link(&host, "w1").await.state, "attached");
    pool.set_window_visible("w1", false).await.expect("hide");
    pool.set_window_palette("w1", sample_palette())
        .await
        .expect("palette");
    for _ in 0..2 {
        pool.resize("w1", SIZE_B.cols, SIZE_B.rows)
            .await
            .expect("resize");
    }
    // A round trip on the same connection: whatever was written before it
    // has reached the daemon by the time it answers.
    pool.copy_selection("w1")
        .await
        .expect("the connection answers");
    let while_attached = daemon.received();
    assert_eq!(
        while_attached.iter().map(brief).collect::<Vec<_>>(),
        [
            ClientMessage::GetProtocolRevision,
            ClientMessage::AttachView {
                window_id: "w1".to_owned(),
                size: Some(SIZE_A),
            },
            ClientMessage::SetVisible { visible: false },
            ClientMessage::SetPalette(sample_palette()),
            ClientMessage::Resize {
                cols: SIZE_B.cols,
                rows: SIZE_B.rows,
            },
            ClientMessage::CopySelection,
        ]
        .iter()
        .map(brief)
        .collect::<Vec<_>>(),
        "an attached window is told each change at once, and a size only once"
    );
    let endpoint = host.endpoint_name();
    host.set_endpoint(None);

    daemon.hang_up();
    let lost = settled_link(&host, "w1").await;
    pool.set_window_visible("w1", true)
        .await
        .expect("the visibility is recorded while the window is lost");
    pool.resize("w1", SIZE_B.cols, SIZE_B.rows)
        .await
        .expect("the size is recorded while the window is lost");
    let before = daemon.received().len();
    host.set_endpoint(Some(endpoint));
    pool.reconcile(&host, &[window_info("w1", WindowStatus::Running)])
        .await;
    let reattached = settled_link(&host, "w1").await;
    let received = daemon.received_at_least(before + 4).await;

    assert_eq!(
        (lost.state.as_str(), lost.reason.as_deref()),
        ("lost", Some("reconnect_failed"))
    );
    assert_eq!(reattached.state, "attached");
    assert!(
        reattached.epoch > lost.epoch,
        "{lost:?} then {reattached:?}"
    );
    let restored = &received[before..];
    for wanted in [
        ClientMessage::AttachView {
            window_id: "w1".to_owned(),
            size: Some(SIZE_B),
        },
        ClientMessage::SetVisible { visible: true },
        ClientMessage::SetPalette(sample_palette()),
    ] {
        assert!(
            restored.contains(&wanted),
            "{wanted:?} was not restored: {restored:?}"
        );
    }
}

#[tokio::test]
async fn input_held_before_a_window_is_attached_reaches_it_up_to_its_bound_and_is_dropped_when_the_window_cannot_be_reached()
 {
    held_input_stops_at_its_bound().await;
    a_refused_attach_drops_the_held_input_and_keeps_the_record().await;
    a_reconnect_that_gives_up_drops_the_held_input().await;
    an_attach_that_finds_the_child_exited_drops_the_held_input().await;
}

async fn held_input_stops_at_its_bound() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let attaches = AtomicUsize::new(0);
        let (reached, gate) = (Arc::clone(&reached), Arc::clone(&gate));
        move |request| match request {
            ClientMessage::AttachView { .. } if attaches.fetch_add(1, Ordering::SeqCst) == 0 => {
                Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::clone(&gate),
                    vec![attached(false)],
                )
            }
            ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
            _ => None,
        }
    });
    let host = FakeHost::served_by("held-bound", &daemon).await;
    let pool = Arc::new(ConnectionPool::new());
    // A pane learns its palette after the list made it and before it attaches.
    pool.track_window_list(&[window_info("w1", WindowStatus::Running)])
        .await;
    pool.set_window_palette("w1", sample_palette())
        .await
        .expect("a window the list named records its palette before it attaches");
    let attaching = attach_in_background(&pool, &host, "w1");
    reply_within("the attach of w1", reached.notified()).await;
    let paste = |fill: &str| ClientMessage::Paste {
        text: fill.repeat(MAX_PASTE_BYTES),
    };
    const _: () = assert!(
        MAX_PASTE_BYTES < MAX_PENDING_INPUT_BYTES && 2 * MAX_PASTE_BYTES >= MAX_PENDING_INPUT_BYTES,
        "the bound holds one largest paste and not two"
    );

    // Bounded by the hang guard: an input written to the connection while the
    // daemon holds the attach would block on a full pipe.
    let typed = async {
        pool.send_window_input("w1", &text("a"))
            .await
            .expect("a is held");
        pool.send_window_input("w1", &paste("p"))
            .await
            .expect("one largest paste is held");
        let over = pool.send_window_input("w1", &paste("o")).await;
        let after = pool.send_window_input("w1", &text("z")).await;
        (over, after)
    };
    let (over, after) = reply_within("the input held for w1", typed).await;
    gate.notify_one();
    reply_within("the attach of w1", attaching)
        .await
        .expect("the attach task ran")
        .expect("w1 attaches");
    let link = settled_link(&host, "w1").await;
    pool.send_window_input("w1", &text("q"))
        .await
        .expect("q goes out");
    let received = daemon.received_at_least(7).await;

    assert!(
        matches!(over, Err(IpcError::InvalidArgument(_))),
        "{over:?}"
    );
    assert!(
        matches!(after, Err(IpcError::InvalidArgument(_))),
        "{after:?}"
    );
    assert_eq!(link.state, "attached");
    let expected = [
        ClientMessage::GetProtocolRevision,
        ClientMessage::AttachView {
            window_id: "w1".to_owned(),
            size: None,
        },
        ClientMessage::SetVisible { visible: false },
        ClientMessage::SetPalette(sample_palette()),
        text("a"),
        paste("p"),
        text("q"),
    ];
    assert_eq!(
        received.iter().map(brief).collect::<Vec<_>>(),
        expected.iter().map(brief).collect::<Vec<_>>()
    );
}

async fn a_refused_attach_drops_the_held_input_and_keeps_the_record() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let poll_reached = Arc::new(Notify::new());
    let poll_gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let attaches = AtomicUsize::new(0);
        let polled_attaches = AtomicUsize::new(0);
        let (reached, gate) = (Arc::clone(&reached), Arc::clone(&gate));
        let (poll_reached, poll_gate) = (Arc::clone(&poll_reached), Arc::clone(&poll_gate));
        move |request| match request {
            ClientMessage::AttachView { window_id, .. } if window_id == "w5" => {
                match polled_attaches.fetch_add(1, Ordering::SeqCst) {
                    0 => Reaction::replies_after(
                        Arc::clone(&poll_reached),
                        Arc::clone(&poll_gate),
                        vec![error_frame(ERROR_CODE_INTERNAL)],
                    ),
                    1 => Reaction::reply(error_frame(ERROR_CODE_INTERNAL)),
                    _ => Reaction::reply(attached(false)),
                }
            }
            ClientMessage::AttachView { .. } => match attaches.fetch_add(1, Ordering::SeqCst) {
                0 => Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::clone(&gate),
                    vec![error_frame(ERROR_CODE_INTERNAL)],
                ),
                1 => Reaction::reply(error_frame(ERROR_CODE_INTERNAL)),
                _ => Reaction::reply(attached(false)),
            },
            ClientMessage::CopySelection => {
                Reaction::reply(DaemonMessage::SelectionText { text: None })
            }
            _ => None,
        }
    });
    let host = FakeHost::served_by("held-refused", &daemon).await;
    let pool = Arc::new(ConnectionPool::new());
    let attaching = attach_in_background(&pool, &host, "w2");
    reply_within("the attach of w2", reached.notified()).await;

    pool.send_window_input("w2", &text("x"))
        .await
        .expect("x is held while the attach is in flight");
    gate.notify_one();
    let first = reply_within("the attach of w2", attaching)
        .await
        .expect("the attach task ran");
    let first_lost = settled_link(&host, "w2").await;
    let typed_after = pool.send_window_input("w2", &text("y")).await;
    let resized = pool.resize("w2", SIZE_B.cols, SIZE_B.rows).await;
    let second = attach_pane(&pool, &host, "w2", None, false).await;
    let second_lost = settled_link(&host, "w2").await;
    attach_pane(&pool, &host, "w2", None, false)
        .await
        .expect("the third attach succeeds");
    let reattached = settled_link(&host, "w2").await;
    // A round trip on the new connection: whatever was written before it has
    // reached the daemon by the time it answers.
    pool.copy_selection("w2")
        .await
        .expect("the connection answers");
    let unlisted = pool.attach_window(&host, "unlisted", None).await;
    let unlisted_input = pool.send_window_input("unlisted", &text("u")).await;

    // An attach the window-list poll makes and the daemon refuses keeps the
    // record, since the window is still listed, but leaves it with no client.
    let listed = [
        window_info("w2", WindowStatus::Running),
        window_info("w5", WindowStatus::Running),
    ];
    let polling = {
        let (pool, host, listed) = (Arc::clone(&pool), host.clone(), listed.clone());
        tokio::spawn(async move {
            pool.track_window_list(&listed).await;
            pool.reconcile(&host, &listed).await;
        })
    };
    reply_within("the polled attach of w5", poll_reached.notified()).await;
    let pooled_while_attaching = pool.get_window("w5").await.is_some();
    pool.send_window_input("w5", &text("v"))
        .await
        .expect("v is held while the poll attaches w5");
    poll_gate.notify_one();
    reply_within("the poll of w5", polling)
        .await
        .expect("the poll task ran");
    let pooled_after_refusal = pool.get_window("w5").await.is_some();
    let poll_lost = settled_link(&host, "w5").await;
    let hidden = pool.set_window_visible("w5", false).await;
    let resized_unattached = pool.resize("w5", SIZE_B.cols, SIZE_B.rows).await;
    pool.reconcile(&host, &listed).await;
    pool.reconcile(&host, &listed).await;
    // Read up to the attached report, so a missing lost shows as an absent
    // entry rather than as a wait that never ends.
    let mut later = Vec::new();
    loop {
        let link = settled_link(&host, "w5").await;
        let reattached = link.state == "attached";
        later.push(link);
        if reattached {
            break;
        }
    }
    pool.copy_selection("w5")
        .await
        .expect("the connection answers");

    assert_eq!(
        (poll_lost.state.as_str(), poll_lost.reason.as_deref()),
        ("lost", Some("attach_failed")),
        "a refused polled attach leaves the link lost"
    );
    assert_eq!(
        later
            .iter()
            .map(|link| (link.state.as_str(), link.reason.as_deref()))
            .collect::<Vec<_>>(),
        [("lost", Some("attach_failed")), ("attached", None)],
        "every polled attach reports its outcome, even the same lost again: {later:?}"
    );
    assert!(
        later[0].epoch > poll_lost.epoch,
        "{poll_lost:?} then {later:?}"
    );
    for recorded in [hidden, resized_unattached] {
        assert!(
            recorded.is_ok(),
            "a refused polled attach keeps the record, so what is asked of the window is recorded: {recorded:?}"
        );
    }
    assert!(
        !daemon.received().contains(&text("v")),
        "input held before a refused polled attach is dropped, not delivered by the next attach: {:?}",
        daemon.received()
    );
    assert!(
        pooled_while_attaching,
        "a window's client is in the pool before its polled attach is answered"
    );
    assert!(
        !pooled_after_refusal,
        "a refused polled attach lets go of the window's client"
    );

    let received = daemon.received();
    let w2_attached_at: Vec<Option<TerminalSize>> = received
        .iter()
        .filter_map(|request| match request {
            ClientMessage::AttachView { window_id, size } if window_id == "w2" => Some(*size),
            _ => None,
        })
        .collect();
    assert_eq!(
        w2_attached_at.get(2),
        Some(&Some(SIZE_B)),
        "the size told between refused attaches is what the next attach asks for: {w2_attached_at:?}"
    );
    for refused in [&unlisted, &unlisted_input] {
        assert!(
            matches!(refused, Err(IpcError::WindowNotAttached { window_id }) if window_id == "unlisted"),
            "a window no list named is refused and given no record: {refused:?}"
        );
    }
    assert!(
        !received.iter().any(
            |request| matches!(request, ClientMessage::AttachView { window_id, .. } if window_id == "unlisted")
        ),
        "an attach of a window no list named never reaches the daemon: {received:?}"
    );

    assert!(first.is_err(), "{first:?}");
    assert!(second.is_err(), "{second:?}");
    for lost in [&first_lost, &second_lost] {
        assert_eq!(
            (lost.state.as_str(), lost.reason.as_deref()),
            ("lost", Some("attach_failed"))
        );
    }
    assert!(
        second_lost.epoch > first_lost.epoch,
        "every attach reports its outcome: {first_lost:?} then {second_lost:?}"
    );
    for recorded in [typed_after, resized] {
        assert!(
            recorded.is_ok(),
            "a refused attach keeps the record of a listed window, so what is asked of it is recorded: {recorded:?}"
        );
    }
    assert_eq!(reattached.state, "attached");
    assert!(no_text_reached(&daemon), "{:?}", daemon.received());
}

async fn a_reconnect_that_gives_up_drops_the_held_input() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let attaches = AtomicUsize::new(0);
        let (reached, gate) = (Arc::clone(&reached), Arc::clone(&gate));
        let reconnects = usize::try_from(RECONNECT_ATTEMPT_LIMIT).expect("a small limit");
        move |request| match request {
            ClientMessage::AttachView { .. } => match attaches.fetch_add(1, Ordering::SeqCst) {
                0 => Reaction::reply(attached(false)),
                1 => Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::clone(&gate),
                    vec![error_frame(ERROR_CODE_INTERNAL)],
                ),
                n if n <= reconnects => Reaction::reply(error_frame(ERROR_CODE_INTERNAL)),
                _ => Reaction::reply(attached(false)),
            },
            ClientMessage::CopySelection => {
                Reaction::reply(DaemonMessage::SelectionText { text: None })
            }
            _ => None,
        }
    });
    let host = FakeHost::served_by("held-gave-up", &daemon).await;
    let pool = ConnectionPool::new();
    attach_pane(&pool, &host, "w3", None, false)
        .await
        .expect("attach");
    assert_eq!(settled_link(&host, "w3").await.state, "attached");

    daemon.hang_up();
    let connecting = host.next_link("w3").await;
    reply_within("the reconnect of w3", reached.notified()).await;
    pool.send_window_input("w3", &text("r"))
        .await
        .expect("r is held while the window reconnects");
    gate.notify_one();
    let lost = settled_link(&host, "w3").await;
    pool.send_window_input("w3", &text("l"))
        .await
        .expect("l is dropped while the window is lost");
    pool.reconcile(&host, &[window_info("w3", WindowStatus::Running)])
        .await;
    let reattached = settled_link(&host, "w3").await;
    pool.copy_selection("w3")
        .await
        .expect("the connection answers");

    assert_eq!(connecting.state, "connecting");
    assert_eq!(
        (lost.state.as_str(), lost.reason.as_deref()),
        ("lost", Some("reconnect_failed"))
    );
    assert_eq!(reattached.state, "attached");
    assert!(no_text_reached(&daemon), "{:?}", daemon.received());
}

async fn an_attach_that_finds_the_child_exited_drops_the_held_input() {
    let reached = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let attaches = AtomicUsize::new(0);
        let (reached, gate) = (Arc::clone(&reached), Arc::clone(&gate));
        move |request| match request {
            ClientMessage::AttachView { .. } => match attaches.fetch_add(1, Ordering::SeqCst) {
                0 => Reaction::reply(attached(false)),
                1 => Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::clone(&gate),
                    vec![attached(true)],
                ),
                _ => Reaction::reply(attached(true)),
            },
            ClientMessage::CopySelection => {
                Reaction::reply(DaemonMessage::SelectionText { text: None })
            }
            _ => None,
        }
    });
    let host = FakeHost::served_by("held-exited", &daemon).await;
    let pool = ConnectionPool::new();
    attach_pane(&pool, &host, "w4", None, false)
        .await
        .expect("attach");
    assert_eq!(settled_link(&host, "w4").await.state, "attached");

    daemon.hang_up();
    let connecting = host.next_link("w4").await;
    reply_within("the reconnect of w4", reached.notified()).await;
    pool.send_window_input("w4", &text("e"))
        .await
        .expect("e is held while the window reconnects");
    gate.notify_one();
    let exit = host.next_event(EVENT_EXIT).await;
    pool.copy_selection("w4")
        .await
        .expect("the connection answers");

    assert_eq!(connecting.state, "connecting");
    assert_eq!(exit, json!({ "window_id": "w4", "exit_code": null }));
    assert!(no_text_reached(&daemon), "{:?}", daemon.received());
}
