#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test for `client_config::load_client_config`'s resolution
//! contract: environment variable, then `client.toml`, then
//! the built-in defaults; fail-safe behaviour on every unreadable/malformed
//! file shape; the per-field acceptable-value ranges; and unknown-key
//! tolerance.
//!
//! The acceptable-range bounds asserted below (100..=60_000 ms,
//! 1..=10 factor, 1..=10 attempts) are the documented acceptance ranges, not
//! a re-read of the module's private constants: this file does not import
//! anything from `client_config` beyond its public surface.
//!
//! Design choice this test pins down (the range contract alone does not
//! say at what granularity it applies): an out-of-range field falls back
//! to *that field's* default while the other, in-range fields keep their
//! file values, and no `failure` is reported for it (`ClientConfigFailure`
//! has no range-specific variant). This is the per-field reading of "range outside -> fall back
//! to the default"; a whole-struct reset was the alternative considered.

use std::fs;

use yagura_gui::client_config::{
    ClientConfigFailure, ClientConfigLoadResult, ReconnectConfig, load_client_config,
};
use yagura_test_support::paths::TempDir;

const TEMP_DIR_PREFIX: &str = "yagura-client-config-test";
const CONFIG_FILE_NAME: &str = "client.toml";

/// The acceptance ranges the range tests below measure, independent
/// of the module's own private constants.
const MIN_INITIAL_DELAY_MS: u64 = 100;
const MAX_INITIAL_DELAY_MS: u64 = 60_000;
const MIN_BACKOFF_FACTOR: u32 = 1;
const MAX_BACKOFF_FACTOR: u32 = 10;
const MIN_ATTEMPT_LIMIT: u32 = 1;
const MAX_ATTEMPT_LIMIT: u32 = 10;

/// Mirrors the module's private byte limit (64 KiB) so this file does not
/// depend on it being exported.
const OVERSIZED_FILE_BYTES: usize = 64 * 1024 + 1;

fn dir_with_config(label: &str, contents: &str) -> TempDir {
    let dir = TempDir::new(TEMP_DIR_PREFIX, label);
    dir.write_file(CONFIG_FILE_NAME, contents);
    dir
}

fn full_reconnect_toml(
    auto_respawn: bool,
    initial_delay_ms: u64,
    backoff_factor: u32,
    attempt_limit: u32,
) -> String {
    format!(
        "[reconnect]\nauto_respawn = {auto_respawn}\ninitial_delay_ms = {initial_delay_ms}\nbackoff_factor = {backoff_factor}\nattempt_limit = {attempt_limit}\n"
    )
}

/// Exhaustive match with no wildcard arm: a fifth `ClientConfigFailure`
/// variant added without updating this function fails the build, pinning
/// the failure value domain to exactly the four known kinds.
/// Called from every fail-safe test below on the failure it
/// observed, so each call site also documents which input produced it.
fn assert_closed_to_four_known_failure_kinds(failure: ClientConfigFailure) {
    match failure {
        ClientConfigFailure::NotFound
        | ClientConfigFailure::ReadFailed
        | ClientConfigFailure::InvalidToml
        | ClientConfigFailure::TooLarge => {}
    }
}

// ---------------------------------------------------------------------
// Resolution order (env -> file -> default), env limited to
// the auto_respawn key.
// ---------------------------------------------------------------------

#[test]
fn env_value_overrides_file_for_auto_respawn_key_only() {
    let dir = dir_with_config(
        "order-env-over-file",
        &full_reconnect_toml(true, 5000, 4, 7),
    );

    let result: ClientConfigLoadResult = load_client_config(dir.path(), Some("false"));

    assert!(
        !result.reconnect.auto_respawn,
        "the environment value must win over the file's auto_respawn"
    );
    assert_eq!(
        result.reconnect.initial_delay_ms, 5000,
        "the environment variable carries no other key, so this must come from the file"
    );
    assert_eq!(result.reconnect.backoff_factor, 4);
    assert_eq!(result.reconnect.attempt_limit, 7);
    assert_eq!(result.failure, None);
}

#[test]
fn file_value_used_for_auto_respawn_when_env_absent() {
    let dir = dir_with_config(
        "order-file-over-default",
        &full_reconnect_toml(false, 5000, 4, 7),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect,
        ReconnectConfig {
            auto_respawn: false,
            initial_delay_ms: 5000,
            backoff_factor: 4,
            attempt_limit: 7,
        }
    );
    assert_eq!(result.failure, None);
}

#[test]
fn defaults_used_when_neither_env_nor_file_present() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "order-neither");

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_eq!(result.failure, Some(ClientConfigFailure::NotFound));
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

// ---------------------------------------------------------------------
// An empty environment-variable value counts as unset.
// ---------------------------------------------------------------------

