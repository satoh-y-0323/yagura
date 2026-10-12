#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/claude-code.toml` against real Claude Code screen captures. Each
//! fixture is cut by the capture tool using the engine's own window arithmetic
//! (`VtScreen::bottom_lines(16)`), not a manual line count. See
//! `tests/fixtures/claude-code/README.md` for fixture provenance, geometry, and the
//! per-fixture region a rule's anchor line must fall inside.

mod common;

use common::detection_rules::{
    judge, judge_after_two_stage_tick, mutate_rules, rule_for_status, rule_status,
    set_rule_priority, with_every_pattern, with_status_pattern,
};
use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::parse_definition;
use yagura_protocol::WindowStatus;

const FIXTURE_100X30_UNICODE_IDLE_STARTUP: &str =
    include_str!("fixtures/claude-code/100x30-unicode-idle-startup.txt");
const FIXTURE_100X30_UNICODE_RUNNING: &str =
    include_str!("fixtures/claude-code/100x30-unicode-running.txt");
const FIXTURE_100X30_UNICODE_DONE: &str =
    include_str!("fixtures/claude-code/100x30-unicode-done.txt");
const FIXTURE_100X30_UNICODE_APPROVAL: &str =
    include_str!("fixtures/claude-code/100x30-unicode-approval.txt");
const FIXTURE_100X30_ASCII_IDLE_STARTUP: &str =
    include_str!("fixtures/claude-code/100x30-ascii-idle-startup.txt");
const FIXTURE_100X30_ASCII_RUNNING: &str =
    include_str!("fixtures/claude-code/100x30-ascii-running.txt");
const FIXTURE_100X30_ASCII_DONE: &str = include_str!("fixtures/claude-code/100x30-ascii-done.txt");
const FIXTURE_100X30_ASCII_APPROVAL: &str =
    include_str!("fixtures/claude-code/100x30-ascii-approval.txt");
const FIXTURE_80X24_UNICODE_IDLE_STARTUP: &str =
    include_str!("fixtures/claude-code/80x24-unicode-idle-startup.txt");
const FIXTURE_80X24_UNICODE_RUNNING: &str =
    include_str!("fixtures/claude-code/80x24-unicode-running.txt");
const FIXTURE_80X24_UNICODE_DONE: &str =
    include_str!("fixtures/claude-code/80x24-unicode-done.txt");
const FIXTURE_80X24_UNICODE_APPROVAL: &str =
    include_str!("fixtures/claude-code/80x24-unicode-approval.txt");
const FIXTURE_80X24_ASCII_IDLE_STARTUP: &str =
    include_str!("fixtures/claude-code/80x24-ascii-idle-startup.txt");
const FIXTURE_80X24_ASCII_RUNNING: &str =
    include_str!("fixtures/claude-code/80x24-ascii-running.txt");
const FIXTURE_80X24_ASCII_DONE: &str = include_str!("fixtures/claude-code/80x24-ascii-done.txt");
const FIXTURE_80X24_ASCII_APPROVAL: &str =
    include_str!("fixtures/claude-code/80x24-ascii-approval.txt");
const FIXTURE_100X30_ASCII_TRUST: &str =
    include_str!("fixtures/claude-code/100x30-ascii-trust.txt");
const FIXTURE_100X30_UNICODE_TRUST: &str =
    include_str!("fixtures/claude-code/100x30-unicode-trust.txt");
const FIXTURE_100X30_UNICODE_ASK_USER_QUESTION: &str =
    include_str!("fixtures/claude-code/100x30-unicode-ask-user-question.txt");
const FIXTURE_100X30_UNICODE_APPROVAL_WITHOUT_FOOTER_SYNTHETIC: &str =
    include_str!("fixtures/claude-code/100x30-unicode-approval-without-footer-synthetic.txt");
const FIXTURE_100X30_UNKNOWN_DIALOG_SYNTHETIC: &str =
    include_str!("fixtures/claude-code/100x30-unknown-dialog-synthetic.txt");

