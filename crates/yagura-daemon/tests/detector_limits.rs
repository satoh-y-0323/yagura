#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Resource limits (regex size, rule count) of the detection-definition loader, and the
//! fail-safe behaviour of a window whose definition those limits reject.
//!
//! `size_limit` is proven by an actual rejection: a pattern whose compiled program
//! exceeds it is rejected at load time with `DefinitionError::InvalidPattern`.
//! `dfa_size_limit` bounds the lazy DFA's runtime cache rather than the compiled
//! program, so exceeding it falls back to another engine instead of surfacing an error;
//! it is therefore covered by scanning `definition.rs` for the builder call applying it.
//!
//! The limit constants below are deliberate duplicates of the private ones in
//! `definition.rs` and must be updated whenever those change.

use std::time::Duration;

use tokio::time::sleep;
use yagura_test_support::paths::{unique_detection_dir, write_definition};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::definition::{DefinitionError, parse_definition};
use yagura_daemon::window::{OpenWindowRequest, WindowRegistry};
use yagura_protocol::WindowStatus;

/// Mirror of `definition.rs`'s `MAX_SOURCE_BYTES`.
const MAX_SOURCE_BYTES: usize = 256 * 1024;

/// Mirror of `definition.rs`'s `MAX_RULES`.
const MAX_RULES: usize = 200;

/// Mirror of `definition.rs`'s `REGEX_SIZE_LIMIT_BYTES`, i.e. the value
/// passed to `RegexBuilder::size_limit`.
const REGEX_SIZE_LIMIT_BYTES: usize = 1024 * 1024;

/// A pattern whose compiled program exceeds [`REGEX_SIZE_LIMIT_BYTES`] while its source
/// text stays a few dozen bytes, so the rejection can only come from the regex size
/// limit and never from [`MAX_SOURCE_BYTES`].
const OVER_SIZE_LIMIT_PATTERN: &str = "(?:a{1000}){1000}";

/// Same shape as [`OVER_SIZE_LIMIT_PATTERN`] but small enough to compile within the
/// limit, for the accepting direction of the same assertion.
const WITHIN_SIZE_LIMIT_PATTERN: &str = "(?:a{100}){100}";

/// Marker echoed into a window to move it away from `Unknown` -- only ever
/// matched when the window's definition actually loaded.
const READY_MARKER: &str = "LIMITS-READY-MARK";

/// Builds a single-rule definition around `pattern`.
fn definition_with_pattern(name: &str, pattern: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '''{pattern}'''
priority = 10
region = "bottom"
"#
    )
}

/// Builds a definition holding exactly `count` identical, trivially small
/// rules, so that the only limit it can trip is the rule count.
fn definition_with_rule_count(name: &str, count: usize) -> String {
    let mut source =
        format!("schema_version = 1\nname = \"{name}\"\n\n[debounce]\nconfirm_ms = 30\n");
    for index in 0..count {
        source.push_str(&format!(
            "\n[[rules]]\nstatus = \"running\"\npattern = 'rule-{index}'\npriority = 10\nregion = \"bottom\"\n"
        ));
    }
    source
}

/// A definition that carries the [`READY_MARKER`] rule *and* an over-limit
/// pattern: because the whole document is rejected, the marker rule never
/// takes effect, which is what pins the window to `Unknown`.
fn over_limit_definition_with_marker(name: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '{READY_MARKER}'
priority = 10
region = "bottom"

[[rules]]
status = "idle"
pattern = '''{OVER_SIZE_LIMIT_PATTERN}'''
priority = 20
region = "bottom"
"#
    )
}

/// The accepted counterpart of [`over_limit_definition_with_marker`]: the
/// same marker rule, with a pattern that stays within the size limit.
fn within_limit_definition_with_marker(name: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '{READY_MARKER}'
priority = 10
region = "bottom"

[[rules]]
status = "idle"
pattern = '''{WITHIN_SIZE_LIMIT_PATTERN}'''
priority = 5
region = "bottom"
"#
    )
}

fn echo_request(detection: &str) -> OpenWindowRequest {
    OpenWindowRequest {
        command: vec![
            echo_child_path()
                .to_str()
                .expect("echo_child path must be valid UTF-8")
                .to_string(),
        ],
        cwd: None,
        name: None,
        vendor: None,
        detection: Some(detection.to_string()),
        on_name_taken: Default::default(),
    }
}

