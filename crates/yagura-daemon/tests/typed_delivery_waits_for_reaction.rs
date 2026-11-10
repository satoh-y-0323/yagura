#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window typed into (`wake = "none"`) is written the next
//! text only after it has reacted to the previous one.
//!
//! The receiver is a `submit_child` that paints a composer and shows no sign
//! of working on its own screen: like a CLI whose turn is known only from a
//! structured report, it reads `waiting_input` until the test reports
//! `running` and then `waiting_input` again. Two chat messages wait for it;
//! the second must not reach its composer before that reaction, and must
//! arrive without waiting out the delivery confirmation once it has.

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{
    ChatPlaybook, Harness, SUBMIT_CHILD_PROBE, call_open_window, chat_config,
    composer_section_toml, identify, read_response, report_status, send, unique_temp_dir,
    wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{self, echo_child_command};

const READY_MARKER: &str = "receiver-ready";
const FIRST: &str = "first-ALPHA";
const SECOND: &str = "second-BRAVO";

/// The delivery confirmation the receiver's definition declares: non-zero, so
/// the agent waits for a reaction at all, and the longest the daemon allows.
const DELIVERY_CONFIRM_MS: u64 = 30_000;

/// The test's clock, stopped and held still: tokio does not move a paused
/// clock by itself while a blocking task runs, so no deadline of the daemon's
/// can pass while this is held, however long real work takes.
struct StillClock {
    release: std::sync::mpsc::Sender<()>,
    holder: tokio::task::JoinHandle<()>,
}

impl StillClock {
    fn hold() -> Self {
        let (release, released) = std::sync::mpsc::channel::<()>();
        tokio::time::pause();
        // Spawned before anything is awaited, so the clock never moves between
        // the pause and the hold.
        let holder = tokio::task::spawn_blocking(move || {
            let _ = released.recv();
        });
        Self { release, holder }
    }

    async fn let_go(self) {
        drop(self.release);
        self.holder.await.expect("the clock holder must not panic");
        tokio::time::resume();
    }
}

/// Waits until `done` holds while the clock is held still, paced by the real
/// clock: no timer of the stopped one would ever fire.
async fn wait_while_still(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(10)))
            .await
            .expect("the pacing sleep must not panic");
    }
}

fn write_definition(dir: &Path, name: &str) {
    let toml = format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = 20

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_input"
pattern = '{READY_MARKER}'
priority = 10
region = "bottom"

[input]
bracketed_paste = true
ready_hold_ms = 0
submit_delay_ms = 120
clear = ""
delivery_confirm_ms = {DELIVERY_CONFIRM_MS}
resend_max = 0
probe = "{SUBMIT_CHILD_PROBE}"
"#
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write detection definition file");
}

fn wake_none_delivery() -> yagura_daemon::profile_delivery::ProfileDelivery {
    yagura_daemon::profile_delivery::ProfileDelivery {
        output: yagura_daemon::inbox::DoorOutputs::all(&yagura_daemon::inbox::OutputForm::Plain),
        max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
        wake: yagura_daemon::profile_delivery::WakeMode::None,
        wake_stream: yagura_protocol::DeliveryStream::Stderr,
        wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
        launch_prompt: None,
        launch_context: None,
    }
}

async fn open_receiver(h: &Harness, detection: &str) -> String {
    h.registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command: process::submit_child_command_with_composer(READY_MARKER, 0, 0, 0, 0),
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(detection.to_string()),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(wake_none_delivery()),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id
}

async fn post_chat(client: &mut Connection, body: &str) {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: None,
            body: body.to_string(),
        },
    )
    .await;
    loop {
        match read_response(client).await {
            DaemonMessage::ChatChanged { .. } => {}
            DaemonMessage::ChatPosted { targets, .. } => {
                assert_eq!(targets.len(), 1, "the message must reach the receiver");
                return;
            }
            other => panic!("expected chat_posted, got {other:?}"),
        }
    }
}

