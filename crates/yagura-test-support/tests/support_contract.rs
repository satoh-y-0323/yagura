#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The fixtures under test hand out raw endpoints, so pinning their contract
// means connecting a raw client to a raw server here.
#![allow(clippy::disallowed_types)]
// Integration-test target: clippy's `allow-unwrap-in-tests` /
// `allow-expect-in-tests` cover only `#[cfg(test)]` code, so a `tests/` file
// needs this header explicitly.

//! Contract tests for the shared fixtures of `yagura-test-support`.
//!
//! Each assertion pins an observable property the callers of these helpers
//! rely on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use yagura_test_support::{ipc, paths, process, scan};

// =====================================================================
// paths: unique naming, whitespace fail-loud, TempDir, path normalisation
// =====================================================================

#[test]
fn unique_temp_dir_reflects_the_prefix_and_tag_and_creates_the_directory() {
    let dir: PathBuf = paths::unique_temp_dir("yagura-red-temp", "alpha");

    assert!(
        dir.is_dir(),
        "unique_temp_dir must create the directory it returns: {dir:?}"
    );
    let name = dir
        .file_name()
        .expect("temp dir must have a file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("yagura-red-temp-alpha-"),
        "the caller-supplied prefix and tag must lead the directory name: {name}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unique_temp_dir_does_not_collide_on_consecutive_calls() {
    let first = paths::unique_temp_dir("yagura-red-temp", "collide");
    let second = paths::unique_temp_dir("yagura-red-temp", "collide");

    assert_ne!(
        first, second,
        "two calls with the same prefix and tag must still yield distinct directories"
    );

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
#[should_panic(expected = "must not contain whitespace")]
fn unique_temp_dir_fails_loud_when_the_path_contains_whitespace() {
    // The grandchild fixture interpolates a path from this directory into a
    // command line the shell re-parses, where re-quoting is not safe.
    let _ = paths::unique_temp_dir("yagura red temp", "whitespace");
}

#[test]
fn temp_dir_exposes_its_directory_through_path() {
    let dir = paths::TempDir::new("yagura-red-tempdir", "beta");
    let path: &Path = dir.path();

    assert!(
        path.is_dir(),
        "TempDir::new must create the directory it hands out: {path:?}"
    );
    let name = path
        .file_name()
        .expect("temp dir must have a file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("yagura-red-tempdir-beta-"),
        "the caller-supplied prefix and label must lead the directory name: {name}"
    );
}

#[test]
fn temp_dir_write_file_returns_the_path_it_wrote() {
    let dir = paths::TempDir::new("yagura-red-tempdir", "write");

    let written = dir.write_file("keymap.toml", "[bindings]\n");

    assert_eq!(
        written,
        dir.path().join("keymap.toml"),
        "write_file must return the path of the file it created"
    );
    assert_eq!(
        std::fs::read_to_string(&written).expect("written fixture file must be readable"),
        "[bindings]\n"
    );
}

#[test]
fn temp_dir_removes_its_directory_on_drop() {
    let recorded = {
        let dir = paths::TempDir::new("yagura-red-tempdir", "drop");
        let path = dir.path().to_path_buf();
        assert!(
            path.is_dir(),
            "precondition: the directory exists: {path:?}"
        );
        path
    };

    assert!(
        !recorded.exists(),
        "TempDir must remove its directory when dropped: {recorded:?}"
    );
}

#[test]
fn temp_dir_does_not_collide_on_consecutive_instances() {
    let first = paths::TempDir::new("yagura-red-tempdir", "collide");
    let second = paths::TempDir::new("yagura-red-tempdir", "collide");

    assert_ne!(
        first.path(),
        second.path(),
        "two TempDir instances with the same prefix and label must not share a directory"
    );
}

#[test]
#[should_panic(expected = "whitespace")]
fn temp_dir_fails_loud_when_the_path_contains_whitespace() {
    let _ = paths::TempDir::new("yagura-red-tempdir", "with space");
}

#[test]
fn unique_suffix_stays_unique_across_consecutive_calls_in_one_process() {
    const ROUNDS: usize = 2000;

    let mut seen = HashSet::new();
    for _ in 0..ROUNDS {
        seen.insert(paths::unique_suffix());
    }

    assert_eq!(
        seen.len(),
        ROUNDS,
        "a pid + timestamp pair alone collides when consecutive calls land in the same clock \
         tick; the per-process counter is what makes the suffix unique"
    );
}

#[test]
fn unique_suffix_starts_with_the_process_id() {
    let suffix = paths::unique_suffix();

    assert!(
        suffix.starts_with(&format!("{}-", std::process::id())),
        "the suffix must keep the pid as its leading element (parallel `cargo test` binaries \
         share the temp directory): {suffix}"
    );
}

#[test]
fn unique_detection_dir_reflects_the_prefix_and_tag() {
    let dir: PathBuf =
        paths::unique_detection_dir("yagura-red-detection", "gamma", "create temp detection dir");

    assert!(
        dir.is_dir(),
        "unique_detection_dir must create the directory it returns: {dir:?}"
    );
    let name = dir
        .file_name()
        .expect("detection dir must have a file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("yagura-red-detection-gamma-"),
        "the caller-supplied prefix and tag must lead the directory name: {name}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[should_panic(expected = "create temp cwd dir")]
fn unique_detection_dir_reports_the_caller_supplied_context_when_creation_fails() {
    // An interior NUL can never be part of a filesystem path, so creation is
    // guaranteed to fail and must be reported with the caller's own context.
    let _ = paths::unique_detection_dir("yagura-red-detection", "nul\0tag", "create temp cwd dir");
}

#[test]
fn write_definition_writes_a_toml_file_named_after_the_definition() {
    let dir = paths::TempDir::new("yagura-red-defs", "write-definition");

    paths::write_definition(dir.path(), "generic-shell", "schema_version = 1\n");

    let written = dir.path().join("generic-shell.toml");
    assert_eq!(
        std::fs::read_to_string(&written).expect("definition file must be written as <name>.toml"),
        "schema_version = 1\n"
    );
}

#[test]
fn strip_extended_length_prefix_restores_a_unc_path() {
    assert_eq!(
        paths::strip_extended_length_prefix(Path::new(r"\\?\UNC\server\share\dir")),
        PathBuf::from(r"\\server\share\dir")
    );
}

#[test]
fn strip_extended_length_prefix_removes_the_plain_extended_length_prefix() {
    assert_eq!(
        paths::strip_extended_length_prefix(Path::new(r"\\?\C:\foo\bar")),
        PathBuf::from(r"C:\foo\bar")
    );
}

#[test]
fn strip_extended_length_prefix_leaves_an_ordinary_path_untouched() {
    assert_eq!(
        paths::strip_extended_length_prefix(Path::new(r"C:\foo\bar")),
        PathBuf::from(r"C:\foo\bar")
    );
    assert_eq!(
        paths::strip_extended_length_prefix(Path::new("relative/leaf")),
        PathBuf::from("relative/leaf")
    );
}

// =====================================================================
// ipc: endpoint length budget + name-building/fallback wiring.
// Both functions are pure and compiled on every OS, so the macOS-derived
// budget logic is testable off-macOS.
// =====================================================================

#[test]
fn endpoint_candidate_within_budget_is_true_at_the_ninety_five_byte_boundary() {
    let candidate = "a".repeat(95);
    assert!(
        ipc::endpoint_candidate_within_budget(&candidate),
        "a 95-byte candidate is exactly at the macOS-derived budget and must be accepted"
    );
}

#[test]
fn endpoint_candidate_within_budget_is_false_one_byte_over_the_boundary() {
    let candidate = "a".repeat(96);
    assert!(
        !ipc::endpoint_candidate_within_budget(&candidate),
        "a 96-byte candidate is one byte over the 95-byte budget and must be rejected"
    );
}

/// A macOS-length `temp_root` pushes the candidate over the budget, so the
/// name must be rebuilt under the `/tmp` origin. The fixture asserts each
/// input length so a reader can recompute the totals.
#[test]
fn endpoint_name_from_switches_to_the_tmp_origin_when_the_candidate_exceeds_the_budget() {
    let prefix = "yagura-red-endpoint";
    let suffix = "12345-1756700000000000000-0";
    assert_eq!(
        prefix.len(),
        19,
        "fixture precondition: prefix must be 19 bytes"
    );
    assert_eq!(
        suffix.len(),
        27,
        "fixture precondition: suffix must be 27 bytes"
    );
    let temp_root = "/var/folders/ab/cdefghijklmnopqrstuvwxyz0123456789/T";
    assert_eq!(
        temp_root.len(),
        52,
        "fixture precondition: the example macOS-length temp_root must be 52 bytes"
    );

    let name = ipc::endpoint_name_from(
        temp_root,
        || "/tmp".to_string(),
        &format!("ytest-{prefix}-{suffix}"),
    );

    assert_eq!(
        name, "/tmp/ytest-yagura-red-endpoint-12345-1756700000000000000-0/e.sock",
        "a 113-byte candidate (52 + 15 + 19 + 27) exceeds the 95-byte budget and must be \
         replaced by a /tmp-origin candidate (4 + 15 + 19 + 27 = 65 bytes)"
    );
}

/// The same inputs under a `temp_root` that is already `/tmp` stay within
/// budget, so the candidate must be returned unchanged.
#[test]
fn endpoint_name_from_keeps_the_given_origin_when_the_candidate_is_within_budget() {
    let prefix = "yagura-red-endpoint";
    let suffix = "12345-1756700000000000000-0";
    let temp_root = "/tmp";

    let name = ipc::endpoint_name_from(
        temp_root,
        || "/tmp".to_string(),
        &format!("ytest-{prefix}-{suffix}"),
    );

    assert_eq!(
        name, "/tmp/ytest-yagura-red-endpoint-12345-1756700000000000000-0/e.sock",
        "a 65-byte candidate (4 + 15 + 19 + 27) is within the 95-byte budget and must be \
         returned unchanged"
    );
}

// =====================================================================
// paths::short_tmp_dir (Unix only): bounded-origin scratch directory for
// socket-path fixtures. Its panic conditions (budget overflow, whitespace)
// are environment-dependent and are not covered here.
// =====================================================================

#[cfg(unix)]
mod unix_paths {
    use std::os::unix::fs::MetadataExt;

    use yagura_test_support::paths;

    #[test]
    fn short_tmp_dir_originates_under_tmp() {
        let dir = paths::short_tmp_dir("yagura-red-short", "origin");

        let path = dir.path().to_str().expect("utf8 path");
        assert!(
            path.starts_with("/tmp/"),
            "short_tmp_dir must originate under /tmp/: {path}"
        );
    }

    #[test]
    fn short_tmp_dir_grants_no_group_or_other_access() {
        let dir = paths::short_tmp_dir("yagura-red-short", "mode");

        let meta = std::fs::symlink_metadata(dir.path()).expect("the directory must exist");
        assert_eq!(
            meta.mode() & 0o077,
            0,
            "short_tmp_dir must create the directory with mode 0700"
        );
    }

    #[test]
    fn short_tmp_dir_does_not_collide_on_consecutive_calls() {
        let first = paths::short_tmp_dir("yagura-red-short", "collide");
        let second = paths::short_tmp_dir("yagura-red-short", "collide");

        assert_ne!(
            first.path(),
            second.path(),
            "two calls with the same prefix and label must not share a directory"
        );
    }

    #[test]
    fn short_tmp_dir_removes_its_directory_on_drop() {
        let recorded = {
            let dir = paths::short_tmp_dir("yagura-red-short", "drop");
            let path = dir.path().to_path_buf();
            assert!(
                path.is_dir(),
                "precondition: the directory exists: {path:?}"
            );
            path
        };

        assert!(
            !recorded.exists(),
            "short_tmp_dir must remove its directory when dropped: {recorded:?}"
        );
    }
}

// =====================================================================
// scan: substring counting, byte-subsequence search, source-tree walking
// =====================================================================

#[test]
fn count_occurrences_counts_every_non_overlapping_match() {
    assert_eq!(scan::count_occurrences("abcabc", "abc"), 2);
    assert_eq!(
        scan::count_occurrences("aaaa", "aa"),
        2,
        "matches are counted without overlap"
    );
    assert_eq!(scan::count_occurrences("abcabc", "zzz"), 0);
}

#[test]
fn contains_subsequence_is_true_for_an_empty_needle() {
    assert!(scan::contains_subsequence(b"payload", b""));
    assert!(
        scan::contains_subsequence(b"", b""),
        "an empty needle is contained even in an empty haystack"
    );
}

#[test]
fn contains_subsequence_is_false_when_the_haystack_is_shorter_than_the_needle() {
    assert!(!scan::contains_subsequence(b"ab", b"abc"));
    assert!(!scan::contains_subsequence(b"", b"a"));
}

#[test]
fn contains_subsequence_finds_a_present_needle_and_rejects_an_absent_one() {
    assert!(scan::contains_subsequence(b"hello world", b"lo wo"));
    assert!(scan::contains_subsequence(b"hello", b"hello"));
    assert!(!scan::contains_subsequence(b"hello world", b"word"));
}

#[test]
fn resolve_from_anchors_a_relative_path_at_the_given_manifest_directory() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let resolved = scan::resolve_from(manifest_dir, "Cargo.toml");

    assert_eq!(resolved, PathBuf::from(manifest_dir).join("Cargo.toml"));
    assert!(
        resolved.is_file(),
        "the resolved path must address the real manifest: {resolved:?}"
    );
}

// =====================================================================
// process (Windows-only fixtures): the sleepy / exit argv shapes
// =====================================================================

#[cfg(windows)]
mod windows_process {
    use yagura_test_support::process;

    #[test]
    fn sleepy_command_wraps_the_ping_delay_without_spawning_a_grandchild() {
        assert_eq!(
            process::sleepy_command("7"),
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "ping".to_string(),
                "-n".to_string(),
                "7".to_string(),
                "127.0.0.1".to_string(),
                ">nul".to_string(),
            ]
        );
    }

    #[test]
    fn exit_with_code_command_keeps_the_cmd_exe_argv_shape_verbatim() {
        // Pins the exact argv the consolidated call sites relied on,
        // including the `cmd.exe` spelling.
        assert_eq!(
            process::exit_with_code_command("5"),
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "exit".to_string(),
                "5".to_string(),
            ]
        );
    }
}

// =====================================================================
// process (Unix-only fixtures): the sleepy / exit argv shapes
// =====================================================================

#[cfg(not(windows))]
mod unix_process {
    use yagura_test_support::process;

    #[test]
    fn sleepy_command_maps_ping_count_to_seconds_without_spawning_a_grandchild() {
        assert_eq!(
            process::sleepy_command("7"),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 6".to_string(),
            ],
            "ping_count is mapped to max(ping_count - 1, 1) seconds so callers pass the same \
             numbers on both platforms"
        );
    }

    #[test]
    fn sleepy_command_floors_at_one_second() {
        assert_eq!(
            process::sleepy_command("1"),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 1".to_string()
            ],
            "max(ping_count - 1, 1) never yields a zero-second sleep"
        );
    }

    #[test]
    fn exit_with_code_command_keeps_the_sh_c_argv_shape_verbatim() {
        // Unix counterpart of the `cmd.exe` argv-shape assertion.
        assert_eq!(
            process::exit_with_code_command("5"),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 5".to_string(),
            ]
        );
    }
}

