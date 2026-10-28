#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The detection-definition TOML loader (`yagura_daemon::detector::definition`).
//!
//! `parse_definition` is a pure parse layer: no filesystem access and no `DaemonConfig`
//! defaults applied, with the resource limits enforced on the parsed content itself.
//! `load_definition` surfaces IO failures as errors and never degrades to `Unknown`.
//! Rejections are asserted only as `Result::Err`, never matched variant by variant, so
//! the error shape stays free to change. The `[input]` range and consistency rules are
//! the one exception: naming the offending key in the message is part of their contract,
//! so those cases assert on the rendered message rather than on the error's variant.

mod common;

use common::detection_rules::rule_for_status;
use yagura_daemon::detector::definition::{
    ComposerSection, InputSection, Region, is_valid_detection_name, parse_definition,
};
use yagura_protocol::WindowStatus;

/// A minimal valid definition: no `bottom_line_count`, no `quiescence`, no
/// `activity`.
fn minimal_valid_toml() -> String {
    r#"
schema_version = 1
name = "generic-shell"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '\$\s*$'
priority = 1
region = "bottom"
"#
    .to_string()
}

/// A fully populated valid definition: `bottom_line_count`, two rules, and
/// both time-based sections.
fn full_valid_toml() -> String {
    r#"
schema_version = 1
name = "claude-code"
bottom_line_count = 5

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed\?'
priority = 10
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '^>\s*$'
priority = 5
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000

[activity]
status = "running"
window_ms = 500
"#
    .to_string()
}

#[test]
fn loads_valid_definition_with_all_sections() {
    let definition = parse_definition(&full_valid_toml()).unwrap();

    assert_eq!(definition.schema_version, 1);
    assert_eq!(definition.name, "claude-code");
    assert_eq!(definition.bottom_line_count, Some(5));
    assert_eq!(definition.rules.len(), 2);

    let approval = rule_for_status(&full_valid_toml(), WindowStatus::WaitingApproval);
    assert_eq!(approval.priority, 10);
    assert_eq!(approval.region, Region::Bottom);

    let input = rule_for_status(&full_valid_toml(), WindowStatus::WaitingInput);
    assert_eq!(input.priority, 5);

    let quiescence = definition
        .quiescence
        .as_ref()
        .expect("fixture opts into quiescence");
    assert_eq!(quiescence.status, WindowStatus::Idle);
    assert_eq!(quiescence.after_ms, 2000);

    let activity = definition
        .activity
        .as_ref()
        .expect("fixture opts into activity");
    assert_eq!(activity.status, WindowStatus::Running);
    assert_eq!(activity.window_ms, 500);
}

#[test]
fn bottom_line_count_is_none_when_omitted() {
    let definition = parse_definition(&minimal_valid_toml()).unwrap();

    assert_eq!(definition.bottom_line_count, None);
}

#[test]
fn time_based_sections_are_none_when_both_omitted() {
    let definition = parse_definition(&minimal_valid_toml()).unwrap();

    assert!(definition.quiescence.is_none());
    assert!(definition.activity.is_none());
}

#[test]
fn quiescence_is_none_when_only_activity_is_present() {
    let toml = r#"
schema_version = 1
name = "activity-only"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '\$\s*$'
priority = 1
region = "bottom"

[activity]
status = "running"
window_ms = 500
"#;

    let definition = parse_definition(toml).unwrap();

    assert!(definition.quiescence.is_none());
    assert!(definition.activity.is_some());
}

#[test]
fn activity_is_none_when_only_quiescence_is_present() {
    let toml = r#"
schema_version = 1
name = "quiescence-only"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '\$\s*$'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000
"#;

    let definition = parse_definition(toml).unwrap();

    assert!(definition.activity.is_none());
    assert!(definition.quiescence.is_some());
}

