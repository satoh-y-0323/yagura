#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: reading a listing command's standard output into
//! [`ModelChoice`]s, and the one-line reasons given when a listing cannot be
//! read at all.
//!
//! Every fixture here is a literal byte string built by the test itself;
//! nothing spawns a process.

use yagura_daemon::model_choices::{
    ChoicesFailure, MAX_CHOICES, failure_for_exit, failure_for_spawn_error, parse_choices,
};
use yagura_daemon::profile::{JsonFilter, OutputFormat};
use yagura_protocol::ModelChoice;

fn choice(id: &str, label: Option<&str>) -> ModelChoice {
    ModelChoice {
        id: id.to_string(),
        label: label.map(str::to_string),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn lines_format(separator: &str) -> OutputFormat {
    OutputFormat::Lines {
        separator: separator.to_string(),
    }
}

fn json_format(
    items: &str,
    id: &str,
    label: Option<&str>,
    filter: Option<JsonFilter>,
) -> OutputFormat {
    OutputFormat::Json {
        items: items.to_string(),
        id: id.to_string(),
        label: label.map(str::to_string),
        filter,
    }
}

fn json_filter(pointer: &str, equals: &str) -> JsonFilter {
    JsonFilter {
        pointer: pointer.to_string(),
        equals: equals.to_string(),
    }
}

fn parse(format: &OutputFormat, stdout: &str) -> Result<Vec<ModelChoice>, ChoicesFailure> {
    parse_choices(format, stdout.as_bytes())
}

// --- lines: one choice per line ------------------------------------------------

#[test]
fn lf_and_crlf_line_endings_both_parse_and_blank_lines_are_skipped() {
    let source = "alpha-id\tAlpha Label\r\n\r\nbeta-id\tBeta Label\ngamma-id\tGamma Label\r\n";
    let result = parse(&lines_format("\t"), source).expect("every non-blank line is well-formed");
    assert_eq!(
        result,
        vec![
            choice("alpha-id", Some("Alpha Label")),
            choice("beta-id", Some("Beta Label")),
            choice("gamma-id", Some("Gamma Label")),
        ]
    );
}

#[test]
fn a_line_splits_only_at_the_first_separator() {
    let result =
        parse(&lines_format(","), "multi-sep-id,Name,Extra").expect("the line is well-formed");
    assert_eq!(result, vec![choice("multi-sep-id", Some("Name,Extra"))]);
}

#[test]
fn a_line_without_a_separator_keeps_only_the_id() {
    let result = parse(&lines_format("\t"), "solo-id").expect("a bare id is well-formed");
    assert_eq!(result, vec![choice("solo-id", None)]);
}

#[test]
fn an_id_breaking_the_launch_option_value_rule_drops_its_whole_line() {
    let source = "bad id\tSome Label\nsolo-good-id\tGood Label\n";
    let result = parse(&lines_format("\t"), source).expect("the well-formed line remains");
    assert_eq!(result, vec![choice("solo-good-id", Some("Good Label"))]);
}

#[test]
fn a_label_breaking_the_window_name_rule_keeps_only_the_id() {
    let overlong_label = "y".repeat(200);
    let source = format!("long-label-id\t{overlong_label}");
    let result = parse(&lines_format("\t"), &source).expect("the id alone is well-formed");
    assert_eq!(result, vec![choice("long-label-id", None)]);
}

#[test]
fn a_duplicated_id_keeps_only_its_first_occurrence() {
    let source = "dup-id\tFirst Label\ndup-id\tSecond Label\n";
    let result = parse(&lines_format("\t"), source).expect("the lines are well-formed");
    assert_eq!(result, vec![choice("dup-id", Some("First Label"))]);
}

const REAL_LISTING_SHAPED_LINES: [(&str, &str); 14] = [
    ("model-aurora", "Aurora"),
    ("model-basalt", "Basalt"),
    ("model-cobalt", "Cobalt"),
    ("model-driftwood", "Driftwood"),
    ("model-ember", "Ember"),
    ("model-flint", "Flint"),
    ("model-granite", "Granite"),
    ("model-harbor", "Harbor"),
    ("model-indigo", "Indigo"),
    ("model-jasper", "Jasper"),
    ("model-kestrel", "Kestrel"),
    ("model-lumen", "Lumen"),
    ("model-marrow", "Marrow"),
    ("model-nectar", "Nectar"),
];

#[test]
fn fourteen_lines_shaped_like_the_real_listing_all_parse() {
    let source = REAL_LISTING_SHAPED_LINES
        .iter()
        .map(|(id, label)| format!("{id}\t{label}"))
        .collect::<Vec<_>>()
        .join("\n");
    let result = parse(&lines_format("\t"), &source).expect("all fourteen lines are well-formed");
    let expected: Vec<ModelChoice> = REAL_LISTING_SHAPED_LINES
        .iter()
        .map(|(id, label)| choice(id, Some(label)))
        .collect();
    assert_eq!(result, expected);
}

// --- json: read through pointers -----------------------------------------------

#[test]
fn json_element_with_a_non_string_id_is_dropped() {
    let source = r#"{"models":[{"slug":123,"display_name":"NumId"},{"slug":"valid-one","display_name":"Valid"}]}"#;
    let format = json_format("/models", "/slug", Some("/display_name"), None);
    let result = parse(&format, source).expect("the well-formed element remains");
    assert_eq!(result, vec![choice("valid-one", Some("Valid"))]);
}

#[test]
fn json_missing_label_key_keeps_only_the_id() {
    let source = r#"{"models":[{"slug":"valid-two"}]}"#;
    let format = json_format("/models", "/slug", Some("/display_name"), None);
    let result = parse(&format, source).expect("the element is well-formed");
    assert_eq!(result, vec![choice("valid-two", None)]);
}

#[test]
fn json_label_that_is_not_a_string_keeps_only_the_id() {
    let source = r#"{"models":[{"slug":"valid-three","display_name":42}]}"#;
    let format = json_format("/models", "/slug", Some("/display_name"), None);
    let result = parse(&format, source).expect("the element is well-formed");
    assert_eq!(result, vec![choice("valid-three", None)]);
}

#[test]
fn json_label_breaking_the_window_name_rule_keeps_only_the_id() {
    let overlong = "z".repeat(200);
    let source = format!(r#"{{"models":[{{"slug":"valid-four","display_name":"{overlong}"}}]}}"#);
    let format = json_format("/models", "/slug", Some("/display_name"), None);
    let result = parse(&format, &source).expect("the element is well-formed");
    assert_eq!(result, vec![choice("valid-four", None)]);
}

const VISIBILITY_SHAPED_JSON: &str = r#"{"models":[
    {"slug":"model-has-list","display_name":"Alpha","visibility":"list"},
    {"slug":"model-no-visibility-field","display_name":"Beta"},
    {"slug":"model-wrong-string","display_name":"Gamma","visibility":"preview"},
    {"slug":"model-non-string-visibility","display_name":"Delta","visibility":123}
]}"#;

#[test]
fn a_filter_keeps_only_elements_whose_pointer_value_equals_the_configured_string() {
    let format = json_format(
        "/models",
        "/slug",
        Some("/display_name"),
        Some(json_filter("/visibility", "list")),
    );
    let result =
        parse(&format, VISIBILITY_SHAPED_JSON).expect("the matching element is well-formed");
    assert_eq!(result, vec![choice("model-has-list", Some("Alpha"))]);
}

#[test]
fn without_a_filter_every_valid_element_is_kept_regardless_of_visibility() {
    let format = json_format("/models", "/slug", Some("/display_name"), None);
    let result = parse(&format, VISIBILITY_SHAPED_JSON).expect("every element is well-formed");
    assert_eq!(
        result,
        vec![
            choice("model-has-list", Some("Alpha")),
            choice("model-no-visibility-field", Some("Beta")),
            choice("model-wrong-string", Some("Gamma")),
            choice("model-non-string-visibility", Some("Delta")),
        ]
    );
}

const REAL_JSON_LISTING_SHAPED_MODELS: &str = r#"{"models":[
    {"slug":"model-list-one","display_name":"List One","visibility":"list"},
    {"slug":"model-hidden-one","display_name":"Hidden One","visibility":"hide"},
    {"slug":"model-list-two","display_name":"List Two","visibility":"list"},
    {"slug":"model-hidden-two","display_name":"Hidden Two","visibility":"hide"}
]}"#;

