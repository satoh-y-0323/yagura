#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test for the state detection engine
//! (`yagura_daemon::detector::engine`).
//!
//! Every mutating entry point returns `Some(new_status)` only when the confirmed
//! status actually changed, and `None` otherwise; the assertions below read that
//! return value as the engine's event stream.
//!
//! The engine is driven by explicit calls rather than owning a task, so its
//! time-dependent behaviour is verifiable under `tokio::time::pause()` /
//! `advance()` without racing a background task. The `DetectorHandle` cases drive
//! their own task and therefore run in real time.

mod common;

use common::ScreenFeed;
use std::time::Duration;

use common::{
    SCREEN_COLS, SCREEN_ROWS, TIME_BOTH_PROMPT_DEF, advance_ms, feed_handle, new_engine, repaint,
    screen_of, tick_after_ms,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{
    DetectionEngine, DetectorHandle, InputEcho, TURN_ACTIVE_QUIET,
};
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Pattern-only definition: no `[quiescence]`, no `[activity]`, so any
/// unmatched screen must fall back to `Unknown`.
const PATTERN_DEF: &str = r#"
schema_version = 1
name = "test-pattern"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = 'Human:'
priority = 10
region = "bottom"
"#;

/// Time-based judgement configuration (a): both tables opted in (`window_ms` 500 /
/// `after_ms` 2000 / `confirm_ms` 300). The single rule never matches, so only the
/// time-based branch is exercised.
const TIME_BOTH_DEF: &str = r#"
schema_version = 1
name = "test-time-both"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'zzz-never-matches-zzz'
priority = 10
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000

[activity]
status = "running"
window_ms = 500
"#;

/// Time-based judgement configuration (b): `[quiescence]` only. The rule is
/// a matching one so that the "no hold region in one-sided configurations"
/// rule can be observed when the matched text disappears.
const TIME_QUIESCENCE_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-time-quiescence-only"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed'
priority = 10
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000
"#;

/// Time-based judgement configuration (c): `[activity]` only.
const TIME_ACTIVITY_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-time-activity-only"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'zzz-never-matches-zzz'
priority = 10
region = "bottom"

[activity]
status = "running"
window_ms = 500
"#;

/// Time-based judgement configuration (d): neither table.
const TIME_NONE_DEF: &str = r#"
schema_version = 1
name = "test-time-none"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'zzz-never-matches-zzz'
priority = 10
region = "bottom"
"#;

/// No `bottom_line_count`: the engine must substitute
/// `DaemonConfig::bottom_line_count` (5).
const MARKER_DEF_WITHOUT_COUNT: &str = r#"
schema_version = 1
name = "test-marker-default-count"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'MARKER'
priority = 10
region = "bottom"
"#;

/// Same rule, but the definition narrows the matched region to 3 lines.
const MARKER_DEF_WITH_COUNT_THREE: &str = r#"
schema_version = 1
name = "test-marker-count-three"
bottom_line_count = 3

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'MARKER'
priority = 10
region = "bottom"
"#;

/// Two rules matching the same text with different priorities.
const PRIORITY_DEF: &str = r#"
schema_version = 1
name = "test-priority"

[debounce]
confirm_ms = 300

[[rules]]
status = "running"
pattern = 'ambiguous-output'
priority = 5
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = 'ambiguous-output'
priority = 50
region = "bottom"
"#;

/// A definition whose `[debounce]` differs from the `DaemonConfig` default.
const SLOW_DEBOUNCE_DEF: &str = r#"
schema_version = 1
name = "test-slow-debounce"

[debounce]
confirm_ms = 1000

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed'
priority = 10
region = "bottom"
"#;

/// A definition without a `[debounce]` table: `DaemonConfig::confirm_ms`
/// applies.
const NO_DEBOUNCE_DEF: &str = r#"
schema_version = 1
name = "test-no-debounce"

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed'
priority = 10
region = "bottom"
"#;

/// A shell-like definition used for the representative-sample sequence.
const SHELL_LIKE_DEF: &str = r#"
schema_version = 1
name = "test-shell-like"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = '\[y/N\]'
priority = 20
region = "bottom"

[[rules]]
status = "idle"
pattern = '^PS .*>$'
priority = 10
region = "bottom"
"#;

// --- fail-safe (mandatory cases) -------------------------------------------

#[tokio::test]
async fn unmatched_output_keeps_confirmed_status_unknown() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    assert_eq!(engine.feed(b"nothing here matches any rule\r\n"), None);
    assert_eq!(
        tick_after_ms(&mut engine, 5_000).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn missing_definition_keeps_confirmed_status_unknown() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    assert_eq!(engine.feed(b"Do you want to proceed?\r\n"), None);
    assert_eq!(tick_after_ms(&mut engine, 5_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn an_arrival_notice_with_no_new_output_keeps_the_screen_and_its_status() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );

    // What a subscriber that fell behind reports: the model already holds the
    // output, so nothing about the screen was lost.
    assert_eq!(engine.on_output(), None);
    assert_eq!(engine.status(), WindowStatus::WaitingApproval);
    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::WaitingApproval);
}

// --- pattern matching ------------------------------------------------------

#[tokio::test]
async fn pattern_match_transitions_confirmed_status() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    assert_eq!(engine.feed(b"Human: "), None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingInput)
    );
    assert_eq!(engine.status(), WindowStatus::WaitingInput);
}

