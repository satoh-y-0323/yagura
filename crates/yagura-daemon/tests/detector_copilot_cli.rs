#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/copilot-cli.toml` against real GitHub Copilot CLI screen captures
//! (1.0.101). See `tests/fixtures/copilot-cli/README.md` for fixture provenance, the
//! `VtScreen::bottom_lines(16)` extraction rule, and the redactions applied.

mod common;

use common::detection_rules::judge;
use common::repo_definition_text;
use yagura_protocol::{DetectedStatus, UnknownReason, WindowStatus};

const FIXTURE_100X30_IDLE_STARTUP: &str =
    include_str!("fixtures/copilot-cli/100x30-idle-startup.txt");
const FIXTURE_100X30_IDLE_STARTUP_WSL: &str =
    include_str!("fixtures/copilot-cli/100x30-idle-startup-wsl.txt");
const FIXTURE_100X30_DONE: &str = include_str!("fixtures/copilot-cli/100x30-done.txt");
const FIXTURE_100X30_WAITING_INPUT_DRAFT: &str =
    include_str!("fixtures/copilot-cli/100x30-waiting-input-draft.txt");
const FIXTURE_100X30_INTERRUPTED: &str =
    include_str!("fixtures/copilot-cli/100x30-interrupted.txt");
const FIXTURE_100X30_RUNNING: &str = include_str!("fixtures/copilot-cli/100x30-running.txt");
const FIXTURE_100X30_RUNNING_ESC_ONCE: &str =
    include_str!("fixtures/copilot-cli/100x30-running-esc-once.txt");
const FIXTURE_100X30_APPROVAL_COMMAND: &str =
    include_str!("fixtures/copilot-cli/100x30-approval-command.txt");
const FIXTURE_100X30_TRUST: &str = include_str!("fixtures/copilot-cli/100x30-trust.txt");
const FIXTURE_100X30_TRUST_WSL: &str = include_str!("fixtures/copilot-cli/100x30-trust-wsl.txt");
const FIXTURE_100X30_RUNNING_JUST_SUBMITTED: &str =
    include_str!("fixtures/copilot-cli/100x30-running-just-submitted.txt");
const FIXTURE_100X30_WAITING_INPUT_WRAPPED_DRAFT: &str =
    include_str!("fixtures/copilot-cli/100x30-waiting-input-wrapped-draft.txt");
const FIXTURE_100X30_IDLE_STARTUP_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-idle-startup-no-palette.txt");
const FIXTURE_100X30_IDLE_STARTUP_WSL_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-idle-startup-wsl-no-palette.txt");
const FIXTURE_100X30_DONE_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-done-no-palette.txt");
const FIXTURE_100X30_WAITING_INPUT_DRAFT_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-waiting-input-draft-no-palette.txt");
const FIXTURE_100X30_INTERRUPTED_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-interrupted-no-palette.txt");
const FIXTURE_100X30_RUNNING_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-running-no-palette.txt");
const FIXTURE_100X30_RUNNING_ESC_ONCE_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-running-esc-once-no-palette.txt");
const FIXTURE_100X30_RUNNING_JUST_SUBMITTED_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-running-just-submitted-no-palette.txt");
const FIXTURE_100X30_WAITING_INPUT_WRAPPED_DRAFT_NO_PALETTE: &str =
    include_str!("fixtures/copilot-cli/100x30-waiting-input-wrapped-draft-no-palette.txt");
const FIXTURE_100X30_TAB_SESSIONS: &str =
    include_str!("fixtures/copilot-cli/100x30-tab-sessions.txt");
const FIXTURE_100X30_TAB_ISSUES: &str = include_str!("fixtures/copilot-cli/100x30-tab-issues.txt");
const FIXTURE_100X30_TAB_PULL_REQUESTS: &str =
    include_str!("fixtures/copilot-cli/100x30-tab-pull-requests.txt");
const FIXTURE_100X30_TAB_GISTS: &str = include_str!("fixtures/copilot-cli/100x30-tab-gists.txt");
const FIXTURE_100X30_TAB_BACK_TO_CURRENT: &str =
    include_str!("fixtures/copilot-cli/100x30-tab-back-to-current.txt");
