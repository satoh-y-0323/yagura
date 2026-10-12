#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `send_keys` writes its named keys the way the window's terminal
//! modes call for at the moment they are written, including after an approval wait.

mod common;

use std::time::Duration;

use common::{
    CONTROL_DETECTION_NAME, Harness, approval_config, approver_base_config,
    assert_hex_echo_receives, decide, detection_config, expect_approval_requested, hex_echo_path,
    hex_of, identify, read_response, register_approver, send, unique_temp_dir,
    wait_until_model_shows, wait_until_status,
};
use yagura_protocol::{ClientMessage, ControlKey, DaemonMessage, WindowStatus};

const EMITTED_MARKER: &[u8] = b"QUERIES-EMITTED";
const TIMEOUT: Duration = Duration::from_secs(10);
const DECCKM: &[u8] = b"\x1b[?1h";

fn hex_echo_command(mode: &str, sequences: &[&[u8]]) -> Vec<String> {
    let mut command = vec![
        hex_echo_path()
            .to_str()
            .expect("hex_echo path must be valid UTF-8")
            .to_string(),
        mode.to_string(),
    ];
    command.extend(sequences.iter().map(|sequence| hex_of(sequence)));
    command
}

fn send_up(window_id: &str) -> ClientMessage {
    ClientMessage::SendKeys {
        window_id: window_id.to_string(),
        keys: vec![ControlKey::Up],
    }
}

#[tokio::test]
async fn send_keys_up_reaches_a_child_in_cursor_keys_mode_as_ss3() {
    let dir = unique_temp_dir("send-keys-decckm");
    let h = Harness::start(
        "send-keys-decckm",
        detection_config(&dir, approver_base_config()),
    )
    .await;
    let mut client = h.connect().await;
    let window_id = h
        .open(
            hex_echo_command("--emit", &[DECCKM]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    wait_until_status(&mut client, &window_id, WindowStatus::Idle, TIMEOUT).await;
    wait_until_model_shows(&h, &window_id, EMITTED_MARKER, TIMEOUT).await;
    identify(&mut client, &window_id).await;

    send(&mut client, &send_up(&window_id)).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected keys_sent, got {other:?}"),
    }
    assert_hex_echo_receives(&h, &window_id, EMITTED_MARKER, b"\x1bOA", TIMEOUT).await;
}

#[tokio::test]
async fn send_keys_up_is_encoded_with_the_modes_at_approval_not_at_request() {
    let dir = unique_temp_dir("send-keys-decckm-approval");
    let h = Harness::start("send-keys-decckm-approval", approval_config(&dir, 30)).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut requester = h.connect().await;
    let window_id = h
        .open(
            hex_echo_command("--emit-on-input", &[DECCKM]),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    wait_until_status(&mut requester, &window_id, WindowStatus::Idle, TIMEOUT).await;

    send(&mut requester, &send_up(&window_id)).await;
    let request = expect_approval_requested(&mut approver).await;

    // The child sets cursor keys mode while the request waits for its decision.
    h.write_input(&window_id, b"!").await;
    wait_until_model_shows(&h, &window_id, EMITTED_MARKER, TIMEOUT).await;

    decide(&mut approver, &request.request_id, true).await;
    match read_response(&mut requester, Duration::from_secs(5)).await {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected keys_sent, got {other:?}"),
    }
    assert_hex_echo_receives(&h, &window_id, EMITTED_MARKER, b"\x1bOA", TIMEOUT).await;
}
