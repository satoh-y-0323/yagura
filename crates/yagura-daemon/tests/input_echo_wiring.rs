#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Window-layer wiring for PTY input echo attribution: `classify_input_echo` and
//! `WindowEntry::write_pty_input`.
//!
//! ConPTY emits terminal mode-set sequences before the child process runs at all, and
//! in a two-sided (`[activity]` + `[quiescence]`) definition that output alone can open
//! the activity band right after a window opens. Every real-PTY assertion here therefore
//! starts from an explicitly confirmed status reached through an unattributed write
//! (`PtySession::write_input`, which never calls `notify_input`).
//!
//! The "prompt-shaped line must sit in the judged region" contract applies only to the
//! fixtures that carry a rule list. The empty-rule fixtures always judge by time, and
//! applying it there would make one test unreachable and another a tautology.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use yagura_test_support::paths::{unique_detection_dir, write_definition};

use yagura_daemon::detector::engine::InputEcho;
use yagura_daemon::waiter::StatusAt;
use yagura_daemon::window::{InputOrigin, classify_input_echo};
use yagura_daemon::{DaemonConfig, WindowEntry, WindowRegistry};
use yagura_protocol::WindowStatus;

#[test]
fn interactive_write_without_a_line_terminator_is_echo_only() {
    assert_eq!(
        classify_input_echo(b"x", InputOrigin::Interactive),
        InputEcho::EchoOnly,
        "ordinary line-editing keystrokes (no CR/LF) must be classified as echo-only"
    );
}

#[test]
fn interactive_write_containing_carriage_return_is_may_start_work() {
    assert_eq!(
        classify_input_echo(b"ls\r", InputOrigin::Interactive),
        InputEcho::MayStartWork,
        "a submitted line (containing \\r) must be classified as may-start-work"
    );
}

#[test]
fn interactive_write_containing_newline_is_may_start_work() {
    assert_eq!(
        classify_input_echo(b"ls\n", InputOrigin::Interactive),
        InputEcho::MayStartWork,
        "a submitted line (containing \\n) must be classified as may-start-work"
    );
}

#[test]
fn interactive_write_with_an_empty_payload_is_echo_only() {
    assert_eq!(
        classify_input_echo(b"", InputOrigin::Interactive),
        InputEcho::EchoOnly,
        "an empty interactive payload contains no line terminator, so it must classify the \
         same as ordinary line editing: echo-only"
    );
}

#[test]
fn message_write_without_a_line_terminator_is_echo_only() {
    assert_eq!(
        classify_input_echo(b"hello", InputOrigin::Message),
        InputEcho::EchoOnly,
        "who wrote a payload does not change what it does: one carrying no line terminator \
         submits nothing, so the output that follows it is its own echo"
    );
}

#[test]
fn message_write_containing_carriage_return_is_may_start_work() {
    assert_eq!(
        classify_input_echo(b"hello\r", InputOrigin::Message),
        InputEcho::MayStartWork,
        "a payload carrying a line terminator submits the line, so the output that follows \
         it is the receiving program's own work"
    );
}

#[test]
fn control_origin_is_always_may_start_work() {
    assert_eq!(
        classify_input_echo(&[0x03], InputOrigin::Control),
        InputEcho::MayStartWork,
        "an interrupt byte sequence delivered by interrupt_window is never line editing, \
         regardless of its payload"
    );
}

// Fixture time values are deliberately short, so every test in this file finishes in
// well under a second of real waiting.

/// `[debounce].confirm_ms` for every fixture in this file.
const CONFIRM_MS: u64 = 30;

/// `[activity].window_ms` for every two-sided fixture in this file. Short enough that
/// the falsification case reaches confirmed `Running` in well under a second.
const ACTIVITY_WINDOW_MS: u64 = 150;

/// `[quiescence].after_ms` for every fixture in this file. Large relative to
/// `ACTIVITY_WINDOW_MS` + `CONFIRM_MS`, so "activity band still open" and "quiescence
/// has taken over" stay unambiguous under real scheduling jitter.
const QUIESCENCE_AFTER_MS: u64 = 400;

/// `DaemonConfig::input_echo_window_ms` for the suppression side: far larger than any
/// write spacing used here, so every echo is suppressed regardless of scheduling jitter.
const ECHO_SUPPRESS_MS: u64 = 5_000;

