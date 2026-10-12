#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/antigravity-cli.toml` against real Antigravity CLI (`agy`) screen
//! captures (1.2.11) and one synthetic fail-safe fixture. See
//! `tests/fixtures/antigravity-cli/README.md` for fixture provenance, the
//! `VtScreen::bottom_lines(16)` extraction rule, and why a background-task footer reads
//! as `running` and an unrecognised dialog reads as `unknown` rather than `waiting_input`.

mod common;

use common::detection_rules::judge;
use common::{repaint_lines, repo_definition_text};
use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::parse_definition;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_protocol::WindowStatus;

const FIXTURE_100X30_IDLE_STARTUP: &str =
    include_str!("fixtures/antigravity-cli/100x30-idle-startup.txt");
const FIXTURE_100X30_WAITING_INPUT_DRAFT: &str =
    include_str!("fixtures/antigravity-cli/100x30-waiting-input-draft.txt");
const FIXTURE_100X30_RUNNING: &str = include_str!("fixtures/antigravity-cli/100x30-running.txt");
const FIXTURE_100X30_APPROVAL_COMMAND: &str =
    include_str!("fixtures/antigravity-cli/100x30-approval-command.txt");
const FIXTURE_100X30_APPROVAL_MCP: &str =
    include_str!("fixtures/antigravity-cli/100x30-approval-mcp.txt");
const FIXTURE_100X30_RUNNING_BACKGROUND: &str =
    include_str!("fixtures/antigravity-cli/100x30-running-background.txt");
const FIXTURE_110X34_TRUST: &str = include_str!("fixtures/antigravity-cli/110x34-trust.txt");
const FIXTURE_100X30_SLASH_MENU: &str =
    include_str!("fixtures/antigravity-cli/100x30-slash-menu.txt");
const FIXTURE_100X30_INTERRUPTED: &str =
    include_str!("fixtures/antigravity-cli/100x30-interrupted.txt");
const FIXTURE_100X30_DONE: &str = include_str!("fixtures/antigravity-cli/100x30-done.txt");
const FIXTURE_100X30_UNKNOWN_DIALOG_UNKNOWN_SYNTHETIC: &str =
    include_str!("fixtures/antigravity-cli/100x30-unknown-dialog-unknown-synthetic.txt");

/// The ten real fixtures with their recorded geometry and expected verdict, per
/// `tests/fixtures/antigravity-cli/README.md`. Covers every real fixture reading as
/// its recorded verdict, including the two rows whose evidence sits above the
/// composer line: the background-task footer and the spinner line.
const POSITIVE_CASES: &[(&str, &str, u16, u16, WindowStatus)] = &[
    (
        "100x30-idle-startup",
        FIXTURE_100X30_IDLE_STARTUP,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-waiting-input-draft",
        FIXTURE_100X30_WAITING_INPUT_DRAFT,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-running",
        FIXTURE_100X30_RUNNING,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-approval-command",
        FIXTURE_100X30_APPROVAL_COMMAND,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-approval-mcp",
        FIXTURE_100X30_APPROVAL_MCP,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-running-background",
        FIXTURE_100X30_RUNNING_BACKGROUND,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "110x34-trust",
        FIXTURE_110X34_TRUST,
        110,
        34,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-slash-menu",
        FIXTURE_100X30_SLASH_MENU,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-interrupted",
        FIXTURE_100X30_INTERRUPTED,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-done",
        FIXTURE_100X30_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
];

fn load_antigravity_cli_source() -> String {
    repo_definition_text("antigravity-cli.toml")
}

#[tokio::test]
async fn antigravity_cli_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = load_antigravity_cli_source();

    for (name, fixture, cols, rows, expected) in POSITIVE_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&source, *cols, *rows, &lines).await;
        assert_eq!(
            status, *expected,
            "fixture {name} must classify as {expected:?}"
        );
    }
}

// --- An unrecognised dialog with history caret lines stays Unknown ---
//
// The synthetic fixture carries two `> `-prefixed composer-shaped history lines and ends
// in a bare `esc to cancel` footer -- no `↑/↓ Navigate` line (so `waiting_approval` does
// not match) and no spinner or `N task(s)` text (so `running` does not match either). The
// safety property this definition has to hold is that such a screen still does not fall
// through to `waiting_input` on the strength of its history lines: `esc to cancel` alone
// must be read by its own `unknown` rule, which outranks `waiting_input`.

#[tokio::test]
async fn unknown_dialog_synthetic_fixture_stays_unknown_despite_history_caret_lines() {
    tokio::time::pause();
    let source = load_antigravity_cli_source();
    let lines: Vec<&str> = FIXTURE_100X30_UNKNOWN_DIALOG_UNKNOWN_SYNTHETIC
        .lines()
        .collect();

    let status = judge(&source, 100, 30, &lines).await;

    assert_eq!(
        status,
        WindowStatus::Unknown,
        "a dialog carrying no waiting_approval or running anchor, only a bare esc to \
         cancel footer, must stay Unknown even though history lines above it start with \
         the same caret the composer uses"
    );
}

