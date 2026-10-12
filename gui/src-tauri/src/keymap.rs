//! Reads and writes `keymap.toml`, the user's keyboard shortcut overrides.
//!
//! The file holds a `[keys]` table of `ActionId` name to chord (`""` means
//! unassigned). Chord syntax and the `ActionId` vocabulary are validated on
//! the TypeScript side; this module only moves the raw pairs across, checks
//! the size limits, and reports why a file could not be read or written.
//! A legacy `[bindings]` table (chord to action) is counted, never read.
//!
//! Directories are caller-supplied arguments rather than something this
//! module resolves itself, so it needs no `AppHandle` and is reachable from
//! tests with a plain temp directory.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name of the keymap, resolved under the instance's config root
/// (`config_paths::config_root` in production).
const KEYMAP_FILE_NAME: &str = "keymap.toml";

/// The path of `keymap.toml` under a config root: the file
/// [`load_keymap`] reads and [`save_keymap_in`] replaces.
pub fn keymap_file_path(root: &Path) -> std::path::PathBuf {
    root.join(KEYMAP_FILE_NAME)
}

/// Size limit for `keymap.toml`, applied on read and to the serialized
/// content on save, so a saved file always loads back. Enforced during the
/// read, since another process running as the same user could grow the file.
const MAX_KEYMAP_BYTES: u64 = 64 * 1024;

/// Entry-count limit across the `[keys]` and legacy `[bindings]` tables.
const MAX_KEYMAP_ENTRIES: usize = 200;

/// Why `keymap.toml` could not be loaded. Serializes as a `snake_case`
/// string across the Tauri command boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapFailure {
    /// No file at `<base_dir>/keymap.toml`. Not a defect: most installs
    /// never create this file and keep the built-in defaults.
    NotFound,
    /// The file exists but could not be read as text (e.g. it is a
    /// directory, or its bytes are not valid UTF-8).
    ReadFailed,
    /// The file's bytes parse as text but not as TOML whose `[keys]` table
    /// maps strings to strings and whose `[bindings]`, if present, is a table.
    InvalidToml,
    /// The file exceeds [`MAX_KEYMAP_BYTES`], or its `[keys]` and
    /// `[bindings]` tables together exceed [`MAX_KEYMAP_ENTRIES`] entries.
    TooLarge,
}

/// Why [`save_keymap_in`] did not write `keymap.toml`. Serializes as a
/// `snake_case` string across the Tauri command boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapSaveFailure {
    /// There is no config location to write to (no config base, or the
    /// instance did not resolve).
    NoConfigRoot,
    /// More than [`MAX_KEYMAP_ENTRIES`] entries, or the serialized file would
    /// exceed [`MAX_KEYMAP_BYTES`] and so fail to load on the next start.
    TooLarge,
    /// The file could not be serialized or written; any existing file is
    /// left as it was.
    WriteFailed,
}

/// Result of a [`load_keymap`] attempt.
///
/// `keys` is empty and `legacy_entries` is 0 whenever `failure` is `Some`:
/// there is no partial success (an invalid file yields no overrides at all,
/// fail-safe).
///
/// Scalar fields are declared before `keys` because the `toml` crate
/// requires scalars to precede table-like fields in a struct's field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeymapLoadResult {
    /// Why loading failed. Omitted from the wire payload on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<KeymapFailure>,
    /// Number of entries in a legacy `[bindings]` table, whatever their
    /// values; they are not applied, only reported so the user can be told.
    pub legacy_entries: usize,
    /// `ActionId` name to chord, exactly as written in the `[keys]` table.
    pub keys: HashMap<String, String>,
}

impl KeymapLoadResult {
    /// An empty result reporting why it is empty.
    ///
    /// An unresolvable config root is reported as `not_found`: there is no
    /// location to look in, the same outcome an absent file produces.
    pub(crate) fn empty(failure: KeymapFailure) -> Self {
        Self {
            failure: Some(failure),
            legacy_entries: 0,
            keys: HashMap::new(),
        }
    }
}

/// The shape `keymap.toml` must deserialize into. Legacy `[bindings]` values
/// are kept untyped because they are only counted: a stale entry of any type
/// must not make the whole file unreadable and block saving new keys.
#[derive(Debug, Deserialize)]
struct KeymapFile {
    #[serde(default)]
    keys: HashMap<String, String>,
    #[serde(default)]
    bindings: toml::Table,
}

/// The shape [`save_keymap_in`] writes: only the `[keys]` table, so a save
/// drops any legacy `[bindings]` table.
#[derive(Serialize)]
struct KeymapFileToWrite<'a> {
    keys: &'a BTreeMap<String, String>,
}

