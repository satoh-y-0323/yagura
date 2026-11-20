//! The connection logic against a host that records and a daemon that is
//! scripted: connecting, losing the connection, and registering as an approver.

use std::sync::atomic::AtomicUsize;

use serde_json::json;
use tokio::sync::Notify;
use yagura_protocol::{
    ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, DaemonMessage, MAX_PASTE_BYTES,
    StatusBoot, WindowStatus,
};
use yagura_test_support::ipc::reply_within;

use super::*;
use crate::connection_pool::ConnectionPool;
use crate::events::{EVENT_DAEMON_CONNECTED, EVENT_WINDOW_LINK};
use crate::host::fake::{
    FakeHost, Link, Reaction, RecordedEvent, ScriptedDaemon, announced, attach_pane, attached,
    brief, connected, error_frame, probes_answered_in, protocol_revision, sample_palette, text,
};

const BOOT_A: StatusBoot = StatusBoot { origin: 7, seq: 1 };
const BOOT_B: StatusBoot = StatusBoot { origin: 7, seq: 2 };
const WEAK_START: &str = "started without breaking away";

/// A daemon that lets every window attach.
fn attaching_daemon() -> ScriptedDaemon {
    ScriptedDaemon::new(|request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
        _ => None,
    })
}

const ATTACH_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

/// A host served by `daemon`, a pool, and the client the pool attached to
/// `w1` through it, as a pane that is hidden.
async fn attached_window(
    daemon: &ScriptedDaemon,
    tag: &str,
) -> (FakeHost, ConnectionPool, Arc<IpcClient>) {
    let host = FakeHost::served_by(tag, daemon).await;
    let pool = ConnectionPool::new();
    attach_pane(&pool, &host, "w1", Some(ATTACH_SIZE), false)
        .await
        .expect("attach");
    let client = pool.get_window("w1").await.expect("w1 is tracked");
    (host, pool, client)
}

/// The requests of the connection that began with the `nth` revision probe
/// (counted from 0), up to the next connection's probe.
fn connection(received: &[ClientMessage], nth: usize) -> &[ClientMessage] {
    let probes: Vec<usize> = received
        .iter()
        .enumerate()
        .filter(|(_, request)| matches!(request, ClientMessage::GetProtocolRevision))
        .map(|(index, _)| index)
        .collect();
    let start = *probes.get(nth).expect("that connection was made");
    let end = probes.get(nth + 1).copied().unwrap_or(received.len());
    &received[start..end]
}

/// The next link report about `window_id`, with every event emitted before
/// it, which [`FakeHost::next_link`] would skip.
async fn next_link_with_events(host: &FakeHost, window_id: &str) -> (Link, Vec<RecordedEvent>) {
    let mut before = Vec::new();
    loop {
        let mut events = host.events_through(EVENT_WINDOW_LINK).await;
        let link = events.pop().expect("the events end with the link report");
        before.append(&mut events);
        if link.payload["window_id"] == window_id {
            return (Link::from_payload(&link.payload), before);
        }
        before.push(link);
    }
}