#[test]
fn regex_size_limit_rejects_a_pattern_whose_compiled_program_is_too_big() {
    let source = definition_with_pattern("over-size-limit", OVER_SIZE_LIMIT_PATTERN);

    // Preconditions that isolate the limit: the document is far below MAX_SOURCE_BYTES
    // and holds a single rule, so neither SourceTooLarge nor TooManyRules can fire first.
    assert!(
        source.len() <= MAX_SOURCE_BYTES,
        "the probe definition must stay within the {MAX_SOURCE_BYTES}-byte source limit so that \
         the rejection can only come from the regex size limit, but it is {} bytes",
        source.len()
    );

    // A rejected pattern must come back as a typed Err, never as a panic.
    let err = parse_definition(&source)
        .expect_err("an over-size-limit pattern must be rejected, not compiled");

    match &err {
        DefinitionError::InvalidPattern { pattern, .. } => {
            assert_eq!(
                pattern, OVER_SIZE_LIMIT_PATTERN,
                "the rejection must name the offending pattern"
            );
        }
        other => panic!(
            "expected DefinitionError::InvalidPattern for an over-size-limit pattern, got \
             {other:?} -- a different variant would mean the definition was rejected for another \
             reason (source size / rule count) and this test no longer proves the regex limit"
        ),
    }

    let rendered = err.to_string();
    assert!(
        rendered.contains("size limit") && rendered.contains(&REGEX_SIZE_LIMIT_BYTES.to_string()),
        "the rejection must be attributable to the {REGEX_SIZE_LIMIT_BYTES}-byte regex size \
         limit, got {rendered:?}"
    );
}

#[test]
fn regex_size_limit_accepts_a_pattern_that_stays_within_it() {
    let source = definition_with_pattern("within-size-limit", WITHIN_SIZE_LIMIT_PATTERN);

    let definition = parse_definition(&source)
        .expect("a pattern whose compiled program fits the size limit must be accepted");

    assert_eq!(
        definition.rules.len(),
        1,
        "the accepted definition must carry its single compiled rule"
    );
    assert_eq!(
        definition.rules[0].pattern.as_str(),
        WITHIN_SIZE_LIMIT_PATTERN,
        "the compiled rule must retain the source pattern"
    );
}

#[test]
fn rule_count_above_the_limit_is_rejected_as_too_many_rules() {
    let over_limit = MAX_RULES + 1;
    let source = definition_with_rule_count("too-many-rules", over_limit);

    assert!(
        source.len() <= MAX_SOURCE_BYTES,
        "the probe definition must stay within the {MAX_SOURCE_BYTES}-byte source limit so that \
         the rejection can only come from the rule count, but it is {} bytes",
        source.len()
    );

    let err = parse_definition(&source)
        .expect_err("a definition with more rules than the limit must be rejected");

    match &err {
        DefinitionError::TooManyRules { max, actual } => {
            assert_eq!(
                *max, MAX_RULES,
                "the rejection must report the enforced limit"
            );
            assert_eq!(
                *actual, over_limit,
                "the rejection must report the actual rule count"
            );
        }
        other => panic!("expected DefinitionError::TooManyRules, got {other:?}"),
    }
}

#[test]
fn rule_count_at_the_limit_is_accepted() {
    let source = definition_with_rule_count("at-rule-limit", MAX_RULES);

    let definition =
        parse_definition(&source).expect("a definition holding exactly MAX_RULES rules must load");

    assert_eq!(
        definition.rules.len(),
        MAX_RULES,
        "every rule up to the limit must be compiled and kept"
    );
}

#[tokio::test]
async fn window_with_an_over_limit_definition_stays_unknown_and_others_are_unaffected() {
    let dir = unique_detection_dir(
        "yagura-test-limits-detection",
        "over-limit",
        "create temp detection dir",
    );
    write_definition(
        &dir,
        "over-limit-def",
        &over_limit_definition_with_marker("over-limit-def"),
    );
    write_definition(
        &dir,
        "within-limit-def",
        &within_limit_definition_with_marker("within-limit-def"),
    );

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let rejected_id = registry
        .open(echo_request("over-limit-def"))
        .await
        .expect("open with an over-limit definition must not error or panic")
        .window_id;
    let accepted_id = registry
        .open(echo_request("within-limit-def"))
        .await
        .expect("open with a within-limit definition should succeed")
        .window_id;

    let rejected = registry
        .get(&rejected_id)
        .expect("the window must stay registered despite its rejected definition");
    let accepted = registry
        .get(&accepted_id)
        .expect("the second window must be registered");

    assert_eq!(
        rejected.current_status(),
        WindowStatus::Unknown,
        "a definition rejected by a resource limit must fail safe to Unknown"
    );

    // Only the window whose definition loaded can act on the marker; the other must stay
    // Unknown even though its rejected definition declared the very same rule.
    let marker = format!("{READY_MARKER}\r\n");
    rejected
        .session()
        .write_input(marker.as_bytes())
        .await
        .expect("write to the rejected window's pty");
    accepted
        .session()
        .write_input(marker.as_bytes())
        .await
        .expect("write to the accepted window's pty");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if accepted.current_status() == WindowStatus::Running {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the window with a valid definition to detect the marker; \
                 status is {:?}",
                accepted.current_status()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        rejected.current_status(),
        WindowStatus::Unknown,
        "the window with the rejected definition must stay Unknown even after its declared \
         marker rule would have matched"
    );

    let ids: Vec<String> = registry
        .list()
        .into_iter()
        .map(|info| info.window_id)
        .collect();
    assert_eq!(
        ids,
        vec![rejected_id, accepted_id],
        "a rejected definition must not remove or reorder registry entries"
    );

    registry.terminate_all().await;
}