// =====================================================================
// ipc (Windows-only): pipe naming and the read outcome value type
// =====================================================================

#[cfg(windows)]
mod windows_ipc {
    use std::collections::HashSet;

    use yagura_protocol::DaemonMessage;
    use yagura_test_support::ipc;

    #[test]
    fn unique_endpoint_name_reflects_the_prefix_and_tag() {
        let name = ipc::unique_endpoint_name("yagura-red-pipe", "delta");

        assert!(
            name.starts_with(r"\\.\pipe\yagura-red-pipe-delta-"),
            "the caller-supplied prefix and tag must lead the pipe name: {name}"
        );
    }

    #[test]
    fn unique_endpoint_name_does_not_collide_on_consecutive_calls() {
        const ROUNDS: usize = 500;

        let mut seen = HashSet::new();
        for _ in 0..ROUNDS {
            seen.insert(ipc::unique_endpoint_name("yagura-red-pipe", "collide"));
        }

        assert_eq!(
            seen.len(),
            ROUNDS,
            "concurrently running test binaries and a resident daemon must never share a pipe \
             name"
        );
    }

    #[test]
    fn expect_message_returns_the_frame_carried_by_a_message_outcome() {
        let outcome = ipc::ReadOutcome::Message(DaemonMessage::Pong);

        assert_eq!(outcome.expect_message("red-ctx"), DaemonMessage::Pong);
    }

