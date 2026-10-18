#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `profile::list_profiles` is called directly here: it is the unit under test.
#![allow(clippy::disallowed_methods)]

//! Integration test: the warning a listing carries for a profile naming a
//! detection definition no layer holds, measured against what opening a
//! window from that same profile actually does.
//!
//! Both halves run on one configuration, so the sentence a client reads and
//! the status the window ends up in cannot be compared across two different
//! worlds. The sentence itself is never pinned verbatim: what is measured is
//! the absence of the one claim that is untrue, so the wording stays the
//! implementation's to choose.

mod common;

use std::path::PathBuf;

use common::{echo_child_path, repo_detection_dir, write_profile};

use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::definition::{bundled_definition_names, is_valid_detection_name};
use yagura_daemon::detector::layers::{DetectionLayers, LayerError};
use yagura_daemon::profile::{
    LaunchOptionValues, ProfileEntry, ProfileListing, list_profiles, resolve_launch,
};
use yagura_daemon::window::{OpenWindowRequest, WindowOpenError, WindowRegistry};
use yagura_protocol::WindowStatus;
use yagura_test_support::paths::TempDir;

/// A detection name no layer carries.
const ABSENT_DEFINITION: &str = "absent-definition";
/// A detection name the repository's own definition directory carries.
const PRESENT_DEFINITION: &str = "generic-shell";

/// The profile naming [`ABSENT_DEFINITION`].
const GAP_PROFILE: &str = "detection-gap";
/// The profile naming [`PRESENT_DEFINITION`].
const RESOLVED_PROFILE: &str = "detection-present";

/// A detection name that fails [`is_valid_detection_name`] (uppercase
/// letters, a space and a `!`, none of which `[a-z0-9_-]{1,64}` allows).
const INVALID_DETECTION_NAME: &str = "Not A Valid Name!";

/// Profile naming [`PRESENT_DEFINITION`] with no detection layer configured
/// at all.
const LAYER_ZERO_NAMED_PROFILE: &str = "layer-zero-named";
/// Profile declaring no `detection` field at all, with no layer configured.
const LAYER_ZERO_UNNAMED_PROFILE: &str = "layer-zero-unnamed";
/// Profile naming [`INVALID_DETECTION_NAME`] with a layer configured
const INVALID_NAME_PROFILE: &str = "invalid-name";
/// Profile naming [`INVALID_DETECTION_NAME`] with no layer configured
const LAYER_ZERO_INVALID_NAME_PROFILE: &str = "layer-zero-invalid-name";
/// Profile naming a definition while the directory layer cannot be scanned
const UNSCANNABLE_PROFILE: &str = "unscannable-inventory";
/// Profile naming a bundled definition while the directory layer cannot be
/// scanned, so the bundled layer alone must still resolve it silently
const UNSCANNABLE_BUT_BUNDLED_PROFILE: &str = "unscannable-but-bundled";

/// Substrings only the mistaken claim needs: that detection falls back to
/// some default when the named definition is missing. A corrected sentence
/// still spells `detection`, `definition` and `not found`, so none of those
/// is a needle here.
const FALLBACK_CLAIM_NEEDLES: [&str; 4] = ["falls back", "fall back", "fallback", "default"];

/// Which of [`FALLBACK_CLAIM_NEEDLES`] `warning` contains, compared
/// case-insensitively so a capitalised sentence cannot slip past.
fn fallback_claim_hits(warning: &str) -> Vec<&'static str> {
    let lowered = warning.to_ascii_lowercase();
    FALLBACK_CLAIM_NEEDLES
        .into_iter()
        .filter(|needle| lowered.contains(needle))
        .collect()
}

/// A profile document launching the `echo_child` fixture and naming
/// `detection`. The argv element is a TOML literal string so a Windows
/// path's separators are not read as escapes.
fn profile_source(display_name: &str, detection: &str) -> String {
    let child = echo_child_path();
    let child = child
        .to_str()
        .expect("echo_child path must be valid UTF-8")
        .to_string();
    format!(
        "schema = 1\ndisplay_name = \"{display_name}\"\ndetection = \"{detection}\"\n\n\
         [command]\ndefault = ['{child}']\n"
    )
}

