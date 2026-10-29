#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Two tests below observe what the real `yagura-daemon` binary writes to stderr for
// --shutdown / --emit-status, so they must start it as an actual child process rather
// than call into it in-process.
#![allow(clippy::disallowed_methods)]

//! Integration test: the daemon's side of instance namespacing.
//!
//! `yagura-os` owns identifier validation and namespace composition
//! (`crates/yagura-os/tests/instance.rs`); this file measures the daemon's
//! own responsibilities: its thin CLI-layer wrapper, that its four
//! path-composing call sites stay pinned to today's names for the default
//! instance and diverge for a named one, and that
//! `--shutdown` / `--emit-status` are never refused for a store-override
//! conflict, measured by spawning the compiled `yagura-daemon` binary.
//! Spawning is safe from hitting a real running daemon because every spawn
//! below sets `YAGURA_INSTANCE` to a fixed non-default identifier
//! ([`SCOPED_INSTANCE_ID`]): the endpoint that resolves to is
//! `\\.\pipe\yagura-scopedverify-<user>` (Windows) / `yagura-scopedverify` in
//! the Unix socket path, never the default instance's, so there is
//! structurally nobody there to shut down.

mod common;

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;

use yagura_daemon::cli::{self, DetectionDirResolution, ProfilesDirResolution};
use yagura_daemon::{config, store};
use yagura_os::InstanceNamespace;
use yagura_test_support::paths::TempDir;

/// The default instance's namespace.
fn default_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("the default instance always resolves")
}

/// A named instance's namespace (`yagura-dev`).
fn dev_namespace() -> InstanceNamespace {
    yagura_os::resolve_instance_from(Some(OsStr::new("dev")))
        .expect("\"dev\" is a valid instance identifier")
}

// --- The daemon's thin CLI-layer wrapper does not fall silently to the default -------

#[test]
fn resolve_instance_id_defaults_rejects_empty_and_names_a_valid_identifier() {
    let none_result =
        cli::resolve_instance_id(None).expect("an absent YAGURA_INSTANCE must resolve, not error");
    assert_eq!(
        none_result,
        default_namespace(),
        "an absent YAGURA_INSTANCE must resolve to the same namespace \
         yagura_os::resolve_instance_from(None) produces"
    );

    let empty_result = cli::resolve_instance_id(Some(OsString::new()));
    assert!(
        empty_result.is_err(),
        "an empty YAGURA_INSTANCE must be rejected, not rounded to the default instance \
         (deliberately asymmetric with resolve_playbook's \"empty counts as unset\"); \
         got {empty_result:?}"
    );

    let dev_result = cli::resolve_instance_id(Some(OsString::from("dev")))
        .expect("\"dev\" is a valid instance identifier");
    assert_eq!(
        dev_result,
        dev_namespace(),
        "a valid YAGURA_INSTANCE must resolve to the same namespace \
         yagura_os::resolve_instance_from would produce for the same identifier"
    );
}

// --- The four daemon-owned path-composing call sites -------------------

/// The default instance's directory name: the bundle identifier on Windows.
#[cfg(windows)]
const DEFAULT_DIR: &str = "io.github.satoh-y-0323.yagura";
#[cfg(not(windows))]
const DEFAULT_DIR: &str = "yagura";

/// Lays out `<base>/<DEFAULT_DIR>/{detection,profiles}` and
/// `<base>/<DEFAULT_DIR>-dev/{detection,profiles}`, both pre-created so
/// `resolve_detection_dir` / `resolve_profiles_dir`'s config-root stage
/// adopts them (it only ever adopts a directory that already exists).
fn config_base_with_both_instances_seeded(tree: &TempDir) -> PathBuf {
    let base = tree.make_dir("config-base");
    for instance in [DEFAULT_DIR.to_string(), format!("{DEFAULT_DIR}-dev")] {
        tree.make_dir(&format!("config-base/{instance}/detection"));
        tree.make_dir(&format!("config-base/{instance}/profiles"));
    }
    base
}

#[test]
fn default_instance_composes_todays_four_daemon_paths() {
    let tree = TempDir::new("yagura-cli-test", "daemon-p2-default");
    let base = config_base_with_both_instances_seeded(&tree);
    let namespace = default_namespace();

    let db_path = store::resolve_db_path(None, None, Some(OsString::from(&base)), &namespace);
    assert_eq!(
        db_path,
        Some(base.join(DEFAULT_DIR).join("yagura.db")),
        "the default instance's DB path must stay <data_base>/{DEFAULT_DIR}/yagura.db"
    );

    let log_dir = config::log_dir(&base, &namespace);
    assert_eq!(
        log_dir,
        base.join(DEFAULT_DIR).join("logs"),
        "the default instance's log directory must stay <state_base>/{DEFAULT_DIR}/logs"
    );

    let detection = cli::resolve_detection_dir(None, None, Some(base.clone()), &namespace);
    assert_eq!(
        detection,
        DetectionDirResolution::ConfigRoot(base.join(DEFAULT_DIR).join("detection")),
        "the default instance's detection directory must stay \
         <config_base>/{DEFAULT_DIR}/detection"
    );

    let profiles = cli::resolve_profiles_dir(None, None, Some(base.clone()), &namespace);
    assert_eq!(
        profiles,
        ProfilesDirResolution::ConfigRoot(base.join(DEFAULT_DIR).join("profiles")),
        "the default instance's profiles directory must stay <config_base>/{DEFAULT_DIR}/profiles"
    );
}