    #[test]
    #[should_panic(expected = "red-ctx: connection closed unexpectedly")]
    fn expect_message_fails_loud_with_the_context_when_the_connection_was_closed() {
        let _ = ipc::ReadOutcome::Closed.expect_message("red-ctx");
    }

    #[test]
    fn expect_closed_accepts_a_closed_outcome() {
        // A disconnect is a *value* of the outcome type, not a panic, so a
        // test can assert that the daemon closed the connection.
        ipc::ReadOutcome::Closed.expect_closed("red-ctx");
    }

    #[test]
    #[should_panic(expected = "expected connection to be closed")]
    fn expect_closed_fails_loud_when_a_frame_arrived_instead() {
        ipc::ReadOutcome::Message(DaemonMessage::Pong).expect_closed("red-ctx");
    }
}

// =====================================================================
// ipc (Unix-only): endpoint naming, endpoint security and the read outcome
// value type
// =====================================================================

#[cfg(not(windows))]
mod unix_ipc {
    use std::collections::HashSet;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use yagura_os::{PeerAuth, PlatformIpc};
    use yagura_protocol::DaemonMessage;
    use yagura_test_support::ipc;

    fn current_owner_uid() -> u32 {
        PlatformIpc::default()
            .current_user_id()
            .expect("current_user_id must succeed for the running process")
            .parse()
            .expect("current_user_id must return a decimal uid on Unix")
    }

