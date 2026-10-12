//! Reads `themes.toml`, the user's palette definitions and vendor/default
//! assignments.
//!
//! Colour syntax, the reserved-name vocabulary, and the default-selection
//! mapping are validated on the TypeScript side (`theme-resolve.ts`);
//! this module only turns the file into raw string maps, or
//! reports why it could not (raw data conversion only).
//!
//! The base directory is a caller-supplied argument, not something this
//! module resolves itself, so it needs no `AppHandle` and is
//! reachable from `#[cfg(test)]` with a plain temp directory.
//!
//! File shape (the frozen on-disk contract; distinct from
//! [`ThemesLoadResult`], the frozen wire contract produced from it):
//!
//! ```toml
//! [themes.custom-dark]
//! color-bg-primary = "#101010"
//! # ... the remaining palette keys, no leading "--"
//!
//! [assignments]
//! default = "custom-dark"
//! [assignments.vendor]
//! claude = "custom-dark"
//! codex = "dark"
//! ```

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name of the themes file, resolved under the caller-supplied base
/// directory (`%LOCALAPPDATA%\io.github.satoh-y-0323.yagura\themes.toml` on Windows,
/// `~/Library/Application Support/yagura/themes.toml` on macOS,
/// `~/.config/yagura/themes.toml` on other Unix, in production, via
/// `config_paths::config_root`).
const THEMES_FILE_NAME: &str = "themes.toml";

/// Read limit for `themes.toml`: a hand-written palette file has
/// no legitimate reason to exceed it. Enforced during the read, since another
/// process running as the same user could grow the file.
const MAX_THEMES_BYTES: u64 = 64 * 1024;

/// Entry-count limit for the `[themes.*]` table, checked after
/// parsing.
const MAX_THEME_COUNT: usize = 50;

/// Key-count limit for each individual theme table, checked after
/// parsing.
const MAX_THEME_KEYS: usize = 64;

/// Why `themes.toml` could not be loaded. Serializes as a `snake_case`
/// string across the Tauri command boundary (frozen contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemesFailure {
    /// No file at `<base_dir>/themes.toml`. Not a defect: most installs
    /// never create this file and keep the built-in palettes only.
    NotFound,
    /// The file exists but could not be read as text (e.g. it is a
    /// directory, or its bytes are not valid UTF-8).
    ReadFailed,
    /// The file's bytes parse as text but not as the expected shape (see
    /// the module doc comment).
    InvalidToml,
    /// The file exceeds [`MAX_THEMES_BYTES`], its `[themes.*]` table
    /// exceeds [`MAX_THEME_COUNT`] entries, or one theme table exceeds
    /// [`MAX_THEME_KEYS`] keys.
    TooLarge,
}

/// Result of a [`load_themes`] attempt (frozen contract).
///
/// `themes` and `vendor_assignments` are both empty exactly when `failure`
/// is `Some`: there is no partial success (fail-safe).
///
/// Scalar fields are declared before the table-like ones because the `toml`
/// crate requires that field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemesLoadResult {
    /// Set when [`ThemesLoadResult::themes`] and
    /// [`ThemesLoadResult::vendor_assignments`] are empty because loading
    /// failed. Omitted from the wire payload on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<ThemesFailure>,
    /// The `default` key from the file, unvalidated (`theme-resolve.ts`
    /// resolves it against the reserved-name vocabulary and the builtin
    /// palettes). Omitted from the wire payload when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_theme: Option<String>,
    /// Theme name to (CSS-variable-name-without-`--` to colour value) map,
    /// exactly as written in the file.
    pub themes: HashMap<String, HashMap<String, String>>,
    /// Vendor name to theme name, exactly as written in the file's
    /// `[assignments.vendor]` table.
    pub vendor_assignments: HashMap<String, String>,
}

impl ThemesLoadResult {
    /// An empty result reporting why it is empty.
    ///
    /// An unresolvable config root is reported as `not_found`: there is no
    /// location to look in, the same outcome an absent file produces.
    pub(crate) fn empty(failure: ThemesFailure) -> Self {
        Self {
            failure: Some(failure),
            default_theme: None,
            themes: HashMap::new(),
            vendor_assignments: HashMap::new(),
        }
    }

    fn ok(
        themes: HashMap<String, HashMap<String, String>>,
        vendor_assignments: HashMap<String, String>,
        default_theme: Option<String>,
    ) -> Self {
        Self {
            failure: None,
            default_theme,
            themes,
            vendor_assignments,
        }
    }
}

/// The shape `themes.toml` must deserialize into. Any other shape (wrong
/// table types, non-string leaf values, ...) is `invalid_toml`.
#[derive(Debug, Deserialize)]
struct ThemesFile {
    #[serde(default)]
    themes: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    assignments: Assignments,
}

