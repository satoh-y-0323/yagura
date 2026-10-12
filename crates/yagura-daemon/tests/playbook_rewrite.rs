#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: turning a destructive tool's auto-approval, and the
//! plan-review requirement, on or off by rewriting the daemon's playbook file
//! in place.
//!
//! A playbook is written by hand, so every case here compares whole byte
//! strings where a prior byte string exists to compare against: only the
//! edited value's own line (or, for a multi-line array, the one element line
//! touched) may change, and a refused request leaves every byte in place.
//! Where a request has to add a table or key a from-scratch fixture has none
//! of, the check instead compares the parsed policy's other sections, since
//! there is no earlier line to diff against.
//!
//! `set_auto_approve` and `set_plan_approval_required` decide only whether
//! the *file* needs writing, from the file's own current value; whether the
//! daemon's already-running policy needs updating from a write's outcome is a
//! judgement made elsewhere.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use yagura_daemon::control::{ApprovalPolicy, DestructiveTool, parse_policy};
use yagura_daemon::playbook_rewrite::{
    PlaybookRewriteError, PlaybookWrite, absolute_path, set_auto_approve,
    set_plan_approval_required,
};
use yagura_test_support::paths::TempDir;

// --- fixtures -------------------------------------------------------------------

/// The repository's own bundled playbook: a single-line `auto_approve` array
/// with one entry, and a `[plans]` table whose `require_approval` carries a
/// two-line comment directly above the key.
const REAL_PLAYBOOK: &str = include_str!("../../../playbook.toml");

/// A single-line array with one recognised entry, and the same two-line
/// comment the bundled playbook carries above `require_approval`.
const SINGLE_ENTRY_LF: &str = "[approval]\nauto_approve = [\"send_chat\"]\n\n[plans]\n# Whether a new plan waits for the operator to approve it in the GUI before\n# any of its task windows can open. Omitting this also means true.\nrequire_approval = true\n";

/// A single-line array where the same recognised entry is repeated, plus an
/// entry this build does not know.
const DUPLICATE_ENTRY_LF: &str = "[approval]\nauto_approve = [\"close_window\", \"close_window\", \"unlisted-tool\"]\n\n[plans]\nrequire_approval = true\n";

/// A single-line array with no entries.
const EMPTY_ARRAY_LF: &str = "[approval]\nauto_approve = []\n\n[plans]\nrequire_approval = true\n";

/// An array spread across several lines, carrying an unrecognised entry
/// alongside two recognised ones.
const MULTI_LINE_LF: &str = "[approval]\nauto_approve = [\n    \"send_chat\",\n    \"unlisted-tool\",\n    \"close_window\",\n]\n\n[plans]\nrequire_approval = true\n";

/// `[approval]` written as a root-level inline table.
const INLINE_TABLE_LF: &str =
    "approval = { auto_approve = [\"send_chat\"] }\n\n[plans]\nrequire_approval = true\n";

/// `[approval]` written with a root-level dotted key, no table heading.
const DOTTED_KEY_LF: &str =
    "approval.auto_approve = [\"send_chat\"]\n\n[plans]\nrequire_approval = true\n";

/// `[plans]` is present with no `[approval]` table at all.
const NO_APPROVAL_LF: &str = "[plans]\nrequire_approval = true\n";

/// `[approval]` is present with no `[plans]` table at all.
const NO_PLANS_LF: &str = "[approval]\nauto_approve = [\"send_chat\"]\n";

/// `[plans]` is present but empty: no `require_approval` key.
const NO_REQUIRE_KEY_LF: &str = "[approval]\nauto_approve = [\"send_chat\"]\n\n[plans]\n";

/// `require_approval` carries a same-line comment that must survive a value
/// replacement.
const WITH_TRAILING_COMMENT_LF: &str = "[approval]\nauto_approve = [\"send_chat\"]\n\n[plans]\nrequire_approval = true  # kept true by hand\n";