/// Loads `keymap.toml` from `<base_dir>/keymap.toml`.
///
/// Never panics and never fails outright: every failure mode is reported
/// through [`KeymapLoadResult::failure`] instead (fail-safe).
///
/// The byte limit is enforced on the same file handle that is read, so a
/// process growing the file cannot slip past a separate size check (CWE-367).
pub fn load_keymap(base_dir: &Path) -> KeymapLoadResult {
    let path = keymap_file_path(base_dir);

    // A missing file is a legitimate case (most installs never create
    // keymap.toml); every other open error is a read failure.
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return KeymapLoadResult::empty(KeymapFailure::NotFound);
        }
        Err(_) => return KeymapLoadResult::empty(KeymapFailure::ReadFailed),
    };

    // Bytes that are not valid UTF-8 are a read failure rather than invalid
    // TOML: the parser below never sees content that cannot be decoded.
    let mut limited_reader = file.take(MAX_KEYMAP_BYTES);
    let mut content = String::new();
    match limited_reader.read_to_string(&mut content) {
        Ok(_) => {}
        Err(_) => return KeymapLoadResult::empty(KeymapFailure::ReadFailed),
    }

    // One further byte from the underlying handle means the file exceeded the
    // limit; the capped reader itself can never see past it.
    let mut underlying_file = limited_reader.into_inner();
    let mut extra = [0u8; 1];
    if underlying_file.read_exact(&mut extra).is_ok() {
        return KeymapLoadResult::empty(KeymapFailure::TooLarge);
    }

    let parsed: KeymapFile = match toml::from_str(&content) {
        Ok(parsed) => parsed,
        Err(_) => return KeymapLoadResult::empty(KeymapFailure::InvalidToml),
    };

    let legacy_entries = parsed.bindings.len();
    if parsed.keys.len().saturating_add(legacy_entries) > MAX_KEYMAP_ENTRIES {
        return KeymapLoadResult::empty(KeymapFailure::TooLarge);
    }

    KeymapLoadResult {
        failure: None,
        legacy_entries,
        keys: parsed.keys,
    }
}

