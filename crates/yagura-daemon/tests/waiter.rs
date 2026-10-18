#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::waiter::wait_for_status` and the two window APIs the control
//! tools need (`WindowEntry::watch_status` / `PtySession::terminate_tree`).
//!
//! The waiter holds only a `watch::Receiver`, never a `WindowEntry`, so a dropped
//! sender is its only detach signal. The `Detached` outcome is verified here and
//! only here: the eviction condition makes an integration test finish with
//! `ChildExited` first, so `Detached` cannot be constructed from one.
//!
//! Time-dependent cases run under `#[tokio::test(start_paused = true)]`: the virtual
//! clock keeps the timeout paths deterministic and the assertions check elapsed
//! virtual time instead of racing wall-clock sleeps.

use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use yagura_test_support::paths::{unique_detection_dir, write_definition};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::DaemonConfig;
use yagura_daemon::handoff::READY_STATUSES;
use yagura_daemon::waiter::{
    ReactionOutcome, StatusAt, WaitOutcome, wait_for_any, wait_for_any_held, wait_for_reaction,
    wait_for_status,
};
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

// =============================================================================
// wait_for_any_held:
// the same waiting rules as wait_for_any, plus a hold a ready status must
// have kept, once, before the wait settles.
// =============================================================================

#[tokio::test]
async fn wait_for_any_held_with_zero_hold_reaches_immediately_like_wait_for_any() {
    let (_tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::Running,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    });

    let outcome = wait_for_any_held(
        rx,
        &[WindowStatus::Running],
        Duration::ZERO,
        Duration::from_secs(10),
    )
    .await;

    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "a zero hold must behave like wait_for_any: reach immediately on an \
         already-satisfied status, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_any_held_reaches_even_with_a_zero_timeout_when_the_hold_is_already_satisfied() {
    let hold = Duration::from_millis(50);
    let now = Instant::now();
    let changed_at = now
        .checked_sub(Duration::from_secs(1))
        .expect("the test clock must support a 1s offset into the past");
    let (_tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::Idle,
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    });

    let outcome = wait_for_any_held(rx, READY_STATUSES, hold, Duration::ZERO).await;

    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "a status that already satisfies the hold must reach even with a zero timeout \
         (the same delegated behavior wait_for_any already has), got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_any_held_restarts_the_hold_when_the_status_leaves_the_targets() {
    let hold = Duration::from_millis(300);
    let fresh_ready = StatusAt {
        status: DetectedStatus::WaitingInput,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    };
    let (tx, rx) = watch::channel(fresh_ready);
    let mut handle: tokio::task::JoinHandle<WaitOutcome> = tokio::spawn(wait_for_any_held(
        rx,
        READY_STATUSES,
        hold,
        Duration::from_secs(10),
    ));

    // 80% of the hold elapses while ready, then the status leaves the target set.
    tokio::time::sleep(Duration::from_millis(240)).await;
    tx.send(StatusAt {
        status: DetectedStatus::Running,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    })
    .expect("the waiter still holds a receiver");

    // The total time since the window first became ready now exceeds the hold, but
    // the continuous ready run was interrupted, so the wait must not settle yet.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let still_pending = tokio::time::timeout(Duration::from_millis(10), &mut handle).await;
    assert!(
        still_pending.is_err(),
        "an interrupted ready run must not settle the wait just because the total \
         elapsed time since the first ready observation exceeds the hold"
    );

    // The status returns to ready; only holding for the full duration again may
    // settle the wait.
    tx.send(StatusAt {
        status: DetectedStatus::WaitingInput,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    })
    .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Reached),
        "returning to a ready status must eventually reach once it holds for the full \
         duration again, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_any_held_never_reaches_while_stuck_on_unknown_even_with_an_old_changed_at() {
    let hold = Duration::from_millis(50);
    let now = Instant::now();
    let changed_at = now
        .checked_sub(Duration::from_secs(60))
        .expect("the test clock must support a 60s offset into the past");
    let (_tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::Unknown(UnknownReason::NoRuleMatched),
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    });

    let outcome = wait_for_any_held(rx, READY_STATUSES, hold, Duration::from_millis(100)).await;

    match outcome {
        WaitOutcome::Timeout { current } => assert_eq!(
            current,
            DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
        ),
        other => panic!(
            "Unknown must never settle the wait, even with a stale changed_at that would \
             otherwise satisfy the hold, got {other:?}"
        ),
    }
}