/// Not valid TOML at all (an unclosed table heading).
const MALFORMED_TOML: &str = "[approval\nauto_approve = [\"send_chat\"]\n";

/// A text only a rejected file's own body carries; no error text may repeat
/// it.
const BODY_SENTINEL: &str = "leaked-secret-value";

/// Valid TOML syntax, but an unknown key the schema rejects
/// (`deny_unknown_fields`).
const UNKNOWN_KEY_TOML: &str =
    "[approval]\nauto_approve = [\"send_chat\"]\nbogus_key = \"leaked-secret-value\"\n";

/// How long a case waits between writing a file and calling the operation, so
/// a rewrite would move the file's modification time.
const MTIME_OBSERVATION: Duration = Duration::from_millis(1500);

// --- fixture plumbing ------------------------------------------------------------

/// Writes `source` as a fresh playbook file inside its own temporary
/// directory, returning the tree (kept alive for cleanup) and the file's
/// path.
fn write_playbook(tag: &str, source: &str) -> (TempDir, PathBuf) {
    let tree = TempDir::new("yagura-cli-test", tag);
    let dir = tree.make_dir("cfg");
    let path = dir.join("playbook.toml");
    std::fs::write(&path, source.as_bytes()).expect("fixture playbook should be writable");
    (tree, path)
}

/// A playbook path inside a fresh temporary directory, with no file placed
/// at it.
fn missing_playbook(tag: &str) -> (TempDir, PathBuf) {
    let tree = TempDir::new("yagura-cli-test", tag);
    let dir = tree.make_dir("cfg");
    (tree, dir.join("playbook.toml"))
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("{} should be readable: {err}", path.display()))
}

fn read_text(path: &Path) -> String {
    String::from_utf8(read_bytes(path)).expect("a playbook file is UTF-8")
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .expect("modification time should be readable")
}

fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("directory should be readable")
        .map(|entry| {
            entry
                .expect("directory entry should be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn parsed(source: &str) -> ApprovalPolicy {
    parse_policy(source).unwrap_or_else(|err| panic!("the fixture must parse: {err}"))
}

fn written(result: Result<PlaybookWrite, PlaybookRewriteError>) -> PlaybookWrite {
    match result {
        Ok(outcome) => outcome,
        Err(err) => panic!("the request must be accepted, got: {err}"),
    }
}

fn refused(result: Result<PlaybookWrite, PlaybookRewriteError>) -> PlaybookRewriteError {
    match result {
        Ok(_) => panic!("the request must be refused"),
        Err(err) => err,
    }
}

fn quoted(name: &str) -> String {
    format!("\"{name}\"")
}

fn crlf(source: &str) -> String {
    source.replace('\n', "\r\n")
}

fn has_crlf(source: &str) -> bool {
    source
        .find('\n')
        .is_some_and(|at| source[..at].ends_with('\r'))
}

fn has_bare_lf(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(at, byte)| *byte == b'\n' && (at == 0 || bytes[at - 1] != b'\r'))
}

/// Splits `text` into lines, each keeping its own line ending, so a CRLF file
/// and an LF file compare the same way.
fn lines_with_endings(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// Whether `after` is exactly `before` with one line inserted, returning that
/// line's index and its trimmed text.
fn added_line(before: &str, after: &str) -> Option<(usize, String)> {
    let before_lines = lines_with_endings(before);
    let after_lines = lines_with_endings(after);
    if after_lines.len() != before_lines.len() + 1 {
        return None;
    }
    let mut at = 0;
    while at < before_lines.len() && before_lines[at] == after_lines[at] {
        at += 1;
    }
    if after_lines[at + 1..] == before_lines[at..] {
        Some((at, after_lines[at].trim().to_string()))
    } else {
        None
    }
}

/// The inverse of [`added_line`]: whether `after` is `before` with exactly
/// one line removed.
fn removed_line(before: &str, after: &str) -> Option<(usize, String)> {
    added_line(after, before)
}

/// Whether `before` and `after` have the same number of lines and differ in
/// exactly one of them, returning that line's index and its trimmed text on
/// both sides.
fn changed_line(before: &str, after: &str) -> Option<(usize, String, String)> {
    let before_lines = lines_with_endings(before);
    let after_lines = lines_with_endings(after);
    if before_lines.len() != after_lines.len() {
        return None;
    }
    let mut at = None;
    for (index, (b, a)) in before_lines.iter().zip(after_lines.iter()).enumerate() {
        if b != a {
            if at.is_some() {
                return None;
            }
            at = Some(index);
        }
    }
    at.map(|index| {
        (
            index,
            before_lines[index].trim().to_string(),
            after_lines[index].trim().to_string(),
        )
    })
}

fn without_trailing_comma(line: &str) -> String {
    line.trim_end_matches(',').to_string()
}

fn assert_other_tools_unchanged(
    before: &ApprovalPolicy,
    after: &ApprovalPolicy,
    touched: DestructiveTool,
) {
    for tool in DestructiveTool::ALL {
        if tool == touched {
            continue;
        }
        assert_eq!(
            before.allows(tool),
            after.allows(tool),
            "{tool:?} must not change when only {touched:?} is toggled"
        );
    }
}

fn assert_non_approval_sections_unchanged(before: &ApprovalPolicy, after: &ApprovalPolicy) {
    assert_eq!(
        before.plans, after.plans,
        "the [plans] table must not change"
    );
    assert_eq!(
        before.limits, after.limits,
        "the [limits] table must not change"
    );
    assert_eq!(before.chat, after.chat, "the [chat] table must not change");
}

fn assert_approval_section_unchanged(before: &ApprovalPolicy, after: &ApprovalPolicy) {
    let before_names = before
        .approval
        .as_ref()
        .map(|section| section.auto_approve.clone());
    let after_names = after
        .approval
        .as_ref()
        .map(|section| section.auto_approve.clone());
    assert_eq!(
        before_names, after_names,
        "the [approval] table must not change"
    );
    assert_eq!(
        before.limits, after.limits,
        "the [limits] table must not change"
    );
    assert_eq!(before.chat, after.chat, "the [chat] table must not change");
}

// --- turning a tool on or off: the array's own form ------------------------------

#[test]
fn turning_on_a_tool_in_a_single_line_array_adds_it_to_the_same_line() {
    let (_tree, path) = write_playbook("on-single-line", SINGLE_ENTRY_LF);
    let before = parsed(SINGLE_ENTRY_LF);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, before_line, after_line) = changed_line(SINGLE_ENTRY_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(before_line.contains("auto_approve"));
    assert!(after_line.contains(&quoted("send_chat")));
    assert!(after_line.contains(&quoted("close_window")));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(after_policy.allows(DestructiveTool::CloseWindow));
    assert_other_tools_unchanged(&before, &after_policy, DestructiveTool::CloseWindow);
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

#[test]
fn turning_off_a_tool_removes_every_duplicate_from_a_single_line_array() {
    let (_tree, path) = write_playbook("off-duplicate", DUPLICATE_ENTRY_LF);
    let before = parsed(DUPLICATE_ENTRY_LF);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, false));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, before_line, after_line) = changed_line(DUPLICATE_ENTRY_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(before_line.contains(&quoted("close_window")));
    assert!(
        !after_line.contains("close_window"),
        "every duplicate of the turned-off tool must be gone, got: {after_line:?}"
    );
    assert!(
        after_line.contains(&quoted("unlisted-tool")),
        "an unrecognised entry must be left in place, got: {after_line:?}"
    );
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(!after_policy.allows(DestructiveTool::CloseWindow));
    assert_other_tools_unchanged(&before, &after_policy, DestructiveTool::CloseWindow);
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

#[test]
fn turning_on_a_tool_in_an_empty_array_writes_it_as_the_only_entry() {
    let (_tree, path) = write_playbook("on-empty-array", EMPTY_ARRAY_LF);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::SendMessage, true));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, before_line, after_line) = changed_line(EMPTY_ARRAY_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(before_line.contains("[]"));
    assert!(after_line.contains(&quoted("send_message")));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(after_policy.allows(DestructiveTool::SendMessage));
}

