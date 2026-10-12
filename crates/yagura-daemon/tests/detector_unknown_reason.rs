#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Contract for the detection engine's carried Unknown reason
//! (`yagura_protocol::DetectedStatus` / `UnknownReason`) and the engine's
//! `DetectionEngine::detected()` accessor.
//!
//! `DetectionEngine::status()` projects to the plain
//! `yagura_protocol::WindowStatus` every readiness / destructive gate
//! matches on; `detected()` also carries why, when the status is `Unknown`.

mod common;

use common::ScreenFeed;
use common::{SCREEN_COLS, SCREEN_ROWS, advance_ms, new_engine, screen_of};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::DetectionEngine;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Advances the paused clock by `ms`, then ticks the engine.
async fn tick_after_ms(engine: &mut DetectionEngine, ms: u64) -> Option<DetectedStatus> {
    advance_ms(ms).await;
    engine.on_tick()
}

/// No time-based judgement, one rule that never matches: the reason a
/// definition-carrying engine settles on once the screen holds text the rule
/// does not recognize.
const NEVER_MATCH_DEF: &str = r#"
schema_version = 1
name = "test-unknown-reason-never-match"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'zzz-never-matches-zzz'
priority = 10
region = "bottom"
"#;

/// `NEVER_MATCH_DEF`'s `[debounce] confirm_ms`.
const NEVER_MATCH_CONFIRM_MS: u64 = 300;

/// `[activity]` only (`window_ms` 500), `confirm_ms` 300, one never-matching
/// rule.
const ACTIVITY_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-unknown-reason-activity-only"

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

/// `[quiescence]` only (`after_ms` 2000), `confirm_ms` 300, one
/// never-matching rule.
const QUIESCENCE_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-unknown-reason-quiescence-only"

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
"#;

/// One rule declaring the status `unknown` outright: a definition author's
/// explicit "I don't know" rather than an unmatched screen.
const DECLARED_UNKNOWN_RULE_DEF: &str = r#"
schema_version = 1
name = "test-unknown-reason-declared-rule"

[debounce]
confirm_ms = 300

[[rules]]
status = "unknown"
pattern = 'DECLARED-UNKNOWN-MARKER'
priority = 10
region = "bottom"
"#;

/// Both `[activity]` (`window_ms` 500) and `[quiescence]` (`after_ms` 2000),
/// `confirm_ms` 300, one never-matching rule: the two-sided timeline the
/// screen-loss cases exercise.
const TIME_BOTH_DEF: &str = r#"
schema_version = 1
name = "test-unknown-reason-time-both"

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

// =============================================================================
// Reason derivation, one case per origin
// =============================================================================

#[tokio::test]
async fn no_definition_reports_no_definition() {
    let config = DaemonConfig::default();
    let engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoDefinition)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn a_freshly_constructed_engine_with_a_definition_reports_not_yet_judged() {
    let config = DaemonConfig::default();
    let engine = new_engine(NEVER_MATCH_DEF, &config);

    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NotYetJudged)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn an_unmatched_screen_with_no_time_judgement_reports_no_rule_matched() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(NEVER_MATCH_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    assert_eq!(
        tick_after_ms(&mut engine, NEVER_MATCH_CONFIRM_MS).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn activity_only_past_the_window_reports_no_rule_matched() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(ACTIVITY_ONLY_DEF, &config);

    engine.feed(b"working");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );

    // t=500ms is window_ms: the raw judgement switches there, but the
    // transition only confirms confirm_ms later (t=800).
    assert_eq!(tick_after_ms(&mut engine, 200).await, None);
    assert_eq!(tick_after_ms(&mut engine, 299).await, None);
    assert_eq!(engine.detected(), DetectedStatus::Running);

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn quiescence_only_while_still_waiting_reports_no_rule_matched() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(QUIESCENCE_ONLY_DEF, &config);

    engine.feed(b"nothing here matches any rule");
    // t=300ms is well before after_ms (2000ms): still waiting.
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn a_rule_declaring_unknown_reports_declared_unknown() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(DECLARED_UNKNOWN_RULE_DEF, &config);

    engine.feed(b"DECLARED-UNKNOWN-MARKER");
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown))
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::DeclaredUnknown)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn a_status_event_reporting_unknown_withdraws_itself_and_the_screen_decides() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(NEVER_MATCH_DEF, &config);

    assert_eq!(engine.on_status_event(WindowStatus::Unknown), None);
    assert_eq!(
        tick_after_ms(&mut engine, NEVER_MATCH_CONFIRM_MS).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched))
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn a_definition_less_engine_stays_no_definition_without_becoming_not_yet_judged() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(None, &config, screen_of(SCREEN_COLS, SCREEN_ROWS));

    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoDefinition)
    );
    assert_eq!(engine.on_tick(), None);
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoDefinition),
        "a definition-less engine must never report NotYetJudged: there is nothing pending to \
         judge"
    );

    advance_ms(5_000).await;
    assert_eq!(engine.on_tick(), None);
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoDefinition)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