    #[test]
    fn unique_endpoint_name_ends_in_e_sock() {
        let name = ipc::unique_endpoint_name("yagura-red-endpoint", "delta");

        assert!(
            name.ends_with("/e.sock"),
            "the endpoint name must end in e.sock: {name}"
        );
    }

    #[test]
    fn unique_endpoint_name_does_not_collide_on_consecutive_calls() {
        const ROUNDS: usize = 500;

        let mut seen = HashSet::new();
        for _ in 0..ROUNDS {
            seen.insert(ipc::unique_endpoint_name("yagura-red-endpoint", "collide"));
        }

        assert_eq!(
            seen.len(),
            ROUNDS,
            "concurrently running test binaries and a resident daemon must never share an \
             endpoint directory"
        );
    }

    #[test]
    fn unique_endpoint_name_directory_satisfies_the_bind_security_conditions() {
        // The parent-directory conditions `UnixIpc::bind` checks: exists, not
        // a symlink, a directory, owned by the current user, and 0700.
        let name = ipc::unique_endpoint_name("yagura-red-endpoint", "security");
        let dir = Path::new(&name)
            .parent()
            .expect("endpoint name must have a parent directory");

        let meta = std::fs::symlink_metadata(dir).expect("the directory must exist");
        assert!(!meta.is_symlink(), "the directory must not be a symlink");
        assert!(meta.is_dir(), "the directory must be a directory");
        assert_eq!(
            meta.uid(),
            current_owner_uid(),
            "the directory must be owned by the current user"
        );
        assert_eq!(
            meta.mode() & 0o077,
            0,
            "the directory must grant no group/other access (0700)"
        );
    }