#[tokio::test]
async fn highest_priority_rule_wins_when_several_rules_match() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PRIORITY_DEF, &config);

    engine.feed(b"ambiguous-output");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingInput)
    );
}

#[tokio::test]
async fn resize_keeps_pattern_matching_functional() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.screen().lock().resize(120, 4);
    engine.on_resized();
    engine.feed(b"Do you want to proceed?");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

// --- debounce --------------------------------------------------------------

#[tokio::test]
async fn debounce_holds_transition_until_confirm_ms_elapsed() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert_eq!(tick_after_ms(&mut engine, 299).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn debounce_suppresses_transition_when_raw_judgement_flaps() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    advance_ms(200).await;
    assert_eq!(engine.feed(&repaint("still working")), None);

    assert_eq!(
        tick_after_ms(&mut engine, 5_000).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn definition_debounce_overrides_config_default() {
    tokio::time::pause();
    let config = DaemonConfig {
        confirm_ms: 300,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(SLOW_DEBOUNCE_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert_eq!(tick_after_ms(&mut engine, 999).await, None);
    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn config_confirm_ms_applies_when_definition_omits_debounce() {
    tokio::time::pause();
    let config = DaemonConfig {
        confirm_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(NO_DEBOUNCE_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert_eq!(tick_after_ms(&mut engine, 99).await, None);
    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn a_pending_debounce_schedules_a_deadline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert!(
        engine.next_deadline().is_some(),
        "a pending debounce must wake the driver task"
    );
}

// --- exit ------------------------------------------------------------------

#[tokio::test]
async fn exit_notification_confirms_exited_without_debounce() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    // No clock advance at all: `Exited` is debounce-exempt.
    assert_eq!(engine.on_exit(), Some(DetectedStatus::Exited));
    assert_eq!(engine.status(), WindowStatus::Exited);
}

#[tokio::test]
async fn exited_outranks_later_pattern_matches() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.on_exit();
    assert_eq!(engine.feed(b"Do you want to proceed?"), None);
    assert_eq!(tick_after_ms(&mut engine, 5_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Exited);
}

#[tokio::test]
async fn detector_handle_publishes_exited_without_debounce() {
    let config = DaemonConfig::default();
    let engine = new_engine(PATTERN_DEF, &config);
    let (handle, mut statuses) = DetectorHandle::spawn(engine);

    handle.notify_exited();

    let published = tokio::time::timeout(Duration::from_secs(5), statuses.recv())
        .await
        .expect("no status was published within 5s")
        .expect("status channel closed before publishing");
    assert_eq!(published.status, DetectedStatus::Exited);
}

// --- structured source -----------------------------------------

#[tokio::test]
async fn an_open_turn_outranks_a_waiting_input_screen_while_output_is_recent() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Human: ");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingInput)
    );

    assert_eq!(engine.on_status_event(WindowStatus::Running), None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn an_open_turn_returns_to_the_screen_once_output_has_been_quiet() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"Human: ");
    tick_after_ms(&mut engine, 300).await;
    engine.on_status_event(WindowStatus::Running);
    tick_after_ms(&mut engine, 300).await;
    assert_eq!(engine.status(), WindowStatus::Running);

    // The last output was at t=0, so the turn is over at TURN_ACTIVE_QUIET;
    // the screen still shows the `waiting_input` pattern.
    let quiet_ms = u64::try_from(TURN_ACTIVE_QUIET.as_millis()).unwrap();
    assert_eq!(tick_after_ms(&mut engine, quiet_ms - 600 - 1).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);
    assert_eq!(tick_after_ms(&mut engine, 1).await, None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingInput)
    );
}

#[tokio::test]
async fn an_approval_report_has_no_expiry_over_a_screen_no_rule_matches() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    assert_eq!(engine.on_status_event(WindowStatus::WaitingApproval), None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );

    assert_eq!(tick_after_ms(&mut engine, 600_000).await, None);
    // A judgement the silence started would be confirmed after the debounce.
    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::WaitingApproval);
}

// --- time-based judgement: (a) both tables ---------------------------------

#[tokio::test]
async fn both_time_judgements_bridge_activity_to_quiescence_without_unknown() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    let mut observed = Vec::new();
    push(&mut observed, engine.feed(b"working"));

    // t=300: the activity judgement (t < window_ms) clears the debounce.
    push(&mut observed, tick_after_ms(&mut engine, 300).await);
    // t=500: the hold region begins; the previous raw judgement is kept.
    push(&mut observed, tick_after_ms(&mut engine, 200).await);
    // t=1999: still inside the hold region.
    push(&mut observed, tick_after_ms(&mut engine, 1_499).await);
    // t=2000: the quiescence judgement starts, still debouncing.
    push(&mut observed, tick_after_ms(&mut engine, 1).await);
    // t=2300: after_ms + confirm_ms.
    push(&mut observed, tick_after_ms(&mut engine, 300).await);

    assert_eq!(observed, vec![WindowStatus::Running, WindowStatus::Idle]);
}