/// The 21 real-or-synthetic fixtures with their recorded geometry and expected
/// verdict, per `tests/fixtures/claude-code/README.md`.
const POSITIVE_CASES: &[(&str, &str, u16, u16, WindowStatus)] = &[
    (
        "100x30-unicode-idle-startup",
        FIXTURE_100X30_UNICODE_IDLE_STARTUP,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-unicode-running",
        FIXTURE_100X30_UNICODE_RUNNING,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-unicode-done",
        FIXTURE_100X30_UNICODE_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-unicode-approval",
        FIXTURE_100X30_UNICODE_APPROVAL,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-ascii-idle-startup",
        FIXTURE_100X30_ASCII_IDLE_STARTUP,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-ascii-running",
        FIXTURE_100X30_ASCII_RUNNING,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-ascii-done",
        FIXTURE_100X30_ASCII_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-ascii-approval",
        FIXTURE_100X30_ASCII_APPROVAL,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "80x24-unicode-idle-startup",
        FIXTURE_80X24_UNICODE_IDLE_STARTUP,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-unicode-running",
        FIXTURE_80X24_UNICODE_RUNNING,
        80,
        24,
        WindowStatus::Running,
    ),
    (
        "80x24-unicode-done",
        FIXTURE_80X24_UNICODE_DONE,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-unicode-approval",
        FIXTURE_80X24_UNICODE_APPROVAL,
        80,
        24,
        WindowStatus::WaitingApproval,
    ),
    (
        "80x24-ascii-idle-startup",
        FIXTURE_80X24_ASCII_IDLE_STARTUP,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-ascii-running",
        FIXTURE_80X24_ASCII_RUNNING,
        80,
        24,
        WindowStatus::Running,
    ),
    (
        "80x24-ascii-done",
        FIXTURE_80X24_ASCII_DONE,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-ascii-approval",
        FIXTURE_80X24_ASCII_APPROVAL,
        80,
        24,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-ascii-trust",
        FIXTURE_100X30_ASCII_TRUST,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-unicode-trust",
        FIXTURE_100X30_UNICODE_TRUST,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-unicode-ask-user-question",
        FIXTURE_100X30_UNICODE_ASK_USER_QUESTION,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-unicode-approval-without-footer-synthetic",
        FIXTURE_100X30_UNICODE_APPROVAL_WITHOUT_FOOTER_SYNTHETIC,
        100,
        30,
        WindowStatus::WaitingApproval,
    ),
    (
        "100x30-unknown-dialog-synthetic",
        FIXTURE_100X30_UNKNOWN_DIALOG_SYNTHETIC,
        100,
        30,
        WindowStatus::Unknown,
    ),
];

/// The four real `running` fixtures (two glyph sets, two geometries), isolated from
/// [`POSITIVE_CASES`] because the priority-mutation test below exercises only these.
const RUNNING_CASES: &[(&str, &str, u16, u16)] = &[
    (
        "100x30-unicode-running",
        FIXTURE_100X30_UNICODE_RUNNING,
        100,
        30,
    ),
    (
        "100x30-ascii-running",
        FIXTURE_100X30_ASCII_RUNNING,
        100,
        30,
    ),
    (
        "80x24-unicode-running",
        FIXTURE_80X24_UNICODE_RUNNING,
        80,
        24,
    ),
    ("80x24-ascii-running", FIXTURE_80X24_ASCII_RUNNING, 80, 24),
];

/// The four real `approval` fixtures (two glyph sets, two geometries).
const APPROVAL_CASES: &[(&str, &str, u16, u16)] = &[
    (
        "100x30-unicode-approval",
        FIXTURE_100X30_UNICODE_APPROVAL,
        100,
        30,
    ),
    (
        "100x30-ascii-approval",
        FIXTURE_100X30_ASCII_APPROVAL,
        100,
        30,
    ),
    (
        "80x24-unicode-approval",
        FIXTURE_80X24_UNICODE_APPROVAL,
        80,
        24,
    ),
    ("80x24-ascii-approval", FIXTURE_80X24_ASCII_APPROVAL, 80, 24),
];

/// The two `ascii` `idle-startup`/`done` fixture pairs whose `waiting_input` glyph
/// acceptance is under test, per `tests/fixtures/claude-code/README.md`'s
/// "Glyph-acceptance scope" section.
const ASCII_PROMPT_CASES: &[(&str, &str, u16, u16)] = &[
    (
        "100x30-ascii-idle-startup",
        FIXTURE_100X30_ASCII_IDLE_STARTUP,
        100,
        30,
    ),
    ("100x30-ascii-done", FIXTURE_100X30_ASCII_DONE, 100, 30),
    (
        "80x24-ascii-idle-startup",
        FIXTURE_80X24_ASCII_IDLE_STARTUP,
        80,
        24,
    ),
    ("80x24-ascii-done", FIXTURE_80X24_ASCII_DONE, 80, 24),
];