// --- The composer readout on a real fixture ---
//
// Built directly against `VtScreen::read_composer`, bypassing `DetectionEngine`: this is
// the same "read what the daemon would type over" contract `composer_fixture.rs` proves
// for the mock fixture binaries, exercised here against a real agy screen capture instead.
//
// `read_composer` scans upward from the screen's cursor, not from any fixed row, so a
// caller has to put the cursor back where the CLI actually left it before judging the
// result -- `repaint_lines` alone does not do that: it simply writes every one of the
// fixture's lines in turn, including the trailing blank padding rows
// `tests/fixtures/antigravity-cli/README.md`'s "Extraction rule" keeps, and leaves the
// cursor wherever that stream of writes put it (the last line), not on the composer row.
// The two cursor positions below are recorded data, not a guess: they are the real
// terminal's own cursor position (row, col; 0-indexed within this fixture's own
// `bottom_lines(16)` window) at the moment each fixture was captured, read by the same
// `redump` tool that produced the fixture text (see
// `tests/fixtures/antigravity-cli/README.md`'s "Composer cursor position" section for
// where that number came from and why it is kept here as a constant rather than folded
// into the fixture file itself).
const IDLE_STARTUP_COMPOSER_CURSOR: (u16, u16) = (8, 2);
const WAITING_INPUT_DRAFT_COMPOSER_CURSOR: (u16, u16) = (8, 14);

/// Feeds `lines` the way [`repaint_lines`] always has, then explicitly repositions the
/// screen cursor to `(row, col)` (0-indexed) with a Cursor Position escape -- the one
/// piece of the real capture `repaint_lines` on its own cannot reproduce, since it has no
/// notion of where a screen's cursor sat when it was captured.
fn read_composer_from_fixture_at_cursor(
    source: &str,
    cols: u16,
    rows: u16,
    lines: &[&str],
    (row, col): (u16, u16),
) -> ComposerReadout {
    let definition = parse_definition(source).expect("the definition must parse");
    let section = definition
        .composer
        .expect("antigravity-cli.toml must declare a [composer] section");
    let mut screen = VtScreen::new(cols, rows);
    let mut bytes = repaint_lines(lines);
    bytes.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    screen.feed(&bytes);
    screen.read_composer(&section)
}

#[tokio::test]
async fn composer_readout_is_empty_on_the_idle_startup_fixture() {
    let source = load_antigravity_cli_source();
    let lines: Vec<&str> = FIXTURE_100X30_IDLE_STARTUP.lines().collect();

    let readout = read_composer_from_fixture_at_cursor(
        &source,
        100,
        30,
        &lines,
        IDLE_STARTUP_COMPOSER_CURSOR,
    );

    assert_eq!(
        readout,
        ComposerReadout::Empty,
        "the idle-startup fixture's composer carries no typed body"
    );
}

#[tokio::test]
async fn composer_readout_is_the_typed_body_on_the_waiting_input_draft_fixture() {
    let source = load_antigravity_cli_source();
    let lines: Vec<&str> = FIXTURE_100X30_WAITING_INPUT_DRAFT.lines().collect();

    let readout = read_composer_from_fixture_at_cursor(
        &source,
        100,
        30,
        &lines,
        WAITING_INPUT_DRAFT_COMPOSER_CURSOR,
    );

    match readout {
        ComposerReadout::Body(text) => assert_eq!(
            text.split_whitespace().collect::<String>(),
            "zzTypedDraft",
            "the readout's non-whitespace content must equal the typed draft"
        ),
        other => panic!("expected a Body readout on the draft fixture, got {other:?}"),
    }
}

// A falsifiability check for the two tests above: parking the cursor on a row that is
// neither the composer's marker line nor one of its continuation lines (here, the
// idle-startup fixture's blank last row) must read as NotObserved, not as Empty or Body
// by coincidence. This proves the two tests above are not trivially satisfied regardless
// of where the cursor sits -- the cursor position genuinely decides the outcome.
#[tokio::test]
async fn composer_readout_is_not_observed_when_the_cursor_sits_off_the_composer_row() {
    let source = load_antigravity_cli_source();
    let lines: Vec<&str> = FIXTURE_100X30_IDLE_STARTUP.lines().collect();
    let last_row = u16::try_from(lines.len() - 1).expect("fixture row count fits in u16");

    let readout = read_composer_from_fixture_at_cursor(&source, 100, 30, &lines, (last_row, 0));

    assert_eq!(
        readout,
        ComposerReadout::NotObserved,
        "a cursor left on a blank row far from the composer's marker line must not be \
         read as the composer by accident"
    );
}
