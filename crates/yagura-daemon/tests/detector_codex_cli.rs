#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Judges `detection/codex-cli.toml` against real Codex CLI screen captures (0.152.1 for
//! the command-approval and composer screens, 0.154.0 for the directory-trust screen).
//! See `tests/fixtures/codex-cli/README.md` for fixture provenance, the
//! `VtScreen::bottom_lines(16)` extraction rule, why a dialog is recognised by the shape
//! of its footer, and why a numbered option row is not read as the composer.

mod common;

use common::detection_rules::{
    judge, judge_after_two_stage_tick, mutate_rules, rule_for_status, rule_priority_for_status,
    rule_status, set_rule_priority, with_every_pattern, with_status_pattern,
};
use yagura_daemon::detector::definition::Rule;
use yagura_protocol::WindowStatus;

const FIXTURE_100X30_IDLE_STARTUP: &str =
    include_str!("fixtures/codex-cli/100x30-idle-startup.txt");
const FIXTURE_100X30_RUNNING: &str = include_str!("fixtures/codex-cli/100x30-running.txt");
const FIXTURE_100X30_RUNNING_BACKGROUND: &str =
    include_str!("fixtures/codex-cli/100x30-running-background.txt");
const FIXTURE_100X30_APPROVAL: &str = include_str!("fixtures/codex-cli/100x30-approval.txt");
const FIXTURE_100X30_DONE: &str = include_str!("fixtures/codex-cli/100x30-done.txt");
const FIXTURE_100X30_FULLAUTO_RUNNING: &str =
    include_str!("fixtures/codex-cli/100x30-fullauto-running.txt");
const FIXTURE_100X30_FULLAUTO_DONE: &str =
    include_str!("fixtures/codex-cli/100x30-fullauto-done.txt");
const FIXTURE_80X24_IDLE_STARTUP: &str = include_str!("fixtures/codex-cli/80x24-idle-startup.txt");
const FIXTURE_80X24_RUNNING: &str = include_str!("fixtures/codex-cli/80x24-running.txt");
const FIXTURE_80X24_APPROVAL: &str = include_str!("fixtures/codex-cli/80x24-approval.txt");
const FIXTURE_80X24_DONE: &str = include_str!("fixtures/codex-cli/80x24-done.txt");
const FIXTURE_100X30_APPROVAL_WITH_WORKING_SYNTHETIC: &str =
    include_str!("fixtures/codex-cli/100x30-approval-with-working-synthetic.txt");
const FIXTURE_100X30_TRUST: &str = include_str!("fixtures/codex-cli/100x30-trust.txt");
const FIXTURE_100X30_NUMBERED_DIALOG_UNKNOWN_SYNTHETIC: &str =
    include_str!("fixtures/codex-cli/100x30-numbered-dialog-unknown-synthetic.txt");

/// The 14 real/synthetic fixtures with their recorded geometry and expected verdict, per
/// `tests/fixtures/codex-cli/README.md`.
const POSITIVE_CASES: &[(&str, &str, u16, u16, WindowStatus)] = &[
    (
        "100x30-idle-startup",
        FIXTURE_100X30_IDLE_STARTUP,
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
        "100x30-running-background",
        FIXTURE_100X30_RUNNING_BACKGROUND,
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
        "100x30-done",
        FIXTURE_100X30_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-fullauto-running",
        FIXTURE_100X30_FULLAUTO_RUNNING,
        100,
        30,
        WindowStatus::Running,
    ),
    (
        "100x30-fullauto-done",
        FIXTURE_100X30_FULLAUTO_DONE,
        100,
        30,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-idle-startup",
        FIXTURE_80X24_IDLE_STARTUP,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "80x24-running",
        FIXTURE_80X24_RUNNING,
        80,
        24,
        WindowStatus::Running,
    ),
    (
        "80x24-approval",
        FIXTURE_80X24_APPROVAL,
        80,
        24,
        WindowStatus::WaitingApproval,
    ),
    (
        "80x24-done",
        FIXTURE_80X24_DONE,
        80,
        24,
        WindowStatus::WaitingInput,
    ),
    (
        "100x30-approval-with-working-synthetic",
        FIXTURE_100X30_APPROVAL_WITH_WORKING_SYNTHETIC,
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
        "100x30-numbered-dialog-unknown-synthetic",
        FIXTURE_100X30_NUMBERED_DIALOG_UNKNOWN_SYNTHETIC,
        100,
        30,
        WindowStatus::Unknown,
    ),
];

