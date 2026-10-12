#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: reading and writing `keymap.toml` through the public
//! `yagura_gui::keymap` surface, and the config location `StartupContext`
//! hands to the save path.
//!
//! The file keeps action-to-chord overrides in a `[keys]` table. A legacy
//! `[bindings]` table is only counted, never read as overrides.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use yagura_gui::instance::StartupContext;
use yagura_gui::keymap::{
    KeymapFailure, KeymapLoadResult, KeymapSaveFailure, load_keymap, save_keymap_in,
};
use yagura_test_support::paths::TempDir;

const TEMP_DIR_PREFIX: &str = "yagura-keymap-file";
const KEYMAP_FILE_NAME: &str = "keymap.toml";
const BYTE_LIMIT: usize = 64 * 1024;
const ENTRY_LIMIT: usize = 200;

fn load_from(dir: &TempDir, content: &str) -> KeymapLoadResult {
    dir.write_file(KEYMAP_FILE_NAME, content);
    load_keymap(dir.path())
}

fn string_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn ordered_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn numbered_keys(count: usize) -> BTreeMap<String, String> {
    (0..count)
        .map(|index| (format!("action-{index:03}"), String::new()))
        .collect()
}

/// The directory entries, sorted, so a leftover temporary file shows up.
fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("list directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn assert_no_overrides(result: &KeymapLoadResult) {
    assert!(result.keys.is_empty(), "a failed load must yield no keys");
    assert_eq!(
        result.legacy_entries, 0,
        "a failed load must count no legacy entries"
    );
}

/// A config base with a not-yet-created instance root below it.
struct SaveTarget {
    base: TempDir,
    root: PathBuf,
}

impl SaveTarget {
    fn new(label: &str) -> Self {
        let base = TempDir::new(TEMP_DIR_PREFIX, label);
        let root = base.path().join("yagura");
        Self { base, root }
    }

    fn save(&self, keys: &BTreeMap<String, String>) -> Result<(), KeymapSaveFailure> {
        save_keymap_in(Some((self.base.path(), self.root.as_path())), keys)
    }

    fn file(&self) -> PathBuf {
        self.root.join(KEYMAP_FILE_NAME)
    }

    fn write_existing(&self, content: &str) {
        std::fs::create_dir_all(&self.root).expect("create root");
        std::fs::write(self.file(), content).expect("write existing keymap");
    }
}

// ---------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------

#[test]
fn keys_table_is_read_verbatim_including_unassigned_and_unknown_actions() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "keys");
    // The vocabulary belongs to the WebView, so an unknown action name must
    // reach it to be counted as discarded there.
    let result: KeymapLoadResult = load_from(
        &dir,
        "[keys]\nclose-window = \"Ctrl+Shift+q\"\ntoggle-plans = \"\"\nno-such-action = \"Ctrl+Shift+y\"\n",
    );

    assert_eq!(result.failure, None);
    assert_eq!(
        result.keys,
        string_map(&[
            ("close-window", "Ctrl+Shift+q"),
            ("toggle-plans", ""),
            ("no-such-action", "Ctrl+Shift+y"),
        ])
    );
    assert_eq!(result.legacy_entries, 0);
}

#[test]
fn legacy_bindings_table_is_counted_but_not_read_as_keys() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "legacy");
    let result: KeymapLoadResult = load_from(
        &dir,
        "[bindings]\n\"Ctrl+Alt+ArrowRight\" = \"focus-next-window\"\n\"Ctrl+Alt+n\" = \"open-window-form\"\n",
    );

    assert_eq!(result.failure, None);
    assert!(
        result.keys.is_empty(),
        "chord-to-action entries must not become action-to-chord overrides"
    );
    assert_eq!(result.legacy_entries, 2);
}

