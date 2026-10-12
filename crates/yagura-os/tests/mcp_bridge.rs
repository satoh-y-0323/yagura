#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the `yagura-os` building blocks the MCP bridge channel
//! needs: sibling-executable resolution with its containment check, plain
//! path conversion, and `WSLENV` merging.
//!
//! None of the functions under test spawn a process or translate a path for
//! WSL: path translation is left to WSL itself via `WSLENV`'s `/p` flag, so
//! this crate offers no translation function and this file adds no test
//! for one.
//!
//! [`resolve_sibling_executable`], [`to_plain_path`] and [`merge_wslenv`] are
//! referenced at the crate root rather than through an internal module path:
//! the crate root is the one name they stay reachable under wherever the
//! module that defines them lives, mirroring how `instance`'s items are
//! re-exported there. [`contains_cmd_metacharacter`] lives next to the
//! private `CMD_METACHARACTERS` constant in `program.rs`, so it is referenced
//! through `program::`.

use std::ffi::OsStr;
use std::path::PathBuf;

use yagura_os::program::{Platform, contains_cmd_metacharacter};
use yagura_os::{merge_wslenv, resolve_sibling_executable, to_plain_path};
use yagura_test_support::paths::TempDir;

// ---------------------------------------------------------------------
// Resolving the executable next to a given directory
// ---------------------------------------------------------------------

#[test]
fn windows_resolve_sibling_executable_finds_the_exe_suffixed_file() {
    let dir = TempDir::new("yagura-mcp-bridge", "windows-present");
    dir.write_file("yagura-mcp.exe", "not a real binary");

    let resolved = resolve_sibling_executable(dir.path(), "yagura-mcp", Platform::Windows);

    assert_eq!(
        resolved.as_ref().and_then(|p| p.file_name()),
        Some(OsStr::new("yagura-mcp.exe")),
        "Windows must probe the given name with an appended .exe, got {resolved:?}"
    );
}

#[test]
fn unix_resolve_sibling_executable_finds_the_exact_named_file() {
    let dir = TempDir::new("yagura-mcp-bridge", "unix-present");
    dir.write_file("yagura-mcp", "not a real binary");

    let resolved = resolve_sibling_executable(dir.path(), "yagura-mcp", Platform::Unix);

    assert_eq!(
        resolved.as_ref().and_then(|p| p.file_name()),
        Some(OsStr::new("yagura-mcp")),
        "Unix must probe the exact given name with no extension appended, got {resolved:?}"
    );
}

#[test]
fn resolve_sibling_executable_returns_none_when_the_candidate_is_absent() {
    let dir = TempDir::new("yagura-mcp-bridge", "absent");

    for platform in [Platform::Windows, Platform::Unix] {
        let resolved = resolve_sibling_executable(dir.path(), "yagura-mcp", platform);
        assert_eq!(
            resolved, None,
            "an absent candidate must resolve to None under {platform:?}"
        );
    }
}

#[test]
fn resolve_sibling_executable_returns_none_when_the_candidate_is_a_directory() {
    let dir = TempDir::new("yagura-mcp-bridge", "candidate-is-dir");
    // Both spellings are created so the same fixture directory covers the
    // Windows (`.exe`-suffixed) and Unix (exact-name) candidate at once.
    std::fs::create_dir(dir.path().join("yagura-mcp.exe"))
        .expect("create directory candidate (windows name)");
    std::fs::create_dir(dir.path().join("yagura-mcp"))
        .expect("create directory candidate (unix name)");

    for platform in [Platform::Windows, Platform::Unix] {
        let resolved = resolve_sibling_executable(dir.path(), "yagura-mcp", platform);
        assert_eq!(
            resolved, None,
            "a directory sitting at the candidate path must not resolve under {platform:?}"
        );
    }
}

/// Windows is excluded (`#[cfg(unix)]`) because creating a symlink there
/// needs a privilege an ordinary test run cannot assume.
#[cfg(unix)]
#[test]
fn unix_resolve_sibling_executable_rejects_a_symlink_whose_real_file_lives_outside_the_directory() {
    let outside = TempDir::new("yagura-mcp-bridge", "escape-outside");
    let real_file = outside.write_file("yagura-mcp", "not a real binary");
    let containing = TempDir::new("yagura-mcp-bridge", "escape-containing");
    std::os::unix::fs::symlink(&real_file, containing.path().join("yagura-mcp"))
        .expect("create a symlink whose real file lives outside the containing directory");

    let resolved = resolve_sibling_executable(containing.path(), "yagura-mcp", Platform::Unix);

    assert_eq!(
        resolved, None,
        "a symlink whose real file lives in a different directory must not resolve, even \
         though the symlink's target is itself a regular file"
    );
}