/// `DaemonConfig::input_echo_window_ms` for the falsification side: `0` makes
/// `AttributionGate::echo_window` (`engine.rs`) `Duration::ZERO`, so the shared
/// time-proximity predicate can never hold. A small-but-nonzero window would not do: a
/// ConPTY echo round trip completes in well under a millisecond and would still be
/// suppressed.
///
/// Not to be confused with `resolve_input_echo_window_ms` (`config.rs`), which treats a
/// `0` in the environment variable as invalid and falls back to its default. This
/// constant feeds `DaemonConfig::input_echo_window_ms` directly and is test-only: no
/// production path can put `0` there.
const ECHO_DISARM_MS: u64 = 0;

/// `DaemonConfig::resize_redraw_window_ms` for the tests that never arm a resize: it
/// cannot affect their outcome and exists only because `detection_config` requires one.
const RESIZE_IRRELEVANT_MS: u64 = 200;

/// `DaemonConfig::resize_redraw_window_ms` for the suppression side of the resize
/// measurement: far larger than any write spacing used here, so the redraw is
/// suppressed regardless of scheduling jitter.
const RESIZE_SUPPRESS_MS: u64 = 5_000;

/// `DaemonConfig::resize_redraw_window_ms` for the falsification side: `0`, for the same
/// reason as `ECHO_DISARM_MS` -- a small-but-nonzero window loses to a sub-millisecond
/// ConPTY round trip, while a comparison against `Duration::ZERO` can never hold.
const RESIZE_DISARM_MS: u64 = 0;

/// A trailing shell prompt, identical in shape to `detection/generic-shell.toml`'s
/// prompt rule, so the reported defect scenario is reproduced rather than approximated.
const PROMPT_TEXT: &[u8] = b"$ ";

fn time_both_with_prompt_rule_def() -> String {
    format!(
        r#"
schema_version = 1
name = "test-input-echo-both-prompt"

[debounce]
confirm_ms = {CONFIRM_MS}

[[rules]]
status = "idle"
pattern = '[$#>]\s*$'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
after_ms = {QUIESCENCE_AFTER_MS}

[activity]
status = "running"
window_ms = {ACTIVITY_WINDOW_MS}
"#
    )
}

/// Two-sided but with an empty rule list: `match_rules` (`engine.rs`) never matches, so
/// every judgement goes through `judge_by_time`. `rules = []` is written out because the
/// raw definition has no `#[serde(default)]`: an absent key is a parse error.
fn time_both_no_rule_def() -> String {
    format!(
        r#"
schema_version = 1
name = "test-input-echo-both-no-rule"
rules = []

[debounce]
confirm_ms = {CONFIRM_MS}

[quiescence]
status = "idle"
after_ms = {QUIESCENCE_AFTER_MS}

[activity]
status = "running"
window_ms = {ACTIVITY_WINDOW_MS}
"#
    )
}

/// A temporary detection-definition directory, injected straight into
/// `DaemonConfig.detection_dir` so parallel tests never share state.
fn detection_dir(tag: &str) -> PathBuf {
    unique_detection_dir("yagura-test-input-echo", tag, "create temp detection dir")
}

fn detection_config(
    dir: &Path,
    detection_name: &str,
    input_echo_window_ms: u64,
    resize_redraw_window_ms: u64,
) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: detection_name.to_string(),
        input_echo_window_ms,
        resize_redraw_window_ms,
        ..DaemonConfig::default()
    }
}

/// `WindowEntry` projection of [`common::open_echo_window`]: opens a window running
/// `echo_child` under `registry`'s detection definition and resolves it to its entry.
async fn open_echo_entry(registry: &Arc<WindowRegistry>) -> Arc<WindowEntry> {
    let window_id = common::open_echo_window(registry, None).await.window_id;
    registry
        .get(&window_id)
        .expect("the window this call just opened must exist in the registry")
}

/// Polls `entry.current_status()` until it equals `expected`, or panics once
/// `timeout` elapses.
async fn wait_for_status(entry: &WindowEntry, expected: WindowStatus, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let current = entry.current_status();
        if current == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for window {} to reach {expected:?} (last seen {current:?})",
                entry.window_id()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Collects every confirmed-status change broadcast on `rx` over `duration`. An empty
/// result is a legitimate outcome, so each caller pairs it with a direct
/// `current_status()` assertion and a vacuous pass cannot look like success.
///
/// `rx` now yields [`StatusAt`] (the pair together with its generation), the
/// fan-out payload every subscriber shares (`WindowEntry::subscribe_status`);
/// this helper still projects down to [`WindowStatus`] because the wiring
/// this file measures (echo attribution) has no interest in the generation.
async fn drain_statuses(
    rx: &mut broadcast::Receiver<StatusAt>,
    duration: Duration,
) -> Vec<WindowStatus> {
    let deadline = tokio::time::Instant::now() + duration;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return out;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(at)) => out.push(at.status.status()),
            Ok(Err(_)) => return out,
            Err(_) => return out,
        }
    }
}