#[test]
fn both_tables_are_reported_together() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "both");
    let result: KeymapLoadResult = load_from(
        &dir,
        "[keys]\nclose-window = \"Ctrl+Shift+q\"\n\n[bindings]\n\"Ctrl+Alt+n\" = \"open-window-form\"\n\"Ctrl+Alt+w\" = \"close-window\"\n\"Ctrl+Alt+m\" = \"toggle-plans\"\n",
    );

    assert_eq!(result.failure, None);
    assert_eq!(result.keys, string_map(&[("close-window", "Ctrl+Shift+q")]));
    assert_eq!(result.legacy_entries, 3);
}

#[test]
fn a_file_with_neither_table_is_an_empty_keymap_not_a_failure() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "neither");
    let result: KeymapLoadResult = load_from(&dir, "# no overrides yet\n");

    assert_eq!(result.failure, None);
    assert!(result.keys.is_empty());
    assert_eq!(result.legacy_entries, 0);
}

#[test]
fn a_non_string_value_in_keys_reports_invalid_toml() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "keys-non-string");
    let result: KeymapLoadResult = load_from(
        &dir,
        "[keys]\nclose-window = \"Ctrl+Shift+q\"\ntoggle-plans = 42\n",
    );

    assert_eq!(result.failure, Some(KeymapFailure::InvalidToml));
    assert_no_overrides(&result);
}

#[test]
fn malformed_toml_reports_invalid_toml() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "malformed");
    let result: KeymapLoadResult = load_from(&dir, "this is not { valid TOML");

    assert_eq!(result.failure, Some(KeymapFailure::InvalidToml));
    assert_no_overrides(&result);
}

#[test]
fn a_missing_file_reports_not_found() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "missing");
    let result: KeymapLoadResult = load_keymap(dir.path());

    assert_eq!(result.failure, Some(KeymapFailure::NotFound));
    assert_no_overrides(&result);
}

#[test]
fn a_directory_in_place_of_the_file_reports_read_failed() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "directory");
    std::fs::create_dir_all(dir.path().join(KEYMAP_FILE_NAME)).expect("create fixture directory");

    let result: KeymapLoadResult = load_keymap(dir.path());

    assert_eq!(result.failure, Some(KeymapFailure::ReadFailed));
    assert_no_overrides(&result);
}

// ---------------------------------------------------------------------
// Read limits
// ---------------------------------------------------------------------

#[test]
fn a_file_over_the_byte_limit_is_too_large_before_parsing() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit");
    // Not valid TOML, so only a size check that runs first can report this.
    let result: KeymapLoadResult = load_from(&dir, &"a".repeat(BYTE_LIMIT + 1));

    assert_eq!(result.failure, Some(KeymapFailure::TooLarge));
    assert_no_overrides(&result);
}

#[test]
fn a_keys_file_exactly_at_the_byte_limit_is_read() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit-boundary");
    let prefix = "[keys]\nclose-window = \"Ctrl+Shift+q\"\n";
    let padding = "#".repeat(BYTE_LIMIT - prefix.len());
    let result: KeymapLoadResult = load_from(&dir, &format!("{prefix}{padding}"));

    assert_eq!(result.failure, None);
    assert_eq!(result.keys, string_map(&[("close-window", "Ctrl+Shift+q")]));
}

fn split_entries_file(keys_count: usize, bindings_count: usize) -> String {
    let mut content = String::from("[keys]\n");
    for index in 0..keys_count {
        content.push_str(&format!("action-{index:03} = \"\"\n"));
    }
    content.push_str("\n[bindings]\n");
    for index in 0..bindings_count {
        content.push_str(&format!("\"Ctrl+Alt+{index}\" = \"open-window-form\"\n"));
    }
    content
}

#[test]
fn the_entry_limit_counts_keys_and_bindings_together() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "entry-limit-boundary");
    // Neither table alone reaches the limit, so a per-table count reads both.
    let result: KeymapLoadResult = load_from(&dir, &split_entries_file(150, ENTRY_LIMIT - 150));

    assert_eq!(result.failure, None);
    assert_eq!(result.keys.len(), 150);
    assert_eq!(result.legacy_entries, ENTRY_LIMIT - 150);
}

