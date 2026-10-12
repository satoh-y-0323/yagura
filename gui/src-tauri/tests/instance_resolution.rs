#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the GUI-side instance resolution surface --
//! `StartupContext`, the config-root and write-seed seams in
//! `config_paths.rs`, `compose_window_title`, and the `current_endpoint_with`
//! seam in `ipc_client.rs`. The sections below cover the config root's
//! default-name freeze and non-default divergence, the window-title
//! composer, the two failure reasons, and the write seams.
//!
//! `StartupContext::resolve_from` never reads the environment (only
//! `main.rs` does): every input below is a literal argument, so both the
//! success and the failure sides are constructed directly rather than
//! through `std::env::set_var`.

use std::path::{Path, PathBuf};

use yagura_gui::client_config::{ClientConfigLoadResult, ReconnectConfig};
use yagura_gui::config_paths::{config_root, ensure_default_playbook_in};
use yagura_gui::instance::{StartupContext, compose_window_title};
use yagura_gui::ipc_client::current_endpoint_with;
use yagura_gui::recovery_state::RecoveryState;
use yagura_os::{InstanceNamespace, resolve_instance_from};
use yagura_test_support::paths::TempDir;

/// The default instance, obtained the same way production code must
/// (`InstanceNamespace` has no public constructor).
fn default_namespace() -> InstanceNamespace {
    resolve_instance_from(None).expect("None must resolve to the default instance")
}

/// A named instance, for the "differs from default" half of the freeze test.
fn dev_namespace() -> InstanceNamespace {
    resolve_instance_from(Some(std::ffi::OsStr::new("dev")))
        .expect("\"dev\" is a valid identifier and must resolve")
}

/// A `StartupContext` built from an identifier the allow-list rejects.
fn invalid_identifier_context() -> StartupContext {
    StartupContext::resolve_from(Some(std::ffi::OsStr::new("Not Valid!")), None, None)
}

/// A `StartupContext` built from a named instance combined with a
/// store-path override -- a combination start-up refuses.
fn store_override_with_named_instance_context() -> StartupContext {
    StartupContext::resolve_from(
        Some(std::ffi::OsStr::new("dev")),
        Some(std::ffi::OsStr::new("C:/tmp/shared.db")),
        None,
    )
}

// ---------------------------------------------------------------------
// The config root's default name is frozen, and a named
// instance's config root never collides with it.
// ---------------------------------------------------------------------

/// The default instance's directory name: the bundle identifier on Windows.
#[cfg(windows)]
const DEFAULT_DIR: &str = "io.github.satoh-y-0323.yagura";
#[cfg(not(windows))]
const DEFAULT_DIR: &str = "yagura";

#[test]
fn config_root_matches_todays_layout_for_the_default_instance() {
    let base = Path::new("fixed-config-base");
    assert_eq!(
        config_root(base, &default_namespace()),
        base.join(DEFAULT_DIR),
        "the default instance's config root must stay <config_base>/{DEFAULT_DIR}"
    );
}

#[test]
fn the_bundle_identifier_is_the_one_the_data_directory_is_named_after() {
    let conf_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
    let conf: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(conf_path).unwrap()).unwrap();
    assert_eq!(
        conf["identifier"].as_str(),
        Some(yagura_os::APP_IDENTIFIER),
        "the uninstaller deletes the identifier's directory as the app's data, so the \
         identifier must name the directory yagura keeps its data in"
    );
}

#[test]
fn config_root_diverges_from_the_default_for_a_named_instance() {
    let base = Path::new("fixed-config-base");
    assert_ne!(
        config_root(base, &dev_namespace()),
        config_root(base, &default_namespace()),
        "a named instance's config root must not collide with the default one's "
    );
}

// ---------------------------------------------------------------------
// The window-title composer takes its stem as an argument and holds
// no "yagura" literal of its own.
// ---------------------------------------------------------------------

#[test]
fn compose_window_title_returns_the_stem_unchanged_for_the_default_instance() {
    assert_eq!(compose_window_title("yagura", None), "yagura");
}

#[test]
fn compose_window_title_appends_the_bracketed_id_for_a_named_instance() {
    assert_eq!(compose_window_title("yagura", Some("dev")), "yagura [dev]");
}

#[test]
fn compose_window_title_uses_the_given_stem_rather_than_a_hardcoded_product_name() {
    // A stem other than "yagura" still drives the output: proof the
    // function holds no "yagura" literal of its own.
    assert_eq!(compose_window_title("Example", None), "Example");
    assert_eq!(
        compose_window_title("Example", Some("dev")),
        "Example [dev]"
    );
}