/// The three newly captured "waiting for a user decision" screens (two directory-trust
/// confirmations, one `AskUserQuestion` dialog): none of them also matches `running` or
/// `waiting_input`, so they are the fixtures the priority-mutation test below can use to
/// show `waiting_approval` does not need to outrank those rules on these particular
/// screens.
const SELECTION_DIALOG_CASES: &[(&str, &str, u16, u16)] = &[
    ("100x30-ascii-trust", FIXTURE_100X30_ASCII_TRUST, 100, 30),
    (
        "100x30-unicode-trust",
        FIXTURE_100X30_UNICODE_TRUST,
        100,
        30,
    ),
    (
        "100x30-unicode-ask-user-question",
        FIXTURE_100X30_UNICODE_ASK_USER_QUESTION,
        100,
        30,
    ),
];

/// The `waiting_approval` positive lines: the approval-prompt footers Claude Code
/// draws, used verbatim by the string-table test below.
const WAITING_APPROVAL_POSITIVE_LINES: &[&str] = &[
    "Enter to confirm · Esc to cancel",
    "Enter to select · ↑/↓ to navigate · Esc to cancel",
    "Esc to cancel · Tab to amend",
    "Do you want to proceed?",
    "Press Esc to cancel",
    "then drafting a proposal — this can take a moment (Esc to cancel)",
    "↑/↓ to navigate · Enter to select · ←/→ to switch · Esc to cancel",
];

/// The `waiting_approval` negative lines from the same string table: screens that wait
/// for the user but must not be classified as `waiting_approval` (they either stay
/// `waiting_input`/`running` because the composer is still visible, or fall back to
/// `Unknown` because no rule covers them).
const WAITING_APPROVAL_NEGATIVE_LINES: &[&str] = &[
    "Login successful. Press Enter to continue…",
    "Usage limit has reset · press enter to continue",
    "Press Ctrl-C again to exit",
    "↑/↓ to navigate · enter to resume as a background session · esc to close",
    "⏸ manual mode on · esc to interrupt · ← for agents",
    "? for shortcuts",
    "Scroll to feel it · ←/→ adjust · r reset to auto · Enter save · Esc cancel",
];

/// A self-authored, `cargo build`-shaped screen unrelated to any Claude Code rule, used
/// to verify the fail-safe fallback to `Unknown`.
const UNRELATED_SCREEN: &[&str] = &[
    "   Compiling yagura-daemon v0.1.0",
    "   Compiling yagura-protocol v0.1.0",
    "    Finished dev [unoptimized + debuginfo] target(s) in 4.21s",
    "     Running `target/debug/yagura-daemon`",
    "2026-09-04T00:00:00Z INFO yagura_daemon: daemon listening",
];

/// Reads the real `detection/claude-code.toml` source (not the parsed `Definition`):
/// every test here needs the raw TOML text, either to feed straight to
/// `common::new_engine_sized` or as the input to `common::detection_rules::mutate_rules`.
fn load_claude_code_source() -> String {
    common::repo_definition_text("claude-code.toml")
}

#[tokio::test]
async fn claude_code_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = load_claude_code_source();

    let mut mismatches = Vec::new();
    for (name, fixture, cols, rows, expected) in POSITIVE_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&source, *cols, *rows, &lines).await;
        if status != *expected {
            mismatches.push(format!("{name}: expected {expected:?}, got {status:?}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "fixtures misclassified:\n{}",
        mismatches.join("\n")
    );
}

#[tokio::test]
async fn each_fixture_reproduces_itself_through_the_engines_16_line_window() {
    for (name, fixture, cols, rows, _expected) in POSITIVE_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let mut screen = VtScreen::new(*cols, *rows);
        screen.feed(&common::repaint_lines(&lines));

        let observed = screen.bottom_lines(16);
        let expected: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
        assert_eq!(
            observed, expected,
            "fixture {name} must reproduce itself through VtScreen::bottom_lines(16): no \
             wrapping or stray control bytes may have crept in"
        );
    }
}

#[tokio::test]
async fn claude_code_definition_leaves_an_unrelated_screen_unknown() {
    tokio::time::pause();
    let source = load_claude_code_source();

    let status = judge(&source, 80, 5, UNRELATED_SCREEN).await;
    assert_eq!(status, WindowStatus::Unknown);
}

// --- mutation tests ---------------------------------------------------------
//
// Each test mutates a fresh `toml::Value` parse of the real definition and checks the
// resulting behaviour, so a passing suite pins that the *real* file's rule shape (not
// just its literal patterns) is what the assertions above depend on.

