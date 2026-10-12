#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `recovery_state` behavior: the pure state machine that decides whether the
//! supervisor spawns the daemon, how long it waits, and what it publishes.
//!
//! Every test below drives [`RecoveryState`] directly, tick by tick, with
//! hand-built [`RecoveryObservation`] values. None of them advance real or
//! mocked time: the wait a tick returns is a value to inspect, never
//! something the test sleeps on.

use std::time::Duration;

use yagura_gui::client_config::{ClientConfigLoadResult, ReconnectConfig};
use yagura_gui::constants::{MAX_RECONNECT_DELAY, TICK_INTERVAL};
use yagura_gui::recovery_state::{
    RecoveryObservation, RecoveryState, RecoveryStatus, rearm_recovery_attempts,
};

fn state_with(reconnect: ReconnectConfig) -> RecoveryState {
    // `RecoveryState::new` takes the resolved `StartupEnvFailure`, if any,
    // alongside the client config: these tests exercise
    // the reconnect state machine only, so the startup side is always `None`.
    RecoveryState::new(
        ClientConfigLoadResult {
            failure: None,
            reconnect,
        },
        None,
    )
}

fn default_state() -> RecoveryState {
    state_with(ReconnectConfig::default())
}

fn connected(holds_windows: bool) -> RecoveryObservation {
    RecoveryObservation {
        control_connected: true,
        holds_windows,
    }
}

fn disconnected(holds_windows: bool) -> RecoveryObservation {
    RecoveryObservation {
        control_connected: false,
        holds_windows,
    }
}

/// The wait a spawning tick must return for its `attempt_number`-th attempt
/// (1-based), computed independently of the production saturation logic so
/// this helper cannot share a bug with it.
fn expected_attempt_wait(reconnect: &ReconnectConfig, attempt_number: u32) -> Duration {
    let mut millis: u128 = u128::from(reconnect.initial_delay_ms);
    for _ in 1..attempt_number {
        millis = millis.saturating_mul(u128::from(reconnect.backoff_factor));
    }
    let capped = millis.min(MAX_RECONNECT_DELAY.as_millis());
    Duration::from_millis(u64::try_from(capped).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------------
// Activation is the AND of "connected at least once" and
// "holds at least one window", tested across all four combinations.
// ---------------------------------------------------------------------

#[test]
fn spawn_is_withheld_when_never_connected_and_no_windows_held() {
    let mut state = default_state();
    let action = state.next_action(disconnected(false));
    assert!(!action.spawn);
}

#[test]
fn spawn_is_withheld_when_never_connected_even_with_windows_held() {
    let mut state = default_state();
    let action = state.next_action(disconnected(true));
    assert!(!action.spawn);
}

#[test]
fn spawn_is_withheld_when_previously_connected_but_no_windows_held() {
    let mut state = default_state();
    state.next_action(connected(true)); // establishes the connection track record
    let action = state.next_action(disconnected(false));
    assert!(!action.spawn);
}

#[test]
fn spawn_is_returned_when_previously_connected_and_windows_held() {
    let mut state = default_state();
    state.next_action(connected(true)); // establishes the connection track record
    let action = state.next_action(disconnected(true));
    assert!(action.spawn);
}

// ---------------------------------------------------------------------
// Once the attempt limit is reached, spawn stays withheld for
// as long as the daemon stays unreachable, no matter how many further
// ticks observe it.
// ---------------------------------------------------------------------

#[test]
fn no_further_spawn_after_the_attempt_limit_is_reached_while_still_disconnected() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 2,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1
    state.next_action(disconnected(true)); // attempt 2, the limit
    for _ in 0..5 {
        let action = state.next_action(disconnected(true));
        assert!(
            !action.spawn,
            "spawn must stay withheld once the attempt limit is reached"
        );
    }
}

// ---------------------------------------------------------------------
// Re-arm. Once a connection is observed and the state returns
// to Idle, a fresh outage can attempt up to the limit again.
// ---------------------------------------------------------------------

#[test]
fn re_arming_after_a_confirmed_connection_allows_another_full_attempt_cycle() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    let first_cycle = state.next_action(disconnected(true)); // attempt 1, the limit
    assert!(first_cycle.spawn);

    // The daemon comes back: this must re-arm the attempt budget.
    state.next_action(connected(true));

    // A second outage must be able to attempt again.
    let second_cycle = state.next_action(disconnected(true));
    assert!(
        second_cycle.spawn,
        "a fresh outage after re-arming must be able to spawn again"
    );
}

