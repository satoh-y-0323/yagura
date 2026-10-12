#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `yagura_daemon::input`: the pure submit-plan layer behind delivering one
//! line into a window's input.
//!
//! Pure functions and pure structs only: no PTY, no window, no clock beyond
//! `Duration` arithmetic. The values this module derives from a detection
//! definition's `[input]` table (`InputProfile::from_section`) and the
//! byte-for-byte shape of the write sequence it plans (`plan_submit_line` /
//! `plan_raw_write`) are exercised here in isolation from the I/O layer that
//! executes them.

use std::time::Duration;

use yagura_daemon::detector::definition::InputSection;
use yagura_daemon::input::{
    BRACKETED_PASTE_END, BRACKETED_PASTE_START, InputProfile, SubmitAction, SubmitOutcome,
    SubmitPlan, TakeBack, plan_raw_write, plan_submit_line,
};
use yagura_protocol::WindowStatus;

/// A profile with values distinct from every default, so a test that reads
/// them back cannot be satisfied by accident.
fn profile(
    bracketed_paste: bool,
    submit: &[u8],
    clear: &[u8],
    submit_delay_ms: u64,
) -> InputProfile {
    InputProfile {
        bracketed_paste,
        submit: submit.to_vec(),
        clear: clear.to_vec(),
        ready_hold: Duration::ZERO,
        submit_delay: Duration::from_millis(submit_delay_ms),
        delivery_confirm: Duration::from_millis(1),
        resend_max: 0,
        probe: None,
        erase: b"\x7f".to_vec(),
    }
}

// =====================================================================
// plan_submit_line: step count and ordering
// =====================================================================

#[test]
fn plan_submit_line_without_paste_returns_two_steps_and_the_body_carries_no_terminator() {
    let p = profile(false, b"\r", b"", 40);
    let plan = plan_submit_line(&p, "hello", SubmitAction::Write);

    assert_eq!(plan.steps.len(), 2, "no clear: body then submit key");
    assert_eq!(plan.steps[0].bytes, b"hello");
    assert!(
        !plan.steps[0].bytes.ends_with(b"\r") && !plan.steps[0].bytes.ends_with(b"\n"),
        "the body step must not carry its own terminator"
    );
    assert_eq!(plan.steps[1].bytes, b"\r");
}

#[test]
fn plan_submit_line_with_bracketed_paste_wraps_the_body_in_the_paste_markers() {
    let p = profile(true, b"\r", b"", 40);
    let plan = plan_submit_line(&p, "hello", SubmitAction::Write);

    let mut expected = BRACKETED_PASTE_START.to_vec();
    expected.extend_from_slice(b"hello");
    expected.extend_from_slice(BRACKETED_PASTE_END);
    assert_eq!(plan.steps[0].bytes, expected);
    assert!(plan.steps[0].bytes.starts_with(BRACKETED_PASTE_START));
    assert!(plan.steps[0].bytes.ends_with(BRACKETED_PASTE_END));
}

#[test]
fn plan_submit_line_does_not_wrap_a_body_that_contains_an_escape_byte() {
    let p = profile(true, b"\r", b"", 40);
    for body in ["\u{1b}leading", "trailing\u{1b}", "in\u{1b}the middle"] {
        let plan = plan_submit_line(&p, body, SubmitAction::Write);
        assert_eq!(
            plan.steps[0].bytes,
            body.as_bytes(),
            "a body containing ESC must never be framed, even when bracketed_paste is true: {body:?}"
        );
        assert!(!plan.steps[0].bytes.starts_with(BRACKETED_PASTE_START));
    }
}

#[test]
fn plan_submit_line_rewrite_with_a_nonempty_clear_prepends_a_clear_step() {
    let p = profile(false, b"\r", b"\x15", 40);
    let plan = plan_submit_line(&p, "hi", SubmitAction::Rewrite);

    assert_eq!(
        plan.steps.len(),
        3,
        "rewrite with a clear sequence: clear, body, submit key"
    );
    assert_eq!(plan.steps[0].bytes, b"\x15");
    assert_eq!(plan.steps[1].bytes, b"hi");
    assert_eq!(plan.steps[2].bytes, b"\r");
}

#[test]
fn plan_submit_line_rewrite_with_an_empty_clear_has_the_same_step_count_as_write() {
    let p = profile(false, b"\r", b"", 40);
    let rewritten = plan_submit_line(&p, "hi", SubmitAction::Rewrite);
    let fresh = plan_submit_line(&p, "hi", SubmitAction::Write);

    assert_eq!(
        rewritten.steps.len(),
        fresh.steps.len(),
        "an empty clear sequence must not grow the plan on a rewrite"
    );
    assert_eq!(rewritten.steps.len(), 2);
}