// =============================================================================
// A reason-only change is still a confirmed transition
// =============================================================================

#[tokio::test]
async fn a_reason_only_change_confirms_at_confirm_ms_not_before() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(NEVER_MATCH_DEF, &config);
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NotYetJudged)
    );

    engine.feed(b"nothing here matches any rule");
    assert_eq!(
        tick_after_ms(&mut engine, NEVER_MATCH_CONFIRM_MS - 1).await,
        None,
        "one millisecond before confirm_ms, the reason-only transition (NotYetJudged -> \
         NoRuleMatched) must not have confirmed yet, even though WindowStatus::Unknown never \
         changes"
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NotYetJudged)
    );

    assert_eq!(
        tick_after_ms(&mut engine, 1).await,
        Some(DetectedStatus::Unknown(UnknownReason::NoRuleMatched)),
        "at confirm_ms exactly, the reason-only transition must confirm and be reported as a \
         change"
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::NoRuleMatched)
    );
}

// =============================================================================
// Screen loss -- three independent detection points
// =============================================================================

/// Builds a `TIME_BOTH_DEF` engine and confirms `Running` purely from the
/// construction-time activity band (no output at all yet), mirroring
/// `a_freshly_constructed_both_sided_engine_reaches_running_without_any_output`
/// in `detector_engine.rs`.
async fn engine_confirmed_running() -> DetectionEngine {
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);
    assert_eq!(engine.on_tick(), None);
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Running)
    );
    engine
}

#[tokio::test]
async fn a_screen_loss_confirms_screen_lost_immediately_even_while_the_activity_band_is_open() {
    // (a) forced downgrade: distinguishes an implementation that would derive
    // the reason from a plain judgement call (which, with the activity band
    // still open at this instant, would say Running) rather than confirming
    // Unknown(ScreenLost) unconditionally and debounce-exempt.
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;

    // t=300ms of window_ms=500ms: the activity band is still open.
    assert_eq!(
        engine.lose_screen(),
        Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
        "a screen loss must confirm Unknown(ScreenLost) immediately, without waiting confirm_ms, \
         even while a normal judgement would still say Running"
    );
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::ScreenLost)
    );
    assert_eq!(engine.status(), WindowStatus::Unknown);
}

#[tokio::test]
async fn a_lost_screen_that_matches_no_rule_stays_screen_lost_across_repeated_evaluation() {
    // (b) judgement reason: distinguishes an implementation where only the
    // one-shot downgrade event writes the reason, and a later time-based
    // re-evaluation (ignoring screen_trusted) reverts it to NoRuleMatched.
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;
    engine.lose_screen();
    assert_eq!(
        engine.detected(),
        DetectedStatus::Unknown(UnknownReason::ScreenLost)
    );

    // Repeatedly re-evaluate across and past confirm_ms with no new output at
    // all.
    for _ in 0..5 {
        assert_eq!(tick_after_ms(&mut engine, 100).await, None);
        assert_eq!(
            engine.detected(),
            DetectedStatus::Unknown(UnknownReason::ScreenLost),
            "the reason must not revert to NoRuleMatched merely because more time passed"
        );
    }
}

#[tokio::test]
async fn a_two_sided_definition_still_confirms_the_quiescence_status_from_elapsed_time_alone_while_untrusted()
 {
    // (c) asymmetry preserved: distinguishes an implementation that maps
    // every judge_by_time branch to ScreenLost whenever screen_trusted is
    // false, including the two-sided branches that never return Unknown at
    // all -- which would break the existing "quiescence confirms from
    // elapsed time alone, no output required" guarantee.
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(TIME_BOTH_DEF, &config);

    // Screen loss before any real output ever arrives: screen_trusted drops
    // and is never restored by output in this timeline.
    assert_eq!(
        engine.lose_screen(),
        Some(DetectedStatus::Unknown(UnknownReason::ScreenLost))
    );

    // last_output_at is untouched by the loss, so the quiescence clock keeps
    // counting from construction; no output ever arrives.
    assert_eq!(
        tick_after_ms(&mut engine, 2_000).await,
        None,
        "t=2000ms=after_ms: the raw judgement switches, debounce begins"
    );
    assert_eq!(
        tick_after_ms(&mut engine, 300).await,
        Some(DetectedStatus::Idle),
        "the two-sided quiescence branch must still confirm Idle from elapsed real time alone, \
         unaffected by screen_trusted being false"
    );
    assert_eq!(engine.status(), WindowStatus::Idle);
}