/// A profile document launching the `echo_child` fixture and declaring no
/// `detection` field at all.
fn profile_source_without_detection(display_name: &str) -> String {
    let child = echo_child_path();
    let child = child
        .to_str()
        .expect("echo_child path must be valid UTF-8")
        .to_string();
    format!("schema = 1\ndisplay_name = \"{display_name}\"\n\n[command]\ndefault = ['{child}']\n")
}

/// The temporary tree must outlive the configuration pointing into it.
struct Fixture {
    _tree: TempDir,
    config: DaemonConfig,
}

/// One profile in an otherwise-empty directory, under whatever layer
/// configuration `config_of` builds around it. Separate from [`fixture`]
/// because most of the tests here need exactly one profile plus a
/// layer configuration `fixture` does not build (layer zero, an unscannable
/// directory, ...).
fn single_profile_fixture(
    tag: &str,
    id: &str,
    source: String,
    config_of: impl FnOnce(PathBuf) -> DaemonConfig,
) -> Fixture {
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles = tree.make_dir("profiles");
    write_profile(&profiles, id, &source);
    let config = config_of(profiles);
    Fixture {
        _tree: tree,
        config,
    }
}

/// One profile plus a `detection_dir` that was never created, so
/// [`DetectionLayers::list`] reports a directory-scan warning alongside
/// whatever `bundled` layer this fixture configures.
fn unscannable_inventory_fixture(tag: &str, id: &str, source: String, bundled: bool) -> Fixture {
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles = tree.make_dir("profiles");
    write_profile(&profiles, id, &source);
    let ghost_detection_dir = profiles
        .parent()
        .expect("the profiles dir has a parent inside the temp tree")
        .join("missing-detection-dir");
    let config = DaemonConfig {
        profiles_dir: Some(profiles),
        detection_dir: Some(ghost_detection_dir),
        bundled_definitions: bundled,
        ..DaemonConfig::default()
    };
    Fixture {
        _tree: tree,
        config,
    }
}

/// Both profiles in one directory, read against both detection layers: the
/// gap is then a property of the name, not of a layer being switched off.
fn fixture(tag: &str) -> Fixture {
    let tree = TempDir::new("yagura-cli-test", tag);
    let profiles = tree.make_dir("profiles");
    write_profile(
        &profiles,
        GAP_PROFILE,
        &profile_source("Detection Gap", ABSENT_DEFINITION),
    );
    write_profile(
        &profiles,
        RESOLVED_PROFILE,
        &profile_source("Detection Present", PRESENT_DEFINITION),
    );
    let config = DaemonConfig {
        profiles_dir: Some(profiles),
        detection_dir: Some(repo_detection_dir()),
        bundled_definitions: true,
        ..DaemonConfig::default()
    };
    Fixture {
        _tree: tree,
        config,
    }
}

fn entry_for<'a>(listing: &'a ProfileListing, id: &str) -> &'a ProfileEntry {
    listing
        .profiles
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| {
            panic!(
                "profile {id:?} must be listed, got {:?} with warnings {:?}",
                listing
                    .profiles
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>(),
                listing.warnings
            )
        })
}

fn gap_warning(fixture: &Fixture) -> String {
    let listing = list_profiles(&fixture.config);
    entry_for(&listing, GAP_PROFILE)
        .detection_warning
        .clone()
        .expect("a profile naming a definition no layer holds must carry a warning")
}

#[test]
fn enumeration_warning_does_not_claim_a_fallback_to_a_default_definition() {
    let fixture = fixture("detection-wording-claim");
    let warning = gap_warning(&fixture);

    let hits = fallback_claim_hits(&warning);
    assert!(
        hits.is_empty(),
        "a window whose named detection definition is missing is pinned to Unknown, not run \
         under some other definition, so the warning must not claim a fallback: found {hits:?} \
         in {warning:?}"
    );
}

