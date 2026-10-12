#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a `send_message` whose receiver shows nothing waits for
//! an event, and what can happen on the wire while it waits.
//!
//! The wait is no longer bounded by a clock, so the connection that asked
//! must keep being served meanwhile, its departure must reach the delivery,
//! and the keys that could unstick the receiver must not queue behind it.
//! Every case drives a real `IpcServer` against `submit_child` in
//! composer-draw mode. The hang guards only bound a failure.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, SubmitDefinitionInput, approver_base_config, expect_error, identify, policy,
    read_response, send, unique_temp_dir, wait_until_input_log_contains, wait_until_status,
    write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::window::{InputOrigin, WindowEntry};
use yagura_protocol::{ClientMessage, ControlKey, DaemonMessage, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// The longest any single step may take before the case is called hung.
const HANG_GUARD: Duration = Duration::from_secs(60);

/// Typed into a window last; read back, it proves the child has read
/// everything written before it.
const SENTINEL: &[u8] = b"SENTINEL-AFTER";

/// `send_message` and `send_keys` are approved by the playbook, so no
/// approver connection is needed; `max_waiters_per_conn` is the case's own.
fn lifecycle_config(dir: &Path, max_waiters_per_conn: usize) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["send_message", "send_keys"])),
        max_waiters_per_conn,
        ..approver_base_config()
    }
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// A `submit_child` window in composer-draw mode. `clear` is the definition's
/// clear sequence (`""` for none), `clear_scope` the fixture's mode, and
/// `input_log` a file the fixture appends everything it reads to.
async fn open_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    clear: &'static str,
    clear_scope: &str,
    input_log: &Path,
) -> String {
    let marker = format!("{name}-marker");
    write_submit_definition(
        dir,
        name,
        &marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear,
            delivery_confirm_ms: 2000,
            resend_max: 0,
        },
    );
    let mut command = process::submit_child_command_with_composer(&marker, 0, 0, 0, 0);
    command.push(String::new()); // fold_placeholder: none
    command.push(clear_scope.to_string());
    command.push(String::new()); // overlay: none
    command.push(String::new()); // overlay_heading
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    let window_id = h.open(command, Some(name)).await;
    wait_until_status(client, &window_id, WindowStatus::WaitingInput, HANG_GUARD).await;
    window_id
}

/// A window whose only use is to be identified as by a requester connection,
/// so that connection's end shows in the listing.
async fn open_requester_window(h: &Harness, client: &mut Connection, dir: &Path) -> String {
    let log = dir.join("requester-input.log");
    open_window(h, client, dir, "requester", "", "", &log).await
}

async fn send_text(client: &mut Connection, window_id: &str, text: &str) {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
}

fn expect_text_sent(msg: DaemonMessage, window_id: &str) {
    match msg {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }
}

/// Waits until the daemon has finished with the connection identified as
/// `requester_window`: its identification is released only after the
/// connection's end has been signalled to everything it asked for.
async fn wait_until_the_requester_is_gone(h: &Harness, requester_window: &str) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let connected = h
            .registry
            .list()
            .into_iter()
            .find(|info| info.window_id == requester_window)
            .and_then(|info| info.mcp_connected);
        if connected == Some(false) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon never saw the requester's connection end"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

// =====================================================================
// The asking connection keeps being served
// =====================================================================

#[tokio::test]
async fn a_ping_is_answered_on_the_connection_whose_send_message_waits_on_a_window_showing_nothing()
{
    let dir = unique_temp_dir("lifecycle-ping");
    let log = dir.join("child-input.log");
    let h = Harness::start("lifecycle-ping", lifecycle_config(&dir, 16)).await;
    let mut client = h.connect().await;
    let window_id = open_window(&h, &mut client, &dir, "ping", "\\u0015", "frozen:", &log).await;

    send_text(&mut client, &window_id, "BODY\r").await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", HANG_GUARD)
        .await;
    send(&mut client, &ClientMessage::Ping).await;
    let pong = read_response(&mut client, HANG_GUARD).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "a ping must be answered while the same connection's send_message waits, got {pong:?}"
    );

    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    expect_error(read_response(&mut client, HANG_GUARD).await, "not_written");
}

// =====================================================================
// The requester leaves mid-delivery
// =====================================================================

/// The receiver stops reading once the body's paste begins and goes on only
/// when the test says so, after the daemon has seen the requester go.
#[tokio::test]
async fn a_requester_leaving_mid_send_message_gets_no_submit_key_and_its_body_taken_back_so_the_next_send_delivers_once()
 {
    let dir = unique_temp_dir("lifecycle-requester-leaves");
    let log = dir.join("child-input.log");
    let resume = dir.join("resume");
    let h = Harness::start("lifecycle-requester-leaves", lifecycle_config(&dir, 16)).await;
    let mut observer = h.connect().await;
    let requester_window = open_requester_window(&h, &mut observer, &dir).await;
    let stall = format!(
        "stall-until|{}|",
        resume
            .to_str()
            .expect("the resume path must be valid UTF-8")
    );
    let window_id = open_window(&h, &mut observer, &dir, "leaves", "\\u0015", &stall, &log).await;

    let mut requester = h.connect().await;
    identify(&mut requester, &requester_window).await;
    send_text(&mut requester, &window_id, "BODY\r").await;
    h.wait_for_scrollback(&window_id, b"STALL-BEGAN", HANG_GUARD)
        .await;
    drop(requester);
    wait_until_the_requester_is_gone(&h, &requester_window).await;
    std::fs::write(&resume, b"").expect("let the receiver read on");

    let mut next = h.connect().await;
    send_text(&mut next, &window_id, "BODY\r").await;
    expect_text_sent(read_response(&mut next, HANG_GUARD).await, &window_id);

    entry(&h, &window_id)
        .write_pty_input(SENTINEL, InputOrigin::Interactive)
        .await
        .expect("the sentinel must not fail");
    let read = wait_until_input_log_contains(&log, SENTINEL, HANG_GUARD).await;
    let first_paste = read
        .windows(b"\x1b[200~BODY".len())
        .position(|window| window == b"\x1b[200~BODY")
        .expect("the first body must have reached the receiver");
    let second_paste = first_paste
        + 1
        + read[first_paste + 1..]
            .windows(b"\x1b[200~BODY".len())
            .position(|window| window == b"\x1b[200~BODY")
            .expect("the next send_message must have written its body");
    let between = &read[first_paste..second_paste];
    assert!(
        !between.contains(&b'\r'),
        "no submit key may follow the departed requester's body: {read:?}"
    );
    assert!(
        between.contains(&0x15),
        "the departed requester's body must have been taken back: {read:?}"
    );
    assert_eq!(
        count(&read, b"\r"),
        1,
        "only the next send_message presses the submit key: {read:?}"
    );
    assert_eq!(
        count(&h.scrollback(&window_id), b"SUBMITTED BODY"),
        1,
        "the body reaches the receiver exactly once"
    );
}