fn is_input(request: &ClientMessage) -> bool {
    matches!(
        request,
        ClientMessage::Key(_)
            | ClientMessage::Text { .. }
            | ClientMessage::Paste { .. }
            | ClientMessage::Pointer(_)
            | ClientMessage::Wheel(_)
            | ClientMessage::Focus { .. }
    )
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
    assert_eq!(announced(&host.take_events()), [connected(None, None)]);
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
    let answer = Arc::new(std::sync::Mutex::new(protocol_revision(None)));
    let daemon = ScriptedDaemon::new({
        let answer = Arc::clone(&answer);
        move |request| match request {
            ClientMessage::GetProtocolRevision => {
                Reaction::reply(answer.lock().expect("probe answer").clone())
            }
            _ => None,
        }
    });
    let answer_with = |message| *answer.lock().expect("probe answer") = message;
    let host = FakeHost::new("spawned");
    host.starts_daemon_with_warning(&daemon, WEAK_START);
    let client = Arc::new(IpcClient::new());
    let weak = connected(None, Some(WEAK_START));

    client.connect(&host).await.expect("connect");

    assert_eq!(host.spawn_count(), 1);
    assert!(client.is_connected().await);
    assert_eq!(announced(&host.take_events()), std::slice::from_ref(&weak));

    // A daemon whose answer names no boot may be another boot on every
    // connection, so each one is announced, and it may be the start this
    // client made, so its warning stays.
    daemon.hang_up();
    assert_eq!(host.next_event(EVENT_DAEMON_CONNECTED).await, weak);

    // The reconnect's own probe fails; two callers then settle that
    // generation together, and it is announced once.
    answer_with(error_frame(ERROR_CODE_INTERNAL));
    daemon.hang_up();
    daemon.received_at_least(6).await;
    answer_with(protocol_revision(None));
    tokio::join!(
        client.ensure_protocol_revision(&host),
        client.ensure_protocol_revision(&host)
    );
    assert_eq!(announced(&host.take_events()), [weak]);

    answer_with(protocol_revision(Some(BOOT_B)));
    daemon.hang_up();
    assert_eq!(
        host.next_event(EVENT_DAEMON_CONNECTED).await,
        connected(Some(BOOT_B), None)
    );
    assert_eq!(host.spawn_count(), 1);
}

#[tokio::test]
async fn a_connection_the_daemon_hangs_up_is_reported_as_disconnected_when_it_cannot_be_restored() {
    let daemon = attaching_daemon();
    let (host, _pool, client) = attached_window(&daemon, "lost").await;
    assert_eq!(host.next_link("w1").await.state, "attached");
    host.set_endpoint(None);

    daemon.hang_up();

    let connecting = host.next_link("w1").await;
    let lost = host.next_link("w1").await;
    assert_eq!(connecting.state, "connecting");
    assert_eq!(
        (lost.state.as_str(), lost.reason.as_deref()),
        ("lost", Some("reconnect_failed"))
    );
    assert!(
        lost.epoch > connecting.epoch,
        "{connecting:?} then {lost:?}"
    );
    assert!(!client.is_connected().await);
    assert_eq!(client.window_id().await, None);
}

#[tokio::test]
async fn a_lost_connection_is_restored_and_the_window_attached_again_as_it_was() {
    let probe = probes_answered_in(vec![Some(BOOT_A), Some(BOOT_B)]);
    let daemon = ScriptedDaemon::new(move |request| match request {
        ClientMessage::AttachView { .. } => Reaction::reply(attached(false)),
        _ => probe(request),
    });
    let (host, _pool, client) = attached_window(&daemon, "restore").await;
    let (first, mut events) = next_link_with_events(&host, "w1").await;
    assert_eq!(first.state, "attached");
    let first_connection = daemon.received_at_least(3).await;
    assert_eq!(first_connection.len(), 3);

    daemon.hang_up();
    let (connecting, more) = next_link_with_events(&host, "w1").await;
    events.extend(more);
    let (reattached, more) = next_link_with_events(&host, "w1").await;
    events.extend(more);
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
    assert_eq!(
        (connecting.state.as_str(), reattached.state.as_str()),
        ("connecting", "attached"),
        "a reconnect that succeeds reports nothing else in between"
    );
    assert!(
        first.epoch < connecting.epoch && connecting.epoch < reattached.epoch,
        "{first:?}, {connecting:?}, {reattached:?}"
    );
    assert!(client.is_connected().await);
    assert_eq!(client.window_id().await.as_deref(), Some("w1"));
    assert!(
        announced(&events).is_empty(),
        "a window connection announces no daemon boot, even a new one"
    );
}