#[test]
fn turning_on_a_tool_in_a_multi_line_array_adds_one_element_line() {
    let (_tree, path) = write_playbook("on-multi-line", MULTI_LINE_LF);
    let before = parsed(MULTI_LINE_LF);

    let outcome: PlaybookWrite = written(set_auto_approve(&path, DestructiveTool::SendKeys, true));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, added) = added_line(MULTI_LINE_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must be added, got:\n{after}"));
    assert_eq!(without_trailing_comma(&added), quoted("send_keys"));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(after_policy.allows(DestructiveTool::SendKeys));
    assert_other_tools_unchanged(&before, &after_policy, DestructiveTool::SendKeys);
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

#[test]
fn turning_off_a_tool_in_a_multi_line_array_removes_only_its_element_line() {
    let (_tree, path) = write_playbook("off-multi-line", MULTI_LINE_LF);
    let before = parsed(MULTI_LINE_LF);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, false));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, removed) = removed_line(MULTI_LINE_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must be removed, got:\n{after}"));
    assert_eq!(without_trailing_comma(&removed), quoted("close_window"));
    assert!(after.contains(&quoted("send_chat")));
    assert!(after.contains(&quoted("unlisted-tool")));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(!after_policy.allows(DestructiveTool::CloseWindow));
    assert_other_tools_unchanged(&before, &after_policy, DestructiveTool::CloseWindow);
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

#[test]
fn a_crlf_playbook_keeps_crlf_endings_after_a_toggle() {
    let source = crlf(SINGLE_ENTRY_LF);
    let (_tree, path) = write_playbook("crlf-toggle", &source);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(outcome.written);
    let after = read_text(&path);
    assert!(
        has_crlf(&after),
        "a CRLF file must keep CRLF endings:\n{after:?}"
    );
    assert!(
        !has_bare_lf(&after),
        "a CRLF file must not gain a bare LF:\n{after:?}"
    );
    let (_, _, after_line) = changed_line(&source, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(after_line.contains(&quoted("close_window")));
}

#[test]
fn turning_on_a_tool_written_as_an_inline_table_edits_the_same_line() {
    let (_tree, path) = write_playbook("on-inline-table", INLINE_TABLE_LF);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, before_line, after_line) = changed_line(INLINE_TABLE_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(before_line.contains("approval ="));
    assert!(after_line.contains(&quoted("send_chat")));
    assert!(after_line.contains(&quoted("close_window")));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(after_policy.allows(DestructiveTool::CloseWindow));
    assert!(after_policy.allows(DestructiveTool::SendChat));
}

#[test]
fn turning_off_a_tool_written_with_a_dotted_root_key_edits_the_same_line() {
    let (_tree, path) = write_playbook("off-dotted-key", DOTTED_KEY_LF);

    let outcome: PlaybookWrite = written(set_auto_approve(&path, DestructiveTool::SendChat, false));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, before_line, after_line) = changed_line(DOTTED_KEY_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must change, got:\n{after}"));
    assert!(before_line.contains("approval.auto_approve"));
    assert!(!after_line.contains("send_chat"));
    let after_policy: ApprovalPolicy = parsed(&after);
    assert!(!after_policy.allows(DestructiveTool::SendChat));
}

// --- creating a missing table or key ---------------------------------------------

#[test]
fn turning_on_a_tool_with_no_approval_table_creates_one() {
    let (_tree, path) = write_playbook("create-approval-table", NO_APPROVAL_LF);
    let before = parsed(NO_APPROVAL_LF);

    let outcome: PlaybookWrite = written(set_auto_approve(&path, DestructiveTool::SendKeys, true));

    assert!(outcome.written);
    let after_policy: ApprovalPolicy = parsed(&read_text(&path));
    assert!(after_policy.allows(DestructiveTool::SendKeys));
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

#[test]
fn replacing_require_approval_keeps_the_comment_above_the_key() {
    let (_tree, path) = write_playbook("plans-comment-above", SINGLE_ENTRY_LF);

    let outcome: PlaybookWrite = written(set_plan_approval_required(&path, false));

    assert!(outcome.written);
    let expected =
        SINGLE_ENTRY_LF.replacen("require_approval = true", "require_approval = false", 1);
    assert_eq!(read_text(&path), expected);
}

#[test]
fn replacing_require_approval_keeps_a_same_line_comment() {
    let (_tree, path) = write_playbook("plans-trailing-comment", WITH_TRAILING_COMMENT_LF);

    let outcome: PlaybookWrite = written(set_plan_approval_required(&path, false));

    assert!(outcome.written);
    let expected =
        WITH_TRAILING_COMMENT_LF.replacen("require_approval = true", "require_approval = false", 1);
    assert_eq!(read_text(&path), expected);
}

#[test]
fn turning_plan_approval_off_adds_the_key_when_the_table_is_empty() {
    let (_tree, path) = write_playbook("plans-add-key", NO_REQUIRE_KEY_LF);

    let outcome: PlaybookWrite = written(set_plan_approval_required(&path, false));

    assert!(outcome.written);
    let after = read_text(&path);
    let (_, added) = added_line(NO_REQUIRE_KEY_LF, &after)
        .unwrap_or_else(|| panic!("exactly one line must be added, got:\n{after}"));
    assert_eq!(added, "require_approval = false");
}

#[test]
fn turning_plan_approval_off_with_no_plans_table_creates_one() {
    let (_tree, path) = write_playbook("plans-create-table", NO_PLANS_LF);
    let before = parsed(NO_PLANS_LF);

    let outcome: PlaybookWrite = written(set_plan_approval_required(&path, false));

    assert!(outcome.written);
    let after_policy: ApprovalPolicy = parsed(&read_text(&path));
    assert_eq!(
        after_policy
            .plans
            .as_ref()
            .and_then(|section| section.require_approval),
        Some(false)
    );
    assert_approval_section_unchanged(&before, &after_policy);
}

// --- a request matching the current state writes nothing -------------------------

#[test]
fn a_request_matching_the_current_auto_approve_state_writes_nothing() {
    let (_tree, path) = write_playbook("noop-auto-approve", REAL_PLAYBOOK);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let outcome: PlaybookWrite = written(set_auto_approve(&path, DestructiveTool::SendChat, true));

    assert!(
        !outcome.written,
        "a request matching the current state must not write"
    );
    assert_eq!(read_bytes(&path), before);
    assert_eq!(modified(&path), mtime_before);
}

#[test]
fn a_request_matching_the_current_plan_approval_state_writes_nothing() {
    let (_tree, path) = write_playbook("noop-plan-approval", REAL_PLAYBOOK);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let outcome: PlaybookWrite = written(set_plan_approval_required(&path, true));

    assert!(
        !outcome.written,
        "a request matching the current state must not write"
    );
    assert_eq!(read_bytes(&path), before);
    assert_eq!(modified(&path), mtime_before);
}

// --- verification against the rest of the policy ----------------------------------

#[test]
fn a_toggle_leaves_every_other_tool_and_section_unchanged() {
    let (_tree, path) = write_playbook("other-tools-unchanged", REAL_PLAYBOOK);
    let before = parsed(REAL_PLAYBOOK);

    let outcome: PlaybookWrite = written(set_auto_approve(&path, DestructiveTool::SendKeys, true));

    assert!(outcome.written);
    let after_policy: ApprovalPolicy = parsed(&read_text(&path));
    assert!(after_policy.allows(DestructiveTool::SendKeys));
    assert_other_tools_unchanged(&before, &after_policy, DestructiveTool::SendKeys);
    assert_non_approval_sections_unchanged(&before, &after_policy);
}

// --- write failures leave the file untouched --------------------------------------

#[test]
fn a_missing_playbook_is_unreadable_and_is_not_created() {
    let (_tree, path) = missing_playbook("missing");

    let err: PlaybookRewriteError =
        refused(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(
        matches!(err, PlaybookRewriteError::Unreadable),
        "expected Unreadable, got: {err}"
    );
    assert!(
        !path.exists(),
        "a missing playbook must not be created by a refused request"
    );
}

#[test]
fn a_playbook_that_fails_to_parse_is_unreadable_and_is_left_untouched() {
    let (_tree, path) = write_playbook("malformed", MALFORMED_TOML);
    let before = read_bytes(&path);

    let err: PlaybookRewriteError =
        refused(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(
        matches!(err, PlaybookRewriteError::Unreadable),
        "expected Unreadable, got: {err}"
    );
    assert_eq!(read_bytes(&path), before);
}

#[test]
fn a_playbook_with_an_unknown_key_is_unreadable_and_its_error_carries_no_file_content() {
    let (_tree, path) = write_playbook("unknown-key", UNKNOWN_KEY_TOML);
    let before = read_bytes(&path);

    let err: PlaybookRewriteError = refused(set_plan_approval_required(&path, false));

    assert!(
        matches!(err, PlaybookRewriteError::Unreadable),
        "expected Unreadable, got: {err}"
    );
    let text = err.to_string();
    assert!(
        !text.contains(BODY_SENTINEL),
        "the error text must not carry the file's content, got {text:?}"
    );
    assert_eq!(read_bytes(&path), before);
}

// --- only the target file is written ----------------------------------------------

#[test]
fn writing_the_playbook_leaves_no_temporary_file_in_its_directory() {
    let (_tree, path) = write_playbook("no-temp-file", SINGLE_ENTRY_LF);
    let dir = path
        .parent()
        .expect("the playbook path must have a parent")
        .to_path_buf();
    let neighbour = dir.join("neighbour.toml");
    std::fs::write(&neighbour, "kept").expect("neighbour file should be writable");
    let neighbour_before = read_bytes(&neighbour);
    let dir_before = entry_names(&dir);

    let outcome: PlaybookWrite =
        written(set_auto_approve(&path, DestructiveTool::CloseWindow, true));

    assert!(outcome.written);
    assert_eq!(read_bytes(&neighbour), neighbour_before);
    assert_eq!(entry_names(&dir), dir_before);
}

// --- resolving a path to an absolute one, without touching the process's cwd -----

#[test]
fn an_already_absolute_path_is_returned_unchanged() {
    let (_tree, path) = write_playbook("absolute-path-noop", SINGLE_ENTRY_LF);
    assert!(path.is_absolute());

    let resolved: PathBuf = absolute_path(&path).expect("an absolute path must resolve");

    assert_eq!(resolved, path);
}

#[test]
fn a_relative_path_resolves_against_the_current_directory() {
    let relative = Path::new("some-sub-dir").join("playbook.toml");
    let expected = std::env::current_dir()
        .expect("the current directory must be readable")
        .join(&relative);

    let resolved: PathBuf = absolute_path(&relative).expect("a relative path must resolve");

    assert_eq!(resolved, expected);
}

#[test]
fn a_bare_file_name_resolves_to_a_path_with_a_non_empty_parent_directory() {
    let bare = Path::new("playbook.toml");

    let resolved: PathBuf = absolute_path(bare).expect("a bare file name must resolve");

    let parent = resolved
        .parent()
        .expect("an absolute path must have a parent");
    assert_ne!(
        parent.as_os_str(),
        "",
        "the resolved parent directory must never be the empty path"
    );
}
