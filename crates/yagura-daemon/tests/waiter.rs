#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::waiter::wait_for_status` and the two window APIs the control
//! tools need (`WindowEntry::watch_status` / `PtySession::terminate_tree`).
//!
//! The waiter holds only a `watch::Receiver`, never a `WindowEntry`, so a dropped
//! sender is its only detach signal. The `Detached` outcome is verified here and
//! only here: the eviction condition makes an integration test finish with
//! `ChildExited` first, so `Detached` cannot be constructed from one.
//!
//! Every case without a real window runs under `#[tokio::test(start_paused = true)]`:
//! the virtual clock keeps the timeout and hold paths deterministic, and the
//! assertions check elapsed virtual time instead of racing wall-clock sleeps.

use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use yagura_test_support::paths::{unique_detection_dir, write_definition};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::DaemonConfig;
use yagura_daemon::handoff::READY_STATUSES;
use yagura_daemon::waiter::{StatusAt, WaitOutcome, wait_for_any, wait_for_status};
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Long enough that any outcome other than `Timeout` must come from an actual status
/// observation, not the deadline. Under a paused clock it costs no wall-clock time.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// A pair whose status changed just now. The cases below exercise the waiting
/// rules rather than a hold, so the age the pair carries never matters to them.
fn just_changed(status: DetectedStatus) -> StatusAt {
    StatusAt {
        status,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    }
}

#[tokio::test(start_paused = true)]
async fn returns_reached_immediately_when_the_current_value_already_matches_target() {
    let (_tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let start = tokio::time::Instant::now();

    let outcome = wait_for_status(rx, WindowStatus::Running, WAIT_TIMEOUT).await;

    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "a receiver already holding the target must report Reached, got {outcome:?}"
    );
    assert_eq!(
        start.elapsed(),
        Duration::ZERO,
        "the already-satisfied case must not wait for a status change"
    );
}

#[tokio::test(start_paused = true)]
async fn returns_reached_when_the_status_transitions_to_target() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Idle));
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Running, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "a transition to the target must report Reached, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn does_not_complete_when_the_status_transitions_to_unknown() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Idle));
    let start = tokio::time::Instant::now();
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Running, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )))
    .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    match outcome {
        WaitOutcome::Timeout { current } => assert_eq!(
            current,
            DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
            "the timeout must carry the observed pair, reason included"
        ),
        other => panic!(
            "passing through Unknown must never complete the wait; the deadline must be the \
             only way out, got {other:?}"
        ),
    }
    assert!(
        start.elapsed() >= WAIT_TIMEOUT,
        "the wait must have run until the deadline, elapsed {:?}",
        start.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn returns_reached_when_the_target_is_reached_after_passing_through_unknown() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Idle));
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Running, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )))
    .expect("the waiter still holds a receiver");
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "an Unknown detour must not disarm the wait: reaching the target afterwards must \
         still report Reached, got {outcome:?}"
    );
}

/// `Unknown` is exactly the state in which destructive or automated actions must not
/// fire, so the waiter must never report Reached for it, even for a caller that
/// bypasses the IPC-boundary rejection of `target_status: "unknown"`.
#[tokio::test(start_paused = true)]
async fn never_returns_reached_when_target_is_unknown_and_the_current_value_is_unknown() {
    let (_tx, rx) = watch::channel(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )));

    let outcome = wait_for_status(rx, WindowStatus::Unknown, WAIT_TIMEOUT).await;

    assert!(
        !matches!(outcome, WaitOutcome::Reached),
        "target Unknown must never be reported as Reached, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn never_returns_reached_when_target_is_unknown_and_the_status_transitions_to_unknown() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Idle));
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Unknown, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )))
    .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        !matches!(outcome, WaitOutcome::Reached),
        "a transition into Unknown must never satisfy a wait whose target is Unknown, \
         got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn returns_timeout_with_the_current_status_once_the_deadline_elapses() {
    let (_tx, rx) = watch::channel(just_changed(DetectedStatus::Idle));
    let start = tokio::time::Instant::now();

    let outcome = wait_for_status(rx, WindowStatus::Running, WAIT_TIMEOUT).await;

    match outcome {
        WaitOutcome::Timeout { current } => assert_eq!(
            current,
            DetectedStatus::Idle,
            "the timeout must carry the pair observed at the deadline"
        ),
        other => panic!("expected Timeout after the deadline elapsed, got {other:?}"),
    }
    assert_eq!(
        start.elapsed(),
        WAIT_TIMEOUT,
        "the wait must end exactly at the requested deadline, never earlier and never hang"
    );
}

