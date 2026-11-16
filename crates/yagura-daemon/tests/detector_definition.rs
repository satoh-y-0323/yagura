#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The detection-definition TOML loader (`yagura_daemon::detector::definition`).
//!
//! `parse_definition` is a pure parse layer: no filesystem access and no `DaemonConfig`
//! defaults applied, with the resource limits enforced on the parsed content itself.
//! `load_definition` surfaces IO failures as errors and never degrades to `Unknown`.
//! Rejections are asserted only as `Result::Err`, never matched variant by variant, so
//! the error shape stays free to change. An unknown key is the one exception: naming it
//! in the message is part of the contract, so those cases assert on the rendered message.
//! The keys the daemon typed with load with a warning and are not used.

use crate::common;

use common::detection_rules::rule_for_status;
use yagura_daemon::detector::definition::{
    ComposerSection, Region, TurnEndKey, is_valid_detection_name, parse_definition,
};
use yagura_daemon::input_encode::NamedPress;
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
// The `[input]` table and the `[composer]` placeholder: how a definition said its CLI
// was typed into, which the daemon no longer does
// =====================================================================

/// A valid definition carrying `[debounce] confirm_ms = 300`, with `input` appended
/// verbatim so each case differs only in the table under test.
fn definition_with_input(name: &str, input: &str) -> String {
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

{input}
"#
    )
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

/// The keys the daemon used to type into a window with: every `[input]` key, and the
/// `[composer]` placeholder that only told an empty composer from a typed one. Each
/// comes with two values, the second one the old loader refused.
const DEPRECATED_TYPING_KEYS: [(&str, &str, &str, &str); 12] = [
    ("input", "bracketed_paste", "true", "false"),
    ("input", "submit", r#""\r""#, r#""""#),
    ("input", "submit_delay_ms", "800", "5001"),
    ("input", "clear", r#""\u0015""#, r#""123456789""#),
    ("input", "delivery_confirm_ms", "8000", "30001"),
    ("input", "resend_max", "1", "5"),
    ("input", "ready_hold_ms", "0", "10001"),
    ("input", "reflect_wait_ms", "500", "10001"),
    ("input", "probe", r#""x""#, r#""xzx""#),
    ("input", "erase", r#""\u0008""#, r#""""#),
    ("input", "leading_keys", r#"["!"]"#, r#"["!", "ab"]"#),
    ("composer", "placeholder", "'hint'", "'('"),
];

/// A definition reading its composer, with `key = value` written into `table`.
fn definition_with_typing_key(table: &str, key: &str, value: &str) -> String {
    let composer = "[composer]\nmarker = 'MARK'\nseparator = \" \"\ncontinuation_prefix = \"\"";
    let tables = if table == "composer" {
        format!("{composer}\n{key} = {value}")
    } else {
        format!("{composer}\n\n[{table}]\n{key} = {value}")
    };
    definition_with_input("typing-key", &tables)
}

/// A definition that still writes a typing key loads whatever its value, warns naming
/// the key, and loads the same whichever value it writes: nothing reads the value.
/// The composer table itself stays, since its marker keeps the composer's text out of
/// the status rules.
#[test]
fn a_typing_key_loads_with_a_warning_and_its_value_changes_nothing() {
    for (table, key, one, other) in DEPRECATED_TYPING_KEYS {
        let named = regex::Regex::new(&format!(r"\b{key}\b")).unwrap();
        let mut loaded = Vec::new();
        for value in [one, other] {
            let source = definition_with_typing_key(table, key, value);
            let (warns, parsed) =
                common::store_fixtures::capture_warn_events(|| parse_definition(&source));
            let definition = parsed.unwrap_or_else(|err| {
                panic!("{table}.{key} = {value} is deprecated and must not fail the load: {err}")
            });
            assert!(
                warns.iter().any(|event| {
                    named.is_match(&event.message)
                        || event.fields.values().any(|field| named.is_match(field))
                }),
                "loading {table}.{key} = {value} must warn naming {key}; warned: {:?}",
                warns.iter().map(|event| &event.message).collect::<Vec<_>>()
            );
            assert!(
                definition.composer.is_some(),
                "{table}.{key} = {value} must leave the [composer] table loaded"
            );
            loaded.push(format!("{definition:?}"));
        }
        assert_eq!(
            loaded[0], loaded[1],
            "{table}.{key} = {one} and {table}.{key} = {other} must load the same definition"
        );
    }
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
fn generic_shell_detection_file_declares_no_composer() {
    let definition = common::repo_definition("generic-shell.toml");

    assert!(
        definition.composer.is_none(),
        "a plain shell's screen carries no way to tell its own prompt apart from a \
         program's own output, so the bundled definition must not claim it can read one"
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

    // A definition without the `[turn_end_key]` table declares no key whose
    // press ends a turn.
    let definition = parse_definition(&minimal_valid_toml()).expect("the minimal definition loads");
    assert!(
        definition.turn_end_key.is_none(),
        "a definition without the turn_end_key table declared a key"
    );
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

    // The `[turn_end_key]` table is kept as written: the key by its word and
    // the screen text that names it as a pattern.
    let source = definition_with_input(
        "turn-end-key",
        "[turn_end_key]\nkey = \"ctrl_c\"\nshown = 'ctrl\\+c interrupt'",
    );
    let definition = parse_definition(&source).expect("a valid turn_end_key table loads");
    let declared: &TurnEndKey = definition
        .turn_end_key
        .as_ref()
        .expect("the turn_end_key table is kept");
    let mut failures: Vec<String> = Vec::new();
    if declared.key != NamedPress::CtrlC {
        failures.push(format!("key: expected CtrlC, got {:?}", declared.key));
    }
    if !declared.shown.is_match("ctrl+c interrupt") {
        failures.push("shown does not match the screen text it names".to_string());
    }
    assert!(
        failures.is_empty(),
        "the turn_end_key table was not kept as written:\n{}",
        failures.join("\n")
    );
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

    // A `[turn_end_key]` table the daemon cannot act on fails the load.
    let mut failures: Vec<String> = Vec::new();
    let tables = [
        (
            "a key outside the press vocabulary",
            "[turn_end_key]\nkey = \"f1\"\nshown = 'f1 interrupt'",
        ),
        ("no shown", "[turn_end_key]\nkey = \"ctrl_c\""),
        (
            "a shown that is not a pattern",
            "[turn_end_key]\nkey = \"ctrl_c\"\nshown = '('",
        ),
        (
            "an unknown key in the table",
            "[turn_end_key]\nkey = \"ctrl_c\"\nshown = 'ctrl\\+c interrupt'\nkeys = [\"escape\"]",
        ),
    ];
    for (what, table) in tables {
        if parse_definition(&definition_with_input("turn-end-key-bad", table)).is_ok() {
            failures.push(format!("{what} loaded"));
        }
    }
    assert!(
        failures.is_empty(),
        "a turn_end_key table the daemon cannot act on loaded:\n{}",
        failures.join("\n")
    );
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
