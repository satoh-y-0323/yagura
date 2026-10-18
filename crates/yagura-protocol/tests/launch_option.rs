#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the wire and validation contract of the two launch
//! options a launch profile can map (`model` and `effort`), fixed here as
//! public contracts so no caller restates them.
//!
//! Three things are pinned:
//!
//! - `validate_launch_option_value` enforces the argv token rule: a supplied
//!   value becomes one argv element handed to a launcher that may delegate
//!   its arguments to a shell (a `wsl --` wrapper, a `.cmd` shim), so it
//!   admits only ASCII letters, digits and `. _ - : / + @`, and refuses a
//!   leading `-` so a value cannot inject a flag. This is a distinct rule
//!   from the window-label rule (`validate_window_name`), with its own error
//!   type; the tests below also pin the two rules apart where they disagree.
//! - The two shared argument validators gain one more question each -- "was a
//!   launch option supplied?" -- with a fixed place in their existing order,
//!   so a caller cannot reach a state where a value is accepted for a request
//!   shape that has nowhere to put it.
//! - The new message fields are optional on the wire in both directions: a
//!   frame written before they existed still decodes, and an absent value is
//!   carried as `null` exactly like its sibling optional fields.

use yagura_protocol::{
    ClientMessage, HandoffArgError, LAUNCH_OPTION_VALUE_MAX_BYTES, LaunchOptionInfo,
    LaunchOptionValueError, OpenWindowExclusivityError, ProfileInfo, WINDOW_NAME_MAX_BYTES,
    decode_client_message, encode_client_message, validate_handoff_target,
    validate_launch_option_value, validate_open_window_exclusivity, validate_window_name,
};

// ===========================================================================
// validate_launch_option_value: the argv token rule, applied to a value a
// caller supplies for one of the two launch options.
// ===========================================================================

/// Values the argv token rule accepts: a plain token, a token exercising
/// every admitted punctuation character, and a value exactly at the byte
/// limit.
fn accepted_values() -> Vec<String> {
    vec![
        "alpha".to_string(),
        "tier-a".to_string(),
        "a.b_c:d/e+f@g".to_string(),
        "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES),
    ]
}

/// Values the argv token rule rejects: empty, one byte over the limit, a
/// leading `-` (both a long-flag and a short-flag shape), every shell or
/// launcher metacharacter and expansion character the rule was written to
/// exclude, a C0 control, the two line terminators, a bidirectional
/// override, a zero-width joiner, and a non-ASCII letter.
fn rejected_values() -> Vec<String> {
    vec![
        String::new(),
        "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES + 1),
        "--flag".to_string(),
        "-x".to_string(),
        "a b".to_string(),
        "a$b".to_string(),
        "a`b".to_string(),
        "a;b".to_string(),
        "a|b".to_string(),
        "a&b".to_string(),
        "a(b".to_string(),
        "a)b".to_string(),
        "a<b".to_string(),
        "a>b".to_string(),
        "a\"b".to_string(),
        "a'b".to_string(),
        "a\\b".to_string(),
        "a*b".to_string(),
        "a?b".to_string(),
        "a[b".to_string(),
        "a]b".to_string(),
        "a{b".to_string(),
        "a}b".to_string(),
        "a~b".to_string(),
        "a!b".to_string(),
        "a#b".to_string(),
        "a=b".to_string(),
        "a%b".to_string(),
        "a^b".to_string(),
        "a\u{0007}b".to_string(),
        "a\rb".to_string(),
        "a\nb".to_string(),
        "a\u{202E}b".to_string(),
        "a\u{200D}b".to_string(),
        "a\u{00E9}b".to_string(),
    ]
}

#[test]
fn validate_launch_option_value_accepts_the_argv_token_set() {
    for value in accepted_values() {
        assert_eq!(
            validate_launch_option_value(&value),
            Ok(()),
            "a value made only of the admitted characters must be accepted: {value:?}"
        );
    }
}

#[test]
fn validate_launch_option_value_refuses_a_value_outside_the_argv_token_set() {
    for value in rejected_values() {
        assert!(
            validate_launch_option_value(&value).is_err(),
            "a value outside the admitted character set must be refused: {value:?}"
        );
    }
}