    #[test]
    fn expect_message_returns_the_frame_carried_by_a_message_outcome() {
        let outcome = ipc::ReadOutcome::Message(DaemonMessage::Pong);

        assert_eq!(outcome.expect_message("red-ctx"), DaemonMessage::Pong);
    }

    #[test]
    #[should_panic(expected = "red-ctx: connection closed unexpectedly")]
    fn expect_message_fails_loud_with_the_context_when_the_connection_was_closed() {
        let _ = ipc::ReadOutcome::Closed.expect_message("red-ctx");
    }

    #[test]
    fn expect_closed_accepts_a_closed_outcome() {
        // A disconnect is a *value* of the outcome type, not a panic, so a
        // test can assert that the daemon closed the connection.
        ipc::ReadOutcome::Closed.expect_closed("red-ctx");
    }

    #[test]
    #[should_panic(expected = "expected connection to be closed")]
    fn expect_closed_fails_loud_when_a_frame_arrived_instead() {
        ipc::ReadOutcome::Message(DaemonMessage::Pong).expect_closed("red-ctx");
    }
}

// =====================================================================
// detection: shared control / submit definitions
// =====================================================================

mod detection_contract {
    use toml::Value;
    use yagura_test_support::detection;

    /// Parses `text`; the expected tables below are written out independently
    /// of the generator, so a change on either side shows up as a difference.
    fn parse(text: &str) -> Value {
        text.parse::<Value>()
            .unwrap_or_else(|e| panic!("generated definition must be valid TOML: {e}\n{text}"))
    }

    const CONTROL_CORE: &str = r#"
schema_version = 1
name = "control-test"

[debounce]
confirm_ms = 30

[[rules]]
status = "idle"
pattern = 'CTRL-READY'
priority = 10
region = "bottom"
"#;

    const COMPOSER_TABLE: &str = r#"
[composer]
marker = 'YAGURA-COMPOSER'
separator = ">>"
continuation_prefix = "::"
"#;

    const REQUESTS_TABLE: &str = r#"
[requests]
abandoned_at_turn_end = true
"#;

