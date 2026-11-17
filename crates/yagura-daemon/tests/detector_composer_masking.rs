#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The status a detection definition reports must come from what the CLI
//! itself draws, not from the text sitting in the composer.
//!
//! Every case feeds a real (or, where noted, synthesized) PTY byte stream to a
//! `DetectionEngine` built from either a bundled definition or a neutral
//! definition written in this file, and reads the confirmed status. The real
//! streams live under `tests/fixtures/<cli>/composer/`; see the README next to
//! them for their origin.

use crate::common;

use common::ScreenFeed;
use std::path::Path;

use yagura_daemon::DaemonConfig;
use yagura_protocol::WindowStatus;

fn fixture(cli: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(cli)
        .join("composer")
        .join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

/// Feeds `bytes` to an engine for `definition_file` on a `cols` x `rows`
/// screen and returns the status confirmed after the debounce has elapsed.
async fn judge_stream(definition_file: &str, cols: u16, rows: u16, bytes: &[u8]) -> WindowStatus {
    let source = common::repo_definition_text(definition_file);
    judge_stream_source(&source, cols, rows, bytes).await
}

/// [`judge_stream`] for a definition given as its TOML text.
async fn judge_stream_source(source: &str, cols: u16, rows: u16, bytes: &[u8]) -> WindowStatus {
    let config = DaemonConfig::default();
    let mut engine = common::new_engine_sized(source, &config, cols, rows);
    engine.feed(bytes);
    common::tick_after_ms(&mut engine, 350).await;
    common::tick_after_ms(&mut engine, 350).await;
    engine.status()
}

async fn judge_fixture(
    definition_file: &str,
    cli: &str,
    cols: u16,
    rows: u16,
    name: &str,
) -> WindowStatus {
    judge_stream(definition_file, cols, rows, &fixture(cli, name)).await
}

// =====================================================================
// Text typed into the composer that happens to contain a rule's wording
// must not decide the status.
// =====================================================================

#[tokio::test(start_paused = true)]
async fn claude_code_body_containing_a_dialog_phrase_is_still_waiting_input() {
    let status = judge_fixture(
        "claude-code.toml",
        "claude-code",
        100,
        30,
        "100x30-waiting-body-do-you-want-to",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn codex_cli_body_containing_a_dialog_phrase_is_still_waiting_input() {
    let status = judge_fixture(
        "codex-cli.toml",
        "codex-cli",
        100,
        30,
        "100x30-waiting-body-press-enter",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn antigravity_cli_body_containing_a_task_count_is_still_waiting_input() {
    let status = judge_fixture(
        "antigravity-cli.toml",
        "antigravity-cli",
        100,
        30,
        "100x30-waiting-body-task-count",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

// =====================================================================
// A composer that has grown taller (but still fits the screen) reads like an
// empty one.
// =====================================================================

#[tokio::test(start_paused = true)]
async fn claude_code_composer_grown_on_a_small_screen_is_waiting_input() {
    let status = judge_fixture(
        "claude-code.toml",
        "claude-code",
        80,
        24,
        "80x24-grown-body-one-envelope",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn claude_code_composer_grown_by_two_envelopes_is_waiting_input() {
    let status = judge_fixture(
        "claude-code.toml",
        "claude-code",
        100,
        30,
        "100x30-grown-body-two-envelopes",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn codex_cli_composer_grown_by_two_envelopes_is_waiting_input() {
    let status = judge_fixture(
        "codex-cli.toml",
        "codex-cli",
        100,
        30,
        "100x30-grown-body-two-envelopes",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn antigravity_cli_composer_grown_by_two_envelopes_is_waiting_input() {
    let status = judge_fixture(
        "antigravity-cli.toml",
        "antigravity-cli",
        100,
        30,
        "100x30-grown-body-two-envelopes",
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

// =====================================================================
// A composer folded at its height limit, whose body has blank lines, is
// still found, so the CLI's own lines above and below it decide the status.
// =====================================================================

/// A neutral definition whose composer body may hold blank lines. Its window
/// is shorter than the folded body below, so a rule sees the CLI's own lines
/// only once the body has been taken out.
const BLANK_LINES_DEFINITION: &str = r#"
schema_version = 1
name = "example-blank-lines-cli"
bottom_line_count = 6

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = '^\s{0,4}Use arrows to choose\b'
priority = 30
region = "bottom"

[[rules]]
status = "running"
pattern = '^\* Working\b'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '^>( |$)'
priority = 10
region = "bottom"

[composer]
marker = '^>'
separator = " "
continuation_prefix = "  "
blank_lines_in_body = true
"#;

/// The 1-based screen row of the line above the composer in
/// [`folded_composer_screen`], and the row and column the cursor rests at.
const STATUS_ROW: u16 = 3;
const CURSOR_ROW: u16 = 19;
const CURSOR_COL: u16 = 3;

/// A draft folded under `> ↑ 25 more lines`, every fifth line blank (drawn as
/// nothing), with the cursor shown past the prefix of a blank body line.
/// `status_line` is the CLI's own line just above the composer.
fn folded_composer_screen(status_line: &str) -> String {
    let mut screen = String::from("\x1b[2J\x1b[H");
    screen.push_str("Answer to the earlier question.\r\n\r\n");
    screen.push_str(status_line);
    screen.push_str("\r\n> \u{2191} 25 more lines\r\n");
    for line in 26..40 {
        if line % 5 == 0 {
            screen.push_str("\r\n");
        } else {
            screen.push_str(&format!("  line {line}\r\n"));
        }
    }
    screen.push_str("  ");
    screen
}

// =====================================================================
// A composer folded at its height limit, whose body has blank lines, is
// still found, so the CLI's own lines above and below it decide the status.
// =====================================================================

#[tokio::test(start_paused = true)]
async fn folded_body_with_blank_lines_is_waiting_input() {
    let screen = folded_composer_screen("Ready.");
    let status = judge_stream_source(BLANK_LINES_DEFINITION, 100, 30, screen.as_bytes()).await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

#[tokio::test(start_paused = true)]
async fn running_under_a_folded_body_with_blank_lines_is_running() {
    let screen = folded_composer_screen("* Working (esc to interrupt)");
    let status = judge_stream_source(BLANK_LINES_DEFINITION, 100, 30, screen.as_bytes()).await;
    assert_eq!(status, WindowStatus::Running);
}

/// The same draft after the turn ends: the CLI repaints its line above the
/// composer and puts the cursor back where it was.
#[tokio::test(start_paused = true)]
async fn finished_turn_under_a_folded_body_with_blank_lines_is_waiting_input() {
    let mut stream = folded_composer_screen("* Working (esc to interrupt)");
    stream.push_str(&format!(
        "\x1b[{STATUS_ROW};1H\x1b[2K* Worked for 12s\x1b[{CURSOR_ROW};{CURSOR_COL}H"
    ));
    let status = judge_stream_source(BLANK_LINES_DEFINITION, 100, 30, stream.as_bytes()).await;
    assert_eq!(status, WindowStatus::WaitingInput);
}

// =====================================================================
// Claude's spinner verb is not always ASCII.
// =====================================================================

#[tokio::test(start_paused = true)]
async fn claude_code_spinner_with_a_non_ascii_verb_is_running() {
    let status = judge_fixture(
        "claude-code.toml",
        "claude-code",
        100,
        30,
        "100x30-running-body-flambeing",
    )
    .await;
    assert_eq!(status, WindowStatus::Running);
}

#[tokio::test(start_paused = true)]
async fn claude_code_spinner_with_a_non_ascii_verb_and_a_longer_body_is_running() {
    let status = judge_fixture(
        "claude-code.toml",
        "claude-code",
        100,
        30,
        "100x30-running-body-flambeing-second-line",
    )
    .await;
    assert_eq!(status, WindowStatus::Running);
}

/// The completion line ("Baked for 14s", "Worked for 4s") names a duration,
/// not ongoing work.
#[tokio::test(start_paused = true)]
async fn claude_code_finished_line_is_not_running() {
    for name in ["100x30-finished-baked-for", "100x30-finished-worked-for"] {
        let status = judge_fixture("claude-code.toml", "claude-code", 100, 30, name).await;
        assert_eq!(status, WindowStatus::WaitingInput, "{name}");
    }
}

// =====================================================================
// A dialog stays a dialog.
// =====================================================================

#[tokio::test(start_paused = true)]
async fn real_dialogs_stay_waiting_approval() {
    let cases: [(&str, &str, u16, u16, &str); 9] = [
        (
            "claude-code.toml",
            "claude-code",
            100,
            30,
            "100x30-permission-dialog",
        ),
        (
            "claude-code.toml",
            "claude-code",
            80,
            24,
            "80x24-permission-dialog",
        ),
        (
            "claude-code.toml",
            "claude-code",
            100,
            30,
            "100x30-rewind-dialog",
        ),
        (
            "codex-cli.toml",
            "codex-cli",
            100,
            30,
            "100x30-permission-dialog",
        ),
        (
            "codex-cli.toml",
            "codex-cli",
            100,
            30,
            "100x30-permission-dialog-empty-composer",
        ),
        (
            "codex-cli.toml",
            "codex-cli",
            80,
            24,
            "80x24-permission-dialog",
        ),
        (
            "antigravity-cli.toml",
            "antigravity-cli",
            100,
            30,
            "100x30-permission-dialog",
        ),
        (
            "antigravity-cli.toml",
            "antigravity-cli",
            100,
            30,
            "100x30-permission-dialog-empty-composer",
        ),
        (
            "antigravity-cli.toml",
            "antigravity-cli",
            80,
            24,
            "80x24-permission-dialog",
        ),
    ];
    for (definition, cli, cols, rows, name) in cases {
        let status = judge_fixture(definition, cli, cols, rows, name).await;
        assert_eq!(status, WindowStatus::WaitingApproval, "{cli}/{name}");
    }
}

/// A dialog whose option line looks like a composer line (marker, then a
/// numbered choice), followed by an indented explanation and an indented
/// footer, with the cursor hidden, as a dialog draws it. With the cursor
/// hidden nothing here is a composer, so the footer must still decide. That
/// holds for a definition whose composer body may hold blank lines too, even
/// where the dialog's blank rows sit between its marker-shaped row and its
/// footer.
#[tokio::test(start_paused = true)]
async fn dialog_shaped_like_a_composer_with_a_hidden_cursor_stays_waiting_approval() {
    let screen = concat!(
        "\x1b[2J\x1b[H",
        "  Command\r\n",
        "\r\n",
        "    echo hello\r\n",
        "\r\n",
        "\u{203a} 1. Yes, proceed\r\n",
        "  Run the command exactly as shown.\r\n",
        "  2. No, and tell me what to do differently\r\n",
        "\r\n",
        "  Press enter to confirm or Esc to cancel",
        "\x1b[?25l",
    );
    let status = judge_stream("codex-cli.toml", 100, 30, screen.as_bytes()).await;
    assert_eq!(status, WindowStatus::WaitingApproval);

    let blank_rows_dialog = concat!(
        "\x1b[2J\x1b[H",
        "  Run this command?\r\n",
        "\r\n",
        "> 1. Yes\r\n",
        "\r\n",
        "  2. No\r\n",
        "\r\n",
        "  Use arrows to choose, enter to confirm",
        "\x1b[?25l",
    );
    let status = judge_stream_source(
        BLANK_LINES_DEFINITION,
        100,
        30,
        blank_rows_dialog.as_bytes(),
    )
    .await;
    assert_eq!(status, WindowStatus::WaitingApproval, "blank lines in body");
}
