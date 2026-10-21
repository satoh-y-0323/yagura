#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Canary for the screen-based write confirmation: does a
//! delivery-receiving child fixture actually draw its composer to the
//! screen, once launched in composer-draw mode?
//!
//! Every test here spawns a fixture through a bare [`PtySession`] and calls
//! [`yagura_daemon::detector::VtScreen::read_composer`] on that session's own
//! model directly -- never through `WindowRegistry`, `IpcServer` or the
//! input-plan judgment layer, so this file is independent of every other composer-delivery layer. The
//! marker/separator/continuation-prefix triple the composer section below
//! uses comes from the one shared location both this crate and
//! `yagura-mcp`'s tests take it from
//! ([`yagura_test_support::process::COMPOSER_MARKER`] and its two
//! neighbors), so a match here and in `yagura-mcp/tests/composer_fixture.rs`
//! is not a coincidence.
//!
//! `submit_child`, `echo_child` and `ready_child` are the three fixture
//! binaries a delivery test ever opens a window on (the other two binaries
//! under `src/bin/` -- `env_probe` and `argv_probe` -- print their startup
//! arguments and exit, so no delivery ever reaches them). All three are
//! covered here.

use std::time::Duration;

use regex::Regex;

use yagura_daemon::detector::definition::ComposerSection;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_test_support::process;

/// The composer section every test in this file reads through, built from
/// the one shared string triple [`process::COMPOSER_MARKER`] /
/// [`process::COMPOSER_SEPARATOR`] / [`process::COMPOSER_CONTINUATION_PREFIX`]
/// publishes.
fn composer_section() -> ComposerSection {
    ComposerSection {
        marker: Regex::new(process::COMPOSER_MARKER)
            .expect("the shared composer marker must compile as a regex"),
        separator: process::COMPOSER_SEPARATOR.to_string(),
        continuation_prefix: process::COMPOSER_CONTINUATION_PREFIX.to_string(),
        blank_lines_in_body: false,
        left_panel: None,
    }
}

/// Spawns `argv` in a bare PTY, bypassing `WindowRegistry` entirely: this
/// file's contract is to observe a fixture's own drawing, not the daemon's
/// judgment about what it wrote.
async fn spawn(argv: &[String]) -> PtySession {
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    PtySession::spawn_with_command(&refs, &DaemonConfig::default())
        .await
        .expect("spawn_with_command should start the composer-draw-mode fixture")
}

/// Reads the composer off `session`'s model and polls
/// until `accept` is satisfied or `timeout` elapses. Returns the last
/// observed readout either way, so a timing failure still reports what was
/// actually on screen.
async fn observe_composer_until(
    session: &PtySession,
    section: &ComposerSection,
    timeout: Duration,
    mut accept: impl FnMut(&ComposerReadout) -> bool,
) -> ComposerReadout {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let section = section.clone();
        let readout = session
            .read_screen(move |screen| screen.read_composer(&section))
            .await
            .unwrap();
        if accept(&readout) {
            return readout;
        }
        if tokio::time::Instant::now() >= deadline {
            return readout;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_body(readout: &ComposerReadout, expected: &str, ctx: &str) {
    match readout {
        ComposerReadout::Body(text) => assert_eq!(text, expected, "{ctx}"),
        other => panic!("{ctx}: expected Body({expected:?}), got {other:?}"),
    }
}

fn assert_empty(readout: &ComposerReadout, ctx: &str) {
    assert_eq!(*readout, ComposerReadout::Empty, "{ctx}");
}

// =====================================================================
// The composer reads as empty before any write.
// =====================================================================

async fn assert_starts_empty(argv: &[String], ctx: &str) {
    let section = composer_section();
    let session = spawn(argv).await;

    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        !matches!(r, ComposerReadout::NotObserved)
    })
    .await;

    assert_empty(&readout, ctx);
}

// =====================================================================
// A single write reflects as the body.
// =====================================================================

async fn assert_shows_a_written_body(argv: &[String], body: &str, ctx: &str) {
    let section = composer_section();
    let session = spawn(argv).await;
    observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        !matches!(r, ComposerReadout::NotObserved)
    })
    .await;

    session
        .write_input(body.as_bytes())
        .await
        .expect("write_input should reach the running fixture");

    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        matches!(r, ComposerReadout::Body(_))
    })
    .await;

    assert_body(&readout, body, ctx);
}

// =====================================================================
// Input written one byte at a time (standing in for a human's
// keystrokes) reflects the same as a single bulk write.
// =====================================================================

async fn assert_reflects_keystroke_by_keystroke_input(argv: &[String], body: &str, ctx: &str) {
    let section = composer_section();
    let session = spawn(argv).await;
    observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        !matches!(r, ComposerReadout::NotObserved)
    })
    .await;

    for byte in body.as_bytes() {
        session
            .write_input(&[*byte])
            .await
            .expect("write_input should reach the running fixture");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        matches!(r, ComposerReadout::Body(_))
    })
    .await;

    assert_body(&readout, body, ctx);
}

// =====================================================================
// submit_child
// =====================================================================

const SUBMIT_STARTUP_MARKER: &str = "SUBMIT-COMPOSER-READY";

