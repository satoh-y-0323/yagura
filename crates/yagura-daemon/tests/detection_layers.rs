#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Coverage of the two-layer detection definition resolver
//! (`detector::layers::DetectionLayers`): which layer answers `resolve`,
//! what `list` reports about each layer, and the `open_window` warning that
//! eventually carries a resolution failure to a client.

mod common;

use yagura_daemon::detector::definition::{
    bundled_definition_names, bundled_definition_source, parse_definition,
};
use yagura_daemon::detector::layers::{DefinitionSource, DetectionLayers, LayerError};
use yagura_daemon::{DaemonConfig, WindowRegistry};
use yagura_test_support::paths::{unique_detection_dir, write_definition};

/// A minimal definition body that parses and validates under `name`.
fn valid_definition_toml(name: &str) -> String {
    format!(
        r#"
schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = 'DETECTION-LAYERS-MARK'
priority = 10
region = "bottom"
"#
    )
}

/// A body that is not valid TOML at all.
const MALFORMED_TOML: &str = "detection-layers-fixture :: not [ valid toml";

/// One bundled definition name, for tests that need a name the bundled
/// layer actually carries.
fn a_bundled_name() -> &'static str {
    bundled_definition_names()
        .first()
        .copied()
        .expect("the crate always bundles at least one definition")
}

// --- resolve(): the directory layer wins when it has the name -------------

#[test]
fn resolve_prefers_a_valid_directory_definition_over_a_same_named_bundled_one() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l1",
        "creating the directory-layer fixture must succeed",
    );
    let name = a_bundled_name();
    write_definition(&dir, name, &valid_definition_toml(name));

    let layers = DetectionLayers {
        directory: Some(dir.clone()),
        bundled: true,
    };

    let resolved = layers
        .resolve(name)
        .expect("a valid directory definition must resolve");
    assert_eq!(resolved.definition.name, name);
    match resolved.source {
        DefinitionSource::Directory { path, bytes, .. } => {
            assert_eq!(path, dir.join(format!("{name}.toml")));
            assert!(
                bytes.is_some(),
                "a definition read from disk must carry its size"
            );
        }
        DefinitionSource::Bundled => panic!(
            "a directory definition present under a bundled name must not fall through to \
             the bundled layer"
        ),
    }
}

#[test]
fn resolve_reports_a_directory_parse_failure_without_falling_back_to_bundled() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l2",
        "creating the directory-layer fixture must succeed",
    );
    let name = a_bundled_name();
    write_definition(&dir, name, MALFORMED_TOML);
    let expected = parse_definition(MALFORMED_TOML).expect_err("the fixture body must not parse");

    let layers = DetectionLayers {
        directory: Some(dir.clone()),
        bundled: true,
    };

    match layers.resolve(name) {
        Err(LayerError::Directory { path, source }) => {
            assert_eq!(path, dir.join(format!("{name}.toml")));
            assert_eq!(
                source.to_string(),
                expected.to_string(),
                "the directory layer's own parse failure must reach the caller unchanged"
            );
        }
        other => panic!("expected a directory parse failure, got {other:?}"),
    }
}

#[test]
fn resolve_falls_back_to_the_bundled_layer_when_the_directory_lacks_the_name() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l3",
        "creating the directory-layer fixture must succeed",
    );
    let name = a_bundled_name();
    // The directory is left empty: no file exists for `name`.

    let layers = DetectionLayers {
        directory: Some(dir),
        bundled: true,
    };

    let resolved = layers.resolve(name).expect("the bundled layer must answer");
    assert!(
        matches!(resolved.source, DefinitionSource::Bundled),
        "a name absent from the directory but present in the bundled layer must resolve there"
    );
    let expected = parse_definition(bundled_definition_source(name).expect("a bundled source"))
        .expect("the bundled source itself must parse");
    assert_eq!(resolved.definition.name, expected.name);
}