// ---------------------------------------------------------------------
// Manual reset (`rearm_recovery_attempts`) does the same thing
// as an observed reconnection, without requiring one.
// ---------------------------------------------------------------------

#[test]
fn manual_reset_allows_another_full_attempt_cycle_without_an_observed_reconnection() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    let first_cycle = state.next_action(disconnected(true)); // attempt 1, the limit
    assert!(first_cycle.spawn);

    // Exhaust it: one more disconnected tick past the limit.
    state.next_action(disconnected(true));
    assert_eq!(state.snapshot().status, RecoveryStatus::Exhausted);

    rearm_recovery_attempts(&mut state);

    let after_reset = state.next_action(disconnected(true));
    assert!(
        after_reset.spawn,
        "a manual reset must allow another attempt without an observed reconnection"
    );
}

// ---------------------------------------------------------------------
// Recovery stops the cycle -- spawn stops and the status is no
// longer Retrying -- as soon as a connection is observed.
// ---------------------------------------------------------------------

#[test]
fn spawn_stops_and_status_leaves_retrying_once_a_connection_is_observed() {
    let mut state = default_state();
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1
    assert_eq!(
        state.snapshot().status,
        RecoveryStatus::Retrying,
        "an attempt must move the status to Retrying before recovery can be observed"
    );

    let recovered = state.next_action(connected(true));
    assert!(!recovered.spawn);
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);
}

// ---------------------------------------------------------------------
// The give-up is confirmed on the *next* tick, not the tick
// that made the limit-th attempt.
// ---------------------------------------------------------------------

#[test]
fn the_limit_th_attempt_tick_does_not_itself_emit_exhausted() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    let limit_tick = state.next_action(disconnected(true)); // attempt 1, the limit
    assert!(limit_tick.spawn);
    assert_ne!(
        limit_tick.emit.map(|update| update.status),
        Some(RecoveryStatus::Exhausted),
        "Exhausted must not be emitted on the tick that made the limit-th attempt"
    );
}

#[test]
fn exhausted_is_emitted_only_once_the_next_tick_finds_the_daemon_still_unreachable() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, the limit

    let confirming_tick = state.next_action(disconnected(true));
    assert_eq!(
        confirming_tick.emit.map(|update| update.status),
        Some(RecoveryStatus::Exhausted)
    );
    assert_eq!(state.snapshot().status, RecoveryStatus::Exhausted);
}

#[test]
fn idle_is_emitted_instead_of_exhausted_when_the_tick_after_the_limit_finds_a_connection() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, the limit

    let confirming_tick = state.next_action(connected(true));
    assert_eq!(
        confirming_tick.emit.map(|update| update.status),
        Some(RecoveryStatus::Idle)
    );
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);
}

// ---------------------------------------------------------------------
// An explicitly disabled configuration never spawns, even when
// both activation conditions hold.
// ---------------------------------------------------------------------

