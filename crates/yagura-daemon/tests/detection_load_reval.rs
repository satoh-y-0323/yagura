#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Detection definition loading at window-open time: the `detection_warning`
//! that `open_window` reports for each failure case, and the sanitization of that
//! warning's summary.
//!
//! Each case pairs one failure trigger with the text its warning must carry, so the
//! wording that crosses the process boundary towards clients stays pinned.

mod common;

use std::ffi::OsString;
use std::path::PathBuf;

use yagura_test_support::paths::{TempDir, write_definition};

use yagura_daemon::cli;
use yagura_daemon::{DaemonConfig, WindowRegistry};

/// Stimulus string that the definitions built here map to `Running`.
const MARKER: &str = "DETECTION-LOAD-MARK";

/// Builds the smallest definition that maps [`MARKER`] to `Running`.
fn definition_toml(name: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '{MARKER}'
priority = 10
region = "bottom"
"#
    )
}

#[test]
fn detection_dir_is_explicit_covers_the_five_combinations() {
    let arg = PathBuf::from("some-arg-dir");
    let env = OsString::from("some-env-dir");
    let empty_env = OsString::new();

    assert!(
        cli::detection_dir_is_explicit(Some(arg.as_path()), None),
        "arg alone must be explicit"
    );
    assert!(
        cli::detection_dir_is_explicit(None, Some(env.as_os_str())),
        "env alone must be explicit"
    );
    assert!(
        cli::detection_dir_is_explicit(Some(arg.as_path()), Some(env.as_os_str())),
        "both arg and env must be explicit"
    );
    assert!(
        !cli::detection_dir_is_explicit(None, None),
        "neither arg nor env must not be explicit"
    );
    assert!(
        !cli::detection_dir_is_explicit(None, Some(empty_env.as_os_str())),
        "an empty-string env value counts as unset, symmetric with resolve_playbook, so \
         it must not put the directory layer into its exclusive mode"
    );
}

// Each case's expected substring is a single named constant matched against
// `OpenedWindow::detection_warning`, so a case's needle and its fixture are each defined
// once rather than repeated inline.

/// Every load failure carries this wrapper, whatever detail the case supplies, so the missing-name case's
/// two forms assert it alongside the needle that separates them.
const LOAD_FAILURE_WRAPPER_NEEDLE: &str = "failed to load the detection definition";
/// `DefinitionError::Io`'s own hardcoded (locale-independent) prefix: the raw read-failure
/// text a missing file in an exclusive directory layer must not surface to the caller.
/// The OS-supplied portion of that same message is locale-dependent (and can be cut short
/// by the summary's code-point cap), so this ASCII prefix -- not that portion -- is what a
/// exclusive-mode assertion can reliably tell apart.
const RAW_READ_FAILURE_NEEDLE: &str = "failed to read detection definition at";
/// With the bundled layer on, the detail must say that layer was searched too.
const OVERLAY_LAYER_NEEDLE: &str = "bundled";
/// The explicit-path missing and unparseable cases both reach `load_definition`'s own `Err`, so [`EXPLICIT_MISSING_NEEDLE`] and
/// [`EXPLICIT_UNPARSEABLE_NEEDLE`] share the same literal text on purpose; they stay two names so each
/// case's test remains independently readable if the two triggers' wording diverges.
const EXPLICIT_MISSING_NEEDLE: &str = "failed to load the detection definition";
const EXPLICIT_UNPARSEABLE_NEEDLE: &str = "failed to load the detection definition";
const INVALID_DEFAULT_NAME_NEEDLE: &str = "invalid detection name";

