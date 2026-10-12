#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window whose terminal model is busy must not hold up anyone else: a read
//! of that window still gets its turn while the child floods the model, and a
//! request about another window is answered while this one's model is held.
//!
//! Each case runs on a single-threaded runtime of its own (see
//! `run_on_own_runtime`), so anything that waits on a model on the runtime's
//! one thread stops the case instead of passing on a spare worker.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{Harness, read_response, run_on_own_runtime, send};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_protocol::{ClientMessage, DaemonMessage};

const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 1000;
/// How many reads must get their turn while the flood runs.
const READS_DURING_FLOOD: usize = 5;
/// Fails a case whose runtime stopped, instead of hanging the test binary.
const CASE_GUARD: Duration = Duration::from_secs(180);
const REPLY_GUARD: Duration = Duration::from_secs(60);

fn load_child_path() -> PathBuf {
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop();
    if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
        dir.pop();
    }
    dir.push(if cfg!(windows) {
        "screen_load_child.exe"
    } else {
        "screen_load_child"
    });
    assert!(dir.exists(), "build the fixtures first: {}", dir.display());
    dir
}

fn load_command(mode: &str) -> Vec<String> {
    vec![
        load_child_path().to_str().unwrap().to_string(),
        mode.to_string(),
    ]
}

/// Waits for the next output the child writes to reach the model.
async fn next_output(session: &PtySession) {
    let mut arrivals = session.output_arrivals();
    arrivals.borrow_and_update();
    if let Err(err) = arrivals.changed().await {
        panic!("the window's output ended: {err}");
    }
}

async fn reads_get_their_turn_during(mode: &str) {
    let config = DaemonConfig::default();
    let argv = load_command(mode);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let session = PtySession::spawn_with_command(&argv, &config)
        .await
        .expect("spawn the load child");
    session.resize(MAX_COLS, MAX_ROWS).await.expect("resize");
    // The child floods without end from its start, so any output means it is.
    next_output(&session).await;

    for _ in 0..READS_DURING_FLOOD {
        session
            .text_tail(100, 64 * 1024)
            .await
            .expect("the text is readable");
    }
    // The child floods until it is killed: the reads above ran while it did.
    next_output(&session).await;
}

#[test]
fn reads_get_their_turn_while_the_child_floods_screen_fills() {
    run_on_own_runtime(CASE_GUARD, || reads_get_their_turn_during("align"));
}

#[test]
fn reads_get_their_turn_while_the_child_floods_repeat_requests() {
    run_on_own_runtime(CASE_GUARD, || reads_get_their_turn_during("rep"));
}

/// Sends `request` about a window whose model is held, then the same request
/// about another window, which must be answered before the model is released.
async fn another_window_is_answered_while_one_model_is_held(request: fn(String) -> ClientMessage) {
    let h = Harness::start("model-responsiveness", DaemonConfig::default()).await;
    let held_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    let other_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    let held_session = h.registry.get(&held_id).unwrap().session().clone();

    let hold = held_session.hold_model();
    let mut held_client = h.connect().await;
    send(&mut held_client, &request(held_id.clone())).await;

    let mut other_client = h.connect().await;
    for _ in 0..3 {
        send(&mut other_client, &request(other_id.clone())).await;
        let reply = read_response(&mut other_client, REPLY_GUARD).await;
        assert_answers(&reply);
    }

    drop(hold);
    let reply = read_response(&mut held_client, REPLY_GUARD).await;
    assert_answers(&reply);
    h.registry.terminate_all().await;
}

fn assert_answers(reply: &DaemonMessage) {
    assert!(
        matches!(
            reply,
            DaemonMessage::WindowOutput { .. } | DaemonMessage::WindowStatus { .. }
        ),
        "unexpected reply {reply:?}"
    );
}

#[test]
fn read_output_of_another_window_is_answered_while_one_model_is_held() {
    run_on_own_runtime(CASE_GUARD, || {
        another_window_is_answered_while_one_model_is_held(|window_id| ClientMessage::ReadOutput {
            window_id,
            lines: Some(50),
        })
    });
}

#[test]
fn the_status_of_another_window_is_answered_while_one_model_is_held() {
    run_on_own_runtime(CASE_GUARD, || {
        another_window_is_answered_while_one_model_is_held(|window_id| {
            ClientMessage::GetWindowStatus { window_id }
        })
    });
}