#[tokio::test]
async fn wait_for_any_held_reports_child_exited_when_the_status_becomes_exited() {
    let hold = Duration::from_millis(300);
    let (tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::Running,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    });
    let handle: tokio::task::JoinHandle<WaitOutcome> = tokio::spawn(wait_for_any_held(
        rx,
        READY_STATUSES,
        hold,
        Duration::from_secs(10),
    ));
    tokio::task::yield_now().await;

    tx.send(StatusAt {
        status: DetectedStatus::Exited,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    })
    .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(
            outcome,
            WaitOutcome::ChildExited {
                current: DetectedStatus::Exited
            }
        ),
        "the child exiting before the hold is satisfied must report ChildExited, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_any_held_times_out_when_the_hold_is_never_satisfied_within_the_deadline() {
    let hold = Duration::from_secs(60);
    let (_tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::WaitingInput,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    });
    let start = Instant::now();

    let outcome = wait_for_any_held(rx, READY_STATUSES, hold, Duration::from_millis(100)).await;

    match outcome {
        WaitOutcome::Timeout { current } => assert_eq!(current, DetectedStatus::WaitingInput),
        other => {
            panic!("a hold that never accumulates within the deadline must time out, got {other:?}")
        }
    }
    assert!(
        start.elapsed() >= Duration::from_millis(100),
        "the wait must run to the deadline, elapsed {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn wait_for_any_held_reports_detached_when_the_status_sender_is_dropped() {
    let hold = Duration::from_millis(300);
    let (tx, rx) = watch::channel(StatusAt {
        status: DetectedStatus::Running,
        changed_at: Instant::now(),
        evidence_since: Instant::now(),
        generation: 0,
    });
    let handle: tokio::task::JoinHandle<WaitOutcome> = tokio::spawn(wait_for_any_held(
        rx,
        READY_STATUSES,
        hold,
        Duration::from_secs(10),
    ));
    tokio::task::yield_now().await;

    drop(tx);

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, WaitOutcome::Detached),
        "a dropped status sender must resolve to Detached, got {outcome:?}"
    );
}

// =============================================================================
// wait_for_reaction: the delivery-confirmation waiter behind send_handoff's
// submit path. Unlike wait_for_any, it
// holds a broadcast::Receiver by &mut (borrowed across retries by the same
// caller rather than consumed) and discards only backlog stamped before the
// caller's submit key, so a reaction confirmed while settling still counts.
// =============================================================================

#[tokio::test]
async fn wait_for_reaction_reports_reacted_when_an_accepted_status_arrives() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "an accepted status must report Reacted, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_keeps_waiting_through_unknown_and_reacts_on_the_later_running() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
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
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "an Unknown detour must not disarm the wait, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_does_not_settle_on_a_round_trip_between_ready_statuses() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(30),
        )
        .await
    });
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the waiter still holds a receiver");
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::Idle))
        .expect("the waiter still holds a receiver");
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Quiet),
        "oscillating between ready statuses must not settle the wait, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_reports_exited_when_the_window_exits() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
    tokio::task::yield_now().await;

    tx.send(just_changed(DetectedStatus::Exited))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Exited),
        "the window exiting must report Exited, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_reports_quiet_when_nothing_arrives_before_the_deadline() {
    let (_tx, mut rx) = broadcast::channel::<StatusAt>(16);

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        Instant::now(),
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Quiet),
        "no event before the deadline must report Quiet, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_does_not_miss_a_brief_running_immediately_overwritten() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
    tokio::task::yield_now().await;

    // No await between these two sends: a short-lived Running must still be
    // observed even though WaitingInput follows it before the waiter gets a
    // chance to run.
    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");
    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "a brief Running must not be missed, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_discards_events_sent_before_it_is_called() {
    let (tx, mut rx) = broadcast::channel(16);
    tx.send(just_changed(DetectedStatus::Running))
        .expect("the channel has a receiver");
    // The paused clock does not move on its own; the call starts after the send.
    tokio::time::advance(Duration::from_millis(1)).await;

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        Instant::now(),
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Quiet),
        "an event sent before the call must not count (the wait has a \
         fixed starting point), got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_counts_a_queued_change_stamped_after_the_submit_key() {
    let (tx, mut rx) = broadcast::channel(16);
    let submitted_at = Instant::now();
    tx.send(StatusAt {
        status: DetectedStatus::Running,
        changed_at: submitted_at + Duration::from_millis(1),
        evidence_since: submitted_at + Duration::from_millis(1),
        generation: 1,
    })
    .expect("the channel has a receiver");

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        submitted_at,
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "a reaction confirmed after the submit key, even one already queued when the wait \
         starts, must count, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_discards_a_queued_change_stamped_before_the_submit_key() {
    let (tx, mut rx) = broadcast::channel(16);
    let changed_at = Instant::now();
    let submitted_at = changed_at + Duration::from_millis(1);
    tx.send(StatusAt {
        status: DetectedStatus::Running,
        changed_at,
        evidence_since: changed_at,
        generation: 1,
    })
    .expect("the channel has a receiver");

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        submitted_at,
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Quiet),
        "a change confirmed before the submit key must not count as its reaction, \
         got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_discards_a_change_whose_evidence_predates_the_submit_key() {
    let (tx, mut rx) = broadcast::channel(16);
    let evidence_since = Instant::now();
    let submitted_at = evidence_since + Duration::from_millis(1);
    tx.send(StatusAt {
        status: DetectedStatus::Running,
        changed_at: submitted_at + Duration::from_millis(1),
        evidence_since,
        generation: 1,
    })
    .expect("the channel has a receiver");

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        submitted_at,
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Quiet),
        "a change whose evidence appeared before the submit key must not count as its \
         reaction, even when its debounce confirmed it afterwards, got {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_reaction_counts_a_change_whose_evidence_starts_at_the_submit_key() {
    let (tx, mut rx) = broadcast::channel(16);
    let submitted_at = Instant::now();
    tx.send(StatusAt {
        status: DetectedStatus::Running,
        changed_at: submitted_at + Duration::from_millis(1),
        evidence_since: submitted_at,
        generation: 1,
    })
    .expect("the channel has a receiver");

    let outcome = wait_for_reaction(
        &mut rx,
        &[WindowStatus::Running],
        submitted_at,
        Duration::from_secs(5),
    )
    .await;

    assert!(
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "a change whose evidence and confirmation both follow the submit key must count, \
         got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_can_be_called_twice_in_a_row_with_the_same_receiver() {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        let first = wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await;
        let second = wait_for_reaction(
            &mut rx,
            &[WindowStatus::Interrupted],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await;
        (first, second)
    });
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::Interrupted))
        .expect("the waiter still holds a receiver");

    let (first, second) = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(first, ReactionOutcome::Reacted(WindowStatus::Running)),
        "the first call must react to Running, got {first:?}"
    );
    assert!(
        matches!(second, ReactionOutcome::Reacted(WindowStatus::Interrupted)),
        "the second call must not be settled by the first call's leftover events, \
         got {second:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_reports_unobservable_when_lagged_during_the_wait() {
    let (tx, rx) = broadcast::channel(1);
    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
    tokio::task::yield_now().await;

    // No await between these two sends: with capacity 1, the second send
    // overtakes the first before the waiting task can read either one, so
    // its next recv() resolves to RecvError::Lagged deterministically on the
    // current-thread runtime #[tokio::test] defaults to.
    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the waiter still holds a receiver");
    tx.send(just_changed(DetectedStatus::Idle))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Unobservable),
        "a Lagged hit while genuinely waiting must report Unobservable, got {outcome:?}"
    );
}