#[test]
fn one_entry_over_the_combined_limit_is_too_large() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "entry-limit");
    let result: KeymapLoadResult = load_from(&dir, &split_entries_file(150, ENTRY_LIMIT - 150 + 1));

    assert_eq!(result.failure, Some(KeymapFailure::TooLarge));
    assert_no_overrides(&result);
}

// ---------------------------------------------------------------------
// Wire shape
// ---------------------------------------------------------------------

#[test]
fn a_successful_result_serializes_keys_and_legacy_entries_without_failure() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "json-ok");
    let result: KeymapLoadResult = load_from(
        &dir,
        "[keys]\ntoggle-plans = \"\"\n\n[bindings]\n\"Ctrl+Alt+n\" = \"open-window-form\"\n",
    );

    let json: serde_json::Value = serde_json::to_value(&result).expect("serialize result");
    let object = json.as_object().expect("result serializes as an object");

    assert!(!object.contains_key("failure"));
    assert!(!object.contains_key("bindings"));
    assert_eq!(json["keys"], serde_json::json!({ "toggle-plans": "" }));
    assert_eq!(json["legacy_entries"], serde_json::json!(1));
}

#[test]
fn a_failure_serializes_as_a_snake_case_string() {
    let dir = TempDir::new(TEMP_DIR_PREFIX, "json-failure");
    let result: KeymapLoadResult = load_keymap(dir.path());

    let json: serde_json::Value = serde_json::to_value(&result).expect("serialize result");

    assert_eq!(json["failure"], serde_json::json!("not_found"));
    assert_eq!(json["keys"], serde_json::json!({}));
    assert_eq!(json["legacy_entries"], serde_json::json!(0));
}

#[test]
fn save_failures_serialize_as_snake_case_strings() {
    let cases: [(KeymapSaveFailure, &str); 3] = [
        (KeymapSaveFailure::NoConfigRoot, "no_config_root"),
        (KeymapSaveFailure::TooLarge, "too_large"),
        (KeymapSaveFailure::WriteFailed, "write_failed"),
    ];
    for (failure, expected) in cases {
        let json: serde_json::Value = serde_json::to_value(failure).expect("serialize failure");
        assert_eq!(json, serde_json::json!(expected), "{failure:?}");
    }
}

// ---------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------

#[test]
fn saved_keys_load_back_unchanged_including_unassigned_ones() {
    let target = SaveTarget::new("round-trip");
    // A quote and a backslash are real key names, so hand-built TOML breaks.
    let keys: BTreeMap<String, String> = ordered_map(&[
        ("close-window", "Ctrl+Shift+q"),
        ("toggle-plans", ""),
        ("focus-next-window", "Ctrl+Shift+\""),
        ("focus-previous-window", "Ctrl+\\"),
    ]);

    let outcome: Result<(), KeymapSaveFailure> = target.save(&keys);
    assert_eq!(outcome, Ok(()));

    let result: KeymapLoadResult = load_keymap(&target.root);
    assert_eq!(result.failure, None);
    assert_eq!(result.keys, keys.into_iter().collect::<HashMap<_, _>>());
    assert_eq!(result.legacy_entries, 0);
}

#[test]
fn saving_rewrites_a_legacy_file_without_its_bindings_table() {
    let target = SaveTarget::new("rewrite-legacy");
    target.write_existing("[bindings]\n\"Ctrl+Alt+n\" = \"open-window-form\"\n");
    let keys: BTreeMap<String, String> = ordered_map(&[("close-window", "Ctrl+Shift+q")]);

    let outcome: Result<(), KeymapSaveFailure> = target.save(&keys);
    assert_eq!(outcome, Ok(()));

    let result: KeymapLoadResult = load_keymap(&target.root);
    assert_eq!(result.failure, None);
    assert_eq!(result.keys, string_map(&[("close-window", "Ctrl+Shift+q")]));
    assert_eq!(result.legacy_entries, 0, "the legacy table must be gone");
    let written = std::fs::read_to_string(target.file()).expect("read saved keymap");
    assert!(!written.contains("bindings"), "{written}");
}