// ---------------------------------------------------------------------
// The two failure inputs -- an invalid identifier, and a named
// instance combined with a store-path override -- resolve to
// distinguishable reasons, and `RecoveryStatusReport` carries the
// distinction rather than collapsing both into one bit.
// ---------------------------------------------------------------------

#[test]
fn resolve_from_rejects_an_invalid_identifier() {
    assert!(
        invalid_identifier_context().resolution().is_err(),
        "an identifier the allow-list rejects must not resolve"
    );
}

#[test]
fn resolve_from_rejects_a_named_instance_combined_with_a_store_path_override() {
    assert!(
        store_override_with_named_instance_context()
            .resolution()
            .is_err(),
        "a named instance plus a store-path override must be refused"
    );
}

#[test]
fn the_two_failure_reasons_are_distinguishable_from_each_other() {
    let invalid_id = invalid_identifier_context()
        .resolution()
        .as_ref()
        .err()
        .cloned()
        .expect("an invalid identifier must fail");
    let store_override = store_override_with_named_instance_context()
        .resolution()
        .as_ref()
        .err()
        .cloned()
        .expect("a named instance plus a store override must fail");

    assert_ne!(
        invalid_id, store_override,
        "the two rejection reasons must not collapse into the same value"
    );
}

#[test]
fn recovery_status_report_carries_the_startup_env_failure_it_was_built_with() {
    let config = ClientConfigLoadResult {
        failure: None,
        reconnect: ReconnectConfig::default(),
    };
    let invalid_id = invalid_identifier_context()
        .resolution()
        .as_ref()
        .err()
        .cloned();
    let store_override = store_override_with_named_instance_context()
        .resolution()
        .as_ref()
        .err()
        .cloned();

    let report_a = RecoveryState::new(config, invalid_id).snapshot();
    let report_b = RecoveryState::new(config, store_override).snapshot();

    assert_eq!(report_a.startup_env_failure, invalid_id);
    assert_eq!(report_b.startup_env_failure, store_override);
    assert_ne!(
        report_a.startup_env_failure, report_b.startup_env_failure,
        "the report must not collapse the two reasons into the same value"
    );
}

// ---------------------------------------------------------------------
// A failed resolution yields no endpoint and no config root, for
// either rejection reason. A successful resolution is the counterpart
// that keeps this from being vacuously true.
// ---------------------------------------------------------------------

#[test]
fn a_resolution_failure_from_an_invalid_identifier_yields_no_endpoint_and_no_config_root() {
    let ctx = invalid_identifier_context();
    assert!(
        current_endpoint_with(ctx.resolution()).is_err(),
        "the endpoint seam must not synthesize an endpoint from a failed resolution"
    );
    assert_eq!(
        ctx.config_root(),
        None,
        "the config-root seam must not fall back to any base when resolution failed"
    );
}

#[test]
fn a_resolution_failure_from_a_store_override_yields_no_endpoint_and_no_config_root() {
    let ctx = store_override_with_named_instance_context();
    assert!(
        current_endpoint_with(ctx.resolution()).is_err(),
        "the endpoint seam must not synthesize an endpoint from a failed resolution"
    );
    assert_eq!(
        ctx.config_root(),
        None,
        "the config-root seam must not fall back to any base when resolution failed"
    );
}

#[test]
fn a_successful_resolution_yields_an_endpoint_and_a_config_root() {
    let base = TempDir::new("yagura-gui-instance-resolution", "p12c-success");
    let ctx = StartupContext::resolve_from(None, None, Some(base.path().to_path_buf()));

    assert!(
        current_endpoint_with(ctx.resolution()).is_ok(),
        "a successful resolution must still let the endpoint seam produce a value"
    );
    let expected_root: PathBuf = base.path().join(DEFAULT_DIR);
    assert_eq!(ctx.config_root(), Some(expected_root));
}

// ---------------------------------------------------------------------
// The write seams only ever touch the named instance's directory,
// never the default one's.
// ---------------------------------------------------------------------

#[test]
fn write_seams_place_files_only_under_the_named_instances_directory() {
    let base = TempDir::new("yagura-gui-instance-resolution", "p12-prime");
    let namespace = dev_namespace();

    ensure_default_playbook_in(base.path(), &namespace);

    assert!(
        base.path()
            .join(format!("{DEFAULT_DIR}-dev"))
            .join("playbook.toml")
            .is_file(),
        "the playbook must land under the named instance's own directory"
    );
    assert!(
        !base.path().join(DEFAULT_DIR).exists(),
        "the default instance's directory must not be created as a side effect "
    );
}