#[tokio::test]
async fn empty_rules_leaves_every_fixture_unknown() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = mutate_rules(&source, |rules| rules.clear());

    for (name, fixture, cols, rows, _expected) in POSITIVE_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::Unknown,
            "with an empty rules table, fixture {name} must fall back to Unknown"
        );
    }
}

#[tokio::test]
async fn lowering_running_priority_below_waiting_input_flips_real_running_screens_to_waiting_input()
{
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = mutate_rules(&source, |rules| {
        for rule in rules.iter_mut() {
            if rule_status(rule) == Some("running") {
                set_rule_priority(rule, 5);
            }
        }
    });

    for (name, fixture, cols, rows) in RUNNING_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::WaitingInput,
            "with running priority lowered below waiting_input, fixture {name} must lose to \
             the raw prompt line's waiting_input match"
        );
    }
}

#[tokio::test]
async fn breaking_the_waiting_approval_pattern_leaves_real_approvals_unknown() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = with_status_pattern(
        &source,
        WindowStatus::WaitingApproval,
        "ZZZZZ_UNMATCHABLE_ZZZZZ",
    );

    for (name, fixture, cols, rows) in APPROVAL_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::Unknown,
            "with waiting_approval's pattern broken, fixture {name} matches no other rule \
             and must fall back to Unknown"
        );
    }
}

#[tokio::test]
async fn restricting_waiting_input_to_the_unicode_glyph_leaves_ascii_prompts_unknown() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = with_status_pattern(&source, WindowStatus::WaitingInput, "^\u{276F}\u{A0}");

    for (name, fixture, cols, rows) in ASCII_PROMPT_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::Unknown,
            "with waiting_input restricted to the unicode prompt glyph, ascii fixture {name} \
             must fall back to Unknown"
        );
    }
}

#[tokio::test]
async fn wildcarding_every_pattern_stops_the_unrelated_screen_from_staying_unknown() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = with_every_pattern(&source, ".");

    let status = judge(&mutated, 80, 5, UNRELATED_SCREEN).await;

    assert_ne!(
        status,
        WindowStatus::Unknown,
        "with every rule's pattern widened to match any line, the unrelated screen must no \
         longer fall back to Unknown"
    );
}

// --- selection-dialog tests --------------------------------------------------
//
// A screen that only waits for the user to pick an option or confirm a dialog (a
// directory-trust confirmation, an `AskUserQuestion` prompt) carries no composer or
// spinner evidence of its own, so it must be reached through the dialog's own footer
// text rather than through a fallback time-based judgement.

/// Exceeds every bundled definition's own `[quiescence] after_ms` (`generic-shell.toml`
/// caps it at 2000ms) with headroom for clock rounding, so a quiescence-style idle
/// fallback would already have raised its raw judgement by the end of this stage.
const UNKNOWN_DIALOG_FIRST_STAGE_MS: u64 = 2100;

/// Exceeds `claude-code.toml`'s own `[debounce] confirm_ms` (300ms): the wait the
/// engine's debounce needs, after a raw judgement changes, before it confirms.
const UNKNOWN_DIALOG_SECOND_STAGE_MS: u64 = 350;

#[tokio::test]
async fn unknown_dialog_synthetic_fixture_stays_unknown_after_a_two_stage_wait() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let lines: Vec<&str> = FIXTURE_100X30_UNKNOWN_DIALOG_SYNTHETIC.lines().collect();

    let status = judge_after_two_stage_tick(
        &source,
        100,
        30,
        &lines,
        UNKNOWN_DIALOG_FIRST_STAGE_MS,
        UNKNOWN_DIALOG_SECOND_STAGE_MS,
    )
    .await;

    assert_eq!(
        status,
        WindowStatus::Unknown,
        "a screen matching none of the definition's rules must stay Unknown, not fall back \
         to a held or time-derived status, no matter how long it goes unchanged"
    );
}

#[tokio::test]
async fn claude_code_definition_has_no_quiescence_judgement() {
    let source = load_claude_code_source();
    let definition = parse_definition(&source).expect("the bundled definition must parse");

    assert!(
        definition.quiescence.is_none(),
        "claude-code.toml must not fall back to a status after a period of no output: \
         every screen it needs to classify is reachable through a rule"
    );
}