#[tokio::test]
async fn reattaching_to_a_child_that_exited_without_a_code_reports_the_code_as_unknown() {
    let attaches = Arc::new(AtomicUsize::new(0));
    let daemon = ScriptedDaemon::new({
        let attaches = Arc::clone(&attaches);
        move |request| match request {
            ClientMessage::AttachView { .. } => {
                let earlier = attaches.fetch_add(1, Ordering::SeqCst);
                Reaction::reply(attached(earlier > 0))
            }
            ClientMessage::CopySelection => {
                Reaction::reply(DaemonMessage::SelectionText { text: None })
            }
            _ => None,
        }
    });
    let (host, pool, client) = attached_window(&daemon, "exited").await;
    assert_eq!(host.next_link("w1").await.state, "attached");
    pool.set_window_visible("w1", false).await.expect("hide");
    pool.set_window_palette("w1", sample_palette())
        .await
        .expect("palette");

    daemon.hang_up();

    let events = host.events_through(EVENT_EXIT).await;
    assert_eq!(
        events.last().map(|event| &event.payload),
        Some(&json!({ "window_id": "w1", "exit_code": null }))
    );
    let links: Vec<Link> = events
        .iter()
        .filter(|event| event.name == EVENT_WINDOW_LINK)
        .map(|event| Link::from_payload(&event.payload))
        .collect();
    assert!(
        links.iter().all(|link| link.state != "attached"),
        "a window whose child is gone is not attached: {links:?}"
    );
    // A round trip on the same connection: whatever was written before it
    // has reached the daemon by the time it answers.
    client
        .copy_selection()
        .await
        .expect("the connection answers");
    let received = daemon.received();
    let second = connection(&received, 1);
    assert!(
        !second.iter().any(|request| matches!(
            request,
            ClientMessage::SetVisible { .. } | ClientMessage::SetPalette(_)
        )),
        "nothing is restored to an exited child: {second:?}"
    );
}

#[tokio::test]
async fn a_stale_readers_disconnect_leaves_the_current_connection_alone() {
    let daemon = attaching_daemon();
    let (host, _pool, client) = attached_window(&daemon, "stale").await;
    assert_eq!(host.next_link("w1").await.state, "attached");
    let stale = client.generation.load(Ordering::SeqCst) - 1;

    client.on_disconnect(&host, false, stale).await;

    assert!(client.is_connected().await);
    assert_eq!(client.window_id().await.as_deref(), Some("w1"));
    assert!(host.take_events().is_empty());
}

#[tokio::test]
async fn a_superseded_window_is_reported_and_never_reconnects() {
    let daemon = attaching_daemon();
    let (host, _pool, client) = attached_window(&daemon, "superseded").await;
    assert_eq!(host.next_link("w1").await.state, "attached");
    let generation = client.generation.load(Ordering::SeqCst);

    client.on_disconnect(&host, true, generation).await;

    assert_eq!(host.next_link("w1").await.state, "superseded");
    assert!(!client.is_connected().await);
    assert_eq!(
        daemon
            .received()
            .iter()
            .filter(|request| matches!(request, ClientMessage::GetProtocolRevision))
            .count(),
        1,
        "a reconnect would have made a second connection ask again"
    );
}

