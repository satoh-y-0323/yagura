#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window whose terminal model is busy must not hold up anyone else: a read
//! of that window still gets its turn while the child floods the model, and
//! the daemon answers what needs no holder while a holder does not answer.
//!
//! Each case runs on a single-threaded runtime of its own (see
//! `run_on_own_runtime`), so anything that waits on a model on the runtime's
//! one thread stops the case instead of passing on a spare worker.

use crate::common;

use std::path::PathBuf;

use common::{Harness, read_response, run_on_own_runtime, send};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_protocol::holder::HolderFault;
use yagura_protocol::{ClientMessage, DaemonMessage};

/// The flooded screen's size. The cases measure that reads get their turn
/// while the child floods, not how much work one fill is, so it stays small.
const FLOOD_COLS: u16 = 100;
const FLOOD_ROWS: u16 = 100;
/// How many reads must get their turn while the flood runs.
const READS_DURING_FLOOD: usize = 5;

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
    let session = common::spawn_session(&argv, &config)
        .await
        .expect("spawn the load child");
    session
        .resize(FLOOD_COLS, FLOOD_ROWS)
        .await
        .expect("resize");
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
    run_on_own_runtime(|| reads_get_their_turn_during("align"));
}

#[test]
fn reads_get_their_turn_while_the_child_floods_repeat_requests() {
    run_on_own_runtime(|| reads_get_their_turn_during("rep"));
}

/// A daemon whose windows are kept by the fault-injecting holder, which stops
/// its model's work once the window has opened: it sends no copy, answers no
/// read of its text and settles nothing after that.
fn stalled_model_config() -> DaemonConfig {
    let shipped = DaemonConfig::default()
        .holder_exe
        .expect("the test binary has a holder beside it");
    let holder_exe =
        shipped.with_file_name(format!("holder_fault{}", std::env::consts::EXE_SUFFIX));
    assert!(
        holder_exe.exists(),
        "build the holder's binaries first: {}",
        holder_exe.display()
    );
    DaemonConfig {
        holder_exe: Some(holder_exe),
        holder_fault: Some(HolderFault::StallModel),
        ..DaemonConfig::default()
    }
}

/// While a read of one window's text waits on a holder that does not answer,
/// the daemon still answers what it holds itself: another window's status and
/// the list of windows.
async fn the_daemon_answers_while_a_holder_does_not() {
    let h = Harness::start("model-responsiveness", stalled_model_config()).await;
    let stalled_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    let other_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;

    let mut stalled_client = h.connect().await;
    send(
        &mut stalled_client,
        &ClientMessage::ReadOutput {
            window_id: stalled_id.clone(),
            lines: Some(50),
        },
    )
    .await;

    let mut other_client = h.connect().await;
    for _ in 0..3 {
        send(
            &mut other_client,
            &ClientMessage::GetWindowStatus {
                window_id: other_id.clone(),
            },
        )
        .await;
        let reply = read_response(&mut other_client).await;
        assert!(
            matches!(reply, DaemonMessage::WindowStatus { .. }),
            "unexpected reply {reply:?}"
        );
    }
    send(&mut other_client, &ClientMessage::ListWindows).await;
    let reply = read_response(&mut other_client).await;
    assert!(
        matches!(reply, DaemonMessage::WindowList { .. }),
        "unexpected reply {reply:?}"
    );
    h.registry.terminate_all().await;
}

#[test]
fn the_daemon_answers_while_one_holder_does_not() {
    run_on_own_runtime(the_daemon_answers_while_a_holder_does_not);
}