#[test]
fn rejects_unknown_status() {
    let toml = r#"
schema_version = 1
name = "bad-status"

[debounce]
confirm_ms = 300

[[rules]]
status = "not_a_status"
pattern = 'x'
priority = 1
region = "bottom"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_invalid_regex_pattern() {
    let toml = r#"
schema_version = 1
name = "bad-regex"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = '(unterminated'
priority = 1
region = "bottom"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_schema_version_mismatch() {
    let toml = r#"
schema_version = 2
name = "future-schema"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_definition_missing_name() {
    let toml = r#"
schema_version = 1

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_definition_missing_rules_key() {
    let toml = r#"
schema_version = 1
name = "no-rules"

[debounce]
confirm_ms = 300
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_unknown_region_value() {
    let toml = r#"
schema_version = 1
name = "bad-region"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "full"
"#;

    assert!(
        parse_definition(toml).is_err(),
        "\"full\" must be rejected: only region = \"bottom\" and region = \"screen\" exist"
    );
}

#[test]
fn accepts_the_screen_region() {
    let toml = r#"
schema_version = 1
name = "screen-region"

[[rules]]
status = "waiting_approval"
pattern = 'x'
priority = 1
region = "screen"
"#;

    let definition = parse_definition(toml).expect("region = \"screen\" must load");
    assert_eq!(definition.rules[0].region, Region::Screen);
}

#[test]
fn rejects_arbitrary_unknown_region_value() {
    let toml = r#"
schema_version = 1
name = "bad-region-2"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "not-a-region"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_quiescence_missing_after_ms() {
    let toml = r#"
schema_version = 1
name = "bad-quiescence-1"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_quiescence_missing_status() {
    let toml = r#"
schema_version = 1
name = "bad-quiescence-2"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[quiescence]
after_ms = 2000
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_activity_missing_window_ms() {
    let toml = r#"
schema_version = 1
name = "bad-activity-1"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[activity]
status = "running"
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_activity_missing_status() {
    let toml = r#"
schema_version = 1
name = "bad-activity-2"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[activity]
window_ms = 500
"#;

    assert!(parse_definition(toml).is_err());
}

#[test]
fn rejects_source_larger_than_256_kib() {
    const MAX_BYTES: usize = 256 * 1024;
    // The limit is a property of the parsed content's byte length, so it is exercised
    // against `parse_definition`; a single TOML comment line keeps the rest parseable.
    let padding = "a".repeat(MAX_BYTES + 1);
    let toml = format!("# {padding}\n{}", minimal_valid_toml());

    assert!(
        parse_definition(&toml).is_err(),
        "a definition source over 256 KiB must be rejected"
    );
}

#[test]
fn rejects_more_than_200_rules() {
    let mut toml = String::from(
        "schema_version = 1\nname = \"too-many-rules\"\n\n[debounce]\nconfirm_ms = 300\n\n",
    );
    for _ in 0..201 {
        toml.push_str(
            "[[rules]]\nstatus = \"idle\"\npattern = 'x'\npriority = 1\nregion = \"bottom\"\n\n",
        );
    }

    assert!(
        parse_definition(&toml).is_err(),
        "more than 200 rules must be rejected"
    );
}

#[test]
fn accepts_exactly_200_rules() {
    let mut toml = String::from(
        "schema_version = 1\nname = \"two-hundred-rules\"\n\n[debounce]\nconfirm_ms = 300\n\n",
    );
    for _ in 0..200 {
        toml.push_str(
            "[[rules]]\nstatus = \"idle\"\npattern = 'x'\npriority = 1\nregion = \"bottom\"\n\n",
        );
    }

    let definition = parse_definition(&toml).unwrap();

    assert_eq!(definition.rules.len(), 200);
}

#[test]
fn loaded_pattern_is_a_compiled_regex_that_can_match() {
    let approval_rule = rule_for_status(&full_valid_toml(), WindowStatus::WaitingApproval);

    assert!(approval_rule.pattern.is_match("Do you want to proceed?"));
    assert!(!approval_rule.pattern.is_match("nothing relevant here"));
}

#[test]
fn rules_preserve_declared_priority_for_deterministic_ordering() {
    let toml = r#"
schema_version = 1
name = "priority-check"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'proceed'
priority = 10
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = 'proceed'
priority = 20
region = "bottom"
"#;
    let definition = parse_definition(toml).unwrap();

    // Both rules match the same simulated screen line; the loaded priority values must
    // let a caller deterministically pick the numerically larger one.
    let mut matching: Vec<_> = definition
        .rules
        .iter()
        .filter(|rule| rule.pattern.is_match("please proceed"))
        .collect();
    matching.sort_by_key(|rule| std::cmp::Reverse(rule.priority));

    let winner = matching.first().expect("at least one rule should match");
    assert_eq!(winner.status, WindowStatus::WaitingInput);
    assert_eq!(winner.priority, 20);
}

#[test]
fn is_valid_detection_name_accepts_typical_names() {
    assert!(is_valid_detection_name("claude-code"));
    assert!(is_valid_detection_name("generic_shell"));
    assert!(is_valid_detection_name("a"));
    assert!(is_valid_detection_name(&"a".repeat(64)));
}

#[test]
fn is_valid_detection_name_rejects_empty_name() {
    assert!(!is_valid_detection_name(""));
}

#[test]
fn is_valid_detection_name_rejects_name_over_64_chars() {
    assert!(!is_valid_detection_name(&"a".repeat(65)));
}

#[test]
fn is_valid_detection_name_rejects_uppercase() {
    assert!(!is_valid_detection_name("Claude-Code"));
}

#[test]
fn is_valid_detection_name_rejects_forward_slash() {
    assert!(!is_valid_detection_name("foo/bar"));
}

#[test]
fn is_valid_detection_name_rejects_backslash() {
    assert!(!is_valid_detection_name("foo\\bar"));
}

#[test]
fn is_valid_detection_name_rejects_dot_dot() {
    assert!(!is_valid_detection_name("../etc"));
}

#[test]
fn claude_code_detection_file_loads() {
    let definition = common::repo_definition("claude-code.toml");

    assert_eq!(definition.name, "claude-code");
}

#[test]
fn generic_shell_detection_file_loads() {
    let definition = common::repo_definition("generic-shell.toml");

    assert_eq!(definition.name, "generic-shell");
}

#[test]
fn codex_cli_detection_file_loads() {
    let definition = common::repo_definition("codex-cli.toml");

    assert_eq!(definition.name, "codex-cli");
}

// =====================================================================
// The `[input]` table: how a definition says its CLI is written to
// =====================================================================

/// Every `[input]` key at once. Control bytes are written as basic-string unicode
/// escapes, so no raw control character is embedded in this source where it would be
/// invisible to a reader diffing it.
const FULL_INPUT_TABLE: &str = r#"[input]
bracketed_paste = true
submit = "\r"
submit_delay_ms = 800
clear = "\u0015"
delivery_confirm_ms = 9000
resend_max = 4"#;

/// The `[input]` table shared by the two tests that pin the scope of the
/// "submit delay must outlast the debounce" rule: identical values, judged against a
/// definition that declares `[debounce]` and one that does not.
const DEBOUNCE_BOUNDARY_INPUT_TABLE: &str = r#"[input]
submit_delay_ms = 300
delivery_confirm_ms = 8000"#;

/// An `[input]` table that omits `submit_delay_ms` entirely, used by the two tests that
/// pin the rule to the *default* it falls back to (800 ms) rather than to a value the
/// definition wrote down: that default must still outlast the debounce it is judged
/// against.
const OMITTED_SUBMIT_DELAY_INPUT_TABLE: &str = r#"[input]
delivery_confirm_ms = 8000"#;

/// A valid definition carrying `[debounce] confirm_ms = 300`, with `input` appended
/// verbatim so each case differs only in the table under test.
fn definition_with_input(name: &str, input: &str) -> String {
    definition_with_debounce_and_input(name, 300, input)
}

/// [`definition_with_input`] with the `[debounce].confirm_ms` value under the caller's
/// control, so the boundary against a *default* `submit_delay_ms` can be pinned on both
/// sides (300 ms lets the 800 ms default clear it; 900 ms does not).
fn definition_with_debounce_and_input(name: &str, confirm_ms: u64, input: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = {confirm_ms}

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

{input}
"#
    )
}

/// [`definition_with_input`] without the `[debounce]` table: the loader cannot know the
/// effective `confirm_ms` such a definition runs under, since that comes from the
/// daemon's own configuration.
fn definition_without_debounce_with_input(name: &str, input: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

{input}
"#
    )
}

/// The `[input]` table of a definition that must load.
fn input_section_of(source: &str) -> InputSection {
    parse_definition(source)
        .expect("a definition carrying a valid [input] table must load")
        .input
        .expect("a definition that declares [input] must keep it on the loaded definition")
}

/// The rendered message of a rejection, for the cases whose contract includes naming the
/// offending key.
fn rejection_message(source: &str) -> String {
    match parse_definition(source) {
        Ok(definition) => panic!(
            "expected this definition to be rejected, but it loaded as {:?}",
            definition.name
        ),
        Err(err) => err.to_string(),
    }
}

#[test]
fn input_table_is_kept_verbatim_with_no_defaults_applied() {
    let input = input_section_of(&definition_with_input("input-full", FULL_INPUT_TABLE));

    assert_eq!(input.bracketed_paste, Some(true));
    assert_eq!(input.submit.as_deref(), Some("\r"));
    assert_eq!(input.submit_delay_ms, Some(800));
    assert_eq!(input.clear.as_deref(), Some("\u{15}"));
    assert_eq!(
        input.clear.as_deref().map(str::len),
        Some(1),
        "the escape must decode to the single byte the definition means to send, not to \
         its six-character source spelling"
    );
    assert_eq!(input.delivery_confirm_ms, Some(9000));
    assert_eq!(input.resend_max, Some(4));
}

#[test]
fn input_keys_the_definition_omits_stay_none() {
    let input = input_section_of(&definition_with_input(
        "input-partial",
        "[input]\nbracketed_paste = true",
    ));

    assert_eq!(input.bracketed_paste, Some(true));
    assert_eq!(
        input.submit, None,
        "the loader applies no defaults: an omitted key stays absent so the layer that owns \
         the defaults can tell 'not written' from 'written as the default value'"
    );
    assert_eq!(input.submit_delay_ms, None);
    assert_eq!(input.clear, None);
    assert_eq!(input.delivery_confirm_ms, None);
    assert_eq!(input.resend_max, None);
}

#[test]
fn input_is_none_when_the_table_is_omitted() {
    let definition = parse_definition(&minimal_valid_toml()).unwrap();

    assert!(
        definition.input.is_none(),
        "a definition without an [input] table must not gain a synthesised one"
    );
}

#[test]
fn rejects_submit_delay_ms_over_its_range() {
    let message = rejection_message(&definition_with_input(
        "input-submit-delay-over",
        "[input]\nsubmit_delay_ms = 5001",
    ));

    assert!(
        message.contains("input.submit_delay_ms"),
        "an out-of-range value must fail the whole definition and name the offending key, \
         got: {message}"
    );
}

#[test]
fn rejects_delivery_confirm_ms_over_its_range() {
    let message = rejection_message(&definition_with_input(
        "input-delivery-confirm-over",
        "[input]\ndelivery_confirm_ms = 30001",
    ));

    assert!(
        message.contains("input.delivery_confirm_ms"),
        "an out-of-range value must fail the whole definition and name the offending key, \
         got: {message}"
    );
}

#[test]
fn rejects_resend_max_over_its_range() {
    let message = rejection_message(&definition_with_input(
        "input-resend-max-over",
        "[input]\nsubmit_delay_ms = 800\nclear = \"\\u0015\"\ndelivery_confirm_ms = 8000\n\
         resend_max = 5",
    ));

    assert!(
        message.contains("input.resend_max"),
        "an out-of-range value must fail the whole definition and name the offending key, \
         got: {message}"
    );
    assert!(
        !message.contains("input.delivery_confirm_ms") && !message.contains("input.clear"),
        "the other two keys are written so that neither consistency rule can fire, and both \
         of those rules name a second key: this case must be rejected by the range check \
         itself, got: {message}"
    );
}

/// The probe and erase keys are written into the child as they are, so only
/// a single visible character may serve as the probe and an erase key must
/// stay within the input sequence bound.
#[test]
fn probe_and_erase_keys_outside_their_shapes_fail_the_definition() {
    let input = |extra: &str| format!("[input]\nclear = \"\\u0015\"\n{extra}");
    for (extra, key) in [
        (r#"probe = """#, "input.probe"),
        (r#"probe = "xy""#, "input.probe"),
        (r#"probe = " ""#, "input.probe"),
        (r#"probe = "\u0003""#, "input.probe"),
        (r#"probe = "\u001b""#, "input.probe"),
        (r#"erase = """#, "input.erase"),
        (r#"erase = "123456789""#, "input.erase"),
    ] {
        let message = rejection_message(&definition_with_input("input-probe", &input(extra)));
        assert!(
            message.contains(key),
            "{extra} must fail the definition and name {key}, got: {message}"
        );
    }

    for (extra, probe, erase) in [
        (r#"probe = "x""#, Some("x"), None),
        (r#"probe = "é""#, Some("é"), None),
        (
            "probe = \"x\"\nerase = \"\\u0008\"",
            Some("x"),
            Some("\u{8}"),
        ),
    ] {
        let section = input_section_of(&definition_with_input("input-probe", &input(extra)));
        assert_eq!(section.probe.as_deref(), probe, "{extra}");
        assert_eq!(section.erase.as_deref(), erase, "{extra}");
    }
}

/// Every line the daemon writes on its own begins with `[yagura `: a probe of
/// `[` would read alone exactly as such a line begins to show, so the
/// definition is refused and the message says why.
#[test]
fn a_probe_that_is_the_first_character_of_the_daemons_own_lines_fails_the_definition() {
    let message = rejection_message(&definition_with_input(
        "input-probe-bracket",
        "[input]\nclear = \"\\u0015\"\nprobe = \"[\"",
    ));

    assert!(
        message.contains("input.probe") && message.contains("daemon"),
        "a probe of `[` must fail the definition, naming the key and the daemon's own lines, \
         got: {message}"
    );
}

#[test]
fn rejects_an_empty_submit_sequence() {
    let message = rejection_message(&definition_with_input(
        "input-submit-empty",
        "[input]\nsubmit = \"\"",
    ));

    assert!(
        message.contains("input.submit"),
        "a submit sequence that sends nothing is not a submit, so it must fail the whole \
         definition and name the offending key, got: {message}"
    );
}

#[test]
fn rejects_a_submit_sequence_over_eight_bytes() {
    let message = rejection_message(&definition_with_input(
        "input-submit-too-long",
        "[input]\nsubmit = \"123456789\"",
    ));

    assert!(
        message.contains("input.submit"),
        "a nine-byte submit sequence is out of range and must fail the whole definition, \
         naming the offending key, got: {message}"
    );
}

#[test]
fn rejects_a_clear_sequence_over_eight_bytes() {
    let message = rejection_message(&definition_with_input(
        "input-clear-too-long",
        "[input]\nclear = \"123456789\"",
    ));

    assert!(
        message.contains("input.clear"),
        "a nine-byte clear sequence is out of range and must fail the whole definition, \
         naming the offending key, got: {message}"
    );
}

#[test]
fn accepts_the_upper_bound_of_every_input_range() {
    let input = input_section_of(&definition_with_input(
        "input-upper-bounds",
        "[input]\nsubmit = \"12345678\"\nsubmit_delay_ms = 5000\nclear = \"12345678\"\n\
         delivery_confirm_ms = 30000\nresend_max = 4",
    ));

    assert_eq!(input.submit.as_deref(), Some("12345678"));
    assert_eq!(input.submit_delay_ms, Some(5000));
    assert_eq!(input.clear.as_deref(), Some("12345678"));
    assert_eq!(input.delivery_confirm_ms, Some(30000));
    assert_eq!(
        input.resend_max,
        Some(4),
        "the bounds themselves are inside the range: rejecting them would put the values \
         the bundled definitions are allowed to carry out of reach"
    );
}

#[test]
fn rejects_resending_when_delivery_is_never_confirmed() {
    let message = rejection_message(&definition_with_input(
        "input-resend-without-confirm",
        "[input]\ndelivery_confirm_ms = 0\nresend_max = 1\nclear = \"\\u0015\"",
    ));

    assert!(
        message.contains("input.resend_max requires input.delivery_confirm_ms to be non-zero"),
        "a resend only ever happens as the result of a confirmation, so asking for one \
         without confirming delivery is a contradiction the loader must refuse, got: \
         {message}"
    );
}

#[test]
fn accepts_resending_without_a_clear_sequence() {
    // The resend action only writes the submit key again, so a clear
    // sequence is not a precondition for resending.
    let input = input_section_of(&definition_with_input(
        "input-resend-without-clear",
        "[input]\nsubmit_delay_ms = 800\ndelivery_confirm_ms = 8000\nresend_max = 1",
    ));

    assert_eq!(input.resend_max, Some(1));
    assert_eq!(
        input.clear, None,
        "a resend-requesting definition must load even when it declares no clear sequence \
         at all"
    );
}

#[test]
fn accepts_a_clear_sequence_alongside_a_disabled_delivery_confirmation() {
    // The load-time check this loader keeps only rejects
    // `resend_max > 0` combined with `delivery_confirm_ms == 0` (no resend
    // trigger can ever fire), not the presence of a clear sequence by itself.
    let input = input_section_of(&definition_with_input(
        "input-clear-with-confirm-off",
        "[input]\nclear = \"\\u0015\"\ndelivery_confirm_ms = 0",
    ));

    assert_eq!(input.clear.as_deref(), Some("\u{15}"));
    assert_eq!(input.delivery_confirm_ms, Some(0));
}

#[test]
fn rejects_an_input_table_that_still_writes_clear_per_char() {
    let message: String = rejection_message(&definition_with_input(
        "input-clear-per-char-withdrawn",
        "[input]\nclear_per_char = \"\\u007f\"",
    ));

    assert!(
        message.contains("clear_per_char"),
        "clear_per_char was withdrawn: the loader must reject it as an unknown key and name \
         it, got: {message}"
    );
}

#[test]
fn the_same_definition_loads_once_clear_per_char_is_removed() {
    let input: InputSection = input_section_of(&definition_with_input(
        "input-clear-per-char-removed",
        "[input]\nbracketed_paste = true",
    ));

    assert_eq!(
        input.bracketed_paste,
        Some(true),
        "removing the withdrawn key from the same table must let the definition load"
    );
}

#[test]
fn rejects_a_submit_delay_that_does_not_outlast_the_debounce() {
    let message = rejection_message(&definition_with_input(
        "input-submit-delay-under-debounce",
        DEBOUNCE_BOUNDARY_INPUT_TABLE,
    ));

    assert!(
        message.contains("input.submit_delay_ms"),
        "the rejection must name the input key it judged, got: {message}"
    );
    assert!(
        message.contains("debounce.confirm_ms"),
        "the rejection must name the other side of the comparison too, since the fix may \
         belong to either key, got: {message}"
    );
}

#[test]
fn accepts_the_same_submit_delay_when_the_definition_declares_no_debounce() {
    let input = input_section_of(&definition_without_debounce_with_input(
        "input-submit-delay-no-debounce",
        DEBOUNCE_BOUNDARY_INPUT_TABLE,
    ));

    assert_eq!(
        input.submit_delay_ms,
        Some(300),
        "a definition without [debounce] runs under the daemon's own confirm window, which \
         the loader cannot read, so it must not judge the comparison at all"
    );
    assert_eq!(input.delivery_confirm_ms, Some(8000));
}

#[test]
fn rejects_an_omitted_submit_delay_ms_when_its_default_does_not_outlast_the_debounce() {
    let message = rejection_message(&definition_with_debounce_and_input(
        "input-omitted-submit-delay-under-debounce",
        900,
        OMITTED_SUBMIT_DELAY_INPUT_TABLE,
    ));

    assert!(
        message.contains("input.submit_delay_ms"),
        "the rejection must name the input key it judged even though the definition omitted \
         it: the value under judgment is the default (800 ms) that would apply, and that \
         default does not outlast a 900 ms debounce, got: {message}"
    );
    assert!(
        message.contains("debounce.confirm_ms"),
        "the rejection must name the other side of the comparison too, got: {message}"
    );
}

#[test]
fn accepts_an_omitted_submit_delay_ms_when_its_default_outlasts_the_debounce() {
    let input = input_section_of(&definition_with_debounce_and_input(
        "input-omitted-submit-delay-over-debounce",
        300,
        OMITTED_SUBMIT_DELAY_INPUT_TABLE,
    ));

    assert_eq!(
        input.submit_delay_ms, None,
        "the loader judges the omitted key against its default without writing that \
         default onto the loaded definition -- InputProfile::from_section, not the loader, \
         owns applying defaults"
    );
    assert_eq!(input.delivery_confirm_ms, Some(8000));
}

#[test]
fn rejects_an_unknown_top_level_key() {
    let toml = r#"
schema_version = 1
name = "unknown-top-level"
bottom_lines = 5

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"
"#;

    assert!(
        parse_definition(toml).is_err(),
        "a mistyped key must fail the load loudly instead of being silently ignored"
    );
}

#[test]
fn rejects_an_unknown_rule_key() {
    let toml = r#"
schema_version = 1
name = "unknown-rule-key"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"
regions = "bottom"
"#;

    assert!(
        parse_definition(toml).is_err(),
        "a mistyped key must fail the load loudly instead of being silently ignored"
    );
}

#[test]
fn rejects_an_unknown_debounce_key() {
    let toml = r#"
schema_version = 1
name = "unknown-debounce-key"

[debounce]
confirm_ms = 300
confirm_ms_typo = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"
"#;

    assert!(
        parse_definition(toml).is_err(),
        "a mistyped key inside a nested table must fail the load as loudly as one at the \
         top level"
    );
}

#[test]
fn rejects_an_unknown_quiescence_key() {
    let toml = r#"
schema_version = 1
name = "unknown-quiescence-key"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000
after_ms_typo = 2000
"#;

    assert!(
        parse_definition(toml).is_err(),
        "a mistyped key inside a nested table must fail the load as loudly as one at the \
         top level"
    );
}

#[test]
fn rejects_an_unknown_activity_key() {
    let toml = r#"
schema_version = 1
name = "unknown-activity-key"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[activity]
status = "running"
window_ms = 500
window_ms_typo = 500
"#;

    assert!(
        parse_definition(toml).is_err(),
        "a mistyped key inside a nested table must fail the load as loudly as one at the \
         top level"
    );
}

#[test]
fn rejects_an_unknown_input_key() {
    let message = rejection_message(&definition_with_input(
        "unknown-input-key",
        "[input]\nsubmit_delay_ms = 800\nsubmit_key = \"\\r\"",
    ));

    assert!(
        message.contains("submit_key"),
        "a mistyped key inside [input] must fail the load loudly and say which key it did \
         not recognise, got: {message}"
    );
}

#[test]
fn ready_hold_ms_is_none_when_omitted_and_kept_verbatim_at_its_bounds() {
    let omitted = input_section_of(&definition_with_input(
        "input-ready-hold-omitted",
        "[input]\nbracketed_paste = true",
    ));
    assert_eq!(omitted.ready_hold_ms, None);

    let zero = input_section_of(&definition_with_input(
        "input-ready-hold-zero",
        "[input]\nready_hold_ms = 0",
    ));
    assert_eq!(zero.ready_hold_ms, Some(0));

    let upper = input_section_of(&definition_with_input(
        "input-ready-hold-upper",
        "[input]\nready_hold_ms = 10000",
    ));
    assert_eq!(upper.ready_hold_ms, Some(10000));
}

#[test]
fn rejects_ready_hold_ms_over_its_range() {
    let message = rejection_message(&definition_with_input(
        "input-ready-hold-over",
        "[input]\nready_hold_ms = 10001",
    ));

    assert!(
        message.contains("input.ready_hold_ms"),
        "an out-of-range value must fail the whole definition and name the offending key, \
         got: {message}"
    );
}

#[test]
fn reflect_wait_ms_is_kept_verbatim_at_its_upper_bound() {
    let upper: InputSection = input_section_of(&definition_with_input(
        "input-reflect-wait-upper",
        "[input]\nreflect_wait_ms = 10000",
    ));
    assert_eq!(upper.reflect_wait_ms, Some(10000));
}

#[test]
fn accepts_reflect_wait_ms_over_its_old_range_because_the_key_no_longer_bounds_anything() {
    // Delivery no longer waits a duration for a write to show, so a value that
    // used to stall the window cannot, and rejecting the whole definition for it
    // would only turn a harmless old definition into a window stuck at `Unknown`.
    let over: InputSection = input_section_of(&definition_with_input(
        "input-reflect-wait-over",
        "[input]\nreflect_wait_ms = 10001",
    ));

    assert_eq!(over.reflect_wait_ms, Some(10001));
}

#[test]
fn accepts_a_nonzero_ready_hold_ms_alongside_a_disabled_delivery_confirmation() {
    let input = input_section_of(&definition_with_input(
        "input-ready-hold-with-confirm-off",
        "[input]\nready_hold_ms = 1000\ndelivery_confirm_ms = 0",
    ));

    assert_eq!(input.ready_hold_ms, Some(1000));
    assert_eq!(input.delivery_confirm_ms, Some(0));
}

#[test]
fn accepts_a_ready_hold_ms_at_or_above_the_quiescence_after_ms() {
    let toml = r#"
schema_version = 1
name = "input-ready-hold-above-quiescence"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

[quiescence]
status = "idle"
after_ms = 2000

[input]
ready_hold_ms = 2000
"#;

    let input = input_section_of(toml);

    assert_eq!(input.ready_hold_ms, Some(2000));
}

#[test]
fn codex_cli_detection_file_carries_its_input_section() {
    let input = common::repo_definition("codex-cli.toml")
        .input
        .expect("the bundled codex-cli definition must declare how its CLI is written to");

    assert_eq!(input.bracketed_paste, Some(true));
    assert_eq!(input.submit.as_deref(), Some("\r"));
    assert_eq!(input.ready_hold_ms, Some(3000));
    assert_eq!(input.submit_delay_ms, Some(800));
    assert_eq!(input.clear.as_deref(), Some("\u{15}"));
    assert_eq!(input.clear.as_deref().map(str::len), Some(1));
    assert_eq!(input.delivery_confirm_ms, Some(9000));
    assert_eq!(
        input.resend_max,
        Some(4),
        "the resend budget is what carries the last write past the receiver's startup \
         discard boundary, so its value is part of the bundled definition's contract"
    );
}

#[test]
fn claude_code_detection_file_carries_its_input_section() {
    let input = common::repo_definition("claude-code.toml")
        .input
        .expect("the bundled claude-code definition must declare how its CLI is written to");

    assert_eq!(input.bracketed_paste, Some(true));
    assert_eq!(input.submit.as_deref(), Some("\r"));
    assert_eq!(input.ready_hold_ms, None);
    assert_eq!(input.submit_delay_ms, Some(800));
    assert_eq!(input.clear.as_deref(), Some("\u{15}"));
    assert_eq!(input.clear.as_deref().map(str::len), Some(1));
    assert_eq!(input.delivery_confirm_ms, Some(8000));
    assert_eq!(input.resend_max, Some(1));
}

#[test]
fn generic_shell_detection_file_declares_no_composer_and_no_input() {
    let definition = common::repo_definition("generic-shell.toml");

    assert!(
        definition.composer.is_none(),
        "a plain shell's screen carries no way to tell its own prompt apart from a \
         program's own output, so the bundled definition must not claim it can read one"
    );
    assert!(
        definition.input.is_none(),
        "with no composer to read back, the daemon never writes into this window, so the \
         bundled definition carries no [input] table either"
    );
    assert!(
        !definition.rules.is_empty(),
        "the idle rule stays: it drives status display, independent of writing"
    );
    assert!(
        definition.quiescence.is_some(),
        "quiescence stays: it also drives status display, independent of writing"
    );
    assert!(
        definition.activity.is_some(),
        "activity stays: it also drives status display, independent of writing"
    );
}

// =====================================================================
// The `[composer]` table: an optional section that identifies
// the composer region on screen. Exactly three keys: `marker`, `separator`,
// `continuation_prefix`.
// =====================================================================

/// A valid definition carrying `[composer]` verbatim, appended after the
/// mandatory sections.
fn definition_with_composer(name: &str, composer: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 300

[[rules]]
status = "idle"
pattern = 'x'
priority = 1
region = "bottom"

{composer}
"#
    )
}

/// The `[composer]` table of a definition that must load.
fn composer_section_of(source: &str) -> ComposerSection {
    parse_definition(source)
        .expect("a definition carrying a valid [composer] table must load")
        .composer
        .expect("a definition that declares [composer] must keep it on the loaded definition")
}

#[test]
fn composer_is_none_when_the_table_is_omitted() {
    let definition = parse_definition(&minimal_valid_toml()).unwrap();

    assert!(
        definition.composer.is_none(),
        "a definition without a [composer] table must not gain a synthesised one"
    );
}

#[test]
fn composer_section_loads_its_three_fields_verbatim() {
    let composer = composer_section_of(&definition_with_composer(
        "composer-full",
        "[composer]\nmarker = '^>'\nseparator = \" \"\ncontinuation_prefix = \"  \"",
    ));

    assert!(composer.marker.is_match(">"));
    assert!(!composer.marker.is_match("nothing relevant here"));
    assert_eq!(composer.separator, " ");
    assert_eq!(composer.continuation_prefix, "  ");
}

#[test]
fn composer_continuation_prefix_may_be_the_empty_string() {
    let composer = composer_section_of(&definition_with_composer(
        "composer-empty-prefix",
        "[composer]\nmarker = '[$#>]'\nseparator = \" \"\ncontinuation_prefix = \"\"",
    ));

    assert_eq!(
        composer.continuation_prefix, "",
        "a plain shell prompt has no continuation indentation at all"
    );
}

#[test]
fn rejects_an_unknown_composer_key() {
    let message = rejection_message(&definition_with_composer(
        "composer-unknown-key",
        "[composer]\nmarker = '^>'\nseparator = \" \"\ncontinuation_prefix = \"\"\nborder = '---'",
    ));

    assert!(
        message.contains("border"),
        "a mistyped key inside [composer] must fail the load loudly and say which key it did \
         not recognise, got: {message}"
    );
}

#[test]
fn rejects_composer_missing_marker() {
    assert!(
        parse_definition(&definition_with_composer(
            "composer-missing-marker",
            "[composer]\nseparator = \" \"\ncontinuation_prefix = \"\"",
        ))
        .is_err()
    );
}

#[test]
fn rejects_composer_missing_separator() {
    assert!(
        parse_definition(&definition_with_composer(
            "composer-missing-separator",
            "[composer]\nmarker = '^>'\ncontinuation_prefix = \"\"",
        ))
        .is_err()
    );
}

#[test]
fn rejects_composer_missing_continuation_prefix() {
    assert!(
        parse_definition(&definition_with_composer(
            "composer-missing-continuation-prefix",
            "[composer]\nmarker = '^>'\nseparator = \" \"",
        ))
        .is_err()
    );
}

#[test]
fn rejects_an_invalid_composer_marker_pattern() {
    assert!(
        parse_definition(&definition_with_composer(
            "composer-bad-marker",
            "[composer]\nmarker = '(unterminated'\nseparator = \" \"\ncontinuation_prefix = \"\"",
        ))
        .is_err()
    );
}

// =====================================================================
// The bundled definitions' `[composer]` section. The marker,
// separator and continuation-prefix values are a design fact,
// not a probe measurement, so asserting them here pins the acceptance
// criterion rather than restating an observed byte offset.
// =====================================================================

#[test]
fn claude_code_detection_file_carries_a_composer_section() {
    let composer = common::repo_definition("claude-code.toml")
        .composer
        .expect("the bundled claude-code definition must declare a [composer] section");

    assert!(
        composer.marker.is_match("\u{276f}"),
        "claude-code's composer marker must match its target symbol"
    );
    assert_eq!(composer.separator, "\u{a0}");
    assert_eq!(composer.continuation_prefix, "  ");
}

#[test]
fn codex_cli_detection_file_carries_a_composer_section() {
    let composer = common::repo_definition("codex-cli.toml")
        .composer
        .expect("the bundled codex-cli definition must declare a [composer] section");

    assert!(
        composer.marker.is_match("\u{203a}"),
        "codex-cli's composer marker must match its target symbol"
    );
    assert_eq!(composer.separator, " ");
    assert_eq!(composer.continuation_prefix, "  ");
}

// =====================================================================
// `[input].reflect_wait_ms`: the upper bound the daemon waits for a write
// to reflect on screen, shared by the delivery-confirmation wait and the
// composer read-back query. Same loader contract as every other `[input]`
// key: kept verbatim, no default applied here.
// =====================================================================

#[test]
fn reflect_wait_ms_is_none_when_omitted_and_kept_verbatim_when_present() {
    let omitted = input_section_of(&definition_with_input(
        "input-reflect-wait-omitted",
        "[input]\nbracketed_paste = true",
    ));
    assert_eq!(omitted.reflect_wait_ms, None);

    let present = input_section_of(&definition_with_input(
        "input-reflect-wait-present",
        "[input]\nreflect_wait_ms = 500",
    ));
    assert_eq!(present.reflect_wait_ms, Some(500));
}

#[test]
fn accepts_an_erase_sequence_of_eight_bytes() {
    let input = input_section_of(&definition_with_input(
        "input-erase-upper-bound",
        "[input]\nclear = \"\\u0015\"\nprobe = \"x\"\nerase = \"12345678\"",
    ));

    assert_eq!(input.erase.as_deref(), Some("12345678"));
}

#[test]
fn accepts_no_resend_alongside_a_disabled_delivery_confirmation() {
    let input = input_section_of(&definition_with_input(
        "input-no-resend-with-confirm-off",
        "[input]\nclear = \"\\u0015\"\ndelivery_confirm_ms = 0\nresend_max = 0",
    ));

    assert_eq!(input.resend_max, Some(0));
    assert_eq!(input.delivery_confirm_ms, Some(0));
}

// =====================================================================
// The `[requests]` table: how a CLI treats a tool call still out at the end of its turn
// =====================================================================

/// Whether the definition says its CLI drops pending tool calls when a turn ends.
fn abandons_calls_at_turn_end(source: &str) -> bool {
    parse_definition(source)
        .expect("a definition carrying a valid [requests] table must load")
        .requests
        .abandoned_at_turn_end
}

#[test]
fn abandoned_at_turn_end_is_false_when_the_requests_table_is_omitted() {
    assert!(!abandons_calls_at_turn_end(&minimal_valid_toml()));
}

#[test]
fn abandoned_at_turn_end_is_false_when_the_requests_table_leaves_it_out() {
    assert!(!abandons_calls_at_turn_end(&definition_with_input(
        "requests-empty",
        "[requests]"
    )));
}

#[test]
fn abandoned_at_turn_end_is_kept_as_written() {
    assert!(abandons_calls_at_turn_end(&definition_with_input(
        "requests-true",
        "[requests]\nabandoned_at_turn_end = true"
    )));
    assert!(!abandons_calls_at_turn_end(&definition_with_input(
        "requests-false",
        "[requests]\nabandoned_at_turn_end = false"
    )));
}

#[test]
fn rejects_an_unknown_key_in_the_requests_table() {
    assert!(abandons_calls_at_turn_end(&definition_with_input(
        "requests-known-key",
        "[requests]\nabandoned_at_turn_end = true"
    )));
    let source = definition_with_input(
        "requests-unknown-key",
        "[requests]\nabandoned_at_turn_end = true\nabandons_everything = true",
    );
    assert!(parse_definition(&source).is_err());
}

#[test]
fn rejects_an_abandoned_at_turn_end_that_is_not_a_boolean() {
    assert!(abandons_calls_at_turn_end(&definition_with_input(
        "requests-boolean",
        "[requests]\nabandoned_at_turn_end = true"
    )));
    for value in ["\"true\"", "1", "[]"] {
        let source = definition_with_input(
            "requests-not-a-boolean",
            &format!("[requests]\nabandoned_at_turn_end = {value}"),
        );
        assert!(
            parse_definition(&source).is_err(),
            "abandoned_at_turn_end = {value} must be rejected"
        );
    }
}