/// Writes `keys` as the `[keys]` table of `<config_root>/keymap.toml`,
/// replacing the whole file (a legacy `[bindings]` table is not kept).
///
/// `location` is `(config_base, config_root)`: the root is created if absent
/// and restricted to the owner, and the file is replaced atomically, through
/// [`yagura_os::replace_config_file`]. The `ActionId` vocabulary is not
/// checked here; that belongs to the WebView.
///
/// # Errors
///
/// - [`KeymapSaveFailure::NoConfigRoot`] when `location` is `None`.
/// - [`KeymapSaveFailure::TooLarge`] when `keys` has more than
///   [`MAX_KEYMAP_ENTRIES`] entries or serializes past [`MAX_KEYMAP_BYTES`];
///   nothing is written.
/// - [`KeymapSaveFailure::WriteFailed`] when serialization or the file
///   replacement fails; an existing file is left unchanged.
pub fn save_keymap_in(
    location: Option<(&Path, &Path)>,
    keys: &BTreeMap<String, String>,
) -> Result<(), KeymapSaveFailure> {
    let (config_base, config_root) = location.ok_or(KeymapSaveFailure::NoConfigRoot)?;

    if keys.len() > MAX_KEYMAP_ENTRIES {
        return Err(KeymapSaveFailure::TooLarge);
    }

    let content =
        toml::to_string(&KeymapFileToWrite { keys }).map_err(|_| KeymapSaveFailure::WriteFailed)?;
    // The read side caps the file at this many bytes, so a larger file would
    // be saved now and rejected as too large on the next start.
    let within_limit = u64::try_from(content.len()).is_ok_and(|len| len <= MAX_KEYMAP_BYTES);
    if !within_limit {
        return Err(KeymapSaveFailure::TooLarge);
    }

    yagura_os::replace_config_file(config_base, config_root, KEYMAP_FILE_NAME, &content)
        .map_err(|_| KeymapSaveFailure::WriteFailed)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use yagura_test_support::paths::TempDir;

    const TEMP_DIR_PREFIX: &str = "yagura-keymap-test";

    #[test]
    fn missing_file_reports_not_found_with_no_bindings() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "missing");

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, Some(KeymapFailure::NotFound));
        assert!(result.keys.is_empty());
    }

    #[test]
    fn well_formed_file_yields_its_keys_with_no_failure() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "ok");
        dir.write_file(
            KEYMAP_FILE_NAME,
            "[keys]\nfocus-next-window = \"Ctrl+Shift+ArrowRight\"\nopen-window-form = \"Ctrl+Shift+n\"\n",
        );

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(
            result.keys.get("focus-next-window"),
            Some(&"Ctrl+Shift+ArrowRight".to_owned())
        );
        assert_eq!(
            result.keys.get("open-window-form"),
            Some(&"Ctrl+Shift+n".to_owned())
        );
        assert_eq!(result.keys.len(), 2);
    }

    #[test]
    fn empty_bindings_table_is_not_a_failure() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "empty-table");
        dir.write_file(KEYMAP_FILE_NAME, "[bindings]\n");

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, None);
        assert!(result.keys.is_empty());
    }

    #[test]
    fn malformed_toml_reports_invalid_toml() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "malformed");
        dir.write_file(KEYMAP_FILE_NAME, "this is not { valid TOML");

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, Some(KeymapFailure::InvalidToml));
        assert!(result.keys.is_empty());
    }

    #[test]
    fn legacy_bindings_with_non_string_values_are_counted_not_rejected() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "wrong-shape");
        dir.write_file(KEYMAP_FILE_NAME, "[bindings]\n\"Ctrl+Alt+n\" = 42\n");

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.legacy_entries, 1);
        assert!(result.keys.is_empty());
    }

    #[test]
    fn file_too_large_in_bytes_reports_too_large_without_parsing() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit");
        // Deliberately not valid TOML: the byte-size check must trip before
        // any attempt to parse, so this proves it does not depend on parsing.
        let oversized = "a".repeat(MAX_KEYMAP_BYTES as usize + 1);
        dir.write_file(KEYMAP_FILE_NAME, &oversized);

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, Some(KeymapFailure::TooLarge));
        assert!(result.keys.is_empty());
    }

    #[test]
    fn file_at_the_byte_limit_is_not_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit-boundary");
        // A valid, parseable file whose byte length is exactly the limit.
        let prefix = "[bindings]\n";
        let padding = "#".repeat(MAX_KEYMAP_BYTES as usize - prefix.len());
        dir.write_file(KEYMAP_FILE_NAME, &format!("{prefix}{padding}"));

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, None);
    }

    #[test]
    fn too_many_entries_reports_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "entry-limit");
        let mut content = String::from("[bindings]\n");
        for index in 0..=MAX_KEYMAP_ENTRIES {
            content.push_str(&format!("\"Ctrl+Alt+{index}\" = \"open-window-form\"\n"));
        }
        dir.write_file(KEYMAP_FILE_NAME, &content);

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, Some(KeymapFailure::TooLarge));
        assert!(result.keys.is_empty());
    }

    #[test]
    fn exactly_the_entry_limit_is_not_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "entry-limit-boundary");
        let mut content = String::from("[bindings]\n");
        for index in 0..MAX_KEYMAP_ENTRIES {
            content.push_str(&format!("\"Ctrl+Alt+{index}\" = \"open-window-form\"\n"));
        }
        dir.write_file(KEYMAP_FILE_NAME, &content);

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.legacy_entries, MAX_KEYMAP_ENTRIES);
        assert!(result.keys.is_empty());
    }

    #[test]
    fn a_directory_in_place_of_the_file_reports_read_failed() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "read-failed");
        fs::create_dir_all(dir.path().join(KEYMAP_FILE_NAME)).expect("create fixture directory");

        let result = load_keymap(dir.path());

        assert_eq!(result.failure, Some(KeymapFailure::ReadFailed));
        assert!(result.keys.is_empty());
    }

    // Serialized with `toml::to_string` rather than `serde_json`: `toml` is
    // already a dependency here and drives the same `Serialize` impl.
    #[test]
    fn failure_is_omitted_from_the_serialized_success_payload() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "serialize-ok");
        dir.write_file(
            KEYMAP_FILE_NAME,
            "[keys]\nopen-window-form = \"Ctrl+Shift+n\"\n",
        );

        let result = load_keymap(dir.path());
        let rendered = toml::to_string(&result).expect("serialize result");

        assert!(!rendered.contains("failure"));
        assert!(!rendered.contains("bindings"));
        assert!(rendered.contains("keys"));
        assert!(rendered.contains("legacy_entries"));
    }

    #[test]
    fn failure_is_serialized_as_a_snake_case_string() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "serialize-failure");

        let result = load_keymap(dir.path());
        let rendered = toml::to_string(&result).expect("serialize result");

        assert!(rendered.contains("failure = \"not_found\""));
    }
}