#[test]
fn plan_submit_line_submit_step_bytes_always_equal_the_profiles_submit_field() {
    let p = profile(false, b"\n", b"", 40);
    let plan = plan_submit_line(&p, "x", SubmitAction::Write);
    assert_eq!(
        plan.steps
            .last()
            .expect("a plan always has a submit step")
            .bytes,
        b"\n",
        "the submit step must carry the profile's submit bytes, not a hardcoded terminator"
    );
}

#[test]
fn plan_submit_line_only_the_final_step_has_a_zero_delay_and_the_clear_step_delays_too() {
    let p = profile(false, b"\r", b"\x15", 55);
    let plan = plan_submit_line(&p, "x", SubmitAction::Rewrite);
    assert_eq!(
        plan.steps.len(),
        3,
        "test precondition: rewrite with a clear sequence"
    );

    let last = plan.steps.len() - 1;
    for (i, step) in plan.steps.iter().enumerate() {
        if i == last {
            assert_eq!(
                step.delay_after,
                Duration::ZERO,
                "step {i} (final) must have zero delay"
            );
        } else {
            assert_eq!(
                step.delay_after,
                Duration::from_millis(55),
                "step {i} (non-final, including the clear step) must carry submit_delay"
            );
        }
    }
}

// =====================================================================
// plan_submit_line's action argument: four ways to submit, not two
// `Write` and `Rewrite` are exercised above; `ResendOnly` and `ClearOnly`
// below.
// =====================================================================

#[test]
fn plan_submit_line_resend_only_writes_just_the_submit_key() {
    let p = profile(false, b"\r", b"\x15", 40);
    let plan = plan_submit_line(&p, "hi", SubmitAction::ResendOnly);

    assert_eq!(
        plan.steps.len(),
        1,
        "resending an envelope that is still on screen unmodified writes only the submit key"
    );
    assert_eq!(plan.steps[0].bytes, b"\r");
    assert_eq!(plan.steps[0].delay_after, Duration::ZERO);
}

#[test]
fn plan_submit_line_resend_only_ignores_the_body_argument() {
    let p = profile(false, b"\r", b"\x15", 40);
    let with_one_body = plan_submit_line(&p, "one body", SubmitAction::ResendOnly);
    let with_another_body =
        plan_submit_line(&p, "a completely different body", SubmitAction::ResendOnly);

    assert_eq!(
        with_one_body, with_another_body,
        "a resend-only plan does not depend on what body was asked for: it never writes one"
    );
}

#[test]
fn plan_submit_line_clear_only_writes_just_the_clear_sequence() {
    let p = profile(false, b"\r", b"\x15", 40);
    let plan = plan_submit_line(&p, "hi", SubmitAction::ClearOnly);

    assert_eq!(
        plan.steps.len(),
        1,
        "taking back a write that never reflected writes only the clear sequence"
    );
    assert_eq!(plan.steps[0].bytes, b"\x15");
    assert_eq!(plan.steps[0].delay_after, Duration::ZERO);
}

#[test]
fn plan_submit_line_clear_only_never_writes_the_submit_key() {
    let p = profile(false, b"\r", b"\x15", 40);
    let plan = plan_submit_line(&p, "hi", SubmitAction::ClearOnly);

    assert!(
        plan.steps.iter().all(|step| step.bytes != b"\r"),
        "taking back an unreflected write must not press submit: nothing was confirmed on \
         screen for the receiver to act on"
    );
}

// =====================================================================
// plan_raw_write: unmodified single-step passthrough
// =====================================================================

#[test]
fn plan_raw_write_returns_one_step_with_zero_delay_and_the_exact_input_bytes() {
    let plan = plan_raw_write("a\rb\r");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].bytes, b"a\rb\r");
    assert_eq!(plan.steps[0].delay_after, Duration::ZERO);
}

// =====================================================================
// InputProfile::from_section(None): the documented defaults
// =====================================================================

#[test]
fn input_profile_from_none_matches_the_documented_defaults() {
    let p = InputProfile::from_section(None);

    assert!(!p.bracketed_paste, "default bracketed_paste must be false");
    assert_eq!(p.submit, b"\r", "default submit must be CR");
    assert_eq!(p.clear, Vec::<u8>::new(), "default clear must send nothing");
    assert_eq!(
        p.submit_delay,
        Duration::from_millis(800),
        "default submit_delay_ms must be 800"
    );
    assert_eq!(
        p.delivery_confirm,
        Duration::from_millis(8000),
        "default delivery_confirm_ms must be 8000"
    );
    assert_eq!(
        p.resend_max, 0,
        "default resend_max must be 0 (never resend)"
    );
}

