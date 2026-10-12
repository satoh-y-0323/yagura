#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: saving a profile's model alias values into a placed
//! profile file.
//!
//! The save edits a file the operator may have written by hand, so the cases
//! here compare whole byte strings: only the replaced value may differ, and a
//! refused request leaves every byte in place.
//! Every profile is written by the test itself into a directory it just made.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use yagura_daemon::DaemonConfig;
use yagura_daemon::profile::{LaunchOptionValues, list_profiles, parse_profile, resolve_launch};
use yagura_daemon::profile_aliases::{
    AliasRefusal, AliasSaveError, SavedAliases, save_model_aliases,
};
use yagura_test_support::paths::TempDir;

// --- fixtures -------------------------------------------------------------------

/// A text only a profile body carries; no error text may repeat it.
const BODY_SENTINEL: &str = "tiers-program";

/// Aliases held in a `[model.aliases]` table, with a same-line comment, a
/// literal-string value and a multi-line `[mcp]` array after them.
const TABLE_FORM: &str = "# Hand-edited profile: every byte outside the changed value stays.
schema = 1
display_name = \"Tiers\"

[command]
default = [\"tiers-program\"]

[model]
args = [\"--tier\", \"{value}\"]

[model.aliases]
fast = \"tier-a-1\"  # picked by hand
default = \"tier-b-1\"
strong = 'tier-c-1'

[effort]
args = [\"--level\", \"{value}\"]

[mcp]
args = [
  \"--bridge\", \"{bridge_toml}\",
  \"--env\", \"{env_names_toml}\",
]
";

/// The same aliases as an inline table under `[model]`.
const INLINE_FORM: &str = "# Hand-edited profile: every byte outside the changed value stays.
schema = 1
display_name = \"Tiers\"

[command]
default = [\"tiers-program\"]

[model]
args = [\"--tier\", \"{value}\"]
aliases = { fast = \"tier-a-1\", default = \"tier-b-1\", strong = 'tier-c-1' }  # picked by hand

[mcp]
args = [
  \"--bridge\", \"{bridge_toml}\",
  \"--env\", \"{env_names_toml}\",
]
";

/// The same aliases as root-level dotted keys.
const DOTTED_FORM: &str = "# Hand-edited profile: every byte outside the changed value stays.
schema = 1
display_name = \"Tiers\"
model.args = [\"--tier\", \"{value}\"]
model.aliases.fast = \"tier-a-1\"  # picked by hand
model.aliases.default = \"tier-b-1\"
model.aliases.strong = 'tier-c-1'

[command]
default = [\"tiers-program\"]

[mcp]
args = [
  \"--bridge\", \"{bridge_toml}\",
  \"--env\", \"{env_names_toml}\",
]
";

/// A launchable profile with no `[model]` table.
const NO_MODEL_FORM: &str = "schema = 1
display_name = \"Tiers\"

[command]
default = [\"tiers-program\"]
";

/// A document the profile parser rejects (unknown schema).
const BROKEN_FORM: &str = "schema = 99
display_name = \"Tiers\"
";

const PROFILE_ID: &str = "tiers";
const OLD_FAST: &str = "\"tier-a-1\"";
const NEW_FAST: &str = "\"tier-a-2\"";

/// How long a case waits between writing a file and calling the operation, so
/// a rewrite would move the file's modification time.
const MTIME_OBSERVATION: Duration = Duration::from_millis(1500);

fn crlf(source: &str) -> String {
    source.replace('\n', "\r\n")
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(alias, value)| ((*alias).to_string(), (*value).to_string()))
        .collect()
}

fn unchanged_request() -> Vec<(String, String)> {
    pairs(&[
        ("fast", "tier-a-1"),
        ("default", "tier-b-1"),
        ("strong", "tier-c-1"),
    ])
}

fn fast_changed_request() -> Vec<(String, String)> {
    pairs(&[
        ("fast", "tier-a-2"),
        ("default", "tier-b-1"),
        ("strong", "tier-c-1"),
    ])
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
        let tree = TempDir::new("yagura-cli-test", tag);
        let dir = tree.make_dir("profiles");
        let root = dir
            .parent()
            .expect("the profiles directory has a parent")
            .to_path_buf();
        let config = DaemonConfig {
            profiles_dir: Some(dir.clone()),
            default_shell: None,
            detection_dir: None,
            ..DaemonConfig::default()
        };
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

fn saved(result: Result<SavedAliases, AliasSaveError>) -> SavedAliases {
    match result {
        Ok(saved) => saved,
        Err(err) => panic!("the save must be accepted, got: {err}"),
    }
}

fn refused<T>(result: Result<T, AliasSaveError>) -> AliasSaveError {
    match result {
        Ok(_) => panic!("the request must be refused"),
        Err(err) => err,
    }
}

fn assert_error_text_carries_no_body(err: &AliasSaveError) {
    let text = err.to_string();
    assert!(
        !text.contains(BODY_SENTINEL),
        "an error's text must not carry the profile body, got {text:?}"
    );
}

/// `before` with its single occurrence of `old` replaced by `new`.
fn with_one_literal_replaced(before: &str, old: &str, new: &str) -> String {
    assert_eq!(
        before.matches(old).count(),
        1,
        "the fixture must spell {old} exactly once"
    );
    before.replacen(old, new, 1)
}

fn model_aliases_of(id: &str, source: &str) -> Vec<(String, String)> {
    parse_profile(id, source)
        .unwrap_or_else(|err| panic!("the document must be accepted, got {err}:\n{source}"))
        .options
        .model
        .expect("the document declares [model]")
        .aliases
}

// --- only the value changes -------------------------------------------------------

fn assert_only_the_value_changes(tag: &str, source: &str) {
    let fixture = Fixture::new(tag);
    let path = fixture.write(PROFILE_ID, source);
    assert_eq!(
        model_aliases_of(PROFILE_ID, source),
        unchanged_request(),
        "the fixture itself must parse to the aliases it spells"
    );

    let result = saved(save_model_aliases(
        &fixture.config,
        PROFILE_ID,
        &fast_changed_request(),
    ));

    assert!(result.written, "a changed value must be written");
    assert_eq!(result.aliases, fast_changed_request());
    let expected = with_one_literal_replaced(source, OLD_FAST, NEW_FAST);
    let after = read_text(&path);
    assert_eq!(
        after, expected,
        "only the changed value's string literal may differ; line endings, comments, quote \
         styles and other tables must stay byte for byte"
    );
    assert!(
        after.contains("'tier-c-1'"),
        "an unchanged alias keeps its literal-string quotes"
    );
}

#[test]
fn changing_one_value_in_an_aliases_table_with_lf_endings_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-table-lf", TABLE_FORM);
}

#[test]
fn changing_one_value_in_an_aliases_table_with_crlf_endings_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-table-crlf", &crlf(TABLE_FORM));
}

#[test]
fn changing_one_value_in_an_inline_aliases_table_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-inline-lf", INLINE_FORM);
}

#[test]
fn changing_one_value_in_an_inline_aliases_table_with_crlf_endings_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-inline-crlf", &crlf(INLINE_FORM));
}

#[test]
fn changing_one_value_spelled_as_a_dotted_key_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-dotted-lf", DOTTED_FORM);
}

#[test]
fn changing_one_value_spelled_as_a_dotted_key_with_crlf_endings_replaces_only_that_literal() {
    assert_only_the_value_changes("alias-dotted-crlf", &crlf(DOTTED_FORM));
}

// --- refused requests leave the file alone -----------------------------------------

fn assert_save_refused_and_file_untouched(
    tag: &str,
    source: &str,
    request: &[(String, String)],
    check: impl FnOnce(&AliasSaveError),
) {
    let fixture = Fixture::new(tag);
    let path = fixture.write(PROFILE_ID, source);
    let before = read_bytes(&path);

    let err = refused(save_model_aliases(&fixture.config, PROFILE_ID, request));

    check(&err);
    assert_error_text_carries_no_body(&err);
    assert_eq!(
        read_bytes(&path),
        before,
        "a refused save must leave the file byte for byte"
    );
    assert_eq!(
        entry_names(&fixture.dir),
        vec![format!("{PROFILE_ID}.toml")]
    );
}

fn expect_alias_set_changed(err: &AliasSaveError) {
    assert!(
        matches!(err, AliasSaveError::Refused(AliasRefusal::AliasSetChanged)),
        "expected the alias-set refusal, got: {err}"
    );
}

#[test]
fn a_request_missing_an_alias_the_file_declares_is_refused() {
    assert_save_refused_and_file_untouched(
        "alias-missing",
        TABLE_FORM,
        &pairs(&[("fast", "tier-a-2"), ("default", "tier-b-1")]),
        expect_alias_set_changed,
    );
}

#[test]
fn a_request_naming_an_alias_the_file_does_not_declare_is_refused() {
    assert_save_refused_and_file_untouched(
        "alias-extra",
        TABLE_FORM,
        &pairs(&[
            ("fast", "tier-a-2"),
            ("default", "tier-b-1"),
            ("strong", "tier-c-1"),
            ("spare", "tier-d-1"),
        ]),
        expect_alias_set_changed,
    );
}

#[test]
fn a_request_naming_the_same_alias_twice_is_refused() {
    assert_save_refused_and_file_untouched(
        "alias-duplicate",
        TABLE_FORM,
        &pairs(&[
            ("fast", "tier-a-2"),
            ("default", "tier-b-1"),
            ("strong", "tier-c-1"),
            ("fast", "tier-a-3"),
        ]),
        expect_alias_set_changed,
    );
}

#[test]
fn a_value_breaking_the_launch_option_rule_is_refused_naming_the_alias() {
    for (tag, bad) in [("alias-dash", "-tier"), ("alias-semicolon", "tier;x")] {
        assert_save_refused_and_file_untouched(
            tag,
            TABLE_FORM,
            &pairs(&[
                ("fast", bad),
                ("default", "tier-b-1"),
                ("strong", "tier-c-1"),
            ]),
            |err| match err {
                AliasSaveError::Refused(AliasRefusal::InvalidValue { alias, reason }) => {
                    assert_eq!(alias, "fast");
                    assert!(!reason.is_empty(), "the reason must say what is wrong");
                    assert_eq!(err.to_string(), format!("{alias}: {reason}"));
                }
                other => panic!("expected the invalid-value refusal for {bad:?}, got: {other}"),
            },
        );
    }
}

#[test]
fn a_file_the_profile_parser_rejects_is_an_unknown_profile() {
    assert_save_refused_and_file_untouched(
        "alias-broken",
        BROKEN_FORM,
        &unchanged_request(),
        |err| {
            assert!(
                matches!(err, AliasSaveError::UnknownProfile),
                "expected the unknown-profile refusal, got: {err}"
            );
        },
    );
}

#[test]
fn a_profile_without_a_model_table_is_refused() {
    assert_save_refused_and_file_untouched(
        "alias-no-model",
        NO_MODEL_FORM,
        &unchanged_request(),
        |err| {
            assert!(
                matches!(err, AliasSaveError::Refused(AliasRefusal::NoModelOption)),
                "expected the no-model refusal, got: {err}"
            );
        },
    );
}

#[test]
fn an_id_with_no_file_or_an_invalid_id_is_an_unknown_profile_and_writes_nothing() {
    let fixture = Fixture::new("alias-unknown-id");
    let path = fixture.write(PROFILE_ID, TABLE_FORM);
    let before = read_bytes(&path);
    let dir_before = entry_names(&fixture.dir);
    let root_before = entry_names(&fixture.root);

    for id in ["absent", "../tiers", "Tiers!"] {
        let err = refused(save_model_aliases(
            &fixture.config,
            id,
            &fast_changed_request(),
        ));
        assert!(
            matches!(err, AliasSaveError::UnknownProfile),
            "expected the unknown-profile refusal for {id:?}, got: {err}"
        );
    }

    assert_eq!(read_bytes(&path), before);
    assert_eq!(entry_names(&fixture.dir), dir_before);
    assert_eq!(entry_names(&fixture.root), root_before);
}

// --- a request that changes nothing is not written --------------------------------

#[test]
fn a_request_changing_no_value_is_not_written_and_keeps_the_modification_time() {
    let fixture = Fixture::new("alias-no-change");
    let path = fixture.write(PROFILE_ID, TABLE_FORM);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let reordered = pairs(&[
        ("strong", "tier-c-1"),
        ("fast", "tier-a-1"),
        ("default", "tier-b-1"),
    ]);
    let result = saved(save_model_aliases(&fixture.config, PROFILE_ID, &reordered));

    assert!(
        !result.written,
        "a request changing no value must not write"
    );
    assert_eq!(
        result.aliases,
        unchanged_request(),
        "the reported aliases follow the file's order"
    );
    assert_eq!(read_bytes(&path), before);
    assert_eq!(
        modified(&path),
        mtime_before,
        "a request changing no value must not touch the file"
    );
}

#[test]
fn a_request_changing_a_value_is_written_and_moves_the_modification_time() {
    let fixture = Fixture::new("alias-change");
    let path = fixture.write(PROFILE_ID, TABLE_FORM);
    let before = read_bytes(&path);
    let mtime_before = modified(&path);
    std::thread::sleep(MTIME_OBSERVATION);

    let result = saved(save_model_aliases(
        &fixture.config,
        PROFILE_ID,
        &fast_changed_request(),
    ));

    assert!(result.written);
    assert_ne!(read_bytes(&path), before);
    assert_ne!(modified(&path), mtime_before);
}

// --- the saved value is what the next read sees -----------------------------------

#[test]
fn after_a_save_the_listing_and_a_launch_carry_the_new_value() {
    let fixture = Fixture::new("alias-read-back");
    fixture.write(PROFILE_ID, TABLE_FORM);

    saved(save_model_aliases(
        &fixture.config,
        PROFILE_ID,
        &fast_changed_request(),
    ));

    let listing = list_profiles(&fixture.config);
    let entry = listing
        .profiles
        .iter()
        .find(|entry| entry.id == PROFILE_ID)
        .unwrap_or_else(|| panic!("the saved profile must stay listed: {:?}", listing.warnings));
    let listed: Vec<(String, String)> = entry
        .model
        .as_ref()
        .expect("the saved profile still maps a model")
        .aliases
        .iter()
        .map(|alias| (alias.alias.clone(), alias.value.clone()))
        .collect();
    assert_eq!(listed, fast_changed_request());

    let options = LaunchOptionValues {
        model: Some("fast".to_string()),
        effort: None,
    };
    let launch = resolve_launch(&fixture.config, PROFILE_ID, None, None, &options)
        .unwrap_or_else(|err| panic!("the saved profile must still launch, got {err}"));
    assert!(
        launch
            .argv
            .ends_with(&["--tier".to_string(), "tier-a-2".to_string()]),
        "a launch naming the alias must carry the new value, got {:?}",
        launch.argv
    );
}

// --- only the target file is written ----------------------------------------------

#[test]
fn a_save_writes_only_the_target_file_and_leaves_no_temporary_file() {
    let fixture = Fixture::new("alias-only-target");
    let path = fixture.write(PROFILE_ID, TABLE_FORM);
    let neighbour = fixture.write("neighbour", INLINE_FORM);
    let neighbour_before = read_bytes(&neighbour);
    let dir_before = entry_names(&fixture.dir);
    let root_before = entry_names(&fixture.root);

    saved(save_model_aliases(
        &fixture.config,
        PROFILE_ID,
        &fast_changed_request(),
    ));

    assert_eq!(
        read_text(&path),
        with_one_literal_replaced(TABLE_FORM, OLD_FAST, NEW_FAST)
    );
    assert_eq!(read_bytes(&neighbour), neighbour_before);
    assert_eq!(entry_names(&fixture.dir), dir_before);
    assert_eq!(entry_names(&fixture.root), root_before);
}