#[tokio::test]
async fn hold_region_keeps_the_previous_raw_judgement() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.feed(b"working");
    tick_after_ms(&mut engine, 300).await;

    // Inside `window_ms <= t < after_ms` no new raw judgement is produced.
    assert_eq!(tick_after_ms(&mut engine, 200).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);
    assert_eq!(tick_after_ms(&mut engine, 1_499).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);
}

// --- time-based judgement: (b) quiescence only -----------------------------

#[tokio::test]
async fn quiescence_only_confirms_after_after_ms_plus_confirm_ms() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );
}

#[tokio::test]
async fn quiescence_only_does_not_hold_a_vanished_pattern_status() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);

    engine.feed(b"Do you want to proceed?");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );

    // The prompt disappears well before `after_ms`: a one-sided configuration
    // has no hold region, so the engine must fall back to `Unknown`.
    engine.feed(&repaint("still working"));
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
}

// --- time-based judgement: (c) activity only -------------------------------

#[tokio::test]
async fn activity_only_reports_activity_status_inside_the_window() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn activity_only_falls_back_to_unknown_after_window_ms() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    tick_after_ms(&mut engine, 300).await;

    // t=500: `t >= window_ms` with no pattern match means `Unknown`, debounced.
    assert_eq!(tick_after_ms(&mut engine, 200).await, None);
    assert_eq!(tick_after_ms(&mut engine, 299).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
}

// --- time-based judgement: (d) neither table -------------------------------