#[test]
fn saving_creates_a_missing_config_root_and_leaves_no_temporary_file() {
    let target = SaveTarget::new("create-root");
    assert!(!target.root.exists());

    let outcome: Result<(), KeymapSaveFailure> =
        target.save(&ordered_map(&[("close-window", "Ctrl+Shift+q")]));

    assert_eq!(outcome, Ok(()));
    assert_eq!(entry_names(&target.root), vec![KEYMAP_FILE_NAME.to_owned()]);
}

#[test]
fn the_entry_limit_itself_can_be_saved_and_read_back() {
    let target = SaveTarget::new("save-entry-boundary");
    let keys: BTreeMap<String, String> = numbered_keys(ENTRY_LIMIT);

    let outcome: Result<(), KeymapSaveFailure> = target.save(&keys);
    assert_eq!(outcome, Ok(()));

    let result: KeymapLoadResult = load_keymap(&target.root);
    assert_eq!(result.failure, None);
    assert_eq!(result.keys.len(), ENTRY_LIMIT);
}

#[cfg(unix)]
#[test]
fn a_saved_keymap_is_restricted_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    let target = SaveTarget::new("owner-only");
    let outcome: Result<(), KeymapSaveFailure> =
        target.save(&ordered_map(&[("close-window", "Ctrl+Shift+q")]));
    assert_eq!(outcome, Ok(()));

    let file_mode = std::fs::metadata(target.file())
        .expect("stat keymap")
        .permissions()
        .mode()
        & 0o777;
    let root_mode = std::fs::metadata(&target.root)
        .expect("stat root")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
    assert_eq!(root_mode, 0o700);
}

// ---------------------------------------------------------------------
// Save rejections
// ---------------------------------------------------------------------

#[test]
fn one_entry_over_the_limit_is_rejected_without_creating_a_file() {
    let target = SaveTarget::new("save-too-many");

    let outcome: Result<(), KeymapSaveFailure> = target.save(&numbered_keys(ENTRY_LIMIT + 1));

    assert_eq!(outcome, Err(KeymapSaveFailure::TooLarge));
    assert!(!target.file().exists());
}

#[test]
fn a_rejected_save_leaves_the_existing_file_untouched() {
    let target = SaveTarget::new("save-too-many-existing");
    let existing = "[keys]\nclose-window = \"Ctrl+Shift+q\"\n";
    target.write_existing(existing);

    let outcome: Result<(), KeymapSaveFailure> = target.save(&numbered_keys(ENTRY_LIMIT + 1));

    assert_eq!(outcome, Err(KeymapSaveFailure::TooLarge));
    assert_eq!(
        std::fs::read_to_string(target.file()).expect("read existing keymap"),
        existing
    );
    assert_eq!(entry_names(&target.root), vec![KEYMAP_FILE_NAME.to_owned()]);
}

#[test]
fn a_few_entries_that_serialize_past_the_byte_limit_are_rejected() {
    let target = SaveTarget::new("save-too-long");
    let existing = "[keys]\nclose-window = \"Ctrl+Shift+q\"\n";
    target.write_existing(existing);
    // Well within the entry limit, so only a size check can reject it.
    let keys: BTreeMap<String, String> = ordered_map(&[
        ("close-window", "Ctrl+Shift+q"),
        ("toggle-plans", &"a".repeat(BYTE_LIMIT)),
    ]);

    let outcome: Result<(), KeymapSaveFailure> = target.save(&keys);

    assert_eq!(outcome, Err(KeymapSaveFailure::TooLarge));
    assert_eq!(
        std::fs::read_to_string(target.file()).expect("read existing keymap"),
        existing
    );
}

