#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/grok-build.toml` against real Grok Build screen captures (1.0.70).
//! See `tests/fixtures/grok-build/README.md` for fixture provenance, the
//! `reading::bottom_lines(16)` extraction rule, and the note that these captures
//! are to be replaced by ones taken from a daemon-opened window.

use crate::common;

use common::detection_rules::judge;
use common::repo_definition_text;
use yagura_protocol::WindowStatus;

const DEFINITION: &str = "grok-build.toml";

const FIXTURE_IDLE_STARTUP: &str = include_str!("fixtures/grok-build/100x30-idle-startup.txt");
const FIXTURE_DRAFT: &str = include_str!("fixtures/grok-build/100x30-draft.txt");
const FIXTURE_RUNNING: &str = include_str!("fixtures/grok-build/100x30-running.txt");
const FIXTURE_AFTER_TURN: &str = include_str!("fixtures/grok-build/100x30-after-turn.txt");
const FIXTURE_APPROVAL: &str = include_str!("fixtures/grok-build/100x30-approval.txt");
const FIXTURE_APPROVAL_HISTORY_FOCUS: &str =
    include_str!("fixtures/grok-build/100x30-approval-history-focus.txt");
const FIXTURE_INTERRUPTED_HISTORY_FOCUS: &str =
    include_str!("fixtures/grok-build/100x30-interrupted-history-focus.txt");
const FIXTURE_DAEMON_IDLE_VERSION_FOOTER: &str =
    include_str!("fixtures/grok-build/daemon-idle-version-footer.txt");
const FIXTURE_DAEMON_TRUST_SCREEN: &str =
    include_str!("fixtures/grok-build/daemon-trust-screen.txt");
const FIXTURE_WSL_IDLE: &str = include_str!("fixtures/grok-build/100x30-wsl-idle.txt");
const FIXTURE_WSL_RUNNING: &str = include_str!("fixtures/grok-build/100x30-wsl-running.txt");
const FIXTURE_WSL_AFTER_TURN: &str = include_str!("fixtures/grok-build/100x30-wsl-after-turn.txt");

/// The real fixtures with their expected verdict, per `tests/fixtures/grok-build/README.md`.
/// A permission box keeps the turn's `Ctrl+c:cancel` on screen, so it holds only if the
/// approval rule outranks running; a history-focused screen shows no composer the daemon
/// could type into, so it must read Unknown rather than the waiting-input a box suggests.
const CASES: &[(&str, &str, WindowStatus)] = &[
    (
        "idle-startup",
        FIXTURE_IDLE_STARTUP,
        WindowStatus::WaitingInput,
    ),
    ("draft", FIXTURE_DRAFT, WindowStatus::WaitingInput),
    ("running", FIXTURE_RUNNING, WindowStatus::Running),
    ("after-turn", FIXTURE_AFTER_TURN, WindowStatus::WaitingInput),
    ("approval", FIXTURE_APPROVAL, WindowStatus::WaitingApproval),
    (
        "approval-history-focus",
        FIXTURE_APPROVAL_HISTORY_FOCUS,
        WindowStatus::WaitingApproval,
    ),
    (
        "interrupted-history-focus",
        FIXTURE_INTERRUPTED_HISTORY_FOCUS,
        WindowStatus::Unknown,
    ),
    ("wsl-idle", FIXTURE_WSL_IDLE, WindowStatus::WaitingInput),
    ("wsl-running", FIXTURE_WSL_RUNNING, WindowStatus::Running),
    (
        "wsl-after-turn",
        FIXTURE_WSL_AFTER_TURN,
        WindowStatus::WaitingInput,
    ),
];

/// The size of the window the daemon opened when the `daemon-*` fixtures were taken.
const DAEMON_WINDOW: (u16, u16) = (88, 23);

/// Screens read from a daemon-opened window, judged at that window's size.
const DAEMON_CASES: &[(&str, &str, WindowStatus)] = &[
    // After the trust question the start screen keeps the version-only footer under the
    // composer box, so that footer alone does not mean the trust question.
    (
        "daemon-idle-version-footer",
        FIXTURE_DAEMON_IDLE_VERSION_FOOTER,
        WindowStatus::WaitingInput,
    ),
    (
        "daemon-trust-screen",
        FIXTURE_DAEMON_TRUST_SCREEN,
        WindowStatus::WaitingApproval,
    ),
];

/// A screen with no composer box, no turn in progress and no approval box: plain output
/// rows above a shell prompt, as when the CLI has exited back to the shell.
const SYNTHETIC_UNMATCHED: &[&str] = &[
    "Building project...",
    "   Compiling example v0.1.0",
    "    Finished dev profile in 2.31s",
    "",
    "demo@host:~/work/project$ ",
];

#[tokio::test]
async fn grok_build_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = repo_definition_text(DEFINITION);

    let sized_cases = CASES
        .iter()
        .map(|case| ((100, 30), case))
        .chain(DAEMON_CASES.iter().map(|case| (DAEMON_WINDOW, case)));
    let mut mismatches = Vec::new();
    for ((cols, rows), (name, fixture, expected)) in sized_cases {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&source, cols, rows, &lines).await;
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
async fn grok_build_definition_leaves_an_unmatched_screen_unknown() {
    tokio::time::pause();
    let source = repo_definition_text(DEFINITION);

    let status = judge(&source, 100, 30, SYNTHETIC_UNMATCHED).await;

    assert_eq!(
        status,
        WindowStatus::Unknown,
        "a screen matching no rule must fall to Unknown, never to a guessed state"
    );
}