    #[test]
    fn shared_names_and_markers_have_their_pinned_values() {
        assert_eq!(detection::CONTROL_DETECTION_NAME, "control-test");
        assert_eq!(detection::CONTROL_READY_MARKER, "CTRL-READY");
        assert_eq!(detection::CONTROL_READY_LINE, "CTRL-READY\r\n");
    }

    #[test]
    fn idle_definition_without_head_or_tail_is_the_control_core() {
        let text = detection::idle_definition_toml("control-test", "", "");
        assert_eq!(parse(&text), parse(CONTROL_CORE));
    }

    #[test]
    fn idle_definition_takes_its_name_from_the_argument() {
        let text = detection::idle_definition_toml("other-name", "", "");
        let expected = CONTROL_CORE.replace("name = \"control-test\"", "name = \"other-name\"");
        assert_eq!(parse(&text), parse(&expected));
    }

    #[test]
    fn idle_definition_puts_head_before_schema_version_and_tail_at_the_end() {
        let tail = format!("{REQUESTS_TABLE}{COMPOSER_TABLE}");
        let text =
            detection::idle_definition_toml("control-test", "bottom_line_count = 7\n", &tail);

        let expected = format!("bottom_line_count = 7\n{CONTROL_CORE}{tail}");
        assert_eq!(parse(&text), parse(&expected));

        let head_at = text
            .find("bottom_line_count")
            .expect("head must be present");
        let schema_at = text.find("schema_version").expect("schema_version present");
        assert!(
            head_at < schema_at,
            "a top-level key placed after a table would belong to that table, so head must lead: {text}"
        );
        assert!(
            text.trim_end().ends_with(tail.trim_end()),
            "tail must be the last text of the definition: {text}"
        );
    }

    #[test]
    fn idle_definition_with_a_tail_keeps_the_tail_table_out_of_the_rules() {
        let text = detection::idle_definition_toml("control-test", "", REQUESTS_TABLE);
        let parsed = parse(&text);
        assert_eq!(
            parsed["requests"]["abandoned_at_turn_end"].as_bool(),
            Some(true),
            "the [requests] table must be a top-level table: {text}"
        );
        assert_eq!(parsed["rules"].as_array().map(Vec::len), Some(1));
        assert!(parsed["rules"][0].get("abandoned_at_turn_end").is_none());
    }

    #[test]
    fn composer_section_is_the_shared_triple_as_a_composer_table() {
        assert_eq!(
            parse(&detection::composer_section_toml()),
            parse(COMPOSER_TABLE)
        );
    }

    #[test]
    fn composer_section_can_be_appended_to_any_definition() {
        let text = format!(
            "{}{}",
            detection::idle_definition_toml("control-test", "", ""),
            detection::composer_section_toml()
        );
        assert_eq!(
            parse(&text),
            parse(&format!("{CONTROL_CORE}{COMPOSER_TABLE}"))
        );
    }

    #[test]
    fn composer_section_agrees_with_the_constants_the_fixture_paints() {
        let parsed = parse(&detection::composer_section_toml());
        let table = &parsed["composer"];
        assert_eq!(
            table["marker"].as_str(),
            Some(yagura_test_support::process::COMPOSER_MARKER)
        );
        assert_eq!(
            table["separator"].as_str(),
            Some(yagura_test_support::process::COMPOSER_SEPARATOR)
        );
        assert_eq!(
            table["continuation_prefix"].as_str(),
            Some(yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX)
        );
    }
}

// =====================================================================
// process: the grandchild observation guards
// =====================================================================

/// Appends a byte to `path` every 50 ms for `span`, standing in for a
/// grandchild that is still alive.
fn keep_appending(path: &Path, span: std::time::Duration) -> tokio::task::JoinHandle<()> {
    use std::io::Write as _;

    let path = path.to_path_buf();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + span;
        while tokio::time::Instant::now() < deadline {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open the tick file");
            file.write_all(b".").expect("append a tick");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
}

fn tick_file(tag: &str) -> PathBuf {
    let path = paths::unique_temp_dir("yagura-red-tick", tag).join("tick");
    std::fs::write(&path, b"").expect("create the tick file");
    path
}

#[tokio::test]
async fn wait_until_file_grows_returns_once_the_file_grows() {
    let tick = tick_file("grows");
    let _writer = keep_appending(&tick, std::time::Duration::from_secs(5));

    process::wait_until_file_grows(&tick).await;
}