/// The two captured command-approval screens (two geometries), isolated from
/// [`POSITIVE_CASES`] because the mutation tests below exercise only these. The synthetic
/// sibling that also carries a `• Working (...)` line is deliberately excluded: its
/// verdict under a demoted approval rule is decided by the `running` rule instead.
const REAL_APPROVAL_CASES: &[(&str, &str, u16, u16)] = &[
    ("100x30-approval", FIXTURE_100X30_APPROVAL, 100, 30),
    ("80x24-approval", FIXTURE_80X24_APPROVAL, 80, 24),
];

/// Footer lines that must be recognised as a dialog waiting for the user's decision.
/// Every entry is the verbatim footer of a real Codex CLI screen.
const WAITING_APPROVAL_POSITIVE_LINES: &[&str] = &[
    "Press enter to confirm or esc to go back",
    "Press enter to confirm or esc to cancel",
    "Press enter to continue",
    "Press enter to continue or esc to cancel",
    "Press enter to submit or esc to cancel",
    "Press enter to save",
    "Press space to select or enter to save",
    "Press space or enter to toggle; esc to close",
    "Press space to toggle; enter to save or select",
    "Press a number to choose · esc to dismiss · type to continue",
    "tab to add notes | enter to submit answer | esc to interrupt",
    "enter to submit | esc to cancel",
    "Use ↑/↓ to move, press enter to confirm",
    "Use ↑/↓ to move, enter to select, c to customize",
    "Press Enter to select • Ctrl+C to exit",
];

/// Lines that must not be recognised as a dialog footer: status lines, hint lines, the
/// composer itself, and the footers of screens that keep the composer or stay Unknown.
const WAITING_APPROVAL_NEGATIVE_LINES: &[&str] = &[
    "? for shortcuts",
    "• Working (0s • esc to interrupt)",
    "Use tab / ↑ ↓ to move, enter to select, esc to close",
    "Press the new key now. Esc cancels.",
    "Press any key to see what Codex receives.",
    " ↑/↓ to scroll   pgup/pgdn to page   home/end to jump",
    "› Ask Codex to do anything",
    "Press esc to cancel",
    "enter to resume     esc to start new     ctrl + c to quit     tab to toggle sort",
    "enter create task    ctrl+j newline    esc tasks",
];

/// Indentation a dialog footer is drawn with when its view insets the line; the
/// `waiting_approval` rule has to tolerate it.
const FOOTER_INDENT: &str = "  ";

/// Caret lines that are the composer itself, so a window showing one accepts input.
const WAITING_INPUT_COMPOSER_LINES: &[&str] = &[
    "› Ask Codex to do anything",
    "› Run the shell command `curl -sI https://example.com`",
    "› ",
    "› 1.5 is a number",
    "› 2024 was",
];

/// Caret lines that are a numbered option row of a dialog, not the composer: a window
/// showing one is waiting for a choice and must not be treated as accepting input.
const WAITING_INPUT_OPTION_ROW_LINES: &[&str] = &["› 1. Yes, continue", "› 12. Option", "› 1."];

/// The three independent footer shapes the `waiting_approval` rule is built from, each
/// justified by a different set of real screens. Keyed by a name used only to say which
/// one a mutation dropped.
const APPROVAL_FOOTER_ALTERNATIVES: &[(&str, &str)] = &[
    ("press-key", r"^\s{0,4}Press (enter|space|a number)\b"),
    ("enter-to-submit", r"\benter to submit\b"),
    ("use-arrows", r"^\s{0,4}Use ↑/↓ to move,"),
];

/// The alternative the directory-trust fixture's own footer ("Press enter to continue")
/// needs: dropping it is what takes that fixture off the `waiting_approval` rule.
const TRUST_FIXTURE_ALTERNATIVE: &str = "press-key";

