#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `yagura-protocol`'s shared `open_window` validation
//! functions, fixed here as public contracts so no caller restates them.
//!
//! The two argv validators are deliberately *not* the same rule, and this
//! file is where that split is pinned:
//!
//! - `validate_argv_nul_free` is the spawn invariant. A NUL byte survives
//!   JSON decoding but can never become an OS process argument, so
//!   `window.rs::validate_open_request` calls this one for both `OpenWindow`
//!   and `OpenWindowFromProfile`. An *empty* element is left alone: it is a
//!   legitimate argument (`cmd /c start ""`).
//! - `validate_argv_elements` is the launch-profile authoring rule. It adds
//!   "no element may be empty" on top of the spawn invariant and is called
//!   only by `profile.rs::parse_profile`, where an empty element in a TOML
//!   file is an authoring mistake rather than a legitimate argument.
//!
//! `validate_open_window_exclusivity` / `OpenWindowExclusivityError` fix the
//! `profile` / `command` mutual-exclusion rule the MCP bridge
//! (`yagura-mcp/src/tools.rs::open_window`) and the Tauri command
//! (`gui/src-tauri/src/commands.rs::open_window`) must both call instead of
//! restating their own copy.

use yagura_protocol::{
    ArgvElementError, OpenWindowExclusivityError, validate_argv_elements, validate_argv_nul_free,
    validate_open_window_exclusivity,
};

// ===========================================================================
// validate_argv_nul_free: the spawn invariant. NUL at any position is
// rejected; an empty element is accepted, because `cmd /c start ""` is a
// legitimate command line and `validate_open_request` must not break it.
// ===========================================================================

#[test]
fn validate_argv_nul_free_accepts_any_nul_free_argv_including_empty_elements() {
    // Whether an argv may have zero elements at all is a caller-level concern
    // ("command must not be empty" in window.rs), so an empty argv passes too.
    let cases: [Vec<String>; 6] = [
        vec![],
        vec!["claude".to_string(), "--resume".to_string()],
        vec![String::new()],
        vec![String::new(), "arg".to_string()],
        vec![
            "cmd".to_string(),
            "/c".to_string(),
            "start".to_string(),
            String::new(),
        ],
        vec!["program".to_string(), String::new(), "arg".to_string()],
    ];
    for argv in cases {
        assert_eq!(
            validate_argv_nul_free(&argv),
            Ok(()),
            "an empty element is an authoring rule of parse_profile, not a spawn              invariant, so {argv:?} must be accepted here"
        );
    }
}

#[test]
fn validate_argv_nul_free_rejects_a_nul_byte_at_any_position() {
    let cases: [Vec<String>; 5] = [
        vec!["\u{0}leading".to_string()],
        vec!["a\u{0}b".to_string()],
        vec!["trailing\u{0}".to_string()],
        vec!["program".to_string(), "a\u{0}b".to_string()],
        vec!["program".to_string(), String::new(), "arg\u{0}".to_string()],
    ];
    for argv in cases {
        assert_eq!(
            validate_argv_nul_free(&argv),
            Err(ArgvElementError::Nul),
            "a NUL byte anywhere in {argv:?} cannot become an OS argument and must be \
             rejected"
        );
    }
}

// ===========================================================================
// validate_argv_elements: the launch-profile authoring rule -- the spawn
// invariant plus "no element may be empty", at any position. TOML's NUL-byte
// escape sequence is well-formed and accepted at the parse level
// (profile_load.rs's own NUL-argv test proves the TOML decoder alone does not
// catch it), so this rejection cannot be delegated to it.
// ===========================================================================

#[test]
fn validate_argv_elements_accepts_an_argv_with_no_empty_or_nul_element() {
    // Whitespace is a legitimate argument character (it is not in the cmd.exe
    // metacharacter set), and an empty argv is the caller's to judge.
    let cases: [Vec<String>; 3] = [
        vec![],
        vec!["claude".to_string(), "--resume".to_string()],
        vec!["program".to_string(), "hello world".to_string()],
    ];
    for argv in cases {
        assert_eq!(validate_argv_elements(&argv), Ok(()), "{argv:?}");
    }
}