#[tokio::test]
async fn suppressed_interactive_echo_keeps_confirmed_idle() {
    let dir = detection_dir("w1");
    write_definition(&dir, "w1-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "w1-def", ECHO_SUPPRESS_MS, RESIZE_IRRELEVANT_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    // Start point: an unattributed write makes the bottom line prompt-shaped, so
    // `match_rules` confirms `Idle` ahead of any time judgement.
    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    let mut statuses = entry.subscribe_status();

    // The defect scenario: a further interactive keystroke with no line terminator echoes
    // back without ending in a prompt character, exercising the time-judgement branch.
    entry
        .write_pty_input(b"abc", InputOrigin::Interactive)
        .await
        .expect("write_pty_input");

    let observe_for = Duration::from_millis(ACTIVITY_WINDOW_MS + CONFIRM_MS + 300);
    let observed = drain_statuses(&mut statuses, observe_for).await;
    assert!(
        observed.iter().all(|status| *status == WindowStatus::Idle),
        "an echo suppressed by input_echo_window_ms=5000 must never move confirmed off Idle \
         (neither to Running nor Unknown), observed: {observed:?}"
    );
    assert_eq!(
        entry.current_status(),
        WindowStatus::Idle,
        "confirmed status must still read Idle once the observation window ends"
    );
}

// Falsification side of the suppression contract above: without it, a suppression-free
// stub implementation could pass that contract vacuously.

#[tokio::test]
async fn disarmed_interactive_echo_reaches_running() {
    let dir = detection_dir("w2");
    write_definition(&dir, "w2-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "w2-def", ECHO_DISARM_MS, RESIZE_IRRELEVANT_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    entry
        .write_pty_input(b"abc", InputOrigin::Interactive)
        .await
        .expect("write_pty_input");

    wait_for_status(&entry, WindowStatus::Running, Duration::from_secs(3)).await;
}

// The same measurement for a write the daemon itself makes on behalf of an agent. Both
// windows are configured identically (`ECHO_SUPPRESS_MS`), so the payload is the only
// variable: the body half of a submit carries no terminator and must stay attributable
// as an echo, while the submit-key half must not.

#[tokio::test]
async fn suppressed_message_body_without_a_terminator_keeps_confirmed_idle() {
    let dir = detection_dir("w6");
    write_definition(&dir, "w6-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "w6-def", ECHO_SUPPRESS_MS, RESIZE_IRRELEVANT_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    // Start point: an unattributed write makes the bottom line prompt-shaped, so
    // `match_rules` confirms `Idle` ahead of any time judgement.
    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    let mut statuses = entry.subscribe_status();

    entry
        .write_pty_input(b"abc", InputOrigin::Message)
        .await
        .expect("write_pty_input");

    let observe_for = Duration::from_millis(ACTIVITY_WINDOW_MS + CONFIRM_MS + 300);
    let observed = drain_statuses(&mut statuses, observe_for).await;
    assert!(
        observed.iter().all(|status| *status == WindowStatus::Idle),
        "the body of a submit is written without a line terminator and submits nothing, so \
         its echo must never move confirmed off Idle; a window that reads Running here \
         would let the body's own echo pass for the receiver reacting to it, observed: \
         {observed:?}"
    );
    assert_eq!(
        entry.current_status(),
        WindowStatus::Idle,
        "confirmed status must still read Idle once the observation window ends"
    );
}

// Falsification side of the contract above: without it, a stub that classifies every
// message write as an echo could pass it vacuously.

#[tokio::test]
async fn message_write_containing_carriage_return_reaches_running() {
    let dir = detection_dir("w7");
    write_definition(&dir, "w7-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "w7-def", ECHO_SUPPRESS_MS, RESIZE_IRRELEVANT_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    entry
        .write_pty_input(b"abc\r", InputOrigin::Message)
        .await
        .expect("write_pty_input");

    wait_for_status(&entry, WindowStatus::Running, Duration::from_secs(3)).await;
}

#[tokio::test]
async fn running_survives_suppressed_interactive_keystrokes_past_after_ms() {
    let dir = detection_dir("w5");
    write_definition(&dir, "w5-def", &time_both_no_rule_def());
    let config = detection_config(&dir, "w5-def", ECHO_SUPPRESS_MS, RESIZE_IRRELEVANT_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    // Start point: no rule ever matches, so an unattributed, non-prompt-shaped write
    // opens the activity band and confirmed reaches `Running`.
    entry
        .session()
        .write_input(b"hello")
        .await
        .expect("write hello");
    wait_for_status(&entry, WindowStatus::Running, Duration::from_secs(3)).await;

    let mut statuses = entry.subscribe_status();

    // Keep the echo-attribution gate armed with a train of interactive keystrokes spaced
    // well inside input_echo_window_ms, running longer in total than [quiescence].after_ms.
    let keystroke_count = 7u32;
    let keystroke_spacing = Duration::from_millis(100);
    for _ in 0..keystroke_count {
        entry
            .write_pty_input(b"x", InputOrigin::Interactive)
            .await
            .expect("write_pty_input");
        tokio::time::sleep(keystroke_spacing).await;
    }

    let observed = drain_statuses(&mut statuses, Duration::from_millis(50)).await;
    assert!(
        observed
            .iter()
            .all(|status| *status == WindowStatus::Running),
        "a window confirmed Running must never fall to Idle (or Unknown) while suppressed \
         echoes keep arriving, even once the keystroke train has run longer than \
         [quiescence].after_ms; observed: {observed:?}"
    );
    assert_eq!(
        entry.current_status(),
        WindowStatus::Running,
        "confirmed status must still read Running once the keystroke train ends"
    );
}

// Suppression side of resize-redraw attribution: the resize is armed through
// `entry.notify_resize_started()` and the redraw-like output is written straight through
// the PTY, so `notify_input` never runs and echo attribution stays out of the way.

#[tokio::test]
async fn resize_w1_suppressed_redraw_output_keeps_confirmed_idle() {
    let dir = detection_dir("resize-w1");
    write_definition(&dir, "resize-w1-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "resize-w1-def", ECHO_SUPPRESS_MS, RESIZE_SUPPRESS_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    // Start point: an unattributed write makes the bottom line prompt-shaped, so
    // `match_rules` confirms `Idle` ahead of any time judgement.
    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    // Arming a resize never closes an already-open activity band, so arming while the band
    // opened by `PROMPT_TEXT` is still inside `ACTIVITY_WINDOW_MS` would legitimately
    // report `Running`. Sleep past that band so suppression is measured in isolation.
    tokio::time::sleep(Duration::from_millis(ACTIVITY_WINDOW_MS + 300)).await;

    let mut statuses = entry.subscribe_status();

    assert!(
        entry.notify_resize_started(),
        "a living window must accept notify_resize_started"
    );
    entry
        .session()
        .write_input(b"abc")
        .await
        .expect("write redraw-like output");

    let observe_for = Duration::from_millis(ACTIVITY_WINDOW_MS + CONFIRM_MS + 300);
    let observed = drain_statuses(&mut statuses, observe_for).await;
    assert!(
        observed.iter().all(|status| *status == WindowStatus::Idle),
        "output attributed to a resize's own redraw, suppressed by \
         resize_redraw_window_ms=5000, must never move confirmed off Idle (neither to \
         Running nor Unknown), observed: {observed:?}"
    );
    assert_eq!(
        entry.current_status(),
        WindowStatus::Idle,
        "confirmed status must still read Idle once the observation window ends"
    );
}

// Falsification side of the resize suppression contract: without it, a suppression-free
// stub implementation could pass that contract vacuously.

#[tokio::test]
async fn resize_w2_disarmed_redraw_output_reaches_running() {
    let dir = detection_dir("resize-w2");
    write_definition(&dir, "resize-w2-def", &time_both_with_prompt_rule_def());
    let config = detection_config(&dir, "resize-w2-def", ECHO_SUPPRESS_MS, RESIZE_DISARM_MS);
    let registry = Arc::new(WindowRegistry::new(config));

    let entry = open_echo_entry(&registry).await;

    entry
        .session()
        .write_input(PROMPT_TEXT)
        .await
        .expect("write prompt line");
    wait_for_status(&entry, WindowStatus::Idle, Duration::from_secs(3)).await;

    assert!(
        entry.notify_resize_started(),
        "a living window must accept notify_resize_started"
    );
    entry
        .session()
        .write_input(b"abc")
        .await
        .expect("write redraw-like output");

    wait_for_status(&entry, WindowStatus::Running, Duration::from_secs(3)).await;
}