#[tokio::test]
async fn wait_for_reaction_lagged_during_the_initial_discard_does_not_become_unobservable() {
    let (tx, rx) = broadcast::channel(1);
    // The receiver has read nothing yet, so it is already lagged by the time
    // wait_for_reaction starts discarding its backlog.
    tx.send(just_changed(DetectedStatus::WaitingInput))
        .expect("the channel has a receiver");
    tx.send(just_changed(DetectedStatus::Idle))
        .expect("the channel has a receiver");

    let handle = tokio::spawn(async move {
        let mut rx = rx;
        wait_for_reaction(
            &mut rx,
            &[WindowStatus::Running],
            Instant::now(),
            Duration::from_secs(5),
        )
        .await
    });
    tokio::task::yield_now().await;
    tx.send(just_changed(DetectedStatus::Running))
        .expect("the waiter still holds a receiver");

    let outcome = handle.await.expect("the waiter task must not panic");
    assert!(
        matches!(outcome, ReactionOutcome::Reacted(WindowStatus::Running)),
        "a Lagged hit while discarding stale backlog must not surface as \
         Unobservable, got {outcome:?}"
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
async fn window_entry_terminate_tree_succeeds_and_the_child_converges_to_exited() {
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

    assert!(
        entry.session().terminate_tree().is_ok(),
        "terminate_tree() must succeed for an opened window"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

/// A ready pair whose status was confirmed `age` ago on the paused tokio clock.
fn ready_since(age: Duration) -> StatusAt {
    let now = tokio::time::Instant::now();
    let changed_at = now
        .checked_sub(age)
        .expect("the paused clock must support an offset into the past");
    StatusAt {
        status: DetectedStatus::Idle,
        changed_at,
        evidence_since: changed_at,
        generation: 0,
    }
}

#[tokio::test(start_paused = true)]
async fn a_hold_partly_spent_waits_only_the_remainder_on_the_paused_clock() {
    let (_tx, rx) = watch::channel(ready_since(Duration::from_millis(800)));
    let start = tokio::time::Instant::now();

    let outcome = wait_for_any_held(
        rx,
        READY_STATUSES,
        Duration::from_millis(1000),
        WAIT_TIMEOUT,
    )
    .await;

    assert_eq!(outcome, WaitOutcome::Reached);
    assert_eq!(
        start.elapsed(),
        Duration::from_millis(200),
        "exactly the unspent 200ms of the 1000ms hold must be waited out"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hold_already_spent_reaches_without_the_paused_clock_moving() {
    let (_tx, rx) = watch::channel(ready_since(Duration::from_millis(500)));
    let start = tokio::time::Instant::now();

    let outcome =
        wait_for_any_held(rx, READY_STATUSES, Duration::from_millis(300), WAIT_TIMEOUT).await;

    assert_eq!(outcome, WaitOutcome::Reached);
    assert_eq!(
        start.elapsed(),
        Duration::ZERO,
        "a hold the pair already carries must not be waited at all"
    );
}