#[tokio::test]
async fn input_typed_while_a_window_reconnects_reaches_it_after_what_was_asked_meanwhile_in_the_order_it_was_typed()
 {
    let attach_reached = Arc::new(Notify::new());
    let attach_gate = Arc::new(Notify::new());
    let flush_reached = Arc::new(Notify::new());
    let daemon = ScriptedDaemon::new({
        let attaches = AtomicUsize::new(0);
        let shown = AtomicUsize::new(0);
        let attach_reached = Arc::clone(&attach_reached);
        let attach_gate = Arc::clone(&attach_gate);
        let flush_reached = Arc::clone(&flush_reached);
        move |request| match request {
            // Both reconnects wait for the test to type into the gap first.
            ClientMessage::AttachView { .. } => match attaches.fetch_add(1, Ordering::SeqCst) {
                1 | 2 => Reaction::replies_after(
                    Arc::clone(&attach_reached),
                    Arc::clone(&attach_gate),
                    vec![attached(false)],
                ),
                _ => Reaction::reply(attached(false)),
            },
            // The third connection stops reading once it is told the window
            // is shown, so the input held behind that cannot all be written.
            ClientMessage::SetVisible { visible: true }
                if shown.fetch_add(1, Ordering::SeqCst) == 1 =>
            {
                Reaction::replies_after(
                    Arc::clone(&flush_reached),
                    Arc::new(Notify::new()),
                    Vec::new(),
                )
            }
            _ => None,
        }
    });
    let (host, pool, _client) = attached_window(&daemon, "typed-meanwhile").await;
    let first = host.next_link("w1").await;
    assert_eq!(first.state, "attached");
    assert_eq!(daemon.received_at_least(3).await.len(), 3);

    daemon.hang_up();
    let connecting = host.next_link("w1").await;
    assert_eq!(connecting.state, "connecting");
    reply_within("the second attach", attach_reached.notified()).await;
    pool.send_window_input("w1", &text("a"))
        .await
        .expect("a is held");
    pool.set_window_visible("w1", true)
        .await
        .expect("the visibility is recorded");
    pool.send_window_input("w1", &ClientMessage::Focus { focused: true })
        .await
        .expect("the focus is held");
    pool.send_window_input("w1", &text("b"))
        .await
        .expect("b is held");
    attach_gate.notify_one();
    let reattached = host.next_link("w1").await;
    pool.send_window_input("w1", &text("c"))
        .await
        .expect("c goes out");
    let second = daemon.received_at_least(10).await;

    assert_eq!(reattached.state, "attached");
    assert!(
        first.epoch < connecting.epoch && connecting.epoch < reattached.epoch,
        "{first:?}, {connecting:?}, {reattached:?}"
    );
    assert_eq!(
        connection(&second, 1),
        [
            ClientMessage::GetProtocolRevision,
            ClientMessage::AttachView {
                window_id: "w1".to_owned(),
                size: Some(ATTACH_SIZE),
            },
            ClientMessage::SetVisible { visible: true },
            text("a"),
            ClientMessage::Focus { focused: true },
            text("b"),
            text("c"),
        ]
    );

    // A write that fails while the held input goes out leaves what it did
    // not write for the next attach, which delivers it in order.
    let typed = [
        text("d"),
        ClientMessage::Paste {
            text: "p".repeat(MAX_PASTE_BYTES),
        },
        text("e"),
    ];
    daemon.hang_up();
    assert_eq!(host.next_link("w1").await.state, "connecting");
    reply_within("the third attach", attach_reached.notified()).await;
    for input in &typed {
        reply_within("the input held for w1", pool.send_window_input("w1", input))
            .await
            .expect("the input is held");
    }
    attach_gate.notify_one();
    reply_within(
        "the visibility on the third connection",
        flush_reached.notified(),
    )
    .await;
    daemon.hang_up();
    let mut count = second.len() + 7;
    let all = loop {
        let all = daemon.received_at_least(count).await;
        if all.last() == Some(&text("e")) {
            break all;
        }
        count = all.len() + 1;
    };
    // Waits on the record lock, so the fourth attach has reported its link.
    pool.set_window_visible("w1", true)
        .await
        .expect("the visibility is recorded");
    let links_after_second_hang_up: Vec<String> = host
        .take_events()
        .into_iter()
        .filter(|event| event.name == EVENT_WINDOW_LINK && event.payload["window_id"] == "w1")
        .map(|event| Link::from_payload(&event.payload).state)
        .collect();

    let restored = [
        ClientMessage::GetProtocolRevision,
        ClientMessage::AttachView {
            window_id: "w1".to_owned(),
            size: Some(ATTACH_SIZE),
        },
        ClientMessage::SetVisible { visible: true },
    ];
    assert_eq!(connection(&all, 2), restored);
    let fourth = connection(&all, 3);
    assert_eq!(fourth[..restored.len()], restored);
    let delivered = &fourth[restored.len()..];
    assert!(
        !delivered.is_empty() && delivered.iter().all(is_input) && typed.ends_with(delivered),
        "the input left over arrives as the tail of what was typed: {:?}",
        delivered.iter().map(brief).collect::<Vec<_>>()
    );
    assert_eq!(
        links_after_second_hang_up,
        ["connecting", "attached"],
        "a connection that could not take all the held input does not attach the window"
    );
}