const FIXTURE_100X30_SIDEBAR_IDLE: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-idle.txt");
const FIXTURE_100X30_SIDEBAR_DRAFT: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-draft.txt");
const FIXTURE_100X30_SIDEBAR_RUNNING_JUST_SUBMITTED: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-running-just-submitted.txt");
const FIXTURE_100X30_SIDEBAR_RUNNING: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-running.txt");
const FIXTURE_100X30_SIDEBAR_RUNNING_ESC_ONCE: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-running-esc-once.txt");
const FIXTURE_100X30_SIDEBAR_APPROVAL_COMMAND: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-approval-command.txt");
const FIXTURE_100X30_SIDEBAR_INTERRUPTED: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-interrupted.txt");
const FIXTURE_100X30_SIDEBAR_FOCUSED: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-focused.txt");
const FIXTURE_100X30_SIDEBAR_CLOSED: &str =
    include_str!("fixtures/copilot-cli/100x30-sidebar-closed.txt");

/// The real fixtures read from the screen text alone, with their recorded geometry and
/// expected verdict, per `tests/fixtures/copilot-cli/README.md`.
const CASES: &[(&str, &str, u16, u16, WindowStatus)] = &[
    (
        "100x30-idle-startup",
        FIXTURE_100X30_IDLE_STARTUP,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-idle-startup-wsl",
        FIXTURE_100X30_IDLE_STARTUP_WSL,
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
    (
        "100x30-waiting-input-draft",
        FIXTURE_100X30_WAITING_INPUT_DRAFT,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-interrupted",
        FIXTURE_100X30_INTERRUPTED,
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
        "100x30-running-esc-once",
        FIXTURE_100X30_RUNNING_ESC_ONCE,
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
        "100x30-trust",
        FIXTURE_100X30_TRUST,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-trust-wsl",
        FIXTURE_100X30_TRUST_WSL,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-running-just-submitted",
        FIXTURE_100X30_RUNNING_JUST_SUBMITTED,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-idle-startup-no-palette",
        FIXTURE_100X30_IDLE_STARTUP_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-idle-startup-wsl-no-palette",
        FIXTURE_100X30_IDLE_STARTUP_WSL_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-done-no-palette",
        FIXTURE_100X30_DONE_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-waiting-input-draft-no-palette",
        FIXTURE_100X30_WAITING_INPUT_DRAFT_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-interrupted-no-palette",
        FIXTURE_100X30_INTERRUPTED_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-running-no-palette",
        FIXTURE_100X30_RUNNING_NO_PALETTE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-running-esc-once-no-palette",
        FIXTURE_100X30_RUNNING_ESC_ONCE_NO_PALETTE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-running-just-submitted-no-palette",
        FIXTURE_100X30_RUNNING_JUST_SUBMITTED_NO_PALETTE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-waiting-input-wrapped-draft-no-palette",
        FIXTURE_100X30_WAITING_INPUT_WRAPPED_DRAFT_NO_PALETTE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-tab-sessions",
        FIXTURE_100X30_TAB_SESSIONS,
        100,
        30,
        WindowStatus::Unknown,
    ),
    (
        "100x30-tab-issues",
        FIXTURE_100X30_TAB_ISSUES,
        100,
        30,
        WindowStatus::Unknown,
    ),
    (
        "100x30-tab-pull-requests",
        FIXTURE_100X30_TAB_PULL_REQUESTS,
        100,
        30,
        WindowStatus::Unknown,
    ),
    (
        "100x30-tab-gists",
        FIXTURE_100X30_TAB_GISTS,
        100,
        30,
        WindowStatus::Unknown,
    ),
    (
        "100x30-tab-back-to-current",
        FIXTURE_100X30_TAB_BACK_TO_CURRENT,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-sidebar-idle",
        FIXTURE_100X30_SIDEBAR_IDLE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-sidebar-draft",
        FIXTURE_100X30_SIDEBAR_DRAFT,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-sidebar-running-just-submitted",
        FIXTURE_100X30_SIDEBAR_RUNNING_JUST_SUBMITTED,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-sidebar-running",
        FIXTURE_100X30_SIDEBAR_RUNNING,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-sidebar-running-esc-once",
        FIXTURE_100X30_SIDEBAR_RUNNING_ESC_ONCE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-sidebar-approval-command",
        FIXTURE_100X30_SIDEBAR_APPROVAL_COMMAND,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-sidebar-interrupted",
        FIXTURE_100X30_SIDEBAR_INTERRUPTED,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-sidebar-focused",
        FIXTURE_100X30_SIDEBAR_FOCUSED,
        100,
        30,
        WindowStatus::Unknown,
    ),
    (
        "100x30-sidebar-closed",
        FIXTURE_100X30_SIDEBAR_CLOSED,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
];

/// A draft that wraps pushes the composer's marker row out of the bottom window, so the
/// verdict depends on the daemon reading the draft's continuation rows as the composer's
/// body. That needs the cursor where the CLI leaves it, at the end of the draft; the
/// capture keeps text only, so the cursor is put there after the repaint.
#[tokio::test]
async fn copilot_cli_definition_reads_a_wrapped_draft_through_the_composer() {
    tokio::time::pause();
    let source = repo_definition_text("copilot-cli.toml");
    let lines: Vec<&str> = FIXTURE_100X30_WAITING_INPUT_WRAPPED_DRAFT.lines().collect();
    let closing_rule = lines
        .iter()
        .rposition(|line| line.starts_with('╹'))
        .expect("the composer is closed by a rule row");
    let last_body_row = closing_rule - 1;
    let last_body_col = lines[last_body_row].trim_end().chars().count();

    let mut engine =
        common::new_engine_sized(&source, &yagura_daemon::DaemonConfig::default(), 100, 30);
    let mut bytes = common::repaint_lines(&lines);
    bytes
        .extend_from_slice(format!("\x1b[{};{}H", last_body_row + 1, last_body_col + 1).as_bytes());
    common::ScreenFeed::feed(&mut engine, &bytes);
    common::tick_after_ms(&mut engine, 350).await;

    assert_eq!(engine.status(), WindowStatus::WaitingInput);
}

/// The tabs other than Current and the focused sidebar take a key as an action, not as
/// text, so the definition names them Unknown itself rather than leaving them unmatched.
#[tokio::test]
async fn copilot_cli_definition_declares_screens_without_a_composer_unknown() {
    tokio::time::pause();
    let source = repo_definition_text("copilot-cli.toml");

    for (name, fixture) in [
        ("100x30-tab-sessions", FIXTURE_100X30_TAB_SESSIONS),
        ("100x30-tab-issues", FIXTURE_100X30_TAB_ISSUES),
        ("100x30-tab-pull-requests", FIXTURE_100X30_TAB_PULL_REQUESTS),
        ("100x30-tab-gists", FIXTURE_100X30_TAB_GISTS),
        ("100x30-sidebar-focused", FIXTURE_100X30_SIDEBAR_FOCUSED),
    ] {
        let lines: Vec<&str> = fixture.lines().collect();
        let mut engine =
            common::new_engine_sized(&source, &yagura_daemon::DaemonConfig::default(), 100, 30);
        common::ScreenFeed::feed(&mut engine, &common::repaint_lines(&lines));
        common::tick_after_ms(&mut engine, 350).await;

        assert_eq!(
            engine.detected(),
            DetectedStatus::Unknown(UnknownReason::DeclaredUnknown),
            "{name}"
        );
    }
}

#[tokio::test]
async fn copilot_cli_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = repo_definition_text("copilot-cli.toml");

    let mut mismatches = Vec::new();
    for (name, fixture, cols, rows, expected) in CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&source, *cols, *rows, &lines).await;
        if status != *expected {
            mismatches.push(format!("{name}: expected {expected:?}, got {status:?}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "every captured screen must classify as its recorded verdict: {mismatches:#?}"
    );
}