#[test]
fn empty_env_value_falls_through_to_file() {
    let dir = dir_with_config("empty-env-file", &full_reconnect_toml(false, 5000, 4, 7));

    let result = load_client_config(dir.path(), Some(""));

    assert!(
        !result.reconnect.auto_respawn,
        "an empty string must be treated as unset, not as a truthy value"
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.failure, None);
}

#[test]
fn empty_env_value_falls_through_to_default_when_no_file() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "empty-env-no-file");

    let result = load_client_config(dir.path(), Some(""));

    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_eq!(result.failure, Some(ClientConfigFailure::NotFound));
}

// ---------------------------------------------------------------------
// Environment-variable value interpretation.
// ---------------------------------------------------------------------

#[test]
fn env_zero_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-zero");

    let result = load_client_config(dir.path(), Some("0"));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_false_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-false");

    let result = load_client_config(dir.path(), Some("false"));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_one_enables_auto_respawn_overriding_a_file_that_disables_it() {
    let dir = dir_with_config("env-one-over-file", &full_reconnect_toml(false, 5000, 4, 7));

    let result = load_client_config(dir.path(), Some("1"));

    assert!(result.reconnect.auto_respawn);
    assert_eq!(
        result.reconnect.initial_delay_ms, 5000,
        "the other three keys must still come from the file"
    );
    assert_eq!(result.reconnect.backoff_factor, 4);
    assert_eq!(result.reconnect.attempt_limit, 7);
}

#[test]
fn env_arbitrary_nonempty_value_enables_auto_respawn_overriding_a_file_that_disables_it() {
    let dir = dir_with_config(
        "env-arbitrary-over-file",
        &full_reconnect_toml(false, 5000, 4, 7),
    );

    let result = load_client_config(dir.path(), Some("banana"));

    assert!(
        result.reconnect.auto_respawn,
        "any non-empty value other than 0/false must enable"
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
}

// -----------------------------------------------------------------
// Disabling-value recognition must be case-insensitive and
// tolerant of surrounding whitespace, since a Windows `set` invocation
// tends to leave trailing whitespace, while a whitespace-only value must
// still count as unset like the empty string. The
// `0`/`false`/non-empty-other contracts asserted above must not change.
// -----------------------------------------------------------------

#[test]
fn env_uppercase_false_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-upper-false");

    let result = load_client_config(dir.path(), Some("FALSE"));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_mixed_case_false_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-mixed-false");

    let result = load_client_config(dir.path(), Some("False"));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_false_with_trailing_whitespace_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-false-trailing-ws");

    let result = load_client_config(dir.path(), Some("false "));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_zero_with_leading_whitespace_disables_auto_respawn() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "env-zero-leading-ws");

    let result = load_client_config(dir.path(), Some(" 0"));

    assert!(!result.reconnect.auto_respawn);
}

#[test]
fn env_whitespace_only_value_falls_through_to_file_like_empty() {
    let dir = dir_with_config(
        "env-whitespace-only",
        &full_reconnect_toml(false, 5000, 4, 7),
    );

    let result = load_client_config(dir.path(), Some("   "));

    assert!(
        !result.reconnect.auto_respawn,
        "a whitespace-only value must be treated as unset, falling through to the file"
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.failure, None);
}

// Deliberately no "whitespace-only, no file" counterpart to
// `empty_env_value_falls_through_to_default_when_no_file`: with no file,
// every field but `auto_respawn` already resolves to the built-in
// default, and `DEFAULT_AUTO_RESPAWN` is `true` -- the same value a
// buggy implementation that treats "  " as a non-empty *enabling* value
// would also produce. That combination cannot distinguish the fix from
// its absence, so `env_whitespace_only_value_falls_through_to_file_like_empty`
// above (whose file sets `auto_respawn = false`) is the property's only
// non-vacuous test.

// ---------------------------------------------------------------------
// Fail-safe on every unreadable/malformed file shape.
// ---------------------------------------------------------------------

#[test]
fn missing_file_reports_not_found_and_returns_defaults() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "fail-missing");

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, Some(ClientConfigFailure::NotFound));
    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

#[test]
fn directory_in_place_of_file_reports_read_failed_and_returns_defaults() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "fail-read-failed");
    fs::create_dir_all(dir.path().join(CONFIG_FILE_NAME)).expect("create fixture directory");

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, Some(ClientConfigFailure::ReadFailed));
    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

#[test]
fn malformed_toml_reports_invalid_toml_and_returns_defaults() {
    let dir = dir_with_config("fail-malformed", "this is not { valid TOML");

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, Some(ClientConfigFailure::InvalidToml));
    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

#[test]
fn wrong_type_value_reports_invalid_toml_and_returns_defaults() {
    let dir = dir_with_config("fail-wrong-type", "[reconnect]\nauto_respawn = \"yes\"\n");

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, Some(ClientConfigFailure::InvalidToml));
    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