// Directory-only (bundled = false): with the bundled layer off, a scannable directory missing
// the requested name must report NotFound -- naming the directory it
// actually searched but never claiming the bundled layer was searched too
// -- rather than reshaping the directory layer's own plain read failure.
#[test]
fn resolve_reports_not_found_when_the_directory_is_exclusive_and_scannable_but_lacks_the_name() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l4a",
        "creating the directory-layer fixture must succeed",
    );
    let name = "layers-l4a-missing";
    // The directory itself is scannable (it was just created); only the
    // file for `name` is absent, and the bundled layer is off, so no layer
    // can answer.

    let layers = DetectionLayers {
        directory: Some(dir.clone()),
        bundled: false,
    };

    let err = layers
        .resolve(name)
        .expect_err("a scannable directory missing the name must not resolve");
    match &err {
        LayerError::NotFound {
            name: reported,
            layers: searched,
        } => {
            assert_eq!(reported, name);
            assert_eq!(searched, &layers);
        }
        other => panic!(
            "a scannable directory missing the name must report NotFound regardless of the \
             bundled layer, got {other:?}"
        ),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains(name),
        "must name the requested name, got: {rendered}"
    );
    assert!(
        rendered.contains(&dir.display().to_string()),
        "must name the directory that was searched, got: {rendered}"
    );
    assert!(
        !rendered.to_ascii_lowercase().contains("bundled"),
        "with the bundled layer off, the sentence must not describe a layer that was not \
         searched, got: {rendered}"
    );
}

// A name the bundled layer really carries must still not be served by
// an exclusive directory layer, now that the failure it hits is NotFound
// rather than the old plain read failure.
#[test]
fn resolve_does_not_serve_a_bundled_name_when_the_directory_is_exclusive() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l4a-bundled-name",
        "creating the directory-layer fixture must succeed",
    );
    let name = a_bundled_name();
    // The directory carries no file for a name the bundled layer does have:
    // an exclusive directory layer must not fall through to serve it.

    let layers = DetectionLayers {
        directory: Some(dir),
        bundled: false,
    };

    match layers.resolve(name) {
        Ok(resolved) => {
            panic!("an exclusive directory layer must not serve a bundled name, got {resolved:?}")
        }
        Err(LayerError::NotFound { name: reported, .. }) => {
            assert_eq!(reported, name);
        }
        Err(other) => panic!("expected NotFound, not a bundled fallback, got {other:?}"),
    }
}

// Directory plus bundled layer: both layers were searched, so the sentence must name the
// requested name, the directory, and the bundled layer together.
#[test]
fn resolve_reports_not_found_naming_the_searched_directory_when_bundled_is_overlaid() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l4b",
        "creating the directory-layer fixture must succeed",
    );
    let name = "layers-l4b-missing-everywhere";
    assert!(
        !bundled_definition_names().contains(&name),
        "fixture assumption: this name is not a real bundled definition"
    );

    let layers = DetectionLayers {
        directory: Some(dir.clone()),
        bundled: true,
    };

    let err = layers
        .resolve(name)
        .expect_err("a name absent from both layers must not resolve");
    match &err {
        LayerError::NotFound {
            name: reported,
            layers: searched,
        } => {
            assert_eq!(reported, name);
            assert_eq!(searched, &layers);
        }
        other => panic!("expected NotFound naming the searched directory, got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains(name),
        "must name the requested name, got: {rendered}"
    );
    assert!(
        rendered.contains(&dir.display().to_string()),
        "must name the directory that was searched, got: {rendered}"
    );
    assert!(
        rendered.to_ascii_lowercase().contains("bundled"),
        "must say the bundled layer was searched too, got: {rendered}"
    );
}