#[tokio::test]
async fn a_window_opened_from_the_profile_with_the_absent_definition_stays_unknown() {
    let fixture = fixture("detection-wording-open");
    let launch = resolve_launch(
        &fixture.config,
        GAP_PROFILE,
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("the profile naming an absent definition must still resolve into a launch");

    let registry = WindowRegistry::new(fixture.config.clone());
    let opened = registry
        .open(OpenWindowRequest {
            command: launch.argv,
            cwd: launch.cwd,
            name: Some(launch.name),
            vendor: Some(launch.vendor),
            detection: launch.detection,
            on_name_taken: Default::default(),
        })
        .await
        .expect("a missing detection definition must not stop the window from opening");

    assert_eq!(
        opened.entry.current_status(),
        WindowStatus::Unknown,
        "the fail-safe pins a window with no loadable definition to Unknown; this is the \
         behaviour the enumeration warning has to describe"
    );

    registry.terminate_all().await;
}

#[test]
fn enumeration_warning_names_the_definition_that_was_not_found() {
    let fixture = fixture("detection-wording-name");
    let warning = gap_warning(&fixture);

    assert!(
        warning.contains(ABSENT_DEFINITION),
        "the warning must say which definition was looked for, got {warning:?}"
    );
}

#[test]
fn enumeration_does_not_warn_when_the_definition_resolves() {
    let fixture = fixture("detection-wording-resolved");
    let listing = list_profiles(&fixture.config);

    assert_eq!(
        entry_for(&listing, RESOLVED_PROFILE).detection_warning,
        None,
        "a profile naming a definition a layer really holds has nothing to warn about"
    );
}

#[test]
fn the_fallback_claim_scan_separates_a_mistaken_wording_from_a_corrected_one() {
    for mistaken in [
        "detection definition \"x\" was not found; the window falls back to the daemon's default",
        "detection definition \"x\" is missing, so the default definition is used instead",
        "no definition named \"x\": detection will fall back",
    ] {
        assert!(
            !fallback_claim_hits(mistaken).is_empty(),
            "a sentence claiming a fallback must be caught, got no hit in {mistaken:?}"
        );
    }

    for corrected in [
        "detection definition \"x\" was not found; this window's status stays Unknown",
        "no detection definition named \"x\" is available, so the window cannot be judged",
    ] {
        assert_eq!(
            fallback_claim_hits(corrected),
            Vec::<&str>::new(),
            "a sentence that only states the definition is missing must pass: {corrected:?}"
        );
    }
}

// =============================================================================
// Each side states only what it has confirmed, never more.
// =============================================================================

/// With no layer configured at all, both the listing side and the
/// window side now state that fact instead of one of them staying silent.
/// Both sides must reuse `LayerError::Disabled`'s own `Display` rather than a
/// second literal, and neither may phrase it as "not found".
#[tokio::test]
async fn layer_zero_profile_and_window_state_the_missing_layer_instead_of_staying_silent() {
    let fixture = single_profile_fixture(
        "detection-wording-layer-zero",
        LAYER_ZERO_NAMED_PROFILE,
        profile_source("Layer Zero", PRESENT_DEFINITION),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            ..DaemonConfig::default()
        },
    );

    let listing = list_profiles(&fixture.config);
    let list_warning = entry_for(&listing, LAYER_ZERO_NAMED_PROFILE)
        .detection_warning
        .clone()
        .expect(
            "with no directory and the bundled layer off, the listing side now states that \
             fact instead of staying silent",
        );
    assert_eq!(
        list_warning,
        LayerError::Disabled.to_string(),
        "the listing side must reuse LayerError::Disabled's own Display rather than a second \
         literal, got {list_warning:?}"
    );
    assert!(
        !list_warning.contains("was not found"),
        "layer zero must not be phrased as \"the name was not found\": got {list_warning:?}"
    );
    println!("layer zero (listing side) detection_warning = {list_warning:?}");

    let launch = resolve_launch(
        &fixture.config,
        LAYER_ZERO_NAMED_PROFILE,
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("a profile naming a definition with no layer configured must still resolve");

    let registry = WindowRegistry::new(fixture.config.clone());
    let opened = registry
        .open(OpenWindowRequest {
            command: launch.argv,
            cwd: launch.cwd,
            name: Some(launch.name),
            vendor: Some(launch.vendor),
            detection: launch.detection,
            on_name_taken: Default::default(),
        })
        .await
        .expect("a missing layer must not stop the window from opening");

    let window_warning = opened
        .detection_warning
        .clone()
        .expect("the window side states the missing layer too");
    assert_eq!(
        window_warning,
        LayerError::Disabled.to_string(),
        "the window side must reuse LayerError::Disabled's own Display, with no added prefix, got {window_warning:?}"
    );
    println!("layer zero (window side) detection_warning = {window_warning:?}");

    registry.terminate_all().await;
}

/// Layer zero must warn a `detection`-less entry too. A window
/// opened from a profile that names nothing still resolves the daemon's
/// configured default and stays Unknown exactly like one that names a
/// definition explicitly: the listing side must not go silent just because
/// `detection` itself was never declared.
#[test]
fn layer_zero_warns_even_when_the_profile_declares_no_detection_name() {
    let fixture = single_profile_fixture(
        "detection-wording-layer-zero-no-name",
        LAYER_ZERO_UNNAMED_PROFILE,
        profile_source_without_detection("Layer Zero No Name"),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            ..DaemonConfig::default()
        },
    );

    let listing = list_profiles(&fixture.config);
    let entry = entry_for(&listing, LAYER_ZERO_UNNAMED_PROFILE);
    assert_eq!(
        entry.detection, None,
        "the fixture profile must not declare `detection`"
    );
    let warning = entry.detection_warning.clone().expect(
        "layer zero must warn a `detection`-less entry too, because any window opened from it \
         stays Unknown just the same",
    );
    assert_eq!(warning, LayerError::Disabled.to_string(), "got {warning:?}");
}