// A miss in exclusive mode (`bundled = false`, at the window boundary): a
// missing file falls through regardless of `bundled`, so the directory layer is
// fully searched and finds nothing, and this must be reported as the name
// not being found -- naming the directory that was searched but never
// claiming the bundled layer was searched too -- rather than as an I/O
// error.
#[tokio::test]
async fn open_reports_exclusive_mode_reports_not_found_instead_of_the_raw_read_failure() {
    // The default-name path: `detection` is None, so `config.detection_name` supplies the
    // name, and no matching .toml exists in an otherwise scannable directory. With the
    // bundled layer off, no layer can answer.
    // Intentionally empty: config.detection_name's .toml is absent. Kept short so the
    // whole detail survives the warning's length cap on hosts with a long temp dir.
    let tmp = TempDir::new("yg", "exclusive");
    let dir = tmp.path().to_path_buf();

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir.clone()),
        bundled_definitions: false,
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, None).await;

    assert!(!opened.window_id.is_empty(), "window_id must not be empty");
    let warning = opened
        .detection_warning
        .expect("a miss in exclusive mode must produce a detection_warning");
    assert!(
        warning.contains(LOAD_FAILURE_WRAPPER_NEEDLE),
        "the warning must contain {LOAD_FAILURE_WRAPPER_NEEDLE:?}, got: {warning}"
    );
    assert!(
        !warning.contains(RAW_READ_FAILURE_NEEDLE),
        "with the bundled layer off, an absent name must not surface the directory \
         layer's raw read-failure text ({RAW_READ_FAILURE_NEEDLE:?}), got: {warning}"
    );
    let dir_display = dir.display().to_string();
    assert!(
        warning.contains(&dir_display),
        "the detail must still name the directory that was searched ({dir_display:?}), got: \
         {warning}"
    );
    assert!(
        !warning.to_ascii_lowercase().contains("bundled"),
        "with the bundled layer off, the detail must not claim the bundled layer was searched \
         too, got: {warning}"
    );
    assert!(
        registry.get(&opened.window_id).is_some(),
        "the window must be open despite the warning"
    );

    registry.terminate_all().await;
}