#[test]
fn validate_launch_option_value_reports_the_launch_option_value_error_type() {
    assert_eq!(
        validate_launch_option_value(""),
        Err(LaunchOptionValueError::Empty)
    );
    let too_long = "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES + 1);
    assert_eq!(
        validate_launch_option_value(&too_long),
        Err(LaunchOptionValueError::TooLong {
            max: LAUNCH_OPTION_VALUE_MAX_BYTES,
            actual: LAUNCH_OPTION_VALUE_MAX_BYTES + 1,
        })
    );
    assert_eq!(
        validate_launch_option_value("--flag"),
        Err(LaunchOptionValueError::LeadingDash)
    );
    assert_eq!(
        validate_launch_option_value("-x"),
        Err(LaunchOptionValueError::LeadingDash)
    );
    assert_eq!(
        validate_launch_option_value("ab$c"),
        Err(LaunchOptionValueError::ForbiddenChar {
            at: 2,
            code: '$' as u32,
        }),
        "the offending character is reported by code point and byte offset, \
         never echoed back verbatim"
    );
}

#[test]
fn validate_launch_option_value_accepts_the_maximum_byte_length_and_refuses_one_more() {
    let at_limit = "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES);
    assert_eq!(validate_launch_option_value(&at_limit), Ok(()));
    let over_limit = "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES + 1);
    assert!(
        validate_launch_option_value(&over_limit).is_err(),
        "the limit is the shared constant, not a number restated for this rule"
    );
}

#[test]
fn validate_launch_option_value_checks_empty_then_length_then_leading_dash_then_characters() {
    let long_and_dashed = format!("-{}", "x".repeat(LAUNCH_OPTION_VALUE_MAX_BYTES));
    let long_and_dashed_len = long_and_dashed.len();
    assert_eq!(
        validate_launch_option_value(&long_and_dashed),
        Err(LaunchOptionValueError::TooLong {
            max: LAUNCH_OPTION_VALUE_MAX_BYTES,
            actual: long_and_dashed_len,
        }),
        "the length check runs before the leading-dash check"
    );
    assert_eq!(
        validate_launch_option_value("-$"),
        Err(LaunchOptionValueError::LeadingDash),
        "the leading-dash check runs before the per-character check"
    );
}

#[test]
fn validate_launch_option_value_disagrees_with_the_window_label_rule_on_percent_and_zwj() {
    assert!(validate_window_name("a%b").is_ok());
    assert!(validate_launch_option_value("a%b").is_err());
    assert!(validate_window_name("a\u{200D}b").is_ok());
    assert!(validate_launch_option_value("a\u{200D}b").is_err());
}

#[test]
fn the_launch_option_value_byte_limit_matches_the_window_name_byte_limit() {
    assert_eq!(LAUNCH_OPTION_VALUE_MAX_BYTES, WINDOW_NAME_MAX_BYTES);
}

// ===========================================================================
// validate_open_window_exclusivity: a launch option only means something for
// the profile form of the request.
// ===========================================================================

#[test]
fn open_window_refuses_a_launch_option_without_a_profile() {
    assert_eq!(
        validate_open_window_exclusivity(true, None, None, None, true),
        Err(OpenWindowExclusivityError::LaunchOptionWithoutProfile),
        "an explicit command carries no profile to map the value through, so \
         the request is refused rather than silently launched without it"
    );
}

#[test]
fn open_window_accepts_a_launch_option_alongside_a_profile() {
    assert_eq!(
        validate_open_window_exclusivity(false, Some("alpha"), None, None, true),
        Ok(())
    );
    assert_eq!(
        validate_open_window_exclusivity(false, Some("alpha"), None, None, false),
        Ok(()),
        "a profile form without a launch option is unchanged by the new question"
    );
}

#[test]
fn open_window_reports_neither_provided_before_a_launch_option_without_a_profile() {
    assert_eq!(
        validate_open_window_exclusivity(false, None, None, None, true),
        Err(OpenWindowExclusivityError::NeitherProvided),
        "a request naming nothing to launch is answered by the older, more \
         basic complaint first"
    );
}

#[test]
fn open_window_keeps_its_existing_order_when_a_launch_option_is_present() {
    assert_eq!(
        validate_open_window_exclusivity(false, Some(""), None, None, true),
        Err(OpenWindowExclusivityError::EmptyProfile)
    );
    assert_eq!(
        validate_open_window_exclusivity(true, Some("alpha"), None, None, true),
        Err(OpenWindowExclusivityError::BothProvided)
    );
    assert_eq!(
        validate_open_window_exclusivity(false, Some("alpha"), Some("beta"), None, true),
        Err(OpenWindowExclusivityError::DetectionWithProfile)
    );
    assert_eq!(
        validate_open_window_exclusivity(false, Some("alpha"), None, Some("gamma"), true),
        Err(OpenWindowExclusivityError::VendorWithProfile)
    );
}

