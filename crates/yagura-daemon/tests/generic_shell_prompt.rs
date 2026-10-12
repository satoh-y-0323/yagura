#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Coverage for the bundled `generic-shell.toml` definition's status rules,
//! beyond what `detector_definition.rs` and `detector_tall_screen.rs` already
//! measure: the macOS default zsh prompt settles into `idle` by rule while a
//! `%` glued to the text before it is left to the quiescence fallback, and a
//! `%` figure glued to progress output is never misread as a prompt.
//!
//! `generic-shell.toml` declares neither `[composer]` nor `[input]`: a plain
//! shell's screen carries no way to tell its own prompt apart from a
//! program's own output, so the daemon reads none of it back and writes
//! nothing into it (`detector_definition.rs`). What `[composer]` reading
//! looks like in general, on synthetic definitions, lives in
//! `composer_readout.rs`.

mod common;

use yagura_protocol::WindowStatus;

use common::detection_rules::{judge, judge_after_two_stage_tick, with_status_pattern};

/// Wide enough for every prompt line used below, narrow enough that nothing
/// wraps unexpectedly.
const COLS: u16 = 60;

/// Past the bundled definition's 2000ms quiescence, split so the flip to
/// quiescence-idle still gets its own confirm window afterwards.
const PAST_QUIESCENCE_MS: u64 = 2100;
const PAST_CONFIRM_MS: u64 = 400;

fn generic_shell_source() -> String {
    common::repo_definition_text("generic-shell.toml")
}

// =====================================================================
// Idle by rule: the macOS default zsh prompt settles into `idle` within a
// single debounce-sized tick, well short of the 2000ms quiescence
// fallback this definition also opts into. A `%` glued to the text before
// it is left to that fallback.
// =====================================================================

#[tokio::test]
async fn a_lone_macos_zsh_prompt_is_judged_idle_by_rule_before_quiescence_would_apply() {
    tokio::time::pause();
    let source = generic_shell_source();
    let lines = ["m1@host ~ % "];

    let status: WindowStatus = judge(&source, COLS, lines.len() as u16, &lines).await;

    assert_eq!(
        status,
        WindowStatus::Idle,
        "the idle rule, not the 2000ms quiescence fallback, must classify a lone prompt \
         line within a single debounce-sized tick"
    );
}

#[tokio::test]
async fn a_zsh_builtin_default_prompt_is_left_to_quiescence_not_the_idle_rule() {
    tokio::time::pause();
    let source = generic_shell_source();
    let lines = ["host% "];

    let by_rule: WindowStatus = judge(&source, COLS, lines.len() as u16, &lines).await;
    assert_ne!(
        by_rule,
        WindowStatus::Idle,
        "a `%` glued to the host name is shaped like `45%`, so the idle rule must not \
         match it within a single debounce-sized tick"
    );

    let by_quiescence: WindowStatus = judge_after_two_stage_tick(
        &source,
        COLS,
        lines.len() as u16,
        &lines,
        PAST_QUIESCENCE_MS,
        PAST_CONFIRM_MS,
    )
    .await;
    assert_eq!(
        by_quiescence,
        WindowStatus::Idle,
        "the same prompt must still settle into idle once output has gone quiet"
    );
}

// =====================================================================
// Not misreading a numeric percentage as the zsh prompt: the bundled
// definition, against a control built from a careless pattern.
// =====================================================================

const CARELESS_PERCENT_IDLE_PATTERN: &str = r"[$#>%]\s*$";

#[tokio::test]
async fn a_percent_idle_rule_that_ignores_the_character_before_it_misreads_progress_output() {
    tokio::time::pause();
    let careless = with_status_pattern(
        &generic_shell_source(),
        WindowStatus::Idle,
        CARELESS_PERCENT_IDLE_PATTERN,
    );

    for progress_line in ["Downloading 45%", "[#####] 100%"] {
        let lines = [progress_line];
        let status: WindowStatus = judge(&careless, COLS, lines.len() as u16, &lines).await;

        assert_eq!(
            status,
            WindowStatus::Idle,
            "a `%` allowed after any character matches a progress percentage, which is \
             exactly the misread a whitespace-guarded `%` avoids: got {status:?} for \
             {progress_line:?}"
        );
    }
}

#[tokio::test]
async fn the_bundled_idle_rule_ignores_progress_output_until_a_prompt_follows() {
    tokio::time::pause();
    let guarded: String = generic_shell_source();

    for progress_line in ["Downloading 45%", "[#####] 100%"] {
        let progress_only = [progress_line];
        let status: WindowStatus =
            judge(&guarded, COLS, progress_only.len() as u16, &progress_only).await;
        assert_ne!(
            status,
            WindowStatus::Idle,
            "a whitespace-guarded `%` must not match a progress percentage: \
             got Idle for {progress_line:?}"
        );
    }

    let progress_then_prompt = ["Downloading 45%", "m1@host ~ % "];
    let status: WindowStatus = judge(
        &guarded,
        COLS,
        progress_then_prompt.len() as u16,
        &progress_then_prompt,
    )
    .await;
    assert_eq!(
        status,
        WindowStatus::Idle,
        "the same rule must still classify idle once an actual prompt line follows"
    );
}