#[tokio::test(start_paused = true)]
async fn returns_reached_when_the_child_exits_and_exited_is_the_target() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Exited, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Exited))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "waiting for Exited must be satisfied by the child exiting, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn returns_child_exited_when_the_child_exits_while_waiting_for_another_target() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let start = tokio::time::Instant::now();
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Idle, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Exited))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    match outcome {
        WaitOutcome::ChildExited { current } => assert_eq!(
            current,
            DetectedStatus::Exited,
            "the child-exit outcome must carry the observed Exited pair"
        ),
        other => panic!(
            "a target that can no longer be reached must resolve to ChildExited rather than \
             waiting forever, got {other:?}"
        ),
    }
    assert!(
        start.elapsed() < WAIT_TIMEOUT,
        "the child exit must end the wait immediately, not at the deadline"
    );
}

#[tokio::test(start_paused = true)]
async fn returns_detached_when_the_status_sender_is_dropped() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let start = tokio::time::Instant::now();
    let handle = tokio::spawn(wait_for_status(rx, WindowStatus::Idle, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    // A dropped sender is what an eviction looks like from the waiter's side: it holds
    // no reference to the WindowEntry, so the closed channel is its only signal.
    drop(tx);

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Detached),
        "a dropped status sender must resolve to Detached, got {outcome:?}"
    );
    assert!(
        start.elapsed() < WAIT_TIMEOUT,
        "a dropped sender must end the wait immediately, not at the deadline"
    );
}

// =============================================================================
// wait_for_any: the multi-target waiter behind send_handoff's readiness wait.
// `wait_for_status` is documented to be exactly a 1-element call to this, so the last test below checks that delegation directly.
// =============================================================================

