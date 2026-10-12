#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The two-layer defence against a panic inside the screen model.
//!
//! Every test that drives the canonical resize-then-feed sequence exercises the
//! `catch_unwind` guard in `VtScreen::guard_call`: the fault-injection seam makes the
//! feed after the resize panic, the panic is caught, the screen is rebuilt blank, and
//! the call reports `ScreenIntegrity::Rebuilt` instead of unwinding into the caller. The
//! `ScreenIntegrity` type itself is contracted in `tests/detector_failsafe_api.rs`.

mod common;

use common::ScreenFeed;
use std::time::Duration;

use common::{TIME_BOTH_PROMPT_DEF, advance_ms, feed_handle};
use tokio::sync::mpsc;
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::parse_definition;
use yagura_daemon::detector::engine::{ConfirmedStatus, DetectionEngine, DetectorHandle};
use yagura_daemon::window::WindowRegistry;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

/// Screen-corruption stimulus: fed twice, then a degenerate resize. Matches neither
/// `TIME_BOTH_PROMPT_DEF`'s `idle` rule nor the recovered screen width when repeated.
const RESIZE_CHUNK: &[u8] = b"redrawn screen chunk";

/// Continuation stimulus: matches `TIME_BOTH_PROMPT_DEF`'s `idle` rule and does not
/// wrap the recovered screen width.
const CONTINUATION_PROMPT: &[u8] = b"PS C:\\work>";

/// `TIME_BOTH_PROMPT_DEF`'s `[activity] window_ms`.
const WINDOW_MS: u64 = 500;
/// `TIME_BOTH_PROMPT_DEF`'s `[debounce] confirm_ms`.
const CONFIRM_MS: u64 = 300;
/// `TIME_BOTH_PROMPT_DEF`'s `[quiescence] after_ms`.
const AFTER_MS: u64 = 2000;

fn new_prompt_engine() -> DetectionEngine {
    let definition = parse_definition(TIME_BOTH_PROMPT_DEF).expect("fixture parses");
    let config = DaemonConfig::default();
    DetectionEngine::new(
        Some(definition),
        &config,
        VtScreen::new(80, 24).into_shared(),
    )
}

/// Builds a `TIME_BOTH_PROMPT_DEF` engine at `(80, 24)`, closes the construction-time
/// activity band and confirms `Running`. Each contract test adds the degenerate resize
/// itself.
async fn engine_confirmed_running() -> DetectionEngine {
    let mut engine = new_prompt_engine();

    advance_ms(WINDOW_MS).await;
    assert_eq!(
        engine.on_tick(),
        None,
        "the construction-time band must close via Hold, not confirm a status on its own"
    );

    engine.feed(RESIZE_CHUNK);
    engine.feed(RESIZE_CHUNK);
    advance_ms(CONFIRM_MS).await;
    assert_eq!(engine.on_tick(), Some(DetectedStatus::Running));

    engine
}

#[test]
fn canonical_resize_sequence_does_not_panic_and_the_screen_recovers_to_the_clamped_size() {
    let mut screen = VtScreen::new(80, 24);
    screen.feed(RESIZE_CHUNK);
    screen.feed(RESIZE_CHUNK);
    screen.resize(46, 1);
    screen.poison_after_calls(0);
    screen.feed(RESIZE_CHUNK);

    assert_eq!(screen.size(), (46, 1));
    screen.feed(b"still alive");
    assert_eq!(screen.bottom_lines(1).len(), 1);
}

#[tokio::test]
async fn confirmed_status_drops_to_unknown_immediately_when_the_screen_is_rebuilt() {
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;

    engine.screen().lock().resize(46, 1);
    engine.poison_screen_after_calls(0);
    assert_eq!(
        engine.feed(RESIZE_CHUNK),
        Some(DetectedStatus::Unknown(UnknownReason::ScreenLost)),
        "a Rebuilt screen must confirm Unknown immediately, debounce-exempt"
    );
}