#[test]
fn validate_argv_elements_rejects_an_empty_element_at_any_position() {
    let cases: [Vec<String>; 4] = [
        vec![String::new()],
        vec![String::new(), "arg".to_string()],
        vec!["program".to_string(), String::new()],
        vec!["program".to_string(), String::new(), "arg".to_string()],
    ];
    for argv in cases {
        assert_eq!(
            validate_argv_elements(&argv),
            Err(ArgvElementError::Empty),
            "an empty element anywhere in {argv:?} must be rejected"
        );
    }
}

#[test]
fn validate_argv_elements_rejects_a_nul_byte_at_any_position() {
    let cases: [Vec<String>; 4] = [
        vec!["a\u{0}b".to_string()],
        vec!["program".to_string(), "a\u{0}b".to_string()],
        vec!["\u{0}leading".to_string()],
        vec!["trailing\u{0}".to_string()],
    ];
    for argv in cases {
        assert_eq!(
            validate_argv_elements(&argv),
            Err(ArgvElementError::Nul),
            "a NUL byte anywhere in {argv:?} must be rejected, even though it is not empty"
        );
    }
}

// ===========================================================================
// validate_open_window_exclusivity: the full truth table -- profile only /
// command only / both / neither / empty profile / profile+detection /
// profile+vendor.
// ===========================================================================

#[test]
fn validate_open_window_exclusivity_accepts_profile_only() {
    assert_eq!(
        validate_open_window_exclusivity(false, Some("claude-code"), None, None, false),
        Ok(())
    );
}

#[test]
fn validate_open_window_exclusivity_accepts_command_only() {
    assert_eq!(
        validate_open_window_exclusivity(true, None, None, None, false),
        Ok(())
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_both_command_and_profile() {
    assert_eq!(
        validate_open_window_exclusivity(true, Some("claude-code"), None, None, false),
        Err(OpenWindowExclusivityError::BothProvided)
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_neither_command_nor_profile() {
    assert_eq!(
        validate_open_window_exclusivity(false, None, None, None, false),
        Err(OpenWindowExclusivityError::NeitherProvided)
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_an_empty_profile_string() {
    assert_eq!(
        validate_open_window_exclusivity(false, Some(""), None, None, false),
        Err(OpenWindowExclusivityError::EmptyProfile)
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_an_empty_profile_string_even_alongside_a_command() {
    // An empty profile must not decay into "no profile was given", which
    // would launch a command the caller did not mean to launch on its own
    // (mirrors yagura-mcp's existing
    // open_window_rejects_an_empty_profile_string_even_alongside_a_command).
    assert_eq!(
        validate_open_window_exclusivity(true, Some(""), None, None, false),
        Err(OpenWindowExclusivityError::EmptyProfile)
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_a_detection_override_combined_with_a_profile() {
    assert_eq!(
        validate_open_window_exclusivity(
            false,
            Some("claude-code"),
            Some("codex-cli"),
            None,
            false
        ),
        Err(OpenWindowExclusivityError::DetectionWithProfile)
    );
}

#[test]
fn validate_open_window_exclusivity_rejects_a_vendor_override_combined_with_a_profile() {
    assert_eq!(
        validate_open_window_exclusivity(
            false,
            Some("claude-code"),
            None,
            Some("anthropic"),
            false
        ),
        Err(OpenWindowExclusivityError::VendorWithProfile)
    );
}

#[test]
fn validate_open_window_exclusivity_allows_detection_and_vendor_without_a_profile() {
    // `detection` / `vendor` only conflict *with a profile*; a plain command
    // launch may carry either.
    assert_eq!(
        validate_open_window_exclusivity(true, None, Some("codex-cli"), Some("anthropic"), false),
        Ok(())
    );
}