#[test]
fn oversized_file_reports_too_large_and_returns_defaults() {
    // Deliberately not valid TOML: the byte-size check must trip before any
    // attempt to parse, mirroring `keymap.rs`'s own oversized-file test.
    let oversized = "a".repeat(OVERSIZED_FILE_BYTES);
    let dir = dir_with_config("fail-oversized", &oversized);

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, Some(ClientConfigFailure::TooLarge));
    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_closed_to_four_known_failure_kinds(result.failure.expect("asserted Some above"));
}

#[test]
fn file_of_exactly_the_byte_limit_is_read() {
    let body = full_reconnect_toml(false, 5000, 4, 7);
    let limit = OVERSIZED_FILE_BYTES - 1;
    let contents = format!("{body}#{}\n", "x".repeat(limit - body.len() - 2));
    assert_eq!(contents.len(), limit);
    let dir = dir_with_config("size-at-limit", &contents);

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.failure, None);
    assert_eq!(
        result.reconnect,
        ReconnectConfig {
            auto_respawn: false,
            initial_delay_ms: 5000,
            backoff_factor: 4,
            attempt_limit: 7,
        }
    );
}

// ---------------------------------------------------------------------
// Acceptable ranges; out-of-range falls back per field
// (not clamped), both sides of each of the three bounded fields.
// ---------------------------------------------------------------------

#[test]
fn initial_delay_below_minimum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-delay-under",
        &full_reconnect_toml(true, MIN_INITIAL_DELAY_MS - 1, 5, 6),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.initial_delay_ms,
        ReconnectConfig::default().initial_delay_ms
    );
    assert_eq!(
        result.reconnect.backoff_factor, 5,
        "in-range fields must be kept from the file"
    );
    assert_eq!(result.reconnect.attempt_limit, 6);
    assert_eq!(
        result.failure, None,
        "an out-of-range value is not a reported failure"
    );
}

#[test]
fn initial_delay_above_maximum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-delay-over",
        &full_reconnect_toml(true, MAX_INITIAL_DELAY_MS + 1, 5, 6),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.initial_delay_ms,
        ReconnectConfig::default().initial_delay_ms
    );
    assert_eq!(result.reconnect.backoff_factor, 5);
    assert_eq!(result.reconnect.attempt_limit, 6);
    assert_eq!(result.failure, None);
}

#[test]
fn backoff_factor_below_minimum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-factor-under",
        &full_reconnect_toml(true, 5000, MIN_BACKOFF_FACTOR - 1, 6),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.backoff_factor,
        ReconnectConfig::default().backoff_factor
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.reconnect.attempt_limit, 6);
    assert_eq!(result.failure, None);
}

#[test]
fn backoff_factor_above_maximum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-factor-over",
        &full_reconnect_toml(true, 5000, MAX_BACKOFF_FACTOR + 1, 6),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.backoff_factor,
        ReconnectConfig::default().backoff_factor
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.reconnect.attempt_limit, 6);
    assert_eq!(result.failure, None);
}

#[test]
fn attempt_limit_below_minimum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-limit-under",
        &full_reconnect_toml(true, 5000, 5, MIN_ATTEMPT_LIMIT - 1),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.attempt_limit,
        ReconnectConfig::default().attempt_limit
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.reconnect.backoff_factor, 5);
    assert_eq!(result.failure, None);
}

#[test]
fn attempt_limit_above_maximum_falls_back_to_default_for_that_field_only() {
    let dir = dir_with_config(
        "range-limit-over",
        &full_reconnect_toml(true, 5000, 5, MAX_ATTEMPT_LIMIT + 1),
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect.attempt_limit,
        ReconnectConfig::default().attempt_limit
    );
    assert_eq!(result.reconnect.initial_delay_ms, 5000);
    assert_eq!(result.reconnect.backoff_factor, 5);
    assert_eq!(result.failure, None);
}

// ---------------------------------------------------------------------
// Unknown keys are ignored.
// ---------------------------------------------------------------------

#[test]
fn unknown_key_alongside_known_keys_does_not_change_resolution_or_report_failure() {
    let dir = dir_with_config(
        "unknown-key-with-known",
        "[reconnect]\nauto_respawn = false\ninitial_delay_ms = 5000\nbackoff_factor = 4\nattempt_limit = 7\nsurprise_key = \"unexpected\"\n",
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(
        result.reconnect,
        ReconnectConfig {
            auto_respawn: false,
            initial_delay_ms: 5000,
            backoff_factor: 4,
            attempt_limit: 7,
        }
    );
    assert_eq!(result.failure, None);
}

#[test]
fn unknown_key_only_file_yields_defaults_with_no_failure() {
    let dir = dir_with_config(
        "unknown-key-only",
        "[reconnect]\nsurprise_key = \"unexpected\"\n",
    );

    let result = load_client_config(dir.path(), None);

    assert_eq!(result.reconnect, ReconnectConfig::default());
    assert_eq!(
        result.failure, None,
        "a parseable file with only unknown keys is not a failure, unlike a missing file"
    );
}