#[tokio::test]
async fn a_rebuilt_screen_does_not_override_an_already_exited_status() {
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;
    assert_eq!(engine.on_exit(), Some(DetectedStatus::Exited));

    engine.screen().lock().resize(46, 1);
    engine.poison_screen_after_calls(0);
    assert_eq!(
        engine.feed(RESIZE_CHUNK),
        None,
        "Exited must outrank a Rebuilt screen; the confirmed status must not change"
    );
    assert_eq!(engine.status(), WindowStatus::Exited);
}

#[tokio::test]
async fn detection_reconverges_once_readable_output_arrives_after_a_rebuild() {
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;

    engine.screen().lock().resize(46, 1);
    engine.poison_screen_after_calls(0);
    engine.feed(RESIZE_CHUNK);

    // This stimulus must not exceed the recovered screen width, or it re-triggers
    // recovery instead of reconverging.
    assert!(CONTINUATION_PROMPT.len() <= 46);
    assert_eq!(engine.feed(CONTINUATION_PROMPT), None);
    advance_ms(CONFIRM_MS).await;
    assert_eq!(
        engine.on_tick(),
        Some(DetectedStatus::Idle),
        "a readable prompt fed after recovery must reconverge to the rule-matched status \
         rather than staying stuck at Unknown"
    );
}

#[tokio::test]
async fn confirmed_status_reaches_idle_after_after_ms_plus_confirm_ms_following_a_rebuild() {
    // Written as an explicit call sequence because `DetectionEngine` exposes no raw
    // judgement accessor. An implementation that updates `last_output_at` from the
    // Rebuilt-triggering feed is caught by the second `on_tick`, not the first.
    tokio::time::pause();
    let mut engine = engine_confirmed_running().await;

    engine.screen().lock().resize(46, 1);
    engine.poison_screen_after_calls(0);
    assert_eq!(
        engine.feed(RESIZE_CHUNK),
        Some(DetectedStatus::Unknown(UnknownReason::ScreenLost))
    );

    // The helper leaves the clock at `WINDOW_MS + CONFIRM_MS` past construction with the
    // last Intact feed at `WINDOW_MS`; advance to `after_ms` past that feed.
    advance_ms(AFTER_MS - CONFIRM_MS).await;
    assert_eq!(
        engine.on_tick(),
        None,
        "this tick only fixes the raw-judgement switch time; it must not confirm yet"
    );

    advance_ms(CONFIRM_MS).await;
    assert_eq!(
        engine.on_tick(),
        Some(DetectedStatus::Idle),
        "confirm_ms after the after_ms boundary (measured from the last Intact feed), the \
         engine must confirm Idle -- a misimplementation that resets last_output_at from the \
         Rebuilt feed would still read None here"
    );
}

#[tokio::test]
async fn detector_handle_keeps_publishing_after_the_screen_is_rebuilt() {
    // Real time rather than a paused clock, matching this crate's `DetectorHandle`
    // convention: the driver task owns its own clock.
    let definition = parse_definition(TIME_BOTH_PROMPT_DEF).expect("fixture parses");
    let config = DaemonConfig::default();
    let mut engine = DetectionEngine::new(
        Some(definition),
        &config,
        VtScreen::new(80, 24).into_shared(),
    );
    // The two feeds and the resize below complete; the feed after them panics.
    engine.poison_screen_after_calls(3);
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);

    // Close the construction-time activity band with nothing else happening.
    tokio::time::sleep(Duration::from_millis(WINDOW_MS + 150)).await;
    assert!(
        statuses.try_recv().is_err(),
        "the construction-time band must have closed via Hold with nothing published yet"
    );

    feed_handle(&screen, &handle, RESIZE_CHUNK);
    feed_handle(&screen, &handle, RESIZE_CHUNK);
    let running = recv_within(&mut statuses, Duration::from_millis(CONFIRM_MS + 500)).await;
    assert_eq!(running, DetectedStatus::Running);

    screen.lock().resize(46, 1);
    handle.notify_resized();
    feed_handle(&screen, &handle, RESIZE_CHUNK);
    let unknown = recv_within(&mut statuses, Duration::from_millis(1_000)).await;
    assert_eq!(unknown, DetectedStatus::Unknown(UnknownReason::ScreenLost));

    let idle = recv_within(
        &mut statuses,
        Duration::from_millis(AFTER_MS + CONFIRM_MS + 1_000),
    )
    .await;
    assert_eq!(
        idle,
        DetectedStatus::Idle,
        "the driver task must still be alive and publishing after recovery"
    );
}