/// Distinguishing companion (not a new claim): with a layer
/// configured and the name genuinely absent from its inventory, the listing
/// side keeps saying "not found" -- unchanged from today, and the fact this
/// stays true is what makes the layer-zero and invalid-name cases below distinguishable
/// from it.
#[test]
fn present_layer_absent_name_still_says_not_found() {
    let fixture = fixture("detection-wording-t2-not-found");
    let warning = gap_warning(&fixture);
    assert!(
        warning.contains("was not found"),
        "a name absent from an available inventory is still reported as not found: got \
         {warning:?}"
    );
}

/// A syntactically invalid detection name is reported as
/// invalid, never as "not found" (it was never looked up in any inventory),
/// and the listing must not claim a window opened from this profile would
/// stay Unknown, because opening one is refused outright before detection
/// ever runs.
#[tokio::test]
async fn an_invalid_detection_name_is_reported_as_invalid_not_as_missing() {
    assert!(
        !is_valid_detection_name(INVALID_DETECTION_NAME),
        "the fixture name must actually fail the charset rule"
    );

    let fixture = single_profile_fixture(
        "detection-wording-invalid-name",
        INVALID_NAME_PROFILE,
        profile_source("Invalid Name", INVALID_DETECTION_NAME),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            detection_dir: Some(repo_detection_dir()),
            bundled_definitions: true,
            ..DaemonConfig::default()
        },
    );

    let listing = list_profiles(&fixture.config);
    let warning = entry_for(&listing, INVALID_NAME_PROFILE)
        .detection_warning
        .clone()
        .expect("an invalid detection name must still be reported, not silently dropped");
    assert!(
        !warning.contains("was not found"),
        "an invalid name is not an inventory miss, and must not be phrased as one: got \
         {warning:?}"
    );
    assert!(
        !warning.contains("a window opened from this profile would stay unknown"),
        "opening a window from this profile is refused outright (see the open() assertion \
         below), so the listing must not claim it opens and stays Unknown: got {warning:?}"
    );
    assert_ne!(
        warning,
        LayerError::Disabled.to_string(),
        "an invalid name must be distinguished from the layer-zero case"
    );
    println!("invalid name detection_warning = {warning:?}");

    let launch = resolve_launch(
        &fixture.config,
        INVALID_NAME_PROFILE,
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("parsing/resolving a profile does not itself validate the detection name");

    let registry = WindowRegistry::new(fixture.config.clone());
    let outcome = registry
        .open(OpenWindowRequest {
            command: launch.argv,
            cwd: launch.cwd,
            name: Some(launch.name),
            vendor: Some(launch.vendor),
            detection: launch.detection,
            on_name_taken: Default::default(),
        })
        .await;
    assert!(
        matches!(outcome, Err(WindowOpenError::InvalidArgument { .. })),
        "opening a window naming an invalid detection name must be refused outright, got \
         {outcome:?}"
    );
}