#[test]
fn named_instance_composes_four_daemon_paths_all_different_from_default() {
    let tree = TempDir::new("yagura-cli-test", "daemon-p3-named");
    let base = config_base_with_both_instances_seeded(&tree);
    let default_ns = default_namespace();
    let dev_ns = dev_namespace();

    let default_db = store::resolve_db_path(None, None, Some(OsString::from(&base)), &default_ns);
    let dev_db = store::resolve_db_path(None, None, Some(OsString::from(&base)), &dev_ns);
    assert_ne!(
        default_db, dev_db,
        "the DB path must differ between the default and a named instance"
    );

    let default_log = config::log_dir(&base, &default_ns);
    let dev_log = config::log_dir(&base, &dev_ns);
    assert_ne!(
        default_log, dev_log,
        "the log directory must differ between the default and a named instance"
    );

    let default_detection = cli::resolve_detection_dir(None, None, Some(base.clone()), &default_ns);
    let dev_detection = cli::resolve_detection_dir(None, None, Some(base.clone()), &dev_ns);
    assert_ne!(
        default_detection, dev_detection,
        "the detection directory must differ between the default and a named instance"
    );

    let default_profiles = cli::resolve_profiles_dir(None, None, Some(base.clone()), &default_ns);
    let dev_profiles = cli::resolve_profiles_dir(None, None, Some(base.clone()), &dev_ns);
    assert_ne!(
        default_profiles, dev_profiles,
        "the profiles directory must differ between the default and a named instance"
    );
}

// --- behavioral: --shutdown / --emit-status are never refused for a store-override
//     conflict, because neither command opens the store --------------

/// A fixed, non-default instance identifier reserved for this file's subprocess spawns.
/// Lowercase ASCII letters and digits only, well inside the 16-byte limit -- a valid
/// identifier by [`yagura_os::resolve_instance_from`]'s own rules. The endpoint it
/// resolves to (`yagura-scopedverify` in the namespace, `\\.\pipe\yagura-scopedverify-<user>`
/// on Windows) is never the default instance's, so a spawn under it cannot reach whatever
/// default-instance daemon happens to be running on the host (see the module doc comment).
const SCOPED_INSTANCE_ID: &str = "scopedverify";

/// Wording unique to [`yagura_os::StartupEnvError::StoreOverrideWithNamedInstance`]'s
/// `Display` (`crates/yagura-os/src/instance.rs`). Its presence on `--shutdown` /
/// `--emit-status`'s stderr is the one signal that distinguishes "refused for the
/// store-override conflict" from every other reason either command can fail for (chiefly:
/// no daemon is listening), so the assertions below check for its absence rather than the
/// exit code.
const STORE_OVERRIDE_REJECTION_WORDING: &str = "cannot be combined with a named instance";

/// Path of the `yagura-daemon` binary this test run itself just built (in the target
/// tree this crate's tests run under), never an installed or separately running copy.
fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yagura-daemon"))
}

/// A `YAGURA_STORE_DB` value pointing inside a throwaway temp directory. Never opened by
/// this test: `--shutdown` / `--emit-status` must reject before the store-conflict check
/// would even format this path, let alone touch it.
fn scoped_store_override_path(tree: &TempDir) -> PathBuf {
    tree.make_dir("store-override").join("override.db")
}

#[test]
fn shutdown_is_not_refused_for_a_store_override_with_a_named_instance() {
    let tree = TempDir::new("yagura-cli-test", "daemon-shutdown-store-override");
    let store_override = scoped_store_override_path(&tree);

    let output = Command::new(daemon_bin())
        .arg("--shutdown")
        .env("YAGURA_INSTANCE", SCOPED_INSTANCE_ID)
        .env("YAGURA_STORE_DB", &store_override)
        .output()
        .expect("failed to spawn the yagura-daemon binary for --shutdown");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The exit code is deliberately not asserted on: with nobody listening on the
    // scoped-instance endpoint, --shutdown fails regardless of whether the store-override
    // check ran, so success/failure cannot distinguish "refused for the conflict" from "no
    // daemon there". Only the wording can.
    assert!(
        !stderr.contains(STORE_OVERRIDE_REJECTION_WORDING),
        "--shutdown must never be refused for a store-override conflict, \
         since it never opens the store -- refusing it would make a named instance whose \
         environment also carries YAGURA_STORE_DB impossible to stop; full stderr: {stderr}"
    );
}

#[test]
fn emit_status_is_not_refused_for_a_store_override_with_a_named_instance() {
    let tree = TempDir::new("yagura-cli-test", "daemon-emit-status-store-override");
    let store_override = scoped_store_override_path(&tree);

    let output = Command::new(daemon_bin())
        .args(["--emit-status", "idle"])
        .env("YAGURA_INSTANCE", SCOPED_INSTANCE_ID)
        .env("YAGURA_STORE_DB", &store_override)
        .output()
        .expect("failed to spawn the yagura-daemon binary for --emit-status");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Same non-assertion on the exit code as --shutdown above: --emit-status also fails
    // regardless (no daemon listening, and YAGURA_STATUS_TOKEN is deliberately left unset
    // here), so only the wording distinguishes the reason.
    assert!(
        !stderr.contains(STORE_OVERRIDE_REJECTION_WORDING),
        "--emit-status must never be refused for a store-override \
         conflict, for the same reason --shutdown must not be; full stderr: {stderr}"
    );
}
