#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/muse-code.toml` against real Muse Code screen captures (1.4.12).
//! See `tests/fixtures/muse-code/README.md` for fixture provenance, the
//! `VtScreen::bottom_lines(16)` extraction rule, and the redactions applied.

mod common;

use common::detection_rules::judge;
use common::repo_definition_text;
use yagura_protocol::WindowStatus;

const FIXTURE_100X30_TRUST: &str = include_str!("fixtures/muse-code/100x30-trust.txt");
const FIXTURE_100X30_WSL_TRUST: &str = include_str!("fixtures/muse-code/100x30-wsl-trust.txt");
const FIXTURE_100X30_IDLE_STARTUP: &str =
    include_str!("fixtures/muse-code/100x30-idle-startup.txt");
const FIXTURE_100X30_IDLE_MODEL1: &str = include_str!("fixtures/muse-code/100x30-idle-model1.txt");
const FIXTURE_100X30_PASTE_DRAFT: &str = include_str!("fixtures/muse-code/100x30-paste-draft.txt");
const FIXTURE_100X30_DRAFT_NEWLINE: &str =
    include_str!("fixtures/muse-code/100x30-draft-newline.txt");
const FIXTURE_100X30_AFTER_ENTER2: &str =
    include_str!("fixtures/muse-code/100x30-after-enter2.txt");
const FIXTURE_100X30_DONE1B: &str = include_str!("fixtures/muse-code/100x30-done1b.txt");
const FIXTURE_100X30_AFTER_APPROVE: &str =
    include_str!("fixtures/muse-code/100x30-after-approve.txt");
const FIXTURE_100X30_APPROVAL: &str = include_str!("fixtures/muse-code/100x30-approval.txt");
const FIXTURE_100X30_MCP_APPROVAL: &str =
    include_str!("fixtures/muse-code/100x30-mcp-approval.txt");
const FIXTURE_100X30_SETTLED1: &str = include_str!("fixtures/muse-code/100x30-settled1.txt");
const FIXTURE_100X30_INTERRUPTED: &str = include_str!("fixtures/muse-code/100x30-interrupted.txt");
const FIXTURE_100X30_MCP_LONG_DONE: &str =
    include_str!("fixtures/muse-code/100x30-mcp-long-done.txt");
const FIXTURE_100X30_ECHO_WAKE_ASYNC: &str =
    include_str!("fixtures/muse-code/100x30-echo-wake-async.txt");
const FIXTURE_100X30_WSL_IDLE: &str = include_str!("fixtures/muse-code/100x30-wsl-idle.txt");
const FIXTURE_100X30_WSL_DONE: &str = include_str!("fixtures/muse-code/100x30-wsl-done.txt");

/// The real fixtures with their recorded geometry and expected verdict, per
/// `tests/fixtures/muse-code/README.md`. The approval boxes keep `esc to interrupt)`
/// on screen above them, so they hold only if the approval rule outranks running.
const CASES: &[(&str, &str, u16, u16, WindowStatus)] = &[
    (
        "100x30-trust",
        FIXTURE_100X30_TRUST,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-wsl-trust",
        FIXTURE_100X30_WSL_TRUST,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-idle-startup",
        FIXTURE_100X30_IDLE_STARTUP,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-idle-model1",
        FIXTURE_100X30_IDLE_MODEL1,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-paste-draft",
        FIXTURE_100X30_PASTE_DRAFT,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-draft-newline",
        FIXTURE_100X30_DRAFT_NEWLINE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-after-enter2",
        FIXTURE_100X30_AFTER_ENTER2,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-done1b",
        FIXTURE_100X30_DONE1B,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-after-approve",
        FIXTURE_100X30_AFTER_APPROVE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-approval",
        FIXTURE_100X30_APPROVAL,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-mcp-approval",
        FIXTURE_100X30_MCP_APPROVAL,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-settled1",
        FIXTURE_100X30_SETTLED1,
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
        "100x30-mcp-long-done",
        FIXTURE_100X30_MCP_LONG_DONE,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-echo-wake-async",
        FIXTURE_100X30_ECHO_WAKE_ASYNC,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-wsl-idle",
        FIXTURE_100X30_WSL_IDLE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-wsl-done",
        FIXTURE_100X30_WSL_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
];

/// A screen with no composer, no turn in progress and no approval box: plain output
/// rows above a shell prompt, as when the CLI has exited back to the shell.
const SYNTHETIC_UNMATCHED: &[&str] = &[
    "Building project...",
    "   Compiling example v0.1.0",
    "    Finished dev profile in 2.31s",
    "",
    "demo@host:~/work/project$ ",
];

#[tokio::test]
async fn muse_code_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = repo_definition_text("muse-code.toml");

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

#[tokio::test]
async fn muse_code_definition_leaves_an_unmatched_screen_unknown() {
    tokio::time::pause();
    let source = repo_definition_text("muse-code.toml");

    let status = judge(&source, 100, 30, SYNTHETIC_UNMATCHED).await;

    assert_eq!(
        status,
        WindowStatus::Unknown,
        "a screen matching no rule must fall to Unknown, never to a guessed state"
    );
}