fn submit_argv(reflect_delay_ms: u64) -> Vec<String> {
    process::submit_child_command_with_composer(SUBMIT_STARTUP_MARKER, 0, 0, 0, reflect_delay_ms)
}

#[tokio::test]
async fn submit_child_composer_starts_empty_before_any_write() {
    assert_starts_empty(
        &submit_argv(0),
        "submit_child must draw an empty composer before any write",
    )
    .await;
}

#[tokio::test]
async fn submit_child_composer_shows_a_written_body() {
    assert_shows_a_written_body(
        &submit_argv(0),
        "SubmitDraftBody42",
        "submit_child must draw the body it was written after a single write",
    )
    .await;
}

#[tokio::test]
async fn submit_child_composer_reflects_keystroke_by_keystroke_input() {
    assert_reflects_keystroke_by_keystroke_input(
        &submit_argv(0),
        "KeyByKeySeven",
        "submit_child must draw a body typed one byte at a time",
    )
    .await;
}

// =====================================================================
// echo_child
// =====================================================================

fn echo_argv(reflect_delay_ms: u64) -> Vec<String> {
    process::echo_child_command_with_composer(reflect_delay_ms)
}

#[tokio::test]
async fn echo_child_composer_starts_empty_before_any_write() {
    assert_starts_empty(
        &echo_argv(0),
        "echo_child must draw an empty composer before any write",
    )
    .await;
}

#[tokio::test]
async fn echo_child_composer_shows_a_written_body() {
    assert_shows_a_written_body(
        &echo_argv(0),
        "EchoDraftBody42",
        "echo_child must draw the body it was written after a single write",
    )
    .await;
}

#[tokio::test]
async fn echo_child_composer_reflects_keystroke_by_keystroke_input() {
    assert_reflects_keystroke_by_keystroke_input(
        &echo_argv(0),
        "EchoKeyByKey",
        "echo_child must draw a body typed one byte at a time",
    )
    .await;
}

/// A body whose wrap point falls on a space: the fixture must not end a
/// painted line on it, since the reader trims each line's end.
#[tokio::test]
async fn echo_child_composer_keeps_a_space_at_the_wrap_point() {
    let body = format!("{}{}", "a".repeat(31), " bbbbbbbbbb");
    assert_shows_a_written_body(
        &echo_argv(0),
        &body,
        "echo_child must draw a wrapped body without losing the space at its wrap point",
    )
    .await;
}

/// `echo_child` and `ready_child` share one key handling: a paste's markers
/// are dropped, Enter submits and echoes the body, Ctrl-U empties it.
#[tokio::test]
async fn echo_child_composer_drops_paste_markers_submits_on_enter_and_clears_on_ctrl_u() {
    let section = composer_section();
    let session = spawn(&echo_argv(0)).await;
    observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        !matches!(r, ComposerReadout::NotObserved)
    })
    .await;

    session
        .write_input(b"\x1b[200~PastedBody\x1b[201~")
        .await
        .expect("write_input should reach the running fixture");
    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        matches!(r, ComposerReadout::Body(_))
    })
    .await;
    assert_body(
        &readout,
        "PastedBody",
        "a paste's markers must not reach the body",
    );

    session
        .write_input(b"\r")
        .await
        .expect("write_input should reach the running fixture");
    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        *r == ComposerReadout::Empty
    })
    .await;
    assert_empty(&readout, "Enter must submit the body");
    let (text, _) = session
        .text_tail(50, 64 * 1024)
        .await
        .expect("the text is readable");
    assert!(
        text.lines().any(|line| line == "PastedBody"),
        "Enter must echo the submitted body as a line, got {text:?}"
    );

    session
        .write_input(b"abc")
        .await
        .expect("write_input should reach the running fixture");
    observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        matches!(r, ComposerReadout::Body(_))
    })
    .await;
    session
        .write_input(&[0x15])
        .await
        .expect("write_input should reach the running fixture");
    let readout = observe_composer_until(&session, &section, Duration::from_secs(5), |r| {
        *r == ComposerReadout::Empty
    })
    .await;
    assert_empty(&readout, "Ctrl-U must empty the body");
}

// =====================================================================
// ready_child
// =====================================================================

const READY_STARTUP_MARKER: &str = "READY-COMPOSER-READY";

fn ready_argv(reflect_delay_ms: u64) -> Vec<String> {
    process::ready_child_command_with_composer(READY_STARTUP_MARKER, reflect_delay_ms)
}

#[tokio::test]
async fn ready_child_composer_starts_empty_before_any_write() {
    assert_starts_empty(
        &ready_argv(0),
        "ready_child must draw an empty composer before any write",
    )
    .await;
}

#[tokio::test]
async fn ready_child_composer_shows_a_written_body() {
    assert_shows_a_written_body(
        &ready_argv(0),
        "ReadyDraftBody42",
        "ready_child must draw the body it was written after a single write",
    )
    .await;
}

#[tokio::test]
async fn ready_child_composer_reflects_keystroke_by_keystroke_input() {
    assert_reflects_keystroke_by_keystroke_input(
        &ready_argv(0),
        "ReadyKeyByKey",
        "ready_child must draw a body typed one byte at a time",
    )
    .await;
}