#[tokio::test]
async fn without_time_judgements_an_unmatched_screen_stays_unknown() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_NONE_DEF, &config);

    engine.feed(b"working");
    assert_eq!(
        tick_after_ms(&mut engine, 500).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(tick_after_ms(&mut engine, 30_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

// --- input echo attribution --------------------------------------------------
//
// Every fixture here has both `[activity]` and `[quiescence]` opted in (or
// deliberately just one, for the one-sided invariance cases), since the
// attribution gate is only ever built for two-sided definitions.

#[tokio::test]
async fn suppressed_output_right_after_echo_only_notice_does_not_confirm_running() {
    // Without suppression the construction-time activity band, still open at
    // t=0, would carry this straight to `Running`.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn output_arriving_just_before_the_input_echo_window_is_suppressed() {
    // Boundary just under `input_echo_window_ms`.
    tokio::time::pause();
    let config = DaemonConfig {
        input_echo_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    advance_ms(99).await;
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn output_arriving_at_the_input_echo_window_exactly_is_not_suppressed() {
    // The comparison is a strict `<`, so an arrival at exactly
    // `input_echo_window_ms` must count as activity.
    tokio::time::pause();
    let config = DaemonConfig {
        input_echo_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    advance_ms(100).await;
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn suppressed_then_unsuppressed_output_reaches_running() {
    // A suppressed chunk followed by one that arrives outside the echo window
    // must still reopen the activity band.
    tokio::time::pause();
    let config = DaemonConfig {
        input_echo_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);
    advance_ms(200).await;
    assert_eq!(engine.feed(b"y"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn suppressed_output_still_feeds_the_screen_for_rule_matching() {
    // Rules are evaluated before the time-based judgement, so a suppressed chunk
    // must still reach the virtual screen.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_PROMPT_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"PS C:\\work>"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );
}

#[tokio::test]
async fn may_start_work_disarms_a_still_active_echo_only_notice() {
    // Sending a line must let its own output count as activity right away, even
    // though it follows closely on an `EchoOnly` notice.
    tokio::time::pause();
    let config = DaemonConfig {
        input_echo_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    advance_ms(50).await; // still inside the 100ms echo window
    engine.on_input(InputEcho::MayStartWork);
    // Without disarming, this would still fall inside the echo window.
    assert_eq!(engine.feed(b"running the command"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn suppression_cannot_produce_idle_before_after_ms_elapses() {
    // The safety envelope forbids `Idle` from appearing here purely because
    // activity evidence was suppressed. Observed between `confirm_ms` and
    // `after_ms`, the only window in which a broken implementation disagrees.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);

    let changed = tick_after_ms(&mut engine, 1_000).await;
    assert_ne!(
        changed,
        Some(DetectedStatus::Idle),
        "suppression must not confirm Idle before after_ms elapses"
    );
    assert_ne!(
        engine.status(),
        WindowStatus::Idle,
        "suppression must not confirm Idle before after_ms elapses"
    );
}

#[tokio::test]
async fn echo_only_after_running_is_confirmed_keeps_running() {
    // The hold region keeps a confirmed `Running` when the activity band closes
    // afterward.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.feed(b"working");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);
    assert_eq!(tick_after_ms(&mut engine, 1_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);
}

#[tokio::test]
async fn activity_band_reopens_once_the_echo_window_elapses_without_new_input() {
    // Residual-risk upper bound: once `input_echo_window_ms` passes with no
    // further `on_input` call, the next output is no longer attributed to the
    // earlier keystroke.
    tokio::time::pause();
    let config = DaemonConfig {
        input_echo_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    advance_ms(150).await; // past the echo window, no further keystrokes
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn on_input_does_not_change_the_quiescence_only_timeline() {
    // `[activity]`-less definitions must be entirely unaffected: the attribution
    // gate is confined to two-sided definitions.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );
}

#[tokio::test]
async fn on_input_does_not_change_the_activity_only_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );

    assert_eq!(tick_after_ms(&mut engine, 200).await, None);
    assert_eq!(tick_after_ms(&mut engine, 299).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
}

#[tokio::test]
async fn on_input_does_not_change_the_no_time_judgement_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_NONE_DEF, &config);

    engine.feed(b"working");
    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(
        tick_after_ms(&mut engine, 500).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(tick_after_ms(&mut engine, 30_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn on_input_does_not_change_the_no_definition_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"Do you want to proceed?\r\n"), None);
    assert_eq!(tick_after_ms(&mut engine, 5_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn detector_handle_suppresses_echo_before_publishing_activity() {
    // The async wrapper. `input_echo_window_ms` is set generously so suppression
    // stays in effect for the whole observation window regardless of scheduling
    // jitter; this runs in real time because `DetectorHandle` drives its own task.
    let config = DaemonConfig {
        input_echo_window_ms: 2_000,
        ..DaemonConfig::default()
    };
    let engine = new_engine(TIME_BOTH_DEF, &config);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);

    handle.notify_input(InputEcho::EchoOnly);
    feed_handle(&screen, &handle, b"x");

    let result = tokio::time::timeout(Duration::from_millis(600), statuses.recv()).await;
    assert!(
        result.is_err(),
        "notify_input(EchoOnly) sent before the output must suppress the echo, so no status \
         change should publish while the window is still open"
    );
}

#[tokio::test]
async fn a_freshly_constructed_both_sided_engine_reaches_running_without_any_output() {
    // A window that has produced no output at all must still reach `Running` from
    // the construction-time activity band alone.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    assert_eq!(engine.on_tick(), None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
    assert_eq!(engine.status(), WindowStatus::Running);
}

#[tokio::test]
async fn next_deadline_keeps_the_quiescence_wakeup_after_the_activity_band_closes() {
    // Once the activity band is closed, `next_deadline()` must still return
    // exactly the quiescence boundary -- not `None`, and not the stale activity
    // boundary. Asserting the value rather than just `Some` is what pins that.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_input(InputEcho::EchoOnly);
    let suppressed_at = tokio::time::Instant::now();
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(
        engine.next_deadline(),
        Some(suppressed_at + Duration::from_millis(2_000)),
        "the activity term must vanish but the quiescence wakeup must remain, so the driver \
         never sleeps forever (no `?`-style early return may drop the remaining term)"
    );
}

#[tokio::test]
async fn next_deadline_stays_scheduled_for_activity_only_definitions_after_echo_only() {
    // A one-sided `[activity]` definition never builds an attribution gate, so
    // its own `window_ms` wakeup must survive `on_input`.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
    engine.on_input(InputEcho::EchoOnly);

    assert!(
        engine.next_deadline().is_some(),
        "a one-sided [activity] definition must keep its window_ms wakeup even after on_input"
    );
}

#[tokio::test]
async fn suppressed_echo_after_prompt_match_keeps_confirmed_idle() {
    // A prompt-shaped rule match confirms `Idle`; the next keystroke's echo
    // breaks the match but must flip the confirmed status neither to `Running`
    // nor to `Unknown` -- this fixture is two-sided, so the hold region applies.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_PROMPT_DEF, &config);

    engine.feed(b"PS C:\\work>");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );

    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Idle);
}

// --- resize redraw attribution ------------------------------------------------
//
// Every fixture here has both `[activity]` and `[quiescence]` opted in (or
// deliberately fewer, for the envelope cases), since the attribution gate is only
// ever built for two-sided definitions, the same restriction as echo attribution.

/// Advances past `TIME_BOTH_DEF`'s `window_ms` in a single jump with no evaluation
/// in between, so the construction-time activity band is closed while `status()`
/// still reads the never-evaluated construction value (`Unknown`). Callers depend
/// on that pair: without it a freshly constructed engine reaches `Running` on its
/// own and resize suppression cannot be told apart from a still-open band.
async fn close_construction_band(engine: &mut DetectionEngine) {
    advance_ms(500).await;
    assert_eq!(
        engine.status(),
        WindowStatus::Unknown,
        "status() must still read the never-evaluated construction-time value; a call to \
         on_tick or on_output before this point would have let the engine's own activity \
         timeline confirm Running first and defeat this precondition"
    );
}

#[tokio::test]
async fn suppressed_resize_redraw_output_does_not_confirm_running() {
    // A resize's own redrawn output must not read as activity evidence while
    // resize_redraw_window_ms is still open.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);
    close_construction_band(&mut engine).await;

    engine.on_resize_started();
    assert_eq!(engine.feed(b"redrawn screen contents"), None);

    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn output_arriving_after_the_resize_redraw_window_elapses_reaches_running() {
    // Once resize_redraw_window_ms has elapsed with no further resize, the next
    // output is no longer attributed and counts as activity evidence again.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);
    close_construction_band(&mut engine).await;

    engine.on_resize_started();
    advance_ms(150).await; // past resize_redraw_window_ms, no further resize
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn may_start_work_disarms_a_still_active_resize_redraw_window() {
    // A submitted line's own output must count as activity right away even
    // though it follows closely on an armed resize.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);
    close_construction_band(&mut engine).await;

    engine.on_resize_started();
    advance_ms(50).await; // still inside the 100ms resize window
    engine.on_input(InputEcho::MayStartWork);
    // Without disarming, this would still fall inside the resize window.
    assert_eq!(engine.feed(b"running the command"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
}

#[tokio::test]
async fn rearming_resize_attribution_suppresses_output_in_the_second_window() {
    // A live window is often resized more than once (a drag), and each arm must
    // independently protect its own window, not just the first one seen.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 100,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);
    close_construction_band(&mut engine).await;

    engine.on_resize_started(); // arm #1
    advance_ms(150).await; // window #1 (100ms) elapses with no output
    engine.on_resize_started(); // arm #2

    assert_eq!(engine.feed(b"redrawn again"), None);
    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn resize_arm_does_not_close_a_band_opened_by_real_output() {
    // Unlike `InputEcho::EchoOnly`, arming a resize must never close an activity
    // band a real output already opened, or a window mid-work would lose its
    // Running status just because it got resized.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 50,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_PROMPT_DEF, &config);

    // Real output opens the activity band and matches the prompt rule.
    engine.feed(b"PS C:\\work>");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );

    engine.on_resize_started();
    advance_ms(10).await; // still inside the 50ms resize window
    // Breaks the rule match: the bottom line no longer ends in the prompt
    // pattern, so judgement falls through to the time-based branch.
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running),
        "the activity band real output opened must survive an intervening resize arm; a \
         resize must not close it"
    );
}

// The attribution gate is confined to two-sided definitions, so
// `on_resize_started` is a no-op for the definitions below; each is compared
// against its already-known timeline.

#[tokio::test]
async fn on_resize_started_does_not_change_the_quiescence_only_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    engine.on_resize_started();
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);

    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle)
    );
}

#[tokio::test]
async fn on_resize_started_does_not_change_the_activity_only_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    engine.on_resize_started();
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );

    assert_eq!(tick_after_ms(&mut engine, 200).await, None);
    assert_eq!(tick_after_ms(&mut engine, 299).await, None);
    assert_eq!(engine.status(), WindowStatus::Running);

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
}

#[tokio::test]
async fn on_resize_started_does_not_change_the_no_time_judgement_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_NONE_DEF, &config);

    engine.feed(b"working");
    engine.on_resize_started();
    assert_eq!(
        tick_after_ms(&mut engine, 500).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(tick_after_ms(&mut engine, 2_000).await, None);
    assert_eq!(tick_after_ms(&mut engine, 30_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn on_resize_started_does_not_change_the_no_definition_timeline() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    engine.on_resize_started();
    assert_eq!(engine.feed(b"Do you want to proceed?\r\n"), None);
    assert_eq!(tick_after_ms(&mut engine, 5_000).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn echo_only_still_closes_the_band_immediately_while_a_resize_is_armed() {
    // Resize and echo attribution act independently through separate arm timers,
    // so an `EchoOnly` notice's own immediate-close behaviour must be unaffected
    // by a resize also being armed.
    tokio::time::pause();
    let config = DaemonConfig {
        resize_redraw_window_ms: 5_000,
        ..DaemonConfig::default()
    };
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    engine.on_resize_started();
    engine.on_input(InputEcho::EchoOnly);
    assert_eq!(engine.feed(b"x"), None);

    assert_eq!(tick_after_ms(&mut engine, 300).await, None);
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn detector_handle_suppresses_resize_redraw_before_publishing_activity() {
    // The async wrapper, in real time because `DetectorHandle` drives its own
    // task. A resize arm does not close the construction-time activity band on
    // its own, so the driver is first left to run past `window_ms` with no input
    // -- the same evaluation-free precondition `close_construction_band` gives.
    let config = DaemonConfig {
        resize_redraw_window_ms: 5_000,
        ..DaemonConfig::default()
    };
    let engine = new_engine(TIME_BOTH_DEF, &config);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);

    tokio::time::sleep(Duration::from_millis(650)).await;
    assert!(
        statuses.try_recv().is_err(),
        "the construction-time activity band must have closed with nothing else happening \
         yet -- if this fires, the fixture's own timeline (not resize suppression) is what \
         changed, and the assertion below would prove nothing"
    );

    handle.notify_resize_started();
    feed_handle(&screen, &handle, b"x");

    let result = tokio::time::timeout(Duration::from_millis(600), statuses.recv()).await;
    assert!(
        result.is_err(),
        "notify_resize_started() sent before the output must suppress the redraw output, so \
         no status change should publish while the window is still open"
    );
}

// --- bottom_line_count default substitution --------------------

/// Five display lines with the marker on the topmost one, i.e. the 5th line
/// counted from the bottom.
fn marker_screen() -> &'static [u8] {
    b"MARKER\r\nfiller-1\r\nfiller-2\r\nfiller-3\r\nfiller-4"
}

#[tokio::test]
async fn bottom_line_count_falls_back_to_the_config_default() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    assert_eq!(config.bottom_line_count, 5);
    let mut engine = new_engine(MARKER_DEF_WITHOUT_COUNT, &config);

    engine.feed(marker_screen());
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn bottom_line_count_from_the_definition_wins_over_the_config_default() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(MARKER_DEF_WITH_COUNT_THREE, &config);

    // The marker sits outside the bottom 3 lines, so it must not match.
    engine.feed(marker_screen());
    assert_eq!(
        tick_after_ms(&mut engine, 5_000).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

// --- region = "screen" ------------------------------------------------------

/// The marker rule read over the whole screen while the bottom window is one row.
const MARKER_SCREEN_DEF: &str = r#"
schema_version = 1
name = "test-marker-screen"
bottom_line_count = 1

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'MARKER'
priority = 10
region = "screen"
"#;

/// A screen rule next to a composer whose body is taken out before matching.
const MARKER_SCREEN_COMPOSER_DEF: &str = r#"
schema_version = 1
name = "test-marker-screen-composer"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'MARKER'
priority = 10
region = "screen"

[composer]
marker = '^>'
separator = " "
continuation_prefix = "  "
"#;

#[tokio::test]
async fn a_screen_rule_matches_a_row_above_the_bottom_window() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(MARKER_SCREEN_DEF, &config);

    engine.feed(marker_screen());
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn a_screen_rule_does_not_match_the_composer_body() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let draft = "header\r\n> MARKER";

    // The cursor stays visible at the end of the draft, so the body is taken out.
    let mut engine = new_engine(MARKER_SCREEN_COMPOSER_DEF, &config);
    engine.feed(&repaint(draft));
    tick_after_ms(&mut engine, 5_000).await;
    tick_after_ms(&mut engine, 5_000).await;
    assert_eq!(engine.status(), WindowStatus::Unknown);

    // With the cursor hidden the composer is not located, so the same rows match once
    // the hidden-cursor screen has settled and the verdict has been confirmed.
    let mut hidden = new_engine(MARKER_SCREEN_COMPOSER_DEF, &config);
    hidden.feed(&repaint(draft));
    hidden.feed(b"\x1b[?25l");
    tick_after_ms(&mut hidden, 5_000).await;
    tick_after_ms(&mut hidden, 5_000).await;
    assert_eq!(hidden.status(), WindowStatus::WaitingApproval);
}

// --- event emission --------------------------------------------------------

#[tokio::test]
async fn no_event_is_emitted_while_the_confirmed_status_is_unchanged() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(PATTERN_DEF, &config);

    let mut observed = Vec::new();
    push(&mut observed, engine.feed(b"Do you want to proceed?"));
    push(&mut observed, tick_after_ms(&mut engine, 300).await);
    push(&mut observed, tick_after_ms(&mut engine, 300).await);
    push(
        &mut observed,
        engine.feed(&repaint("Do you want to proceed?")),
    );
    push(&mut observed, tick_after_ms(&mut engine, 5_000).await);

    assert_eq!(observed, vec![WindowStatus::WaitingApproval]);
}

// --- representative sample sequence ----------------------------------------

#[tokio::test]
async fn representative_output_sample_produces_a_fixed_status_sequence() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(SHELL_LIKE_DEF, &config);

    let mut observed = Vec::new();

    push(&mut observed, engine.feed(&repaint("PS C:\\work>")));
    push(&mut observed, tick_after_ms(&mut engine, 300).await);

    push(&mut observed, engine.feed(&repaint("running task...")));
    push(&mut observed, tick_after_ms(&mut engine, 300).await);

    push(
        &mut observed,
        engine.feed(&repaint("Delete all files? [y/N]")),
    );
    push(&mut observed, tick_after_ms(&mut engine, 300).await);

    push(&mut observed, engine.feed(&repaint("PS C:\\work>")));
    push(&mut observed, tick_after_ms(&mut engine, 300).await);

    assert_eq!(
        observed,
        vec![
            WindowStatus::Idle,
            WindowStatus::Unknown,
            WindowStatus::WaitingApproval,
            WindowStatus::Idle,
        ]
    );
}

#[tokio::test]
async fn an_echo_only_input_leaves_a_quiescence_only_timeline_unchanged() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut plain = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);
    let mut echoed = new_engine(TIME_QUIESCENCE_ONLY_DEF, &config);

    plain.feed(b"abc");
    echoed.feed(b"abc");
    echoed.on_input(InputEcho::EchoOnly);

    let mut plain_timeline = Vec::new();
    let mut echoed_timeline = Vec::new();
    for step_ms in [1_000, 1_000, 299, 1, 5_000] {
        advance_ms(step_ms).await;
        plain_timeline.push(plain.on_tick());
        echoed_timeline.push(echoed.on_tick());
    }

    assert!(
        plain_timeline.contains(&Some(DetectedStatus::Idle)),
        "the baseline must reach the quiescence status, or the comparison is vacuous:          {plain_timeline:?}"
    );
    assert_eq!(
        echoed_timeline, plain_timeline,
        "an echo-only input must not move a one-sided definition's status timeline"
    );
}

// --- DaemonConfig defaults -------------------------------------------------

#[test]
fn daemon_config_defaults_expose_the_detector_parameters() {
    let config = DaemonConfig::default();

    assert_eq!(config.confirm_ms, 300);
    assert_eq!(config.bottom_line_count, 5);
    assert_eq!(config.detection_name, "generic-shell");
    assert!(config.detection_dir.is_none());
}

fn push(observed: &mut Vec<WindowStatus>, changed: Option<DetectedStatus>) {
    if let Some(status) = changed {
        observed.push(status.status());
    }
}