/// Rules-only definition: with no time tables the confirmed status has no
/// deadline of its own, so only the resize notice can move it.
const RULES_ONLY_DEF: &str = r#"
schema_version = 1
name = "test-rules-only"

[debounce]
confirm_ms = 50

[[rules]]
status = "idle"
pattern = '[$#>]\s*$'
priority = 1
region = "bottom"
"#;

/// The resize notice sent through the handle, the way the IPC resize does,
/// degrades a rebuilt screen at once, with no output or tick after it.
#[tokio::test]
async fn a_resize_that_rebuilds_the_screen_degrades_through_the_handle() {
    let definition = parse_definition(RULES_ONLY_DEF).expect("fixture parses");
    let config = DaemonConfig::default();
    let engine = DetectionEngine::new(
        Some(definition),
        &config,
        VtScreen::new(80, 24).into_shared(),
    );
    let screen = engine.screen().clone();
    let (handle, mut statuses) = DetectorHandle::spawn(engine);

    feed_handle(&screen, &handle, CONTINUATION_PROMPT);
    let idle = recv_within(&mut statuses, Duration::from_millis(5_000)).await;
    assert_eq!(idle, DetectedStatus::Idle);

    screen.lock().poison_after_calls(0);
    screen.lock().resize(60, 20);
    handle.notify_resized();
    let lost = recv_within(&mut statuses, Duration::from_millis(5_000)).await;
    assert_eq!(lost, DetectedStatus::Unknown(UnknownReason::ScreenLost));
}

async fn recv_within(
    statuses: &mut mpsc::UnboundedReceiver<ConfirmedStatus>,
    timeout: Duration,
) -> DetectedStatus {
    tokio::time::timeout(timeout, statuses.recv())
        .await
        .expect("no status was published within the timeout")
        .expect("status channel closed before publishing")
        .status
}

/// A live window whose detection driver dies is failed back to `Unknown`, rather than
/// left at whatever status it held when the driver stopped reporting.
#[tokio::test]
async fn a_window_whose_detection_driver_dies_is_reported_unknown() {
    let registry = WindowRegistry::new(DaemonConfig::default());
    let window_id = common::open_echo_window(&registry, None).await.window_id;
    let entry = registry
        .get(&window_id)
        .expect("the opened window must be registered");
    let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
    assert_ne!(entry.current_detected(), stopped);

    let mut statuses = entry.watch_status();
    entry.inject_detector_driver_panic();
    let reached = tokio::time::timeout(Duration::from_secs(5), async {
        while statuses.borrow_and_update().status != stopped {
            if statuses.changed().await.is_err() {
                return false;
            }
        }
        true
    })
    .await;

    assert_eq!(
        reached,
        Ok(true),
        "the window must report Unknown(DetectorStopped) once its driver dies, got {:?}",
        entry.current_detected()
    );
    registry.terminate_all().await;
}

// Self-verification of `common::extract_fn_body` against synthetic input, independent
// of any real source file.

#[test]
fn extract_fn_body_matches_by_exact_name_not_by_prefix() {
    // An extractor keyed on `source.contains(fn_name)` rather than on the trailing `(`
    // would match `spawn_status_forwarder_v2` first and return the wrong body.
    let source = "fn spawn_status_forwarder_v2() {\n    let a = 1;\n}\n\n\
                  fn spawn_status_forwarder() {\n    let b = 2;\n}";

    let body = common::extract_fn_body(source, "spawn_status_forwarder")
        .expect("the exact-named function must be found");

    assert!(
        body.contains("let b = 2;"),
        "the exactly-named function's own body must be returned, found:\n{body}"
    );
    assert!(
        !body.contains("let a = 1;"),
        "a same-prefixed function's body must not be returned, found:\n{body}"
    );
}

