#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The bundled Codex definition must report `Running` for a turn in flight
//! whether or not the `Working (... esc to interrupt)` line is on screen, and
//! must not report it for a screen that is waiting for the user.
//!
//! While the user has a body in the composer during a turn, Codex replaces the
//! `Working` line with a `tab to queue message` hint. The streams live under
//! `tests/fixtures/codex-cli/running/`; see the README there for their origin.

mod common;

use common::ScreenFeed;
use std::path::Path;

use yagura_daemon::DaemonConfig;
use yagura_protocol::WindowStatus;

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-cli/running")
        .join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

/// Feeds the stream to an engine built from the bundled definition and returns
/// the status confirmed after the debounce has elapsed.
async fn judge(name: &str) -> WindowStatus {
    let (cols, rows) = name
        .split_once('-')
        .and_then(|(geometry, _)| geometry.split_once('x'))
        .map(|(c, r)| (c.parse::<u16>().unwrap(), r.parse::<u16>().unwrap()))
        .unwrap_or_else(|| panic!("fixture name {name} does not start with COLSxROWS"));
    let source = common::repo_definition_text("codex-cli.toml");
    let config = DaemonConfig::default();
    let mut engine = common::new_engine_sized(&source, &config, cols, rows);
    engine.feed(&fixture(name));
    common::tick_after_ms(&mut engine, 350).await;
    common::tick_after_ms(&mut engine, 350).await;
    engine.status()
}

#[tokio::test(start_paused = true)]
async fn a_turn_showing_the_working_line_is_running() {
    for name in [
        "100x30-working-empty-composer",
        "80x24-working-empty-composer",
        "100x30-working-with-body",
    ] {
        assert_eq!(judge(name).await, WindowStatus::Running, "{name}");
    }
}

/// With a body in the composer and the `Working` line replaced by the
/// `tab to queue message` hint, the turn is still in flight.
#[tokio::test(start_paused = true)]
async fn a_turn_with_a_body_in_the_composer_and_no_working_line_is_running() {
    for name in [
        "100x30-body-no-working-line",
        "80x24-body-no-working-line",
        "50x20-body-no-working-line",
    ] {
        assert_eq!(judge(name).await, WindowStatus::Running, "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_screen_waiting_for_input_is_not_running() {
    for name in [
        "100x30-idle-startup",
        "100x30-idle-after-answer",
        "100x30-idle-typed",
        "80x24-idle-typed",
        "100x30-idle-background-terminal",
    ] {
        assert_eq!(judge(name).await, WindowStatus::WaitingInput, "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_approval_dialog_is_not_running() {
    for name in [
        "100x30-approval-dialog",
        "80x24-approval-dialog",
        "100x30-approval-dialog-with-body",
    ] {
        assert_eq!(judge(name).await, WindowStatus::WaitingApproval, "{name}");
    }
}

/// The words `tab to queue message` typed into an idle composer are the
/// user's text, not the CLI's hint: the cursor is visible there and the
/// composer body is stripped before the rules run.
#[tokio::test(start_paused = true)]
async fn the_hint_words_typed_into_an_idle_composer_are_not_running() {
    let status = judge("100x30-idle-background-terminal-typed-hint").await;
    assert_eq!(status, WindowStatus::WaitingInput);
}