// With no directory layer at all, the
// sentence must not describe a directory that was never searched.
#[test]
fn resolve_reports_not_found_without_a_directory_when_only_bundled_is_configured() {
    let name = "layers-l4c-missing-everywhere";
    assert!(
        !bundled_definition_names().contains(&name),
        "fixture assumption: this name is not a real bundled definition"
    );

    let layers = DetectionLayers {
        directory: None,
        bundled: true,
    };

    let err = layers
        .resolve(name)
        .expect_err("a name absent from the bundled layer must not resolve");
    match &err {
        LayerError::NotFound {
            name: reported,
            layers: searched,
        } => {
            assert_eq!(reported, name);
            assert_eq!(searched, &layers);
        }
        other => panic!("expected NotFound without a directory, got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains(name),
        "must name the requested name, got: {rendered}"
    );
    assert!(
        !rendered.to_ascii_lowercase().contains("directory"),
        "with no directory layer configured, the sentence must not describe one that was \
         never searched, got: {rendered}"
    );
}

// `LayerError` is `pub`, so a caller can construct a `NotFound` value
// whose layers `resolve` itself never produces (no directory, bundled off).
// The Display must still not panic, and must still name the requested name.
#[test]
fn not_found_display_does_not_panic_when_no_layer_was_searched() {
    let err = LayerError::NotFound {
        name: "unreachable-combination".to_string(),
        layers: DetectionLayers {
            directory: None,
            bundled: false,
        },
    };

    let rendered = err.to_string();
    assert!(
        !rendered.is_empty(),
        "the Display must produce a sentence rather than panicking"
    );
    assert!(
        rendered.contains("unreachable-combination"),
        "the sentence must still name the requested name, got: {rendered}"
    );
}

// The regression this must never reintroduce. A directory layer
// that cannot be scanned must not turn a name the bundled layer really
// carries into a failure -- the success path never consults the inventory.
#[test]
fn resolve_still_resolves_via_bundled_when_the_directory_cannot_be_scanned() {
    let root = unique_detection_dir(
        "yg",
        "layers-l4d-unscannable-but-bundled",
        "creating the directory-layer fixture must succeed",
    );
    let unreadable = root.join("does-not-exist");
    let name = a_bundled_name();

    let layers = DetectionLayers {
        directory: Some(unreadable),
        bundled: true,
    };

    let resolved = layers.resolve(name).expect(
        "an unscannable directory overlaid with the bundled layer must still resolve a \
         bundled name",
    );
    assert!(
        matches!(resolved.source, DefinitionSource::Bundled),
        "the bundled layer must be what answered"
    );
}

// When the directory layer cannot be scanned and
// nothing else answers, `resolve` must not claim the name was not found --
// it never actually looked -- so it reports the distinct Unscannable value.
#[test]
fn resolve_reports_unscannable_when_the_directory_cannot_be_scanned_and_nothing_answers() {
    let root = unique_detection_dir(
        "yg",
        "layers-l4e-unscannable-miss",
        "creating the directory-layer fixture must succeed",
    );
    let unreadable = root.join("does-not-exist");
    let name = "layers-l4e-missing-everywhere";
    assert!(!bundled_definition_names().contains(&name));

    let layers = DetectionLayers {
        directory: Some(unreadable),
        bundled: false,
    };

    match layers.resolve(name) {
        Err(LayerError::Unscannable { name: reported }) => {
            assert_eq!(reported, name);
        }
        other => panic!(
            "an unscannable directory with no other layer to answer must report Unscannable \
             rather than NotFound, got {other:?}"
        ),
    }
}

#[test]
fn resolve_reports_disabled_when_neither_layer_is_configured() {
    let layers = DetectionLayers {
        directory: None,
        bundled: false,
    };

    match layers.resolve("anything") {
        Err(LayerError::Disabled) => {}
        other => panic!("expected Disabled when no layer is configured, got {other:?}"),
    }
}

// --- list(): the union, the per-layer sets, and the scan-failure warning ---

#[test]
fn list_returns_the_union_of_both_layers_in_byte_order() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l5-union",
        "creating the directory-layer fixture must succeed",
    );
    let directory_only = ["layers-l5-zzz", "layers-l5-aaa"];
    for name in directory_only {
        assert!(!bundled_definition_names().contains(&name));
        write_definition(&dir, name, &valid_definition_toml(name));
    }

    let layers = DetectionLayers {
        directory: Some(dir),
        bundled: true,
    };
    let listing = layers.list();

    let mut expected_directory_names: Vec<String> =
        directory_only.iter().map(|s| (*s).to_string()).collect();
    expected_directory_names.sort();
    assert_eq!(listing.directory_names, expected_directory_names);

    let expected_bundled_names: Vec<String> = bundled_definition_names()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(listing.bundled_names, expected_bundled_names);

    let mut expected_names = expected_directory_names;
    expected_names.extend(expected_bundled_names);
    expected_names.sort();
    assert_eq!(listing.names, expected_names);
    assert!(listing.warning.is_none());
}

