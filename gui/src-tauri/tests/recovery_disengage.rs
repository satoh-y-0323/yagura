//! `recovery_state` behavior: losing every window while `Retrying` must
//! disengage the retry cycle back to `Idle` and reset the attempt budget,
//! rather than leaving the status stuck on a cycle that is no longer trying
//! anything (the observation that drives it, `holds_windows`, has gone
//! false). `Exhausted` is a report that a completed cycle failed and must
//! not be overwritten by this disengage.
//!
//! None of the tests below advance real or mocked time.

use yagura_gui::client_config::{ClientConfigLoadResult, ReconnectConfig};
use yagura_gui::recovery_state::{RecoveryObservation, RecoveryState, RecoveryStatus};

fn state_with(reconnect: ReconnectConfig) -> RecoveryState {
    // `RecoveryState::new` takes the resolved `StartupEnvFailure`, if any,
    // alongside the client config: these tests exercise
    // the disengage behavior only, so the startup side is always `None`.
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

// ---------------------------------------------------------------------
// Losing the last window while Retrying emits Idle.
// ---------------------------------------------------------------------

#[test]
fn losing_windows_while_retrying_emits_idle() {
    let mut state = default_state();
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, status -> Retrying
    assert_eq!(state.snapshot().status, RecoveryStatus::Retrying);
    let rev_before = state.snapshot().rev;

    let disengage = state.next_action(disconnected(false)); // the last window is gone
    assert_eq!(
        disengage.emit.map(|update| update.status),
        Some(RecoveryStatus::Idle),
        "losing every window while Retrying must emit Idle"
    );
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);
    assert!(state.snapshot().rev > rev_before);
}

// ---------------------------------------------------------------------
// Once disengaged, spawn does not return while the windows
// stay lost -- already guaranteed by the activation AND (`holds_windows`
// is one of its terms), independent of the Idle emission this file adds.
// ---------------------------------------------------------------------

#[test]
fn spawn_stays_withheld_while_windows_remain_lost_after_disengaging() {
    let mut state = default_state();
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, status -> Retrying

    for _ in 0..5 {
        let action = state.next_action(disconnected(false));
        assert!(
            !action.spawn,
            "spawn must stay withheld while no window is held"
        );
    }
}

// ---------------------------------------------------------------------
// The attempt budget is reset by the disengage, so a later
// outage (once a window is held again) gets a full cycle of attempts.
// ---------------------------------------------------------------------

#[test]
fn disengaging_resets_the_attempt_budget_for_a_later_outage() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 3,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1 of 3

    state.next_action(disconnected(false)); // the last window is gone: disengage
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);

    // A window is held again while the daemon is still unreachable: the
    // budget must allow a full cycle up to the limit again.
    for attempt in 1..=3 {
        let action = state.next_action(disconnected(true));
        assert!(
            action.spawn,
            "attempt {attempt} after the budget reset must spawn"
        );
    }
}

// ---------------------------------------------------------------------
// Idle is emitted once, at the disengaging tick, not again on
// every later tick that still finds no window held.
// ---------------------------------------------------------------------

#[test]
fn idle_is_not_re_emitted_on_further_ticks_that_still_find_no_window_held() {
    let mut state = default_state();
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, status -> Retrying

    let disengage = state.next_action(disconnected(false));
    assert_eq!(
        disengage.emit.map(|update| update.status),
        Some(RecoveryStatus::Idle)
    );

    let later = state.next_action(disconnected(false));
    assert_eq!(
        later.emit, None,
        "a further tick that still finds no window held must not re-emit Idle"
    );
}

// ---------------------------------------------------------------------
// Exhausted is a report of a completed cycle and must not be
// overwritten by the disengage when windows are lost afterward.
// ---------------------------------------------------------------------

#[test]
fn exhausted_does_not_move_to_idle_when_windows_are_lost_afterward() {
    let mut state = state_with(ReconnectConfig {
        attempt_limit: 1,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record
    state.next_action(disconnected(true)); // attempt 1, the limit
    let confirm = state.next_action(disconnected(true)); // confirms the give-up
    assert_eq!(
        confirm.emit.map(|update| update.status),
        Some(RecoveryStatus::Exhausted)
    );

    let after_losing_windows = state.next_action(disconnected(false));
    assert_eq!(
        after_losing_windows.emit, None,
        "Exhausted must not transition to Idle when windows are lost afterward"
    );
    assert_eq!(state.snapshot().status, RecoveryStatus::Exhausted);
}

// ---------------------------------------------------------------------
// With auto-respawn disabled, the state never enters
// Retrying, so this disengage never has a cycle to break out of.
// ---------------------------------------------------------------------

#[test]
fn disabled_auto_respawn_never_enters_retrying_so_disengage_never_fires() {
    let mut state = state_with(ReconnectConfig {
        auto_respawn: false,
        ..ReconnectConfig::default()
    });
    state.next_action(connected(true)); // track record

    let would_be_attempt = state.next_action(disconnected(true));
    assert!(!would_be_attempt.spawn);
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);

    let after = state.next_action(disconnected(false));
    assert_eq!(after.emit, None);
    assert_eq!(state.snapshot().status, RecoveryStatus::Idle);
}