// =====================================================================
// may_resend(): resend_max > 0, and only that. A
// resend that finds its own envelope still on screen writes the submit key
// alone, so a definition with no clear sequence can resend too -- the old
// "and a clear sequence exists" half of the condition is gone.
// =====================================================================

#[test]
fn may_resend_is_true_whenever_resend_max_is_positive_regardless_of_clear() {
    let cases: [(u8, &[u8], bool); 4] = [
        (0, b"", false),
        (0, b"\x15", false),
        (2, b"", true),
        (2, b"\x15", true),
    ];
    for (resend_max, clear, expected) in cases {
        let p = InputProfile {
            resend_max,
            clear: clear.to_vec(),
            ..profile(false, b"\r", b"", 40)
        };
        assert_eq!(
            p.may_resend(),
            expected,
            "resend_max={resend_max}, clear={clear:?}: expected may_resend() = {expected}"
        );
    }
}

// =====================================================================
// Taking back a write that never reflected: which take-back a profile can
// make for a body, the one step `ClearOnly` writes for it, and how long the
// window waits before judging what the take-back left. `TakeBack` carries
// only `Unavailable` and `Fixed`: a non-empty `clear` always wins, an empty
// one always leaves nothing to take back, and neither reading depends on
// the body's length.
// =====================================================================

/// Longer than any construction site in this file needs a "long body" to
/// be: chosen as a plain, deliberately large literal rather than a constant
/// this module used to expose for that purpose, since the property under
/// test ("does not depend on the body's length") must not be satisfiable by
/// accident against a length the profile itself still special-cases.
const LONG_BODY_UNITS: usize = 2_000;

#[test]
fn no_take_back_is_available_when_neither_sequence_is_defined() {
    let p = profile(false, b"\r", b"", 40);

    let take_back: TakeBack = p.take_back();

    assert_eq!(take_back, TakeBack::Unavailable);
}

#[test]
fn clear_only_writes_nothing_when_no_take_back_is_available() {
    let p = profile(false, b"\r", b"", 40);

    let plan: SubmitPlan = plan_submit_line(&p, "abc", SubmitAction::ClearOnly);

    assert!(
        plan.steps.is_empty(),
        "with no way to take a write back, nothing is written: got {:?}",
        plan.steps
    );
}

#[test]
fn a_fixed_clear_sequence_does_not_depend_on_the_body_length() {
    let p = profile(false, b"\r", b"\x15", 40);
    let long_body = "a".repeat(LONG_BODY_UNITS);

    let take_back: TakeBack = p.take_back();
    let plan: SubmitPlan = plan_submit_line(&p, &long_body, SubmitAction::ClearOnly);

    assert_eq!(take_back, TakeBack::Fixed);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].bytes, b"\x15");
}

// =====================================================================
// SubmitOutcome: the observation carried out of a placed write
// No I/O path constructs one in this pure-layer
// test file, so the type's shape is fixed with a direct literal.
// =====================================================================

#[test]
fn submit_outcome_holds_the_status_before_submit_and_the_submitted_at_instant() {
    let now = tokio::time::Instant::now();
    let outcome = SubmitOutcome {
        status_before_submit: WindowStatus::WaitingInput,
        submitted_at: now,
    };
    assert_eq!(outcome.status_before_submit, WindowStatus::WaitingInput);
    assert_eq!(outcome.submitted_at, now);
}

// =====================================================================
// InputProfile::ready_hold: how long a ready status must have held, once,
// before a window counts as past its startup phase.
// =====================================================================

#[test]
fn input_profile_default_ready_hold_is_zero() {
    assert_eq!(InputProfile::default().ready_hold, Duration::ZERO);
}

#[test]
fn from_section_writes_ready_hold_from_ready_hold_ms() {
    let section = InputSection {
        ready_hold_ms: Some(3000),
        ..InputSection::default()
    };
    let p = InputProfile::from_section(Some(&section));
    assert_eq!(p.ready_hold, Duration::from_secs(3));
}

#[test]
fn from_section_defaults_ready_hold_to_zero_when_ready_hold_ms_is_omitted() {
    let section = InputSection {
        ready_hold_ms: None,
        ..InputSection::default()
    };
    let p = InputProfile::from_section(Some(&section));
    assert_eq!(p.ready_hold, Duration::ZERO);
}