#[test]
fn every_accepted_save_near_the_byte_limit_loads_back() {
    let target = SaveTarget::new("save-byte-boundary");
    // A save that passes but then reads back as too large would lose the
    // user's keys on the next start, so the limit applies to the written file.
    let mut first_rejected: Option<usize> = None;
    let mut accepted = 0usize;
    for length in (BYTE_LIMIT - 200)..=BYTE_LIMIT {
        let keys: BTreeMap<String, String> = ordered_map(&[("k", &"a".repeat(length))]);
        let outcome: Result<(), KeymapSaveFailure> = target.save(&keys);
        match outcome {
            Ok(()) => {
                assert_eq!(
                    first_rejected, None,
                    "a longer value was accepted after a shorter one was rejected"
                );
                let result: KeymapLoadResult = load_keymap(&target.root);
                assert_eq!(result.failure, None, "value length {length}");
                assert_eq!(result.keys, keys.into_iter().collect::<HashMap<_, _>>());
                accepted += 1;
            }
            Err(failure) => {
                assert_eq!(
                    failure,
                    KeymapSaveFailure::TooLarge,
                    "value length {length}"
                );
                first_rejected.get_or_insert(length);
            }
        }
    }
    assert!(accepted > 0, "a file under the byte limit must be saved");
    assert!(
        first_rejected.is_some(),
        "a value as long as the byte limit cannot fit in the file"
    );
}

#[test]
fn a_directory_in_place_of_the_file_is_a_write_failure() {
    let target = SaveTarget::new("save-directory");
    std::fs::create_dir_all(target.file()).expect("create fixture directory");

    let outcome: Result<(), KeymapSaveFailure> =
        target.save(&ordered_map(&[("close-window", "Ctrl+Shift+q")]));

    assert_eq!(outcome, Err(KeymapSaveFailure::WriteFailed));
    assert!(target.file().is_dir());
    assert_eq!(entry_names(&target.root), vec![KEYMAP_FILE_NAME.to_owned()]);
}

#[test]
fn no_config_location_is_reported_as_no_config_root() {
    let keys: BTreeMap<String, String> = ordered_map(&[("close-window", "Ctrl+Shift+q")]);

    let outcome: Result<(), KeymapSaveFailure> = save_keymap_in(None, &keys);

    assert_eq!(outcome, Err(KeymapSaveFailure::NoConfigRoot));
}

// ---------------------------------------------------------------------
// Config location
// ---------------------------------------------------------------------

#[test]
fn config_location_pairs_the_base_with_the_same_root_config_root_returns() {
    let base = TempDir::new(TEMP_DIR_PREFIX, "location");
    let ctx = StartupContext::resolve_from(
        Some(OsStr::new("dev")),
        None,
        Some(base.path().to_path_buf()),
    );

    let location: Option<(PathBuf, PathBuf)> = ctx.config_location();

    let (location_base, location_root) = location.expect("a resolved instance has a location");
    assert_eq!(location_base, base.path().to_path_buf());
    assert_eq!(Some(location_root), ctx.config_root());
}

#[test]
fn config_location_is_none_without_a_base() {
    let ctx = StartupContext::resolve_from(None, None, None);

    let location: Option<(PathBuf, PathBuf)> = ctx.config_location();

    assert_eq!(location, None);
}

#[test]
fn config_location_is_none_when_the_instance_does_not_resolve() {
    let base = TempDir::new(TEMP_DIR_PREFIX, "location-invalid");
    let invalid = StartupContext::resolve_from(
        Some(OsStr::new("Not Valid!")),
        None,
        Some(base.path().to_path_buf()),
    );
    let store_override = StartupContext::resolve_from(
        Some(OsStr::new("dev")),
        Some(OsStr::new("C:/tmp/shared.db")),
        Some(base.path().to_path_buf()),
    );

    let invalid_location: Option<(PathBuf, PathBuf)> = invalid.config_location();
    let store_override_location: Option<(PathBuf, PathBuf)> = store_override.config_location();

    assert_eq!(invalid_location, None);
    assert_eq!(store_override_location, None);
}