/// A self-authored, `cargo build`-shaped screen unrelated to any Codex CLI rule, used to
/// verify the fail-safe fallback to `Unknown`.
const UNRELATED_SCREEN: &[&str] = &[
    "   Compiling yagura-daemon v0.1.0",
    "   Compiling yagura-protocol v0.1.0",
    "    Finished dev [unoptimized + debuginfo] target(s) in 4.21s",
    "     Running `target/debug/yagura-daemon`",
    "2026-09-03T00:00:00Z INFO yagura_daemon: daemon listening",
];

/// Reads the real `detection/codex-cli.toml` source (not the parsed `Definition`): every
/// test here needs the raw TOML text, either to feed straight to
/// `common::new_engine_sized` or as the input to [`mutate_rules`].
fn load_codex_cli_source() -> String {
    common::repo_definition_text("codex-cli.toml")
}

/// Rebuilds the `waiting_approval` pattern from [`APPROVAL_FOOTER_ALTERNATIVES`],
/// leaving out the alternative named `dropped`.
fn approval_pattern_without(dropped: &str) -> String {
    let alternatives: Vec<&str> = APPROVAL_FOOTER_ALTERNATIVES
        .iter()
        .filter(|(name, _)| *name != dropped)
        .map(|(_, pattern)| *pattern)
        .collect();
    format!("(?i)({})", alternatives.join("|"))
}

#[tokio::test]
async fn codex_cli_definition_classifies_captured_screens() {
    tokio::time::pause();
    let source = load_codex_cli_source();

    for (name, fixture, cols, rows, expected) in POSITIVE_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        let status = judge(&source, *cols, *rows, &lines).await;
        assert_eq!(
            status, *expected,
            "fixture {name} must classify as {expected:?}"
        );
    }
}

#[tokio::test]
async fn codex_cli_definition_leaves_an_unrelated_screen_unknown() {
    tokio::time::pause();
    let source = load_codex_cli_source();

    let status = judge(&source, 80, 5, UNRELATED_SCREEN).await;
    assert_eq!(status, WindowStatus::Unknown);
}

// --- selection-dialog tests --------------------------------------------------
//
// A dialog that only waits for the user to pick a numbered option carries no composer
// and no spinner of its own, so it has to be reached through the shape of its footer --
// and a dialog whose footer is not covered has to stay Unknown rather than be mistaken
// for a composer because its highlighted option row starts with the same caret.

/// Exceeds every bundled definition's own `[quiescence] after_ms` (`generic-shell.toml`
/// caps it at 2000ms) with headroom for clock rounding, so a time-derived fallback would
/// already have raised its raw judgement by the end of this stage.
const NUMBERED_DIALOG_FIRST_STAGE_MS: u64 = 2100;

/// Exceeds `codex-cli.toml`'s own `[debounce] confirm_ms` (300ms): the wait the engine's
/// debounce needs, after a raw judgement changes, before it confirms.
const NUMBERED_DIALOG_SECOND_STAGE_MS: u64 = 350;

#[tokio::test]
async fn numbered_dialog_synthetic_fixture_stays_unknown_after_a_two_stage_wait() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let lines: Vec<&str> = FIXTURE_100X30_NUMBERED_DIALOG_UNKNOWN_SYNTHETIC
        .lines()
        .collect();

    let status = judge_after_two_stage_tick(
        &source,
        100,
        30,
        &lines,
        NUMBERED_DIALOG_FIRST_STAGE_MS,
        NUMBERED_DIALOG_SECOND_STAGE_MS,
    )
    .await;

    assert_eq!(
        status,
        WindowStatus::Unknown,
        "a dialog whose footer matches no rule must stay Unknown however long it goes \
         unchanged, even though its highlighted option row starts with the composer caret"
    );
}