/// When the directory layer cannot even be scanned, the listing
/// side cannot rule the name in or out, so it must not claim the name "was
/// not found" -- that claim requires having actually looked.
#[test]
fn an_unscannable_inventory_is_not_claimed_as_a_confirmed_miss() {
    let fixture = unscannable_inventory_fixture(
        "detection-wording-unscannable",
        UNSCANNABLE_PROFILE,
        profile_source("Unscannable", "any-detection-name-never-checked"),
        false,
    );
    assert!(
        DetectionLayers::from_config(&fixture.config)
            .list()
            .warning
            .is_some(),
        "the fixture must actually fail its own directory scan"
    );

    let listing = list_profiles(&fixture.config);
    let warning = entry_for(&listing, UNSCANNABLE_PROFILE)
        .detection_warning
        .clone()
        .expect("an unscannable inventory must still be reported, not silently dropped");
    assert!(
        !warning.contains("was not found"),
        "the listing side cannot rule the name in or out when its own directory scan failed, \
         so it must not claim the name was not found: got {warning:?}"
    );
    println!("unscannable detection_warning = {warning:?}");
}

/// The resolver and the listing side must state an
/// unscannable inventory with the identical sentence, the same way both
/// sides already reuse `LayerError::Disabled`'s `Display` for the layer-zero case.
/// Deliberately does not construct any `LayerError` variant by name (that
/// would make this whole file fail to compile before the implementation
/// exists, taking every other assertion below down with it); it only
/// compares the two `to_string()` outputs, so today's mismatch is a genuine
/// assertion failure rather than a build break.
#[test]
fn resolve_and_the_listing_side_state_an_unscannable_inventory_with_one_identical_sentence() {
    let name = "mp10-unscannable-name";
    assert!(
        !bundled_definition_names().contains(&name),
        "fixture assumption: this name is not a real bundled definition"
    );
    let profile_id = "mp10-unscannable";

    let fixture = unscannable_inventory_fixture(
        "detection-wording-mp10",
        profile_id,
        profile_source("unscannable", name),
        false,
    );
    let layers = DetectionLayers::from_config(&fixture.config);
    assert!(
        layers.list().warning.is_some(),
        "the fixture must actually fail its own directory scan"
    );

    let resolve_text = layers
        .resolve(name)
        .expect_err("an unscannable inventory must not resolve")
        .to_string();

    let listing = list_profiles(&fixture.config);
    let list_warning = entry_for(&listing, profile_id)
        .detection_warning
        .clone()
        .expect("an unscannable inventory must still be reported, not silently dropped");

    assert_eq!(
        resolve_text, list_warning,
        "the resolver and the listing side must state an unscannable inventory with the same \
         sentence, got resolver={resolve_text:?} listing={list_warning:?}"
    );

    let ghost_dir = fixture
        .config
        .detection_dir
        .as_ref()
        .expect("the fixture configures a detection_dir")
        .display()
        .to_string();
    assert!(
        !list_warning.contains(&ghost_dir),
        "the shared sentence must not carry the directory path: got {list_warning:?}"
    );
}

/// A name found in the inventory must win over "inventory unscannable". The directory scan fails here, but the bundled
/// layer already carries the requested name, so the listing side stays
/// silent exactly like it does when nothing failed at all.
#[test]
fn an_unscannable_inventory_stays_silent_when_the_bundled_layer_already_has_the_name() {
    let name = bundled_definition_names()
        .first()
        .copied()
        .expect("the crate always bundles at least one definition");

    let fixture = unscannable_inventory_fixture(
        "detection-wording-unscannable-bundled",
        UNSCANNABLE_BUT_BUNDLED_PROFILE,
        profile_source("Unscannable But Bundled", name),
        true,
    );
    assert!(
        DetectionLayers::from_config(&fixture.config)
            .list()
            .warning
            .is_some(),
        "the fixture must actually fail its own directory scan"
    );

    let listing = list_profiles(&fixture.config);
    let warning = entry_for(&listing, UNSCANNABLE_BUT_BUNDLED_PROFILE)
        .detection_warning
        .clone();
    assert_eq!(
        warning, None,
        "a name found in the inventory must win over \"inventory unscannable\": got \
         {warning:?}"
    );
}