#[tokio::test]
async fn lowering_waiting_approval_priority_below_waiting_input_leaves_the_selection_dialogs_unaffected()
 {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = mutate_rules(&source, |rules| {
        for rule in rules.iter_mut() {
            if rule_status(rule) == Some("waiting_approval") {
                set_rule_priority(rule, 5);
            }
        }
    });

    for (name, fixture, cols, rows) in SELECTION_DIALOG_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::WaitingApproval,
            "fixture {name} matches no other rule, so lowering waiting_approval's own \
             priority below waiting_input's must not change its verdict"
        );
    }
}

#[tokio::test]
async fn waiting_approval_pattern_matches_every_positive_line_and_no_negative_line() {
    let source = load_claude_code_source();
    let rule = rule_for_status(&source, WindowStatus::WaitingApproval);

    for line in WAITING_APPROVAL_POSITIVE_LINES {
        assert!(
            rule.pattern.is_match(line),
            "waiting_approval's pattern must match {line:?}"
        );
    }
    for line in WAITING_APPROVAL_NEGATIVE_LINES {
        assert!(
            !rule.pattern.is_match(line),
            "waiting_approval's pattern must not match {line:?}"
        );
    }
}

#[tokio::test]
async fn dropping_esc_to_cancel_from_waiting_approval_leaves_only_the_esc_anchored_cases_unmatched()
{
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = with_status_pattern(&source, WindowStatus::WaitingApproval, "(?i)Do you want to");

    for (name, fixture, cols, rows) in SELECTION_DIALOG_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&mutated, *cols, *rows, &lines).await;
        assert_eq!(
            status,
            WindowStatus::Unknown,
            "fixture {name} only matches waiting_approval through Esc to cancel, so \
             dropping that alternative must leave it unmatched"
        );
    }

    let synthetic_lines: Vec<&str> = FIXTURE_100X30_UNICODE_APPROVAL_WITHOUT_FOOTER_SYNTHETIC
        .lines()
        .collect();
    assert_eq!(
        judge(&mutated, 100, 30, &synthetic_lines).await,
        WindowStatus::WaitingApproval,
        "the footerless approval fixture still carries \"Do you want to proceed?\", so it \
         must stay WaitingApproval"
    );

    for (name, fixture, cols, rows) in APPROVAL_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(
            judge(&mutated, *cols, *rows, &lines).await,
            WindowStatus::WaitingApproval,
            "real approval fixture {name} also carries \"Do you want to proceed?\", so it \
             must stay WaitingApproval"
        );
    }

    let rule = rule_for_status(&mutated, WindowStatus::WaitingApproval);
    for line in WAITING_APPROVAL_POSITIVE_LINES {
        let still_matches = rule.pattern.is_match(line);
        let carries_do_you_want_to = line.contains("Do you want to");
        assert_eq!(
            still_matches, carries_do_you_want_to,
            "with Esc to cancel dropped, {line:?} must match only if it carries \"Do you \
             want to\""
        );
    }
}

#[tokio::test]
async fn dropping_do_you_want_to_from_waiting_approval_leaves_only_that_anchored_case_unmatched() {
    tokio::time::pause();
    let source = load_claude_code_source();
    let mutated = with_status_pattern(&source, WindowStatus::WaitingApproval, "(?i)Esc to cancel");

    let synthetic_lines: Vec<&str> = FIXTURE_100X30_UNICODE_APPROVAL_WITHOUT_FOOTER_SYNTHETIC
        .lines()
        .collect();
    assert_eq!(
        judge(&mutated, 100, 30, &synthetic_lines).await,
        WindowStatus::Unknown,
        "the footerless approval fixture only matched waiting_approval through \"Do you \
         want to proceed?\", so dropping that alternative must leave it unmatched"
    );

    for (name, fixture, cols, rows) in SELECTION_DIALOG_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(
            judge(&mutated, *cols, *rows, &lines).await,
            WindowStatus::WaitingApproval,
            "fixture {name} carries Esc to cancel, so it must stay WaitingApproval"
        );
    }

    for (name, fixture, cols, rows) in APPROVAL_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(
            judge(&mutated, *cols, *rows, &lines).await,
            WindowStatus::WaitingApproval,
            "real approval fixture {name} also carries Esc to cancel, so it must stay \
             WaitingApproval"
        );
    }

    let rule = rule_for_status(&mutated, WindowStatus::WaitingApproval);
    for line in WAITING_APPROVAL_POSITIVE_LINES {
        let still_matches = rule.pattern.is_match(line);
        let carries_esc_to_cancel = line.contains("Esc to cancel");
        assert_eq!(
            still_matches, carries_esc_to_cancel,
            "with Do you want to dropped, {line:?} must match only if it carries \"Esc to \
             cancel\""
        );
    }
}