#[tokio::test]
async fn a_dialog_reads_as_waiting_input_only_once_both_defences_are_removed() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let bare_caret = with_status_pattern(&source, WindowStatus::WaitingInput, "^› ");
    let numbered_lines: Vec<&str> = FIXTURE_100X30_NUMBERED_DIALOG_UNKNOWN_SYNTHETIC
        .lines()
        .collect();
    let trust_lines: Vec<&str> = FIXTURE_100X30_TRUST.lines().collect();

    assert_eq!(
        judge(&bare_caret, 100, 30, &numbered_lines).await,
        WindowStatus::WaitingInput,
        "with waiting_input widened back to a bare caret, the uncovered dialog's option \
         row is read as the composer"
    );
    assert_eq!(
        judge(&bare_caret, 100, 30, &trust_lines).await,
        WindowStatus::WaitingApproval,
        "the trust dialog's footer is covered, so widening waiting_input alone must not \
         change its verdict"
    );

    let both_defences_removed = mutate_rules(&bare_caret, |rules| {
        let input_priority = rule_priority_for_status(rules, "waiting_input")
            .expect("codex-cli.toml must declare a waiting_input rule");
        for rule in rules.iter_mut() {
            if rule_status(rule) == Some("waiting_approval") {
                set_rule_priority(rule, input_priority - 1);
            }
        }
    });

    assert_eq!(
        judge(&both_defences_removed, 100, 30, &trust_lines).await,
        WindowStatus::WaitingInput,
        "only once the footer rule is outranked as well does the trust dialog's option \
         row decide its verdict"
    );
    for (name, fixture, cols, rows) in REAL_APPROVAL_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(
            judge(&both_defences_removed, *cols, *rows, &lines).await,
            WindowStatus::WaitingInput,
            "captured approval {name} flips the same way once both defences are removed"
        );
    }
}

#[tokio::test]
async fn demoting_or_breaking_the_approval_rule_never_lets_an_option_row_accept_input() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let demoted = mutate_rules(&source, |rules| {
        for rule in rules.iter_mut() {
            if rule_status(rule) == Some("waiting_approval") {
                set_rule_priority(rule, 5);
            }
        }
    });
    let broken = with_status_pattern(
        &source,
        WindowStatus::WaitingApproval,
        "ZZZZZ_UNMATCHABLE_ZZZZZ",
    );

    for (name, fixture, cols, rows) in REAL_APPROVAL_CASES {
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(
            judge(&demoted, *cols, *rows, &lines).await,
            WindowStatus::WaitingApproval,
            "with waiting_approval demoted below waiting_input, captured approval {name} \
             has no other matching rule to lose to: its numbered option row is not the \
             composer"
        );
        assert_eq!(
            judge(&broken, *cols, *rows, &lines).await,
            WindowStatus::Unknown,
            "with waiting_approval's pattern broken, captured approval {name} matches no \
             rule at all and must fall back to Unknown rather than accept input"
        );
    }

    let trust_lines: Vec<&str> = FIXTURE_100X30_TRUST.lines().collect();
    assert_eq!(
        judge(&broken, 100, 30, &trust_lines).await,
        WindowStatus::Unknown,
        "the trust dialog falls back to Unknown the same way"
    );
}