/// On the crossing construction (no layer configured *and* an
/// invalid name), "name invalid" must win over "layer zero" --
/// otherwise the layer-zero sentence's consequence clause becomes false,
/// because a window cannot even be opened from this profile.
#[tokio::test]
async fn layer_zero_and_an_invalid_name_together_report_the_invalid_name_not_the_missing_layer() {
    let fixture = single_profile_fixture(
        "detection-wording-layer-zero-invalid-name",
        LAYER_ZERO_INVALID_NAME_PROFILE,
        profile_source("Layer Zero Invalid Name", INVALID_DETECTION_NAME),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            ..DaemonConfig::default()
        },
    );

    let listing = list_profiles(&fixture.config);
    let warning = entry_for(&listing, LAYER_ZERO_INVALID_NAME_PROFILE)
        .detection_warning
        .clone()
        .expect("an invalid name with no layer configured must still be reported");
    assert_ne!(
        warning,
        LayerError::Disabled.to_string(),
        "\"name invalid\" must win over \"layer zero\" on this crossing \
         construction: got {warning:?}"
    );
    assert!(
        !warning.contains("was not found"),
        "an invalid name is not an inventory miss: got {warning:?}"
    );
    println!("crossing detection_warning = {warning:?}");

    let launch = resolve_launch(
        &fixture.config,
        LAYER_ZERO_INVALID_NAME_PROFILE,
        None,
        None,
        &LaunchOptionValues::default(),
    )
    .expect("parsing/resolving a profile does not itself validate the detection name");

    let registry = WindowRegistry::new(fixture.config.clone());
    let outcome = registry
        .open(OpenWindowRequest {
            command: launch.argv,
            cwd: launch.cwd,
            name: Some(launch.name),
            vendor: Some(launch.vendor),
            detection: launch.detection,
            on_name_taken: Default::default(),
        })
        .await;
    assert!(
        matches!(outcome, Err(WindowOpenError::InvalidArgument { .. })),
        "opening a window naming an invalid detection name must be refused outright even with \
         no layer configured, got {outcome:?}"
    );
}

/// Extended to the new wordings: none of them may claim a
/// fallback to a default definition, reusing the same needles and helper the
/// existing enumeration test already established for the "absent name" case.
#[test]
fn none_of_the_new_wordings_claim_a_fallback_to_a_default_definition() {
    let cases = [
        (
            "layer zero",
            LAYER_ZERO_NAMED_PROFILE,
            single_profile_fixture(
                "detection-wording-t5-layer-zero",
                LAYER_ZERO_NAMED_PROFILE,
                profile_source("Layer Zero", PRESENT_DEFINITION),
                |profiles| DaemonConfig {
                    profiles_dir: Some(profiles),
                    ..DaemonConfig::default()
                },
            ),
        ),
        (
            "invalid name with a layer configured",
            INVALID_NAME_PROFILE,
            single_profile_fixture(
                "detection-wording-t5-invalid-name",
                INVALID_NAME_PROFILE,
                profile_source("Invalid Name", INVALID_DETECTION_NAME),
                |profiles| DaemonConfig {
                    profiles_dir: Some(profiles),
                    detection_dir: Some(repo_detection_dir()),
                    bundled_definitions: true,
                    ..DaemonConfig::default()
                },
            ),
        ),
        (
            "unscannable inventory",
            UNSCANNABLE_PROFILE,
            unscannable_inventory_fixture(
                "detection-wording-t5-unscannable",
                UNSCANNABLE_PROFILE,
                profile_source("Unscannable", "any-detection-name"),
                false,
            ),
        ),
    ];

    for (label, id, fixture) in &cases {
        let listing = list_profiles(&fixture.config);
        let Some(warning) = entry_for(&listing, id).detection_warning.clone() else {
            continue;
        };
        let hits = fallback_claim_hits(&warning);
        assert!(
            hits.is_empty(),
            "the {label} wording must not claim a fallback: found {hits:?} in {warning:?}"
        );
    }
}