async fn wait_for_submitted(h: &Harness, window_id: &str, token: &str) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !submitted(h, window_id, token) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the text carrying {token:?} never reached the receiver; screen text: {}",
            h.flattened_scrollback_keeping_osc_payload(window_id)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn submitted(h: &Harness, window_id: &str, token: &str) -> bool {
    let text = h.flattened_scrollback_keeping_osc_payload(window_id);
    text.match_indices("SUBMITTED ").any(|(at, _)| {
        text[at..]
            .split("SUBMITTED ")
            .nth(1)
            .is_some_and(|line| line.contains(token))
    })
}

#[tokio::test]
async fn the_next_typed_text_waits_for_the_window_to_react_to_the_previous_one() {
    let dir = unique_temp_dir("typed-delivery-reaction");
    let config: DaemonConfig = chat_config(&dir, &ChatPlaybook::default());
    let h = Harness::start_with_chat("typed-delivery-reaction", config).await;
    write_definition(&dir, "receiver");
    let mut probe = h.connect().await;
    let receiver = open_receiver(&h, "receiver").await;
    wait_until_status(&mut probe, &receiver, WindowStatus::WaitingInput).await;

    let mut receiver_conn = h.connect().await;
    identify(&mut receiver_conn, &receiver).await;
    let sender = call_open_window(&mut receiver_conn, echo_child_command()).await;
    let mut sender_conn = h.connect().await;
    identify(&mut sender_conn, &sender).await;

    post_chat(&mut sender_conn, &format!("@parent {FIRST}")).await;
    post_chat(&mut sender_conn, &format!("@parent {SECOND}")).await;
    wait_for_submitted(&h, &receiver, FIRST).await;
    let entry = h
        .registry
        .get(&receiver)
        .expect("the receiver is registered");

    // The attempt is counted on the agent's task, which goes on to the next
    // text without awaiting anything but the reaction: with the window ready
    // and no hold, an agent that took the second text without one would have
    // taken it before this count could be read.
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first text's attempt never ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        entry.inbox().pending_count(),
        1,
        "the second text was taken for typing before the receiver reacted to the first; \
         screen text: {}",
        h.flattened_scrollback_keeping_osc_payload(&receiver)
    );

    report_status(&mut probe, &receiver, WindowStatus::Running).await;
    wait_until_status(&mut probe, &receiver, WindowStatus::Running).await;
    report_status(&mut probe, &receiver, WindowStatus::WaitingInput).await;
    wait_until_status(&mut probe, &receiver, WindowStatus::WaitingInput).await;

    // With the clock held still the delivery confirmation cannot run out, so
    // only the reaction can release the second text.
    let still = StillClock::hold();
    wait_while_still(
        "the second text waited for the delivery confirmation to run out, not for the reaction",
        || entry.inbox().pending_count() == 0,
    )
    .await;
    still.let_go().await;
    wait_for_submitted(&h, &receiver, SECOND).await;
    h.registry.terminate_all().await;
}

