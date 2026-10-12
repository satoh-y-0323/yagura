#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges real detector fixtures repainted onto a screen much taller than
//! their own recorded geometry -- the way a maximized window looks next to
//! the small one a fixture was captured at. The extra rows below the
//! fixture's own content are filled the way a full-screen TUI fills unused
//! space: painted, not left untouched.

mod common;

use common::detection_rules::judge;
use yagura_daemon::detector::VtScreen;
use yagura_protocol::WindowStatus;

const FIXTURE_CODEX_CLI_IDLE_STARTUP: &str =
    include_str!("fixtures/codex-cli/100x30-idle-startup.txt");
const FIXTURE_CLAUDE_CODE_IDLE_STARTUP: &str =
    include_str!("fixtures/claude-code/100x30-unicode-idle-startup.txt");

/// A plain shell has no fixture of its own, so its prompt is synthesized
/// directly: one line matching `generic-shell.toml`'s idle rule for a bare
/// prompt terminator.
const GENERIC_SHELL_PROMPT: &str = "prompt$";

const TALL_COLS: u16 = 100;
const TALL_ROWS: u16 = 60;

/// One column short of `TALL_COLS`, so a full-width filler line never
/// reaches the last column and triggers the terminal's own line wrap.
const FILL_WIDTH: usize = TALL_COLS as usize - 1;

/// `content`'s lines, followed by enough copies of `fill` to reach
/// `TALL_ROWS` lines in total.
fn padded_lines<'a>(content: &[&'a str], fill: &'a str) -> Vec<&'a str> {
    let mut lines: Vec<&str> = content.to_vec();
    let filler_count = usize::from(TALL_ROWS) - lines.len();
    lines.extend(std::iter::repeat_n(fill, filler_count));
    lines
}

/// Confirms the filler in `lines` actually reaches the screen's last row
/// before a case's real assertion runs, so a wrong filler count fails here
/// instead of silently passing the wrong screen shape below.
fn assert_filler_reaches_the_bottom(lines: &[&str]) {
    let mut screen = VtScreen::new(TALL_COLS, TALL_ROWS);
    screen.feed(&common::repaint_lines(lines));

    let full = screen.bottom_lines(usize::from(TALL_ROWS));
    assert_eq!(full.len(), usize::from(TALL_ROWS));
    assert_ne!(
        full.last().map(String::as_str),
        Some(""),
        "the filler must reach the screen's last row: {full:?}"
    );
}

#[tokio::test]
async fn codex_cli_idle_startup_still_classifies_on_a_tall_screen_padded_with_blank_lines() {
    tokio::time::pause();
    let source = common::repo_definition_text("codex-cli.toml");
    let content: Vec<&str> = FIXTURE_CODEX_CLI_IDLE_STARTUP.lines().collect();
    let fill = " ".repeat(FILL_WIDTH);
    let lines = padded_lines(&content, &fill);
    assert_filler_reaches_the_bottom(&lines);

    let status = judge(&source, TALL_COLS, TALL_ROWS, &lines).await;

    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test]
async fn codex_cli_idle_startup_is_unknown_on_a_tall_screen_padded_with_visible_lines() {
    tokio::time::pause();
    let source = common::repo_definition_text("codex-cli.toml");
    let content: Vec<&str> = FIXTURE_CODEX_CLI_IDLE_STARTUP.lines().collect();
    let fill = "x".repeat(FILL_WIDTH);
    let lines = padded_lines(&content, &fill);
    assert_filler_reaches_the_bottom(&lines);

    let status = judge(&source, TALL_COLS, TALL_ROWS, &lines).await;

    assert_eq!(status, WindowStatus::Unknown);
}

#[tokio::test]
async fn claude_code_idle_startup_still_classifies_on_a_tall_screen_padded_with_blank_lines() {
    tokio::time::pause();
    let source = common::repo_definition_text("claude-code.toml");
    let content: Vec<&str> = FIXTURE_CLAUDE_CODE_IDLE_STARTUP.lines().collect();
    let fill = " ".repeat(FILL_WIDTH);
    let lines = padded_lines(&content, &fill);
    assert_filler_reaches_the_bottom(&lines);

    let status = judge(&source, TALL_COLS, TALL_ROWS, &lines).await;

    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test]
async fn claude_code_idle_startup_is_unknown_on_a_tall_screen_padded_with_visible_lines() {
    tokio::time::pause();
    let source = common::repo_definition_text("claude-code.toml");
    let content: Vec<&str> = FIXTURE_CLAUDE_CODE_IDLE_STARTUP.lines().collect();
    let fill = "x".repeat(FILL_WIDTH);
    let lines = padded_lines(&content, &fill);
    assert_filler_reaches_the_bottom(&lines);

    let status = judge(&source, TALL_COLS, TALL_ROWS, &lines).await;

    assert_eq!(status, WindowStatus::Unknown);
}

#[tokio::test]
async fn generic_shell_prompt_still_classifies_as_idle_on_a_tall_screen_padded_with_blank_lines() {
    tokio::time::pause();
    let source = common::repo_definition_text("generic-shell.toml");
    let content = [GENERIC_SHELL_PROMPT];
    let fill = " ".repeat(FILL_WIDTH);
    let lines = padded_lines(&content, &fill);
    assert_filler_reaches_the_bottom(&lines);

    let status = judge(&source, TALL_COLS, TALL_ROWS, &lines).await;

    assert_eq!(status, WindowStatus::Idle);
}