#[test]
fn list_carries_no_bundled_names_when_the_directory_is_exclusive() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l5-exclusive",
        "creating the directory-layer fixture must succeed",
    );
    let name = "layers-l5-exclusive-only";
    write_definition(&dir, name, &valid_definition_toml(name));

    let layers = DetectionLayers {
        directory: Some(dir),
        bundled: false,
    };
    let listing = layers.list();

    assert!(listing.bundled_names.is_empty());
    assert_eq!(listing.directory_names, vec![name.to_string()]);
    assert_eq!(listing.names, listing.directory_names);
}

#[test]
fn list_still_returns_bundled_names_with_a_warning_when_the_directory_cannot_be_scanned() {
    let root = unique_detection_dir(
        "yg",
        "layers-l5-scan-fail",
        "creating the directory-layer fixture must succeed",
    );
    let unreadable = root.join("does-not-exist");
    // Deliberately not created: reading it must fail.

    let layers = DetectionLayers {
        directory: Some(unreadable),
        bundled: true,
    };
    let listing = layers.list();

    assert!(listing.directory_names.is_empty());
    let expected_bundled_names: Vec<String> = bundled_definition_names()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(listing.bundled_names, expected_bundled_names);
    assert_eq!(listing.names, expected_bundled_names);
    assert!(
        listing.warning.is_some(),
        "a directory scan failure must carry a warning even though the bundled names still \
         answer"
    );
}

#[test]
fn list_is_empty_without_a_warning_when_neither_layer_is_configured() {
    let layers = DetectionLayers {
        directory: None,
        bundled: false,
    };
    let listing = layers.list();

    assert!(listing.names.is_empty());
    assert!(listing.directory_names.is_empty());
    assert!(listing.bundled_names.is_empty());
    assert!(listing.warning.is_none());
}

// --- open_window: the resolver's failures reach detection_warning ---------

#[tokio::test]
async fn open_reports_a_directory_parse_failure_verbatim_even_with_bundled_overlaid() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l6-parse",
        "creating the directory-layer fixture must succeed",
    );
    let name = a_bundled_name();
    write_definition(&dir, name, MALFORMED_TOML);

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir),
        bundled_definitions: true,
        detection_name: name.to_string(),
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, None).await;

    let warning = opened.detection_warning.expect(
        "a directory parse failure must produce a detection_warning even with the bundled \
         layer overlaid",
    );
    assert!(warning.contains("failed to load the detection definition"));
    assert!(warning.contains("failed to parse detection definition"));

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_not_found_naming_the_directory_when_neither_layer_has_the_name() {
    let dir = unique_detection_dir(
        "yg",
        "layers-l6-notfound",
        "creating the directory-layer fixture must succeed",
    );
    let name = "layers-l6-missing-everywhere";
    assert!(!bundled_definition_names().contains(&name));

    let registry = WindowRegistry::new(DaemonConfig {
        detection_dir: Some(dir.clone()),
        bundled_definitions: true,
        ..DaemonConfig::default()
    });

    let opened = common::open_echo_window(&registry, Some(name)).await;

    let warning = opened
        .detection_warning
        .expect("a name missing from both layers must produce a detection_warning");
    assert!(warning.contains("failed to load the detection definition"));
    assert!(warning.contains(name));
    assert!(warning.contains(&dir.display().to_string()));

    registry.terminate_all().await;
}

#[tokio::test]
async fn open_reports_the_missing_layer_when_neither_layer_is_configured() {
    let registry = WindowRegistry::new(DaemonConfig::default());

    let opened = common::open_echo_window(&registry, None).await;

    // With no directory and the bundled layer off, the window side states
    // that fact instead of staying silent, reusing LayerError::Disabled's own Display.
    assert_eq!(
        opened.detection_warning,
        Some(LayerError::Disabled.to_string()),
        "with no directory and the bundled layer off, the window side now states that fact \
         instead of staying silent, got {:?}",
        opened.detection_warning
    );

    registry.terminate_all().await;
}
