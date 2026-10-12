#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: switching a profile's top-level `enabled` key by
//! rewriting the placed file in place.
//!
//! The file may have been written by hand, so every case here compares whole
//! byte strings: only the `enabled` line itself may be added, removed or have
//! its value replaced, and a refused request leaves every byte in place.
//! Every profile is written by the test itself into a directory it just made.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{
    LaunchOptionValues, ProfileLaunchError, ProfileOs, list_profiles, parse_profile, resolve_launch,
};
use yagura_daemon::profile_enabled::{EnabledSet, ProfileEnabledError, set_profile_enabled};
use yagura_test_support::paths::TempDir;

// --- fixtures -------------------------------------------------------------------

/// A text only a profile body carries; no error text may repeat it.
const BODY_SENTINEL: &str = "toggle-program";

const PROFILE_ID: &str = "toggle";

/// A profile with no `enabled` key at all: LF endings, `[command]` is the
/// first (and only) table heading.
const NO_ENABLED_LF: &str = "schema = 1
display_name = \"Toggle\"

[command]
default = [\"toggle-program\"]
";

/// [`NO_ENABLED_LF`] with a comment above `schema`.
const NO_ENABLED_WITH_COMMENT_LF: &str = "# Hand notes above the profile.
schema = 1
display_name = \"Toggle\"

[command]
default = [\"toggle-program\"]
";

/// [`NO_ENABLED_LF`] with a top-level dotted key ahead of `[command]`.
const NO_ENABLED_DOTTED_LF: &str = "schema = 1
display_name = \"Toggle\"
model.args = [\"--tier\", \"{value}\"]
model.aliases.fast = \"tier-a-1\"

[command]
default = [\"toggle-program\"]
";

/// A profile already carrying `enabled = false`.
const HAS_FALSE_LF: &str = "schema = 1
display_name = \"Toggle\"
enabled = false

[command]
default = [\"toggle-program\"]
";

/// A profile with a hand-written `enabled = true`, including a same-line
/// comment that must survive a value replacement.
const HAS_TRUE_LF: &str = "schema = 1
display_name = \"Toggle\"
enabled = true  # kept on by hand

[command]
default = [\"toggle-program\"]
";

/// A profile mapping the `model` axis with aliases and a fixed choices list,
/// so a switch's effect on them can be checked.
const WITH_MODEL_LF: &str = "schema = 1
display_name = \"Toggle\"

[command]
default = [\"toggle-program\"]

[model]
args = [\"--tier\", \"{value}\"]

[model.aliases]
fast = \"tier-a-1\"

[model.choices]
values = [\"tier-a-1\", \"tier-b-1\"]
";

/// A profile whose `[command]` names an argv only for [`ProfileOs::Windows`].
const WINDOWS_ONLY_LF: &str = "schema = 1
display_name = \"Toggle\"

[command]
windows = [\"windows-only-program\"]
";

/// A document the profile parser rejects (unknown schema).
const BROKEN_FORM: &str = "schema = 99
display_name = \"Toggle\"
";

/// How long a case waits between writing a file and calling the operation, so
/// a rewrite would move the file's modification time.
const MTIME_OBSERVATION: Duration = Duration::from_millis(1500);

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

/// A profiles directory inside a fresh temporary tree, with a configuration
/// that reads it and nothing from the process environment.
struct Fixture {
    _tree: TempDir,
    root: PathBuf,
    dir: PathBuf,
    config: DaemonConfig,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        Self::with_os(tag, ProfileOs::Windows)
    }

    fn with_os(tag: &str, host_os: ProfileOs) -> Self {
        let tree = TempDir::new("yagura-cli-test", tag);
        let dir = tree.make_dir("profiles");
        let root = dir
            .parent()
            .expect("the profiles directory has a parent")
            .to_path_buf();
        let config = config_for(dir.clone(), host_os);
        Self {
            _tree: tree,
            root,
            dir,
            config,
        }
    }

    fn write(&self, id: &str, source: &str) -> PathBuf {
        let path = self.dir.join(format!("{id}.toml"));
        std::fs::write(&path, source.as_bytes()).expect("fixture profile should be writable");
        path
    }
}

fn config_for(dir: PathBuf, host_os: ProfileOs) -> DaemonConfig {
    DaemonConfig {
        profiles_dir: Some(dir),
        host_os,
        default_shell: None,
        detection_dir: None,
        ..DaemonConfig::default()
    }
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| panic!("{} should be readable: {err}", path.display()))
}

fn read_text(path: &Path) -> String {
    String::from_utf8(read_bytes(path)).expect("profile files are UTF-8")
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

fn saved(result: Result<EnabledSet, ProfileEnabledError>) -> EnabledSet {
    match result {
        Ok(saved) => saved,
        Err(err) => panic!("the switch must be accepted, got: {err}"),
    }
}

fn refused<T>(result: Result<T, ProfileEnabledError>) -> ProfileEnabledError {
    match result {
        Ok(_) => panic!("the request must be refused"),
        Err(err) => err,
    }
}

fn assert_error_text_carries_no_body(err: &ProfileEnabledError) {
    let text = err.to_string();
    assert!(
        !text.contains(BODY_SENTINEL),
        "an error's text must not carry the profile body, got {text:?}"
    );
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

// --- adding the `enabled = false` line --------------------------------------------

fn assert_enabled_line_added(tag: &str, source: &str) {
    let fixture = Fixture::new(tag);
    let path = fixture.write(PROFILE_ID, source);

    let result = saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    assert!(
        result.written,
        "turning off a profile with no enabled key must write"
    );
    assert!(!result.enabled);
    let after = read_text(&path);
    let (at, line) = added_line(source, &after)
        .unwrap_or_else(|| panic!("exactly one line must be added, got:\n{after}"));
    assert_eq!(line, "enabled = false");
    assert!(
        lines_with_endings(&after)[..at]
            .iter()
            .all(|line| !line.trim_start().starts_with('[')),
        "the added line must come before the first table heading:\n{after}"
    );
    if has_crlf(source) {
        assert!(
            !has_bare_lf(&after),
            "a CRLF file must keep CRLF line endings after the addition:\n{after:?}"
        );
    }
}

#[test]
fn turning_off_a_profile_with_no_enabled_key_adds_one_line_with_lf_endings() {
    assert_enabled_line_added("enabled-add-lf", NO_ENABLED_LF);
}

#[test]
fn turning_off_a_profile_with_no_enabled_key_adds_one_line_with_crlf_endings() {
    assert_enabled_line_added("enabled-add-crlf", &crlf(NO_ENABLED_LF));
}

#[test]
fn turning_off_a_profile_whose_file_opens_with_a_comment_adds_the_line_after_it() {
    assert_enabled_line_added("enabled-add-comment", NO_ENABLED_WITH_COMMENT_LF);
}

#[test]
fn turning_off_a_profile_with_a_top_level_dotted_key_adds_the_line_with_lf_endings() {
    assert_enabled_line_added("enabled-add-dotted-lf", NO_ENABLED_DOTTED_LF);
}

#[test]
fn turning_off_a_profile_with_a_top_level_dotted_key_adds_the_line_with_crlf_endings() {
    assert_enabled_line_added("enabled-add-dotted-crlf", &crlf(NO_ENABLED_DOTTED_LF));
}

// --- removing or replacing the `enabled` line -----------------------------------------

fn assert_enabled_line_removed(tag: &str, source: &str) {
    let fixture = Fixture::new(tag);
    let path = fixture.write(PROFILE_ID, source);

    let result = saved(set_profile_enabled(&fixture.config, PROFILE_ID, true));

    assert!(
        result.written,
        "turning on a profile carrying enabled = false must write"
    );
    assert!(result.enabled);
    let after = read_text(&path);
    let (_, line) = removed_line(source, &after)
        .unwrap_or_else(|| panic!("exactly one line must be removed, got:\n{after}"));
    assert_eq!(line, "enabled = false");
}

#[test]
fn turning_on_a_profile_with_enabled_false_removes_that_line_with_lf_endings() {
    assert_enabled_line_removed("enabled-remove-lf", HAS_FALSE_LF);
}

#[test]
fn turning_on_a_profile_with_enabled_false_removes_that_line_with_crlf_endings() {
    assert_enabled_line_removed("enabled-remove-crlf", &crlf(HAS_FALSE_LF));
}

#[test]
fn turning_off_a_hand_written_enabled_true_replaces_only_the_boolean_literal() {
    let fixture = Fixture::new("enabled-replace-value");
    let path = fixture.write(PROFILE_ID, HAS_TRUE_LF);

    let result = saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    assert!(result.written);
    assert!(!result.enabled);
    let expected = HAS_TRUE_LF.replacen("enabled = true", "enabled = false", 1);
    assert_eq!(
        read_text(&path),
        expected,
        "only the boolean literal changes; the same-line comment and every other byte stay"
    );
}

// --- a request matching the current state writes nothing --------------------------

fn assert_no_write_when_already_that_state(tag: &str, source: &str, enabled: bool) {
    let fixture = Fixture::new(tag);
    let path = fixture.write(PROFILE_ID, source);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let result = saved(set_profile_enabled(&fixture.config, PROFILE_ID, enabled));

    assert!(
        !result.written,
        "a request matching the current state must not write"
    );
    assert_eq!(result.enabled, enabled);
    assert_eq!(read_bytes(&path), before);
    assert_eq!(
        modified(&path),
        mtime_before,
        "a request matching the current state must not touch the file"
    );
}

#[test]
fn requesting_enabled_true_for_a_profile_with_no_enabled_key_writes_nothing() {
    assert_no_write_when_already_that_state("enabled-noop-true", NO_ENABLED_LF, true);
}

#[test]
fn requesting_enabled_false_for_a_profile_already_disabled_writes_nothing() {
    assert_no_write_when_already_that_state("enabled-noop-false", HAS_FALSE_LF, false);
}

#[test]
fn requesting_enabled_true_for_a_profile_with_a_hand_written_enabled_true_writes_nothing() {
    assert_no_write_when_already_that_state("enabled-noop-true-explicit", HAS_TRUE_LF, true);
}

#[test]
fn a_request_changing_the_state_moves_the_modification_time() {
    let fixture = Fixture::new("enabled-write-moves-mtime");
    let path = fixture.write(PROFILE_ID, NO_ENABLED_LF);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let result = saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    assert!(result.written);
    assert_ne!(read_bytes(&path), before);
    assert_ne!(modified(&path), mtime_before);
}

// --- a profile not in the listing is unknown, and the file is untouched -------------

#[test]
fn a_file_the_profile_parser_rejects_is_an_unknown_profile_and_is_left_untouched() {
    let fixture = Fixture::new("enabled-unknown-broken");
    let path = fixture.write(PROFILE_ID, BROKEN_FORM);
    let before = read_bytes(&path);

    let err = refused(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    assert!(
        matches!(err, ProfileEnabledError::UnknownProfile),
        "expected the unknown-profile refusal, got: {err}"
    );
    assert_error_text_carries_no_body(&err);
    assert_eq!(read_bytes(&path), before);
}

#[test]
fn an_id_with_no_file_or_an_invalid_id_is_an_unknown_profile_and_writes_nothing() {
    let fixture = Fixture::new("enabled-unknown-id");
    let path = fixture.write(PROFILE_ID, NO_ENABLED_LF);
    let before = read_bytes(&path);
    let dir_before = entry_names(&fixture.dir);

    for id in ["absent", "../toggle", "Toggle!"] {
        let err = refused(set_profile_enabled(&fixture.config, id, false));
        assert!(
            matches!(err, ProfileEnabledError::UnknownProfile),
            "expected the unknown-profile refusal for {id:?}, got: {err}"
        );
        assert_error_text_carries_no_body(&err);
    }

    assert_eq!(read_bytes(&path), before);
    assert_eq!(entry_names(&fixture.dir), dir_before);
}

#[test]
fn a_profile_with_no_argv_for_the_host_is_an_unknown_profile_and_is_left_untouched() {
    let fixture = Fixture::with_os("enabled-unknown-no-argv", ProfileOs::Linux);
    let path = fixture.write(PROFILE_ID, WINDOWS_ONLY_LF);
    let before = read_bytes(&path);

    let err = refused(set_profile_enabled(&fixture.config, PROFILE_ID, true));

    assert!(
        matches!(err, ProfileEnabledError::UnknownProfile),
        "expected the unknown-profile refusal, got: {err}"
    );
    assert_error_text_carries_no_body(&err);
    assert_eq!(read_bytes(&path), before);
}

// --- a switch is visible to the next listing and launch attempt ---------------------

#[test]
fn after_switching_off_the_listing_and_a_launch_attempt_carry_the_new_state() {
    let fixture = Fixture::new("enabled-read-back-off");
    fixture.write(PROFILE_ID, NO_ENABLED_LF);

    saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    let listing = list_profiles(&fixture.config);
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == PROFILE_ID)
        .unwrap_or_else(|| panic!("the profile must stay listed: {:?}", listing.warnings));
    assert!(!entry.enabled);

    match resolve_launch(
        &fixture.config,
        PROFILE_ID,
        None,
        None,
        &LaunchOptionValues::default(),
    ) {
        Err(ProfileLaunchError::Disabled) => {}
        other => panic!("a switched-off profile must be refused as disabled, got {other:?}"),
    }
}

#[test]
fn after_switching_on_the_listing_and_a_launch_attempt_carry_the_new_state() {
    let fixture = Fixture::new("enabled-read-back-on");
    fixture.write(PROFILE_ID, HAS_FALSE_LF);

    saved(set_profile_enabled(&fixture.config, PROFILE_ID, true));

    let listing = list_profiles(&fixture.config);
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == PROFILE_ID)
        .unwrap_or_else(|| panic!("the profile must stay listed: {:?}", listing.warnings));
    assert!(entry.enabled);

    let launched = resolve_launch(
        &fixture.config,
        PROFILE_ID,
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .unwrap_or_else(|err| panic!("a switched-on profile must launch, got {err:?}"));
    assert_eq!(launched.argv, vec![BODY_SENTINEL.to_string()]);
}

#[test]
fn switching_off_leaves_the_model_aliases_and_choices_unchanged() {
    let fixture = Fixture::new("enabled-model-preserved");
    let path = fixture.write(PROFILE_ID, WITH_MODEL_LF);
    let before_options = parse_profile(PROFILE_ID, WITH_MODEL_LF)
        .expect("the fixture must parse")
        .options;

    saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    let after = read_text(&path);
    let after_options = parse_profile(PROFILE_ID, &after)
        .expect("the file must still parse after the switch")
        .options;
    assert_eq!(
        after_options.model.map(|spec| spec.aliases),
        before_options.model.map(|spec| spec.aliases),
        "model aliases must survive a switch unchanged"
    );
    assert_eq!(
        after_options.model_choices, before_options.model_choices,
        "the model choices table must survive a switch unchanged"
    );
}

// --- only the target file is written -------------------------------------------------

#[test]
fn a_switch_writes_only_the_target_file_and_leaves_no_temporary_file() {
    let fixture = Fixture::new("enabled-only-target");
    fixture.write(PROFILE_ID, NO_ENABLED_LF);
    let neighbour = fixture.write("neighbour", NO_ENABLED_LF);
    let neighbour_before = read_bytes(&neighbour);
    let dir_before = entry_names(&fixture.dir);
    let root_before = entry_names(&fixture.root);

    saved(set_profile_enabled(&fixture.config, PROFILE_ID, false));

    assert_eq!(read_bytes(&neighbour), neighbour_before);
    assert_eq!(entry_names(&fixture.dir), dir_before);
    assert_eq!(entry_names(&fixture.root), root_before);
}