#[test]
fn the_launch_option_exclusivity_error_explains_which_form_accepts_it() {
    assert_eq!(
        OpenWindowExclusivityError::LaunchOptionWithoutProfile.to_string(),
        "model and effort can only be specified with profile"
    );
}

// ===========================================================================
// validate_handoff_target: the same question for a handoff, where the
// competing form addresses a window that is already running.
// ===========================================================================

#[test]
fn a_handoff_refuses_a_launch_option_addressed_to_an_existing_window() {
    assert_eq!(
        validate_handoff_target(Some("w1"), None, None, None, true),
        Err(HandoffArgError::WindowIdWithLaunchOption),
        "a window that is already running was launched with whatever options \
         it was launched with; a value here has nothing to act on"
    );
}

#[test]
fn a_handoff_accepts_a_launch_option_alongside_a_profile() {
    assert_eq!(
        validate_handoff_target(None, Some("alpha"), None, None, true),
        Ok(())
    );
    assert_eq!(
        validate_handoff_target(Some("w1"), None, None, None, false),
        Ok(()),
        "a window-addressed handoff without a launch option is unchanged"
    );
}

#[test]
fn a_handoff_reports_the_older_window_id_complaints_before_the_launch_option_one() {
    assert_eq!(
        validate_handoff_target(None, None, None, None, true),
        Err(HandoffArgError::NeitherProvided)
    );
    assert_eq!(
        validate_handoff_target(Some("w1"), Some("alpha"), None, None, true),
        Err(HandoffArgError::BothProvided)
    );
    assert_eq!(
        validate_handoff_target(Some("w1"), None, Some("/work"), None, true),
        Err(HandoffArgError::WindowIdWithCwd)
    );
    assert_eq!(
        validate_handoff_target(Some("w1"), None, None, Some("reviewer"), true),
        Err(HandoffArgError::WindowIdWithName),
        "the launch-option complaint sits directly after this one, so a request \
         carrying both is answered by the name rule"
    );
}

#[test]
fn the_handoff_launch_option_error_explains_which_form_accepts_it() {
    assert_eq!(
        HandoffArgError::WindowIdWithLaunchOption.to_string(),
        "model and effort cannot be specified with window_id"
    );
}

// ===========================================================================
// ClientMessage: the new fields are optional in both directions.
// ===========================================================================