#[test]
fn json_shaped_like_the_real_listing_excludes_hidden_models_and_keeps_every_listed_one() {
    let format = json_format(
        "/models",
        "/slug",
        Some("/display_name"),
        Some(json_filter("/visibility", "list")),
    );
    let result =
        parse(&format, REAL_JSON_LISTING_SHAPED_MODELS).expect("the listed models are well-formed");
    assert_eq!(
        result,
        vec![
            choice("model-list-one", Some("List One")),
            choice("model-list-two", Some("List Two")),
        ]
    );
}

// --- zero and over-the-limit counts ---------------------------------------------

#[test]
fn empty_stdout_yields_zero_choices_and_fails_as_format_mismatch() {
    let err = parse(&lines_format("\t"), "").expect_err("no lines means no choices");
    assert!(matches!(err, ChoicesFailure::FormatMismatch));
    assert_eq!(err.to_string(), "output did not match the profile's format");
}

#[test]
fn lines_that_all_break_the_id_rule_leave_zero_choices_and_fail_as_format_mismatch() {
    let source = "has a space\tOne\n-leading-dash\tTwo\n";
    let err = parse(&lines_format("\t"), source).expect_err("no line has a valid id");
    assert!(matches!(err, ChoicesFailure::FormatMismatch));
}

#[test]
fn a_json_items_pointer_that_does_not_point_at_an_array_fails_as_format_mismatch() {
    let format = json_format("/models", "/slug", None, None);
    let err = parse(&format, r#"{"models": "not-an-array"}"#).expect_err("items must be an array");
    assert!(matches!(err, ChoicesFailure::FormatMismatch));
}

#[test]
fn stdout_that_is_not_utf8_fails_as_format_mismatch() {
    let stdout: &[u8] = &[0xFF, 0xFE, 0xFD];
    let err =
        parse_choices(&lines_format("\t"), stdout).expect_err("invalid UTF-8 must be refused");
    assert!(matches!(err, ChoicesFailure::FormatMismatch));
    assert_eq!(err.to_string(), "output did not match the profile's format");
}

#[test]
fn stdout_that_is_not_valid_json_fails_as_format_mismatch() {
    let format = json_format("/models", "/slug", None, None);
    let err = parse(&format, "{ this is leak-sentinel-garbage not json")
        .expect_err("unparsable JSON must be refused");
    assert!(matches!(err, ChoicesFailure::FormatMismatch));
    assert!(
        !err.to_string().contains("leak-sentinel"),
        "the failure text must not echo anything read from the output: {err}"
    );
}

fn bare_id_lines(count: usize) -> String {
    (0..count)
        .map(|n| format!("choice-{n:04}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn exactly_the_maximum_choice_count_is_accepted() {
    let source = bare_id_lines(MAX_CHOICES);
    let result =
        parse(&lines_format("\t"), &source).expect("the maximum count must still be accepted");
    assert_eq!(result.len(), MAX_CHOICES);
}

#[test]
fn one_past_the_maximum_choice_count_fails_as_too_many_rather_than_being_truncated() {
    let source = bare_id_lines(MAX_CHOICES + 1);
    let err = parse(&lines_format("\t"), &source).expect_err("one past the maximum must fail");
    assert!(matches!(err, ChoicesFailure::TooMany));
    assert_eq!(err.to_string(), "lists more models than accepted");
}

// --- the one-line reason for a listing command that could not be run at all ------

#[test]
fn exit_code_127_in_a_wsl_shaped_argv_reads_as_the_final_argv_elements_name() {
    let argv = strings(&["wsl", "-e", "bash", "-lic", "exec \"$0\" \"$@\"", "agy"]);
    let failure = failure_for_exit(Some(127), &argv);
    assert!(matches!(failure, ChoicesFailure::NotFound { .. }));
    assert_eq!(failure.to_string(), "agy was not found");
}

#[test]
fn exit_code_127_in_a_native_argv_reads_as_the_program_name_without_its_directory() {
    let argv = strings(&["/leak-sentinel/dir/tool"]);
    let failure = failure_for_exit(Some(127), &argv);
    assert!(matches!(failure, ChoicesFailure::NotFound { .. }));
    let text = failure.to_string();
    assert_eq!(text, "tool was not found");
    assert!(!text.contains("leak-sentinel"));
}

#[test]
fn a_non_127_exit_code_reads_as_exited_with_status() {
    let argv = strings(&["some-program"]);
    let failure = failure_for_exit(Some(2), &argv);
    assert!(matches!(failure, ChoicesFailure::ExitStatus(2)));
    assert_eq!(failure.to_string(), "exited with status 2");
}

#[test]
fn no_exit_code_reads_as_exited_without_code() {
    let argv = strings(&["leak-sentinel-program"]);
    let failure = failure_for_exit(None, &argv);
    assert!(matches!(failure, ChoicesFailure::ExitedWithoutCode));
    assert!(!failure.to_string().contains("leak-sentinel"));
}

#[test]
fn a_spawn_error_reads_as_not_found_naming_only_the_programs_file_name() {
    let argv = strings(&["/leak-sentinel/dir/spawn-tool", "extra-arg"]);
    let io_err = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "leak-sentinel-io-message",
    );
    let failure = failure_for_spawn_error(&io_err, &argv);
    assert!(matches!(failure, ChoicesFailure::NotFound { .. }));
    let text = failure.to_string();
    assert_eq!(text, "spawn-tool was not found");
    assert!(!text.contains("leak-sentinel"));
}

// --- fixed phrases for the failures parse_choices does not construct -------------

#[test]
fn timed_out_reads_as_a_fixed_duration_sentence() {
    let failure = ChoicesFailure::TimedOut { secs: 20 };
    assert_eq!(failure.to_string(), "did not finish within 20 seconds");
}

#[test]
fn output_too_large_reads_as_a_fixed_phrase() {
    assert_eq!(
        ChoicesFailure::OutputTooLarge.to_string(),
        "output too large"
    );
}

#[test]
fn an_unavailable_reason_passes_through_verbatim() {
    let failure = ChoicesFailure::Unavailable("this profile is unavailable".to_string());
    assert_eq!(failure.to_string(), "this profile is unavailable");
}