/// Lets the parcel writer go on however the test ends, so a failed case never
/// leaves its thread stopped.
struct ReleaseOnDrop(yagura_daemon::fault_point::FaultPoints);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The delivery state room `room_id` records for message `seq` to `window_id`.
async fn chat_delivery_state(
    client: &mut Connection,
    room_id: &str,
    seq: u64,
    window_id: &str,
) -> Option<String> {
    send(
        client,
        &ClientMessage::ReadChat {
            room_id: Some(room_id.to_owned()),
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    let messages = loop {
        match read_response(client).await {
            DaemonMessage::ChatChanged { .. } => {}
            DaemonMessage::ChatMessages { messages, .. } => break messages,
            other => panic!("expected chat_messages, got {other:?}"),
        }
    };
    messages
        .into_iter()
        .find(|message| message.seq == seq)?
        .deliveries
        .into_iter()
        .find(|delivery| delivery.window_id == window_id)
        .map(|delivery| delivery.state)
}

/// A push settles its post as taken right before its submit key. When another
/// writer moves the window's input in between, the key is not written: the
/// settling is undone, so the store keeps the post's row and records its delivery
/// as pending again, which is what a next daemon of the instance types from.
#[tokio::test]
async fn a_push_whose_submit_key_is_not_written_keeps_its_post_pending_in_the_store() {
    use crate::common::store_fixtures::inbox_parcel_keys;
    use yagura_daemon::fault_point::{FaultPoint, FaultPoints};

    let dir = unique_temp_dir("typed-delivery-unsettle");
    let faults = FaultPoints::arm(FaultPoint::WriterPaused);
    let _release = ReleaseOnDrop(faults.clone());
    let mut config: DaemonConfig = chat_config(&dir, &ChatPlaybook::default());
    config.faults = faults.clone();
    let db = config
        .store_db_path_resolved
        .clone()
        .expect("chat_config names the chat database");
    let h = Harness::start_with_chat("typed-delivery-unsettle", config).await;
    yagura_daemon::store::wire_into(&h.registry, Some(db.clone()));
    write_definition(&dir, "receiver");
    let mut probe = h.connect().await;
    let receiver = open_receiver(&h, "receiver").await;
    wait_until_status(&mut probe, &receiver, WindowStatus::WaitingInput).await;

    let mut receiver_conn = h.connect().await;
    identify(&mut receiver_conn, &receiver).await;
    let sender = call_open_window(&mut receiver_conn, echo_child_command()).await;
    let mut sender_conn = h.connect().await;
    identify(&mut sender_conn, &sender).await;
    let mut viewer = h.connect().await;
    send(
        &mut viewer,
        &ClientMessage::AttachView {
            window_id: receiver.clone(),
            size: None,
        },
    )
    .await;
    match read_response(&mut viewer).await {
        DaemonMessage::Attached { .. } => {}
        other => panic!("expected attached, got {other:?}"),
    }

    send(
        &mut sender_conn,
        &ClientMessage::SendChat {
            room_id: None,
            body: format!("@parent {FIRST}"),
        },
    )
    .await;
    let (room_id, seq) = loop {
        match read_response(&mut sender_conn).await {
            DaemonMessage::ChatChanged { .. } => {}
            DaemonMessage::ChatPosted {
                room_id,
                seq,
                targets,
                ..
            } => {
                assert_eq!(targets, vec![receiver.clone()]);
                break (room_id, seq);
            }
            other => panic!("expected chat_posted, got {other:?}"),
        }
    };

    // The receiver's profile keeps no delivery mark, so the writer's first write
    // is the push's settling: the push now waits for it, its body confirmed in
    // the composer and its submit key not written.
    tokio::time::timeout(yagura_test_support::HANG_GUARD, faults.reached())
        .await
        .expect("the push never reached its settling");
    let entry = h
        .registry
        .get(&receiver)
        .expect("the receiver is registered");
    let before = entry.write_seq();
    send(&mut viewer, &ClientMessage::Text { text: "z".into() }).await;
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while entry.write_seq() == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the person's key never reached the receiver"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    faults.release();
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the push never ended"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        !submitted(&h, &receiver, FIRST),
        "the submit key must not go out behind another writer's input; screen text: {}",
        h.flattened_scrollback_keeping_osc_payload(&receiver)
    );
    let mut reader = h.connect().await;
    identify(&mut reader, &sender).await;
    assert_eq!(
        chat_delivery_state(&mut reader, &room_id, seq, &receiver)
            .await
            .as_deref(),
        Some("pending"),
        "a post whose submit key was not written must be recorded as pending again"
    );
    let kept = inbox_parcel_keys(&db, &receiver).expect("read the kept parcels");
    assert_eq!(
        kept.len(),
        1,
        "the post's row must be kept again for a next daemon to type: {kept:?}"
    );
    h.registry.terminate_all().await;
}