#[tokio::test(start_paused = true)]
async fn wait_for_any_reaches_when_the_status_becomes_the_first_target() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let targets = [WindowStatus::WaitingInput, WindowStatus::Idle];
    let handle = tokio::spawn(wait_for_any(rx, &targets, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "transitioning to the first of two targets must report Reached, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_any_reaches_when_the_status_becomes_the_second_target() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let targets = [WindowStatus::WaitingInput, WindowStatus::Idle];
    let handle = tokio::spawn(wait_for_any(rx, &targets, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Idle))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "transitioning to the second of two targets must report Reached, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_any_with_ready_statuses_times_out_while_stuck_on_unknown() {
    let (_tx, rx) = watch::channel(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )));
    let start = tokio::time::Instant::now();

    let outcome = wait_for_any(rx, READY_STATUSES, WAIT_TIMEOUT).await;

    match outcome {
        WaitOutcome::Timeout { current } => assert_eq!(
            current,
            DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
        ),
        other => {
            panic!("a window stuck on Unknown must never settle READY_STATUSES, got {other:?}")
        }
    }
    assert_eq!(
        start.elapsed(),
        WAIT_TIMEOUT,
        "the wait must run to the full deadline, not settle early"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_any_never_settles_on_unknown_even_when_unknown_is_itself_a_target() {
    // Unknown is a fail-safe sink even when the caller's target
    // set explicitly names it.
    let (_tx, rx) = watch::channel(just_changed(DetectedStatus::Unknown(
        UnknownReason::NoRuleMatched,
    )));

    let outcome = wait_for_any(
        rx,
        &[WindowStatus::Unknown, WindowStatus::Idle],
        WAIT_TIMEOUT,
    )
    .await;

    assert!(
        !matches!(outcome, WaitOutcome::Reached),
        "Unknown must never be reported Reached even when it is in the target set, \
         got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_any_reports_child_exited_when_none_of_the_targets_is_exited() {
    let (tx, rx) = watch::channel(just_changed(DetectedStatus::Running));
    let targets = [WindowStatus::WaitingInput, WindowStatus::Idle];
    let handle = tokio::spawn(wait_for_any(rx, &targets, WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Exited))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(
            outcome,
            WaitOutcome::ChildExited {
                current: DetectedStatus::Exited
            }
        ),
        "the child exiting before any target is reached must report ChildExited, \
         got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_status_delegates_to_wait_for_any_with_a_single_element_slice() {
    // Two receivers fed the same sequence of sends must agree, whether driven
    // through wait_for_status or through wait_for_any with a 1-element slice.
    let (tx_a, rx_a) = watch::channel(just_changed(DetectedStatus::Idle));
    let (tx_b, rx_b) = watch::channel(just_changed(DetectedStatus::Idle));
    let handle_a = tokio::spawn(wait_for_status(rx_a, WindowStatus::Running, WAIT_TIMEOUT));
    let handle_b = tokio::spawn(wait_for_any(rx_b, &[WindowStatus::Running], WAIT_TIMEOUT));
    tokio::task::yield_now().await;

    tx_a.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");
    tx_b.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");

    let outcome_a = handle_a.await.expect("the waiter task must not panic");
    let outcome_b = handle_b.await.expect("the waiter task must not panic");
    assert_eq!(
        outcome_a, outcome_b,
        "wait_for_status must be exactly wait_for_any with a 1-element target slice"
    );
}

fn echo_request(detection: Option<&str>) -> OpenWindowRequest {
    let child = echo_child_path();
    OpenWindowRequest {
        command: vec![
            child
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: None,
        name: None,
        vendor: None,
        detection: detection.map(|s| s.to_string()),
        on_name_taken: Default::default(),
    }
}

const WATCH_TEST_DEFINITION: &str = r#"
schema_version = 1
name = "waiter-watch"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = 'READY-MARK'
priority = 10
region = "bottom"
"#;

#[tokio::test]
async fn window_entry_watch_status_exposes_the_current_status() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request(None))
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return the entry");

    let rx = entry.watch_status();
    assert_eq!(
        rx.borrow().status.status(),
        entry.current_status(),
        "watch_status() must observe the same confirmed status as current_status()"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn window_entry_watch_status_observes_a_status_transition() {
    let dir = unique_detection_dir(
        "yagura-test-waiter-detection",
        "watch-transition",
        "create temp detection dir",
    );
    write_definition(&dir, "waiter-watch", WATCH_TEST_DEFINITION);
    let config = DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    };
    let registry = WindowRegistry::new(config);
    let id = registry
        .open(echo_request(Some("waiter-watch")))
        .await
        .expect("open with a valid detection definition should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return the entry");

    let mut rx = entry.watch_status();
    entry
        .session()
        .write_input(b"READY-MARK\r\n")
        .await
        .expect("write_input to the echo child");

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if rx.borrow_and_update().status.status() == WindowStatus::Running {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for watch_status() to report the Running transition"
        );
        match tokio::time::timeout(remaining, rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => panic!("the status sender was dropped while the window was alive"),
            Err(_) => panic!("timed out waiting for a watch_status() change notification"),
        }
    }

    registry.terminate_all().await;
}

#[tokio::test]
async fn window_entry_terminate_tree_makes_the_child_converge_to_exited() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let id = registry
        .open(echo_request(None))
        .await
        .expect("open should succeed")
        .window_id;
    let entry = registry.get(&id).expect("get should return the entry");
    assert!(
        entry.session().exit_status().is_none(),
        "test precondition: the child must still be running before terminate_tree()"
    );

    entry.session().terminate_tree();

    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if entry.session().exit_status().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child process did not exit within the timeout after terminate_tree()"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    registry.terminate_all().await;
}
