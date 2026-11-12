#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/antigravity-cli.toml` against real Antigravity CLI (`agy`) screen
//! captures (1.2.11) and one synthetic fail-safe fixture. See
//! `tests/fixtures/antigravity-cli/README.md` for fixture provenance, the
//! `reading::bottom_lines(16)` extraction rule, and why a background-task footer reads
//! as `running` and an unrecognised dialog reads as `unknown` rather than `waiting_input`.

use crate::common;

use common::detection_rules::judge;
use common::repo_definition_text;
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