#[test]
fn open_window_from_profile_decodes_a_frame_written_before_the_fields_existed() {
    let body = br#"{"type":"open_window_from_profile","profile":"alpha","cwd":null,"name":null}"#;
    let decoded = decode_client_message(body).expect("a frame without the new keys must decode");
    match decoded {
        ClientMessage::OpenWindowFromProfile { model, effort, .. } => {
            assert_eq!(model, None);
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[test]
fn open_window_from_profile_decodes_both_supplied_values() {
    let body = br#"{"type":"open_window_from_profile","profile":"alpha","cwd":null,"name":null,"model":"tier-a","effort":"tier-b"}"#;
    let decoded = decode_client_message(body).expect("a frame with both values must decode");
    match decoded {
        ClientMessage::OpenWindowFromProfile {
            profile,
            model,
            effort,
            ..
        } => {
            assert_eq!(profile, "alpha");
            assert_eq!(model.as_deref(), Some("tier-a"));
            assert_eq!(effort.as_deref(), Some("tier-b"));
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[test]
fn open_window_from_profile_writes_an_absent_value_as_null_like_its_siblings() {
    let msg = ClientMessage::OpenWindowFromProfile {
        profile: "alpha".to_string(),
        cwd: None,
        name: None,
        model: None,
        effort: None,
        on_name_taken: Default::default(),
    };
    let encoded = String::from_utf8(encode_client_message(&msg)).expect("valid UTF-8");
    for key in [
        "\"cwd\":null",
        "\"name\":null",
        "\"model\":null",
        "\"effort\":null",
    ] {
        assert!(
            encoded.contains(key),
            "an absent launch option is written the way the sibling optional \
             fields already are, so the wire shape stays uniform: {key} missing \
             from {encoded}"
        );
    }
}

#[test]
fn open_window_from_profile_round_trips_both_supplied_values() {
    let msg = ClientMessage::OpenWindowFromProfile {
        profile: "alpha".to_string(),
        cwd: None,
        name: None,
        model: Some("tier-a".to_string()),
        effort: Some("tier-b".to_string()),
        on_name_taken: Default::default(),
    };
    let encoded = encode_client_message(&msg);
    let decoded = decode_client_message(&encoded).expect("must decode");
    assert_eq!(decoded, msg);
}

#[test]
fn send_handoff_decodes_a_frame_written_before_the_fields_existed() {
    let body = br#"{"type":"send_handoff","profile":"alpha","cwd":null,"name":null,"reference":"r","note":"t"}"#;
    let decoded = decode_client_message(body).expect("a frame without the new keys must decode");
    match decoded {
        ClientMessage::SendHandoff { model, effort, .. } => {
            assert_eq!(model, None);
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

#[test]
fn send_handoff_writes_an_absent_value_as_null_and_round_trips_a_supplied_one() {
    let absent = ClientMessage::SendHandoff {
        window_id: None,
        profile: Some("alpha".to_string()),
        cwd: None,
        name: None,
        model: None,
        effort: None,
        reference: "r".to_string(),
        note: "t".to_string(),
    };
    let encoded = String::from_utf8(encode_client_message(&absent)).expect("valid UTF-8");
    assert!(encoded.contains("\"model\":null"), "got {encoded}");
    assert!(encoded.contains("\"effort\":null"), "got {encoded}");

    let supplied = ClientMessage::SendHandoff {
        window_id: None,
        profile: Some("alpha".to_string()),
        cwd: None,
        name: None,
        model: Some("tier-a".to_string()),
        effort: Some("tier-c".to_string()),
        reference: "r".to_string(),
        note: "t".to_string(),
    };
    let decoded = decode_client_message(&encode_client_message(&supplied)).expect("must decode");
    assert_eq!(decoded, supplied);
}

// ===========================================================================
// ProfileInfo: what a listing reports about the options a profile accepts.
// ===========================================================================

fn profile_info_with(
    model: Option<LaunchOptionInfo>,
    effort: Option<LaunchOptionInfo>,
) -> ProfileInfo {
    ProfileInfo {
        id: "alpha".to_string(),
        display_name: "Alpha".to_string(),
        detection: None,
        detection_warning: None,
        cwd: None,
        command: vec!["alpha-program".to_string()],
        available: true,
        unavailable_reason: None,
        model,
        effort,
        enabled: true,
        unavailable_kind: None,
        unavailable_guidance: None,
    }
}

#[test]
fn a_profile_that_accepts_no_launch_option_reports_null_for_both() {
    let info = profile_info_with(None, None);
    let encoded = serde_json::to_string(&info).expect("ProfileInfo must serialize");
    assert!(
        encoded.contains("\"model\":null"),
        "an axis the profile does not map is reported as null rather than \
         omitted, the same way the other optional fields are: got {encoded}"
    );
    assert!(encoded.contains("\"effort\":null"), "got {encoded}");
    let decoded: ProfileInfo = serde_json::from_str(&encoded).expect("must decode");
    assert_eq!(decoded, info);
}

#[test]
fn a_profile_info_decodes_a_document_written_before_the_fields_existed() {
    let body = r#"{"id":"alpha","display_name":"Alpha","detection":null,"detection_warning":null,"cwd":null,"command":["alpha-program"],"available":true,"unavailable_reason":null}"#;
    let decoded: ProfileInfo =
        serde_json::from_str(body).expect("a document without the new keys must decode");
    assert_eq!(decoded.model, None);
    assert_eq!(decoded.effort, None);
}

#[test]
fn an_axis_with_no_alias_is_reported_as_an_empty_list_rather_than_a_missing_one() {
    let info = profile_info_with(Some(LaunchOptionInfo { aliases: vec![] }), None);
    let encoded = serde_json::to_string(&info).expect("ProfileInfo must serialize");
    assert!(
        encoded.contains("\"aliases\":[]"),
        "an axis a profile maps but declares no alias for is distinct from an \
         axis it does not map at all: the first is an empty list, the second is \
         null. Got {encoded}"
    );
    let decoded: ProfileInfo = serde_json::from_str(&encoded).expect("must decode");
    assert_eq!(decoded, info);
    assert!(
        decoded
            .model
            .expect("the model axis was supplied")
            .aliases
            .is_empty()
    );
}