/// The `[assignments]` table: a `default` theme name plus a nested
/// `[assignments.vendor]` table. Both sections are independently optional,
/// as is `[assignments]` itself.
#[derive(Debug, Default, Deserialize)]
struct Assignments {
    default: Option<String>,
    #[serde(default)]
    vendor: HashMap<String, String>,
}

/// Loads `themes.toml` from `<base_dir>/themes.toml`.
///
/// Never panics: every failure mode is reported through
/// [`ThemesLoadResult::failure`] instead of `Err` (fail-safe). No
/// colour-syntax validation happens here: a value like
/// `"not-a-color"` passes through as-is and is caught by the TS-side choke
/// point.
///
/// The byte limit is enforced on the same file handle that is read, so a
/// process growing the file cannot slip past a separate size check (CWE-367).
pub fn load_themes(base_dir: &Path) -> ThemesLoadResult {
    let path = base_dir.join(THEMES_FILE_NAME);

    // A missing file is a legitimate case (most installs never create
    // themes.toml); every other open error is a read failure.
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ThemesLoadResult::empty(ThemesFailure::NotFound);
        }
        Err(_) => return ThemesLoadResult::empty(ThemesFailure::ReadFailed),
    };

    // Bytes that are not valid UTF-8 are a read failure rather than invalid
    // TOML: the parser below never sees content that cannot be decoded.
    let mut limited_reader = file.take(MAX_THEMES_BYTES);
    let mut content = String::new();
    match limited_reader.read_to_string(&mut content) {
        Ok(_) => {}
        Err(_) => return ThemesLoadResult::empty(ThemesFailure::ReadFailed),
    }

    // One further byte from the underlying handle means the file exceeded the
    // limit; the capped reader itself can never see past it.
    let mut underlying_file = limited_reader.into_inner();
    let mut extra = [0u8; 1];
    if underlying_file.read_exact(&mut extra).is_ok() {
        return ThemesLoadResult::empty(ThemesFailure::TooLarge);
    }

    let parsed: ThemesFile = match toml::from_str(&content) {
        Ok(parsed) => parsed,
        Err(_) => return ThemesLoadResult::empty(ThemesFailure::InvalidToml),
    };

    if parsed.themes.len() > MAX_THEME_COUNT {
        return ThemesLoadResult::empty(ThemesFailure::TooLarge);
    }
    if parsed
        .themes
        .values()
        .any(|tokens| tokens.len() > MAX_THEME_KEYS)
    {
        return ThemesLoadResult::empty(ThemesFailure::TooLarge);
    }

    ThemesLoadResult::ok(
        parsed.themes,
        parsed.assignments.vendor,
        parsed.assignments.default,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use yagura_test_support::paths::TempDir;

    const TEMP_DIR_PREFIX: &str = "yagura-themes-test";

    #[test]
    fn missing_file_reports_not_found_with_no_data() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "missing");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::NotFound));
        assert!(result.themes.is_empty());
        assert!(result.vendor_assignments.is_empty());
        assert_eq!(result.default_theme, None);
    }

    #[test]
    fn a_directory_in_place_of_the_file_reports_read_failed() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "read-failed-dir");
        fs::create_dir_all(dir.path().join(THEMES_FILE_NAME)).expect("create fixture directory");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::ReadFailed));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn bytes_that_are_not_valid_utf8_report_read_failed() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "read-failed-utf8");
        fs::write(dir.path().join(THEMES_FILE_NAME), [0xff, 0xfe, 0x00, 0xff])
            .expect("write invalid utf8 fixture");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::ReadFailed));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn malformed_toml_reports_invalid_toml() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "malformed");
        dir.write_file(THEMES_FILE_NAME, "this is not { valid TOML");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::InvalidToml));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn a_theme_table_with_a_non_string_value_reports_invalid_toml() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "wrong-shape");
        dir.write_file(THEMES_FILE_NAME, "[themes.custom]\ncolor-bg-primary = 42\n");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::InvalidToml));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn file_too_large_in_bytes_reports_too_large_without_parsing() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit");
        // Deliberately not valid TOML: the byte-size check must trip before
        // any attempt to parse, so this proves it does not depend on parsing.
        let oversized = "a".repeat(MAX_THEMES_BYTES as usize + 1);
        dir.write_file(THEMES_FILE_NAME, &oversized);

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::TooLarge));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn file_at_the_byte_limit_is_not_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "byte-limit-boundary");
        // A valid, parseable file whose byte length is exactly the limit.
        let prefix = "[assignments]\n";
        let padding = "#".repeat(MAX_THEMES_BYTES as usize - prefix.len());
        dir.write_file(THEMES_FILE_NAME, &format!("{prefix}{padding}"));

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
    }

    #[test]
    fn too_many_themes_reports_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "theme-count-limit");
        let mut content = String::new();
        for index in 0..=MAX_THEME_COUNT {
            content.push_str(&format!(
                "[themes.t{index}]\ncolor-bg-primary = \"#000000\"\n"
            ));
        }
        dir.write_file(THEMES_FILE_NAME, &content);

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::TooLarge));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn exactly_the_theme_count_limit_is_not_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "theme-count-boundary");
        let mut content = String::new();
        for index in 0..MAX_THEME_COUNT {
            content.push_str(&format!(
                "[themes.t{index}]\ncolor-bg-primary = \"#000000\"\n"
            ));
        }
        dir.write_file(THEMES_FILE_NAME, &content);

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.themes.len(), MAX_THEME_COUNT);
    }

    #[test]
    fn too_many_keys_in_one_theme_reports_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "key-count-limit");
        let mut content = String::from("[themes.custom]\n");
        for index in 0..=MAX_THEME_KEYS {
            content.push_str(&format!("color-key-{index} = \"#000000\"\n"));
        }
        dir.write_file(THEMES_FILE_NAME, &content);

        let result = load_themes(dir.path());

        assert_eq!(result.failure, Some(ThemesFailure::TooLarge));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn exactly_the_key_count_limit_is_not_too_large() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "key-count-boundary");
        let mut content = String::from("[themes.custom]\n");
        for index in 0..MAX_THEME_KEYS {
            content.push_str(&format!("color-key-{index} = \"#000000\"\n"));
        }
        dir.write_file(THEMES_FILE_NAME, &content);

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(
            result.themes.get("custom").map(HashMap::len),
            Some(MAX_THEME_KEYS)
        );
    }

    #[test]
    fn well_formed_file_yields_its_data_with_no_failure() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "ok");
        dir.write_file(THEMES_FILE_NAME,
            "[themes.custom-dark]\ncolor-bg-primary = \"#101010\"\n\n[assignments]\ndefault = \"custom-dark\"\n[assignments.vendor]\nclaude = \"custom-dark\"\ncodex = \"dark\"\n",
        );

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(
            result
                .themes
                .get("custom-dark")
                .and_then(|tokens| tokens.get("color-bg-primary")),
            Some(&"#101010".to_owned())
        );
        assert_eq!(
            result.vendor_assignments.get("claude"),
            Some(&"custom-dark".to_owned())
        );
        assert_eq!(
            result.vendor_assignments.get("codex"),
            Some(&"dark".to_owned())
        );
        assert_eq!(result.default_theme, Some("custom-dark".to_owned()));
    }

    #[test]
    fn a_file_with_no_default_key_yields_none_default_theme() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "no-default");
        dir.write_file(THEMES_FILE_NAME,
            "[themes.custom]\ncolor-bg-primary = \"#101010\"\n\n[assignments.vendor]\nclaude = \"custom\"\n",
        );

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.default_theme, None);
        assert_eq!(
            result.vendor_assignments.get("claude"),
            Some(&"custom".to_owned())
        );
    }

    #[test]
    fn a_file_with_no_assignments_section_yields_empty_assignments() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "no-assignments-section");
        dir.write_file(
            THEMES_FILE_NAME,
            "[themes.custom]\ncolor-bg-primary = \"#101010\"\n",
        );

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.default_theme, None);
        assert!(result.vendor_assignments.is_empty());
    }

    #[test]
    fn a_file_with_default_but_no_vendor_subtable_yields_empty_vendor_assignments() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "no-vendor-subtable");
        dir.write_file(THEMES_FILE_NAME, "[assignments]\ndefault = \"dark\"\n");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert_eq!(result.default_theme, Some("dark".to_owned()));
        assert!(result.vendor_assignments.is_empty());
    }

    #[test]
    fn an_empty_file_yields_empty_data_with_no_failure() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "empty-file");
        dir.write_file(THEMES_FILE_NAME, "");

        let result = load_themes(dir.path());

        assert_eq!(result.failure, None);
        assert!(result.themes.is_empty());
        assert!(result.vendor_assignments.is_empty());
        assert_eq!(result.default_theme, None);
    }

    // Serialized with `toml::to_string` rather than `serde_json`: `toml` is
    // already a dependency here and drives the same `Serialize` impl.
    #[test]
    fn failure_and_default_theme_are_omitted_from_the_serialized_success_payload() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "serialize-ok");
        dir.write_file(
            THEMES_FILE_NAME,
            "[themes.custom]\ncolor-bg-primary = \"#101010\"\n",
        );

        let result = load_themes(dir.path());
        let rendered = toml::to_string(&result).expect("serialize result");

        assert!(!rendered.contains("failure"));
        assert!(!rendered.contains("default_theme"));
        assert!(rendered.contains("themes"));
        assert!(rendered.contains("vendor_assignments"));
    }

    #[test]
    fn failure_is_serialized_as_a_snake_case_string() {
        let dir = TempDir::new(TEMP_DIR_PREFIX, "serialize-failure");

        let result = load_themes(dir.path());
        let rendered = toml::to_string(&result).expect("serialize result");

        assert!(rendered.contains("failure = \"not_found\""));
    }
}
