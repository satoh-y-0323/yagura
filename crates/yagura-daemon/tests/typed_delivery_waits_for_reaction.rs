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

mod common;

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

/// The delivery confirmation the receiver's definition declares: the longest
/// the daemon allows, so that no wait in this test can end by running into it.
const DELIVERY_CONFIRM_MS: u64 = 30_000;

/// How long the second text is watched for while the receiver has not reacted:
/// far below the delivery confirmation, so it can only miss a text written late.
const NOT_WRITTEN_WINDOW: Duration = Duration::from_secs(3);

const STEP_TIMEOUT: Duration = Duration::from_secs(15);

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
        match read_response(client, Duration::from_secs(5)).await {
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
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
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
    wait_until_status(
        &mut probe,
        &receiver,
        WindowStatus::WaitingInput,
        STEP_TIMEOUT,
    )
    .await;

    let mut receiver_conn = h.connect().await;
    identify(&mut receiver_conn, &receiver).await;
    let sender = call_open_window(&mut receiver_conn, echo_child_command()).await;
    let mut sender_conn = h.connect().await;
    identify(&mut sender_conn, &sender).await;

    post_chat(&mut sender_conn, &format!("@parent {FIRST}")).await;
    post_chat(&mut sender_conn, &format!("@parent {SECOND}")).await;
    wait_for_submitted(&h, &receiver, FIRST).await;
    let first_submitted_at = tokio::time::Instant::now();

    let watched_until = first_submitted_at + NOT_WRITTEN_WINDOW;
    while tokio::time::Instant::now() < watched_until {
        assert!(
            !h.flattened_scrollback_keeping_osc_payload(&receiver)
                .contains(SECOND),
            "the second text was written before the receiver reacted to the first; \
             screen text: {}",
            h.flattened_scrollback_keeping_osc_payload(&receiver)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    report_status(&mut probe, &receiver, WindowStatus::Running).await;
    wait_until_status(&mut probe, &receiver, WindowStatus::Running, STEP_TIMEOUT).await;
    report_status(&mut probe, &receiver, WindowStatus::WaitingInput).await;
    wait_for_submitted(&h, &receiver, SECOND).await;

    assert!(
        first_submitted_at.elapsed() < Duration::from_millis(DELIVERY_CONFIRM_MS),
        "the second text came only after the delivery confirmation ran out, not after \
         the reaction"
    );
    h.registry.terminate_all().await;
}