/// The send queued behind a stuck one: its requester leaves before its turn,
/// so once the turn comes it writes nothing. The raw send after it takes the
/// next turn, so nothing of the departed send can land between the two.
#[tokio::test]
async fn a_send_message_waiting_for_its_turn_writes_nothing_once_its_requester_leaves() {
    let dir = unique_temp_dir("lifecycle-queued-leaves");
    let log = dir.join("child-input.log");
    let h = Harness::start("lifecycle-queued-leaves", lifecycle_config(&dir, 16)).await;
    let mut first = h.connect().await;
    let requester_window = open_requester_window(&h, &mut first, &dir).await;
    let window_id = open_window(&h, &mut first, &dir, "queued", "\\u0015", "frozen:", &log).await;

    send_text(&mut first, &window_id, "FIRST\r").await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", HANG_GUARD)
        .await;
    let mut queued = h.connect().await;
    identify(&mut queued, &requester_window).await;
    send_text(&mut queued, &window_id, "SECOND\r").await;
    drop(queued);
    wait_until_the_requester_is_gone(&h, &requester_window).await;

    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    expect_error(read_response(&mut first, HANG_GUARD).await, "not_written");

    let mut last = h.connect().await;
    send_text(&mut last, &window_id, "MARK").await;
    expect_text_sent(read_response(&mut last, HANG_GUARD).await, &window_id);
    let read = wait_until_input_log_contains(&log, b"MARK", HANG_GUARD).await;
    assert!(
        read.windows(b"QMARK".len())
            .any(|window| window == b"QMARK"),
        "nothing may be written between the stuck send's end and the next turn's write: {read:?}"
    );
    assert_eq!(count(&read, b"SECOND"), 0, "{read:?}");
}

// =====================================================================
// send_keys does not queue behind a stuck delivery
// =====================================================================

#[tokio::test]
async fn send_keys_from_another_connection_is_written_while_a_send_message_waits_and_ends_that_wait()
 {
    let dir = unique_temp_dir("lifecycle-send-keys");
    let log = dir.join("child-input.log");
    let h = Harness::start("lifecycle-send-keys", lifecycle_config(&dir, 16)).await;
    let mut client = h.connect().await;
    let window_id = open_window(&h, &mut client, &dir, "keys", "\\u0015", "frozen:", &log).await;

    send_text(&mut client, &window_id, "BODY\r").await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", HANG_GUARD)
        .await;
    let mut keys = h.connect().await;
    send(
        &mut keys,
        &ClientMessage::SendKeys {
            window_id: window_id.clone(),
            keys: vec![ControlKey::Backspace],
        },
    )
    .await;
    match read_response(&mut keys, HANG_GUARD).await {
        DaemonMessage::KeysSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("send_keys must be written while a send_message waits, got {other:?}"),
    }

    let message = expect_error(read_response(&mut client, HANG_GUARD).await, "not_written");
    assert!(
        message.contains("another write reached the window"),
        "the keys are another writer, so the waiting send_message ends interrupted: {message}"
    );
    let read = wait_until_input_log_contains(&log, b"\x1b[201~", HANG_GUARD).await;
    assert!(
        !read.contains(&b'\r'),
        "the interrupted send_message must not press the submit key: {read:?}"
    );
}

// =====================================================================
// A waiting send_message counts against the connection's waits
// =====================================================================

#[tokio::test]
async fn a_waiting_send_message_counts_against_the_connections_pending_waits() {
    let dir = unique_temp_dir("lifecycle-waiter-limit");
    let log = dir.join("child-input.log");
    let h = Harness::start("lifecycle-waiter-limit", lifecycle_config(&dir, 1)).await;
    let mut client = h.connect().await;
    let window_id = open_window(&h, &mut client, &dir, "limit", "\\u0015", "frozen:", &log).await;

    send_text(&mut client, &window_id, "BODY\r").await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", HANG_GUARD)
        .await;

    send(
        &mut client,
        &ClientMessage::WaitForStatus {
            window_id: window_id.clone(),
            target_status: WindowStatus::Running,
            timeout_sec: 1,
        },
    )
    .await;
    expect_error(
        read_response(&mut client, HANG_GUARD).await,
        "resource_exhausted",
    );
    send_text(&mut client, &window_id, "MORE\r").await;
    expect_error(
        read_response(&mut client, HANG_GUARD).await,
        "resource_exhausted",
    );
    send(&mut client, &ClientMessage::Ping).await;
    assert!(matches!(
        read_response(&mut client, HANG_GUARD).await,
        DaemonMessage::Pong
    ));
}