#[tokio::test]
async fn each_registration_the_daemon_accepts_hands_the_pending_requests_to_the_webview_before_a_new_boot_is_announced()
 {
    let pending = vec![approval_request("a"), approval_request("b")];
    let probe = probes_answered_in(vec![Some(BOOT_A), Some(BOOT_A), Some(BOOT_B)]);
    let daemon = ScriptedDaemon::new({
        let pending = pending.clone();
        move |request| match request {
            ClientMessage::RegisterApprover => Reaction::reply(DaemonMessage::ApproverRegistered {
                pending: pending.clone(),
            }),
            _ => probe(request),
        }
    });
    let host = FakeHost::served_by("accepted", &daemon).await;
    let client = Arc::new(IpcClient::new());
    let handed = [
        (
            EVENT_APPROVAL_RESET.to_owned(),
            json!({ "requests": pending }),
        ),
        (
            EVENT_APPROVAL_REQUESTED.to_owned(),
            json!({ "request": pending[0] }),
        ),
        (
            EVENT_APPROVAL_REQUESTED.to_owned(),
            json!({ "request": pending[1] }),
        ),
    ];
    let named = |events: Vec<RecordedEvent>| -> Vec<(String, serde_json::Value)> {
        events
            .into_iter()
            .map(|event| (event.name, event.payload))
            .collect()
    };

    client.connect(&host).await.expect("connect");

    let mut first = handed.to_vec();
    first.push((
        EVENT_DAEMON_CONNECTED.to_owned(),
        connected(Some(BOOT_A), None),
    ));
    assert_eq!(named(host.take_events()), first);
    assert!(client.is_approver_registered());
    assert_eq!(client.sync_approvals(&host).await.expect("sync"), pending);
    assert!(host.take_events().is_empty(), "a pull emits nothing");

    // The second connection reaches the same boot; the pull is a round trip
    // behind its revision probe, so the probe has settled before the next
    // hang-up.
    daemon.hang_up();
    daemon.received_at_least(5).await;
    assert_eq!(client.sync_approvals(&host).await.expect("sync"), pending);
    daemon.hang_up();
    let through_third = host.events_through(EVENT_DAEMON_CONNECTED).await;

    let mut later = handed.to_vec();
    later.extend(handed.iter().cloned());
    later.push((
        EVENT_DAEMON_CONNECTED.to_owned(),
        connected(Some(BOOT_B), None),
    ));
    assert_eq!(
        named(through_third),
        later,
        "the same boot is not announced again, and a new one once, after the registration"
    );
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
    let reported: Vec<String> = host
        .take_events()
        .into_iter()
        .map(|event| event.name)
        .collect();
    assert_eq!(
        reported,
        [EVENT_DAEMON_CONNECTED],
        "the refusal reports nothing of its own"
    );
    assert_eq!(client.approver_registration_due(), Some(1));
}

#[tokio::test]
async fn a_spawned_connection_whose_revision_settles_later_announces_its_boot_and_warning_and_sends_the_palettes_then()
 {
    let probes = AtomicUsize::new(0);
    let daemon = ScriptedDaemon::new(move |request| match request {
        ClientMessage::GetProtocolRevision => match probes.fetch_add(1, Ordering::SeqCst) {
            0 => Reaction::reply(error_frame(ERROR_CODE_INTERNAL)),
            _ => Reaction::reply(protocol_revision(Some(BOOT_A))),
        },
        _ => None,
    });
    let host = FakeHost::new("unsettled");
    host.starts_daemon_with_warning(&daemon, WEAK_START);
    let client = Arc::new(IpcClient::new());
    let palettes = DefaultPalettes {
        app: sample_palette(),
        vendors: Default::default(),
    };
    client
        .set_default_palettes(&host, palettes.clone())
        .await
        .expect("palettes are remembered while not connected");

    client.connect(&host).await.expect("connect");

    assert_eq!(client.protocol_revision(), None);
    assert!(announced(&host.take_events()).is_empty());

    client.ensure_protocol_revision(&host).await;
    assert_eq!(
        announced(&host.take_events()),
        [connected(Some(BOOT_A), Some(WEAK_START))]
    );
    client.ensure_protocol_revision(&host).await;
    assert!(
        announced(&host.take_events()).is_empty(),
        "a generation is announced once"
    );
    assert_eq!(
        daemon.received_at_least(4).await,
        [
            ClientMessage::RegisterApprover,
            ClientMessage::GetProtocolRevision,
            ClientMessage::GetProtocolRevision,
            ClientMessage::SetDefaultPalettes(palettes),
        ]
    );
    assert_eq!(host.spawn_count(), 1);
}