#[tokio::test]
async fn waiting_approval_pattern_matches_every_footer_line_and_no_negative_line() {
    let source = load_codex_cli_source();
    let rule = rule_for_status(&source, WindowStatus::WaitingApproval);

    for line in WAITING_APPROVAL_POSITIVE_LINES {
        assert!(
            rule.pattern.is_match(line),
            "waiting_approval's pattern must match {line:?}"
        );
        let indented = format!("{FOOTER_INDENT}{line}");
        assert!(
            rule.pattern.is_match(&indented),
            "waiting_approval's pattern must match {indented:?} as well: a view can inset \
             its footer"
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
async fn waiting_input_pattern_accepts_composer_lines_but_not_numbered_option_rows() {
    let source = load_codex_cli_source();
    let rule = rule_for_status(&source, WindowStatus::WaitingInput);

    for line in WAITING_INPUT_COMPOSER_LINES {
        assert!(
            rule.pattern.is_match(line),
            "waiting_input's pattern must match the composer line {line:?}"
        );
    }
    for line in WAITING_INPUT_OPTION_ROW_LINES {
        assert!(
            !rule.pattern.is_match(line),
            "waiting_input's pattern must not match the option row {line:?}"
        );
    }
}

// --- mutation tests ---------------------------------------------------------
//
// Each test mutates a fresh `toml::Value` parse of the real definition and checks the
// resulting behaviour, so a passing suite pins that the *real* file's rule shape (not
// just its literal patterns) is what the assertions above depend on.

#[tokio::test]
async fn dropping_one_waiting_approval_alternative_unmatches_only_that_alternatives_lines() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let per_alternative: Vec<(&str, Rule)> = APPROVAL_FOOTER_ALTERNATIVES
        .iter()
        .map(|(name, pattern)| {
            let only_this = with_status_pattern(
                &source,
                WindowStatus::WaitingApproval,
                &format!("(?i)({pattern})"),
            );
            (
                *name,
                rule_for_status(&only_this, WindowStatus::WaitingApproval),
            )
        })
        .collect();
    let real_rule = rule_for_status(&source, WindowStatus::WaitingApproval);
    for line in WAITING_APPROVAL_POSITIVE_LINES
        .iter()
        .chain(WAITING_APPROVAL_NEGATIVE_LINES)
    {
        let covered = per_alternative
            .iter()
            .any(|(_, rule)| rule.pattern.is_match(line));
        assert_eq!(
            real_rule.pattern.is_match(line),
            covered,
            "the real waiting_approval rule must cover exactly the union of its \
             alternatives, so dropping one below measures that alternative alone: {line:?}"
        );
    }

    let trust_lines: Vec<&str> = FIXTURE_100X30_TRUST.lines().collect();
    for (dropped, _) in APPROVAL_FOOTER_ALTERNATIVES {
        let source_without = with_status_pattern(
            &source,
            WindowStatus::WaitingApproval,
            &approval_pattern_without(dropped),
        );
        let rule_without = rule_for_status(&source_without, WindowStatus::WaitingApproval);

        for line in WAITING_APPROVAL_POSITIVE_LINES {
            let covered_elsewhere = per_alternative
                .iter()
                .any(|(name, rule)| name != dropped && rule.pattern.is_match(line));
            assert_eq!(
                rule_without.pattern.is_match(line),
                covered_elsewhere,
                "with the {dropped} alternative dropped, {line:?} must match only if \
                 another alternative covers it"
            );
        }

        let status = judge(&source_without, 100, 30, &trust_lines).await;
        if *dropped == TRUST_FIXTURE_ALTERNATIVE {
            assert_ne!(
                status,
                WindowStatus::WaitingApproval,
                "the trust dialog's footer is covered by the {dropped} alternative alone, \
                 so dropping it must take that fixture off the rule"
            );
        } else {
            assert_eq!(
                status,
                WindowStatus::WaitingApproval,
                "the trust dialog's footer does not need the {dropped} alternative, so \
                 dropping it must leave that fixture's verdict alone"
            );
        }
    }
}

#[tokio::test]
async fn empty_rules_leaves_every_fixture_unknown() {
    tokio::time::pause();
    let source = load_codex_cli_source();
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
async fn swapping_approval_and_running_priority_flips_the_synthetic_fixture_to_running() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let mutated = mutate_rules(&source, |rules| {
        let approval_priority = rule_priority_for_status(rules, "waiting_approval")
            .expect("codex-cli.toml must declare a waiting_approval rule");
        let running_priority = rule_priority_for_status(rules, "running")
            .expect("codex-cli.toml must declare a running rule");
        for rule in rules.iter_mut() {
            match rule_status(rule) {
                Some("waiting_approval") => set_rule_priority(rule, running_priority),
                Some("running") => set_rule_priority(rule, approval_priority),
                _ => {}
            }
        }
    });
    let lines: Vec<&str> = FIXTURE_100X30_APPROVAL_WITH_WORKING_SYNTHETIC
        .lines()
        .collect();

    let status = judge(&mutated, 100, 30, &lines).await;

    assert_eq!(
        status,
        WindowStatus::Running,
        "swapping waiting_approval and running priority must let the running rule win on \
         the synthetic fixture, which carries both anchors in its bottom 16 lines"
    );
}

#[tokio::test]
async fn wildcarding_every_pattern_stops_the_unrelated_screen_from_staying_unknown() {
    tokio::time::pause();
    let source = load_codex_cli_source();
    let mutated = with_every_pattern(&source, ".");

    let status = judge(&mutated, 80, 5, UNRELATED_SCREEN).await;

    assert_ne!(
        status,
        WindowStatus::Unknown,
        "with every rule's pattern widened to match any line, the unrelated screen must no \
         longer fall back to Unknown"
    );
}
