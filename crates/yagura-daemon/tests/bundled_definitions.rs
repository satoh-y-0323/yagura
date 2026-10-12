#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the bundled detection definitions embedded in the
//! `yagura-daemon` binary at build time.
//!
//! Scope: only the two public accessors in
//! `yagura_daemon::detector::definition` (`bundled_definition_names`,
//! `bundled_definition_source`) and the invariant that every bundled name
//! resolves to a definition that parses and validates. It does not cover
//! non-UTF-8 file names on the crate's `detection/` directory.

use std::collections::BTreeSet;
use std::path::PathBuf;

use yagura_daemon::detector::definition::{
    bundled_definition_names, bundled_definition_source, is_valid_detection_name, parse_definition,
};

/// The crate's own `detection/` directory (`CARGO_MANIFEST_DIR/detection`),
/// read independently of the build script so this test does not just
/// re-check the generator's own output against itself.
fn crate_detection_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("detection")
}

/// Every `.toml` file stem directly under [`crate_detection_dir`].
fn toml_stems_on_disk() -> BTreeSet<String> {
    let dir = crate_detection_dir();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|err| {
        panic!("reading {} failed: {err}", dir.display());
    });
    let mut stems = BTreeSet::new();
    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("toml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .expect("UTF-8 file stem")
            .to_string();
        stems.insert(stem);
    }
    stems
}

#[test]
fn bundled_definition_names_matches_the_crate_detection_directory() {
    let expected = toml_stems_on_disk();
    assert!(
        !expected.is_empty(),
        "fixture assumption: the crate's detection/ directory holds at least one *.toml file"
    );

    let names = bundled_definition_names();
    assert!(!names.is_empty());

    let actual: BTreeSet<String> = names.iter().map(|name| name.to_string()).collect();
    assert_eq!(actual, expected);

    let mut sorted = names.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        names, sorted,
        "bundled_definition_names() must already be in byte order"
    );
}

#[test]
fn every_bundled_name_resolves_to_a_valid_parseable_definition() {
    for name in bundled_definition_names() {
        assert!(
            is_valid_detection_name(name),
            "bundled name {name:?} must satisfy is_valid_detection_name"
        );

        let source = bundled_definition_source(name).unwrap_or_else(|| {
            panic!("bundled_definition_source({name:?}) returned None for a name reported by bundled_definition_names()")
        });

        parse_definition(source)
            .unwrap_or_else(|err| panic!("bundled definition {name:?} failed to parse: {err}"));
    }
}

#[test]
fn bundled_definition_source_returns_none_for_an_unknown_name() {
    assert!(bundled_definition_source("does-not-exist-anywhere").is_none());
    assert!(bundled_definition_source("").is_none());
}

/// A CLI that drops its pending tool calls at the end of a turn without
/// cancelling them is declared in its bundled definition; the others are not.
#[test]
fn bundled_definitions_declare_which_cli_abandons_calls_at_turn_end() {
    for (name, abandons) in [
        ("codex-cli", true),
        ("claude-code", false),
        ("antigravity-cli", false),
        ("generic-shell", false),
    ] {
        let source = bundled_definition_source(name)
            .unwrap_or_else(|| panic!("{name} must be a bundled definition"));
        let definition = parse_definition(source)
            .unwrap_or_else(|err| panic!("bundled definition {name:?} failed to parse: {err}"));
        assert_eq!(
            definition.requests.abandoned_at_turn_end, abandons,
            "{name}: abandoned_at_turn_end"
        );
    }
}

#[test]
fn the_generic_shell_definition_does_not_write_a_requests_table() {
    let source = bundled_definition_source("generic-shell")
        .expect("generic-shell must be a bundled definition");
    assert!(!source.contains("[requests]"));
}