// The directory layer cannot even be scanned (it does
// not exist) and the bundled layer is off, so nothing was actually searched
// for this name. The window side must not claim the name "was found" --
// that claim requires having actually looked -- it must instead say the
// inventory could not be confirmed, and it must not leak the directory path
// the confirmed-miss and read-failure wordings both carry.
// Accepted cost: the directory's absolute path does not appear on the
// screen at all for this case.
#[tokio::test]
async fn open_reports_an_unscannable_directory_as_unconfirmed_rather_than_not_found() {
    let root = TempDir::new("yg", "unscannable");
    let unreadable = root.path().join("does-not-exist");
    let name = "unscannable-missing-everywhere";

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(unreadable.clone()),
        bundled_definitions: false,
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("an unscannable inventory must still produce a detection_warning");
    assert!(
        warning.contains(LOAD_FAILURE_WRAPPER_NEEDLE),
        "the warning must still report a load failure, got: {warning}"
    );
    assert!(
        !warning.contains(RAW_READ_FAILURE_NEEDLE),
        "an unscannable directory must not surface a raw read-failure text ({RAW_READ_FAILURE_NEEDLE:?}) \
         either, got: {warning}"
    );
    assert!(
        !warning.contains("was found"),
        "an unscannable inventory must not be phrased as a confirmed miss, got: \
         {warning}"
    );
    assert!(
        warning.contains("could not be"),
        "the warning must say the inventory could not be confirmed rather than staying silent \
         about why, got: {warning}"
    );
    let searched = unreadable.display().to_string();
    assert!(
        !warning.contains(&searched),
        "the shared sentence must not carry the directory path, even though the banner then \
         shows no path; this is the accepted cost of the shared wording, got: {warning}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_overlay_mode_reports_the_name_and_both_layers() {
    // Same default-name path with the bundled layer on, under a name no bundled
    // definition carries, so neither layer answers it. The fixture directory is kept
    // short so the whole detail survives the warning's length cap.
    let dir = TempDir::new("yg", "overlay");
    let name = "overlay-absent";

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir.path().to_path_buf()),
        bundled_definitions: true,
        detection_name: name.to_string(),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, None).await;

    let warning = opened
        .detection_warning
        .expect("a miss in overlay mode must produce a detection_warning");
    assert!(
        warning.contains(LOAD_FAILURE_WRAPPER_NEEDLE),
        "the warning must contain {LOAD_FAILURE_WRAPPER_NEEDLE:?}, got: {warning}"
    );
    assert!(
        warning.contains(name),
        "the detail must name the definition that was asked for ({name:?}), got: {warning}"
    );
    let searched = dir.path().display().to_string();
    assert!(
        warning.contains(&searched),
        "the detail must name the directory that was searched ({searched:?}), got: {warning}"
    );
    assert!(
        warning.contains(OVERLAY_LAYER_NEEDLE),
        "the detail must say the bundled layer was searched too (needle \
         {OVERLAY_LAYER_NEEDLE:?}), got: {warning}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_explicit_detection_path_failed_to_load_when_the_named_toml_is_absent() {
    // A name given explicitly, absent from the directory layer, with the bundled layer
    // off: the directory layer is fully searched and has no file for this name, so this
    // resolves to LayerError::NotFound rather than the directory layer's own read failure.
    let tree = TempDir::new("yagura-cli-test", "explicit-missing");
    let dir = tree.make_dir("repo/detection");
    let name = "missing-def";

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("an absent explicit detection path must produce a detection_warning");
    assert!(
        warning.contains(EXPLICIT_MISSING_NEEDLE),
        "the warning must contain {EXPLICIT_MISSING_NEEDLE:?}, got: {warning}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_explicit_detection_path_failed_to_load_when_the_toml_does_not_parse() {
    // The file exists but its contents are not valid TOML, so the directory layer fails
    // on a definition it does carry.
    let tree = TempDir::new("yagura-cli-test", "explicit-unparseable");
    let dir = tree.make_dir("repo/detection");
    let name = "bad-def";
    std::fs::write(dir.join(format!("{name}.toml")), "this is not [ valid toml")
        .expect("write the unparseable definition file");

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("an unparseable explicit detection path must produce a detection_warning");
    assert!(
        warning.contains(EXPLICIT_UNPARSEABLE_NEEDLE),
        "the warning must contain {EXPLICIT_UNPARSEABLE_NEEDLE:?}, got: {warning}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_defensive_invalid_default_detection_name() {
    // Defensive only: `DaemonConfig::detection_name` bypasses the CLI's own
    // `is_valid_detection_name` gate solely when a test builds the config directly.
    let tree = TempDir::new("yagura-cli-test", "invalid-default-name");
    let dir = tree.make_dir("repo/detection");

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        detection_name: "..".to_string(),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, None).await;

    let warning = opened
        .detection_warning
        .expect("an invalid default detection name must produce a detection_warning");
    assert!(
        warning.contains(INVALID_DEFAULT_NAME_NEEDLE),
        "the warning must contain {INVALID_DEFAULT_NAME_NEEDLE:?}, got: {warning}"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_no_warning_with_an_explicit_valid_detection_name() {
    let tree = TempDir::new("yagura-cli-test", "explicit-valid");
    let dir = tree.make_dir("repo/detection");
    let name = "explicit-def";
    write_definition(&dir, name, &definition_toml(name));

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    assert_eq!(
        opened.detection_warning, None,
        "a valid, explicitly-named definition must not produce a warning"
    );

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_no_warning_with_the_default_detection_name() {
    let tree = TempDir::new("yagura-cli-test", "default-valid");
    let dir = tree.make_dir("repo/detection");
    write_definition(&dir, "generic-shell", &definition_toml("generic-shell"));

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, None).await;

    assert_eq!(
        opened.detection_warning, None,
        "a valid default-named definition must not produce a warning"
    );

    registry.terminate_all().await;
}

// A unique canary is embedded in a syntactically broken TOML fixture: toml's `Display`
// echoes the offending source line back verbatim after its first line, which is exactly
// the leak the sanitization closes.

/// Unique marker planted inside the broken TOML fixture below. It must never reach the
/// caller-facing `detection_warning`; its presence in the raw `Display` output is what
/// proves the fixture exercises the multi-line echo.
const SANITIZATION_CANARY: &str = "E0_CANARY_SECRET_LINE";

#[tokio::test]
async fn open_reports_a_sanitized_single_line_warning_for_an_unparseable_definition_with_a_canary()
{
    let tree = TempDir::new("yagura-cli-test", "sanitize-canary-unparseable");
    let dir = tree.make_dir("repo/detection");
    let name = "sanitize-bad-def";
    // Syntactically broken (unbalanced `[[[`), so `toml::de::Error`'s `Display` echoes
    // this exact line back in its second line, after a first line limited to position
    // information.
    let broken = format!(
        "schema_version = 1\nname = \"{name}\"\n{SANITIZATION_CANARY} this is not valid toml [[[\n"
    );
    std::fs::write(dir.join(format!("{name}.toml")), broken)
        .expect("write the unparseable definition file");

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("an unparseable definition must produce a detection_warning");

    // (a) canary must not leak into the caller-facing warning.
    assert!(
        !warning.contains(SANITIZATION_CANARY),
        "warning must not contain the canary secret line embedded in the broken TOML fixture \
         (only the Display's first line may pass the sanitization); got: {warning}"
    );
    // (b) the warning must be a single line (no embedded newline).
    assert!(
        !warning.contains('\n'),
        "warning must not contain a newline (only the first line passes); got: {warning:?}"
    );
    // (c) needle prefix and the TOML parser's position-information text must
    // both survive the sanitization.
    assert!(
        warning.contains(EXPLICIT_UNPARSEABLE_NEEDLE),
        "warning must retain the needle prefix {EXPLICIT_UNPARSEABLE_NEEDLE:?}; got: {warning}"
    );
    assert!(
        warning.contains("TOML parse error"),
        "warning must retain the toml parser's position-information text; got: {warning}"
    );
    // (d) 200-codepoint cap.
    assert!(
        warning.chars().count() <= 200,
        "warning must be at most 200 code points; got {} code points: {warning}",
        warning.chars().count()
    );

    registry.terminate_all().await;
}

// `DefinitionError::InvalidPattern`'s Display embeds the raw rule source in its FIRST
// line, so a line-cut summary cannot sanitize it wherever the cut falls; only a stock,
// content-free message closes this leak.

/// Unique marker planted as the (invalid) regex pattern itself below. It must never
/// reach the caller-facing `detection_warning`; its presence in the raw `Display` output
/// is what proves the fixture exercises the first-line embedding.
const INVALID_PATTERN_CANARY: &str = "E0_CANARY_REGEX_(";

/// The stock, content-free text `InvalidPattern` must fall back to.
const INVALID_PATTERN_STOCK_TEXT: &str = "invalid regex pattern in the detection definition";

#[tokio::test]
async fn open_reports_a_stock_warning_for_an_invalid_pattern_definition_with_a_canary() {
    let tree = TempDir::new("yagura-cli-test", "invalid-pattern-canary");
    let dir = tree.make_dir("repo/detection");
    let name = "invalid-pattern-def";
    // Syntactically valid TOML that passes schema validation, but the rule's `pattern`
    // content is not a valid regular expression, so `compile_rule` returns
    // `DefinitionError::InvalidPattern` rather than a `toml::de::Error`.
    let contents = format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = '{INVALID_PATTERN_CANARY}'
priority = 10
region = "bottom"
"#
    );
    std::fs::write(dir.join(format!("{name}.toml")), contents)
        .expect("write the invalid-pattern definition file");

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("an invalid regex pattern must produce a detection_warning");

    // (a) canary (the raw, invalid pattern text) must not leak into the
    // caller-facing warning.
    assert!(
        !warning.contains(INVALID_PATTERN_CANARY),
        "warning must not contain the canary pattern text embedded in the invalid-pattern \
         fixture (InvalidPattern must use a stock message, never the pattern \
         text); got: {warning}"
    );
    // (b) the stock, content-free text must be present instead.
    assert!(
        warning.contains(INVALID_PATTERN_STOCK_TEXT),
        "warning must contain the stock text {INVALID_PATTERN_STOCK_TEXT:?}; got: {warning}"
    );
    // (c) needle prefix must still be present (needle contract unchanged).
    assert!(
        warning.contains(EXPLICIT_UNPARSEABLE_NEEDLE),
        "warning must retain the needle prefix {EXPLICIT_UNPARSEABLE_NEEDLE:?}; got: {warning}"
    );

    registry.terminate_all().await;
}

#[test]
fn daemon_config_default_leaves_the_bundled_layer_off() {
    assert!(
        !DaemonConfig::default().bundled_definitions,
        "DaemonConfig::default() is what fixtures build on, so the bundled layer must stay \
         off there: a name a fixture directory does not carry must not quietly resolve to a \
         bundled definition"
    );
}