// ---------------------------------------------------------------------
// Normalizing a path handed out to a child process
// ---------------------------------------------------------------------

#[test]
fn to_plain_path_strips_the_extended_length_prefix_for_a_drive_path() {
    let input = PathBuf::from(r"\\?\C:\Users\yagura\yagura-mcp.exe");

    let resolved = to_plain_path(&input);

    assert_eq!(
        resolved,
        Some(PathBuf::from(r"C:\Users\yagura\yagura-mcp.exe")),
        r"the \\?\ prefix must be stripped for a drive-letter path"
    );
}

#[test]
fn to_plain_path_returns_none_for_the_unrecoverable_unc_extended_length_form() {
    let input = PathBuf::from(r"\\?\UNC\server\share\yagura-mcp.exe");

    let resolved = to_plain_path(&input);

    assert_eq!(
        resolved, None,
        "an extended-length UNC path cannot be handed to a child unchanged, so the \
         caller must stop injecting rather than guess a translation"
    );
}

#[test]
fn to_plain_path_leaves_a_prefixless_path_unchanged() {
    let input = PathBuf::from(r"C:\Users\yagura\yagura-mcp.exe");

    let resolved = to_plain_path(&input);

    assert_eq!(
        resolved,
        Some(input),
        "a path carrying no extended-length prefix must pass through unchanged"
    );
}

// ---------------------------------------------------------------------
// Building the child's WSLENV value
// ---------------------------------------------------------------------

#[test]
fn merge_wslenv_builds_from_a_none_base() {
    let result = merge_wslenv(None, &["YAGURA_WINDOW_ID", "YAGURA_STATUS_TOKEN"]);

    assert_eq!(
        result, "YAGURA_WINDOW_ID:YAGURA_STATUS_TOKEN",
        "a None base must build a colon-separated value from the given names alone"
    );
}

#[test]
fn merge_wslenv_builds_from_an_empty_base() {
    let result = merge_wslenv(Some(""), &["YAGURA_WINDOW_ID"]);

    assert_eq!(
        result, "YAGURA_WINDOW_ID",
        "an empty base must contribute nothing"
    );
}

#[test]
fn merge_wslenv_preserves_existing_items_and_their_order() {
    let result = merge_wslenv(Some("A:B/p"), &["C"]);

    assert_eq!(
        result, "A:B/p:C",
        "existing items and their order must be kept, with new names appended after them"
    );
}

#[test]
fn merge_wslenv_skips_a_name_already_present_ignoring_case_and_flags() {
    // "A" and "b" are already present. "a/p" duplicates "A" once case is
    // ignored; "B/p" duplicates "b" once its own trailing flag and case are
    // both ignored; only "C" is genuinely new.
    let result = merge_wslenv(Some("A:b"), &["a/p", "B/p", "C"]);

    assert_eq!(
        result, "A:b:C",
        "a name already present must not be added again once case and any trailing \
         /flags are ignored for the comparison"
    );
}

#[test]
fn merge_wslenv_drops_empty_items_from_the_base() {
    let result = merge_wslenv(Some("A::B"), &[]);

    assert_eq!(
        result, "A:B",
        "an empty item inside the base must be dropped rather than kept as a bare colon"
    );
}

// ---------------------------------------------------------------------
// The single cmd.exe metacharacter check
// ---------------------------------------------------------------------

/// Independent copy of the cmd.exe metacharacter set, listed
/// verbatim rather than imported from `program`, so this test does not just
/// re-run whatever set the implementation happens to use.
const CMD_METACHARACTERS: [char; 8] = ['%', '&', '|', '<', '>', '^', '"', '\n'];

#[test]
fn contains_cmd_metacharacter_is_true_for_every_reserved_character() {
    for ch in CMD_METACHARACTERS {
        let candidate = format!("safe{ch}value");
        assert!(
            contains_cmd_metacharacter(&candidate),
            "{ch:?} is a reserved cmd.exe metacharacter and must be detected in \
             {candidate:?}"
        );
    }
}

#[test]
fn contains_cmd_metacharacter_is_false_for_a_single_quote_space_and_backslash() {
    for candidate in ["it's", "has space", r"back\slash"] {
        assert!(
            !contains_cmd_metacharacter(candidate),
            "{candidate:?} contains none of the reserved cmd.exe metacharacters"
        );
    }
}