#[test]
fn extract_fn_body_does_not_count_braces_inside_a_string_literal() {
    // An unbalanced `{` inside a string literal must not shift the detected closing
    // brace: miscounting it bleeds the scan into the following, unrelated function.
    let source = "fn caller() {\n    let s = \"unbalanced { brace\";\n    let x = 1;\n}\n\n\
                  fn next_fn() {\n    let y = 2;\n}";

    let body = common::extract_fn_body(source, "caller").expect("caller's body must be found");

    assert!(
        body.contains("let x = 1;"),
        "caller's own body must be captured in full, found:\n{body}"
    );
    assert!(
        !body.contains("next_fn"),
        "the extracted body must not bleed into the following, unrelated function, found:\n{body}"
    );
}

#[test]
fn extract_fn_body_stops_at_its_own_closing_brace_and_excludes_a_following_function() {
    let source = "fn caller() {\n    if true {\n        let x = 1;\n    }\n}\n\n\
                  fn next_fn() {\n    let y = 2;\n}";

    let body = common::extract_fn_body(source, "caller").expect("caller's body must be found");

    assert!(
        body.starts_with("fn caller("),
        "the extracted text must start at the function's own signature, found:\n{body}"
    );
    assert!(
        body.trim_end().ends_with('}'),
        "the extracted text must end at the function's own closing brace, found:\n{body}"
    );
    assert!(
        !body.contains("next_fn"),
        "a following, unrelated function must not be included, found:\n{body}"
    );
}

#[test]
fn extract_fn_body_does_not_count_a_brace_char_literal_as_structural() {
    // An extractor that does not recognize `'{'` as a char literal reads its inner `{`
    // as a structural open, which never balances back and bleeds into `next_fn`.
    let source = "fn caller() {\n    let c = '{';\n    let x = 1;\n}\n\n\
                  fn next_fn() {\n    let y = 2;\n}";

    let body = common::extract_fn_body(source, "caller").expect("caller's body must be found");

    assert!(
        body.contains("let x = 1;"),
        "caller's own body must be captured in full, found:\n{body}"
    );
    assert!(
        !body.contains("next_fn"),
        "the extracted body must not bleed into the following, unrelated function, found:\n{body}"
    );
}

#[test]
fn extract_fn_body_does_not_terminate_early_on_a_closing_brace_char_literal() {
    // An extractor that does not recognize `'}'` as a char literal stops at its inner
    // `}`, truncating the body before `caller`'s own closing brace.
    let source = "fn caller() {\n    let c = '}';\n    let x = 1;\n}\n\n\
                  fn next_fn() {\n    let y = 2;\n}";

    let body = common::extract_fn_body(source, "caller").expect("caller's body must be found");

    assert!(
        body.contains("let x = 1;"),
        "caller's own body must be captured in full, not truncated at the char literal's inner \
         `}}`, found:\n{body}"
    );
    assert!(
        body.trim_end().ends_with('}'),
        "the extracted text must end at the function's own closing brace, found:\n{body}"
    );
    assert!(
        !body.contains("next_fn"),
        "the extracted body must not bleed into the following, unrelated function, found:\n{body}"
    );
}

#[test]
fn extract_fn_body_does_not_misdetect_a_lifetime_as_a_char_literal() {
    // A lifetime such as `'a` must still be told apart from a char literal (no closing
    // `'` one character later), so it must not be skipped as if it opened one.
    let source = "fn caller() {\n    let s: &'a str = value;\n    let x = 1;\n}\n\n\
                  fn next_fn() {\n    let y = 2;\n}";

    let body = common::extract_fn_body(source, "caller").expect("caller's body must be found");

    assert!(
        body.contains("let x = 1;"),
        "caller's own body must be captured in full, found:\n{body}"
    );
    assert!(
        !body.contains("next_fn"),
        "the extracted body must not bleed into the following, unrelated function, found:\n{body}"
    );
}