/// The four sentences (layer zero, absent name, invalid name, unscannable
/// inventory) are each other's mutual falsification.
/// Collapsing any two of the four sentences into the same wording, or moving
/// any of the four fixed facts below to another sentence, makes one of the
/// four claims false. The pairwise `assert_ne!` below is a weak safety net on
/// its own (the fixtures embed different names, so the sentences would
/// differ even if their *structure* collapsed); the load-bearing checks are
/// the four explicit assertions that follow it, one per sentence.
#[test]
fn layer_zero_absent_name_invalid_name_and_unscannable_inventory_produce_pairwise_distinct_wordings()
 {
    let layer_zero_fixture = single_profile_fixture(
        "detection-wording-t6-layer-zero",
        LAYER_ZERO_NAMED_PROFILE,
        profile_source("Layer Zero", PRESENT_DEFINITION),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            ..DaemonConfig::default()
        },
    );
    let absent_name_fixture = fixture("detection-wording-t6-absent-name");
    let invalid_name_fixture = single_profile_fixture(
        "detection-wording-t6-invalid-name",
        INVALID_NAME_PROFILE,
        profile_source("Invalid Name", INVALID_DETECTION_NAME),
        |profiles| DaemonConfig {
            profiles_dir: Some(profiles),
            detection_dir: Some(repo_detection_dir()),
            bundled_definitions: true,
            ..DaemonConfig::default()
        },
    );
    let unscannable_fixture = unscannable_inventory_fixture(
        "detection-wording-t6-unscannable",
        UNSCANNABLE_PROFILE,
        profile_source("Unscannable", "any-detection-name"),
        false,
    );

    let layer_zero_warning = entry_for(
        &list_profiles(&layer_zero_fixture.config),
        LAYER_ZERO_NAMED_PROFILE,
    )
    .detection_warning
    .clone()
    .expect("layer zero must warn");
    let absent_name_warning = gap_warning(&absent_name_fixture);
    let invalid_name_warning = entry_for(
        &list_profiles(&invalid_name_fixture.config),
        INVALID_NAME_PROFILE,
    )
    .detection_warning
    .clone()
    .expect("an invalid name must warn");
    let unscannable_warning = entry_for(
        &list_profiles(&unscannable_fixture.config),
        UNSCANNABLE_PROFILE,
    )
    .detection_warning
    .clone()
    .expect("an unscannable inventory must warn");

    println!("layer_zero_warning = {layer_zero_warning:?}");
    println!("absent_name_warning = {absent_name_warning:?}");
    println!("invalid_name_warning = {invalid_name_warning:?}");
    println!("unscannable_warning = {unscannable_warning:?}");

    let wordings = [
        ("layer zero", &layer_zero_warning),
        ("absent name", &absent_name_warning),
        ("invalid name", &invalid_name_warning),
        ("unscannable inventory", &unscannable_warning),
    ];
    for (i, (label_a, warning_a)) in wordings.iter().enumerate() {
        for (label_b, warning_b) in wordings.iter().skip(i + 1) {
            assert_ne!(
                warning_a, warning_b,
                "a fix that collapses {label_a} and {label_b} into the same sentence would \
                 make one of the two claims false: each of the four sentences must stay \
                 distinguishable from the other three"
            );
        }
    }

    assert_eq!(
        layer_zero_warning,
        LayerError::Disabled.to_string(),
        "layer zero is the only one of the four that legitimately reuses LayerError::Disabled \
         verbatim"
    );
    assert!(
        absent_name_warning.contains("was not found"),
        "absent name is the only one of the four allowed to claim the name was not found: got \
         {absent_name_warning:?}"
    );
    assert!(
        !invalid_name_warning.contains("was not found"),
        "invalid name must not borrow the absent-name claim: got {invalid_name_warning:?}"
    );
    assert!(
        !unscannable_warning.contains("was not found"),
        "an unscannable inventory must not borrow the absent-name claim either: got \
         {unscannable_warning:?}"
    );
}