#[test]
fn disabled_auto_respawn_withholds_spawn_even_when_both_activation_conditions_hold() {
    let mut state = state_with(ReconnectConfig {
        auto_respawn: false,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    let action = state.next_action(disconnected(true));
    assert!(!action.spawn);
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);
}

// ---------------------------------------------------------------------
// Emission conditions and generation bookkeeping.
// ---------------------------------------------------------------------

#[test]
fn retrying_is_emitted_only_on_the_tick_that_makes_the_first_attempt() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 3,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record

    let first_attempt = state.next_action(disconnected(true));
    assert_eq!(
        first_attempt.emit.map(|update| update.status),
        Some(RecoveryStatus::Retrying)
    );

    let second_attempt = state.next_action(disconnected(true));
    assert_eq!(
        second_attempt.emit, None,
        "a further attempt while already Retrying must not re-emit"
    );
}

#[test]
fn idle_is_never_emitted_from_a_tick_that_was_already_idle() {
    let mut state = default_state();
    // No attempt was ever made: the state starts and stays Idle.
    let action = state.next_action(connected(true));
    assert_eq!(action.emit, None);
}

#[test]
fn generation_increases_monotonically_and_the_first_emission_is_at_least_one() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record

    let retrying = state
        .next_action(disconnected(true)) // attempt 1, the limit
        .emit
        .expect("the first attempt must emit Retrying");
    assert!(retrying.rev >= 1);

    let exhausted = state
        .next_action(disconnected(true)) // confirms the give-up
        .emit
        .expect("the confirming tick must emit Exhausted");
    assert!(exhausted.rev > retrying.rev);

    let idle = state
        .next_action(connected(true)) // observed reconnection
        .emit
        .expect("the reconnection tick must emit Idle");
    assert!(idle.rev > exhausted.rev);
}

// ---------------------------------------------------------------------
// What a tick's wait means.
// ---------------------------------------------------------------------

#[test]
fn a_tick_that_makes_no_attempt_waits_the_tick_interval() {
    let mut state = default_state();
    let action = state.next_action(disconnected(false)); // never connected: no attempt
    assert_eq!(action.wait, TICK_INTERVAL);
}

#[test]
fn attempt_waits_grow_by_the_backoff_factor_from_the_initial_delay() {
    let reconnect = ReconnectConfig {
        initial_delay_ms: 1000,
        backoff_factor: 2,
        attempt_limit: 3,
        ..ReconnectConfig::default()
    };
    let mut state = state_with(reconnect);
    state.next_action(connected(true)); // track record

    let first = state.next_action(disconnected(true));
    assert_eq!(first.wait, expected_attempt_wait(&reconnect, 1));

    let second = state.next_action(disconnected(true));
    assert_eq!(second.wait, expected_attempt_wait(&reconnect, 2));

    let third = state.next_action(disconnected(true));
    assert_eq!(third.wait, expected_attempt_wait(&reconnect, 3));
}

#[test]
fn wait_reverts_to_the_tick_interval_once_the_attempt_limit_is_confirmed_reached() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, the limit

    let confirming_tick = state.next_action(disconnected(true));
    assert_eq!(confirming_tick.wait, TICK_INTERVAL);
}

// ---------------------------------------------------------------------
// Saturation and totality across the accepted range.
// ---------------------------------------------------------------------

#[test]
fn the_wait_never_exceeds_the_ceiling_even_at_the_top_of_the_accepted_range() {
    let reconnect = ReconnectConfig {
        initial_delay_ms: 60_000,
        backoff_factor: 10,
        attempt_limit: 10,
        ..ReconnectConfig::default()
    };
    let mut state = state_with(reconnect);
    state.next_action(connected(true)); // track record

    for attempt in 1..=10 {
        let action = state.next_action(disconnected(true));
        assert!(
            action.wait <= MAX_RECONNECT_DELAY,
            "attempt {attempt} produced a wait past the ceiling: {:?}",
            action.wait
        );
    }
}

#[test]
fn give_up_is_returned_even_after_far_more_ticks_than_the_attempt_limit() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 2,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    for _ in 0..30 {
        state.next_action(disconnected(true));
    }
    assert_eq!(state.snapshot().status, RecoveryStatus::Exhausted);
}
