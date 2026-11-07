#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Fail-safe contracts for opening the SQLite store: a store that cannot open warns once
//! and leaves windows working, and an explicit database file is narrowed to its owner.
//!
//! A contract asserting "exactly one warning" wraps only the call under observation with
//! `common::store_fixtures::count_warns`, which counts WARN-level events and ignores
//! target and message text so no log wording is frozen into a test.

use crate::common;

use std::path::Path;
use std::sync::Arc;

use common::store_fixtures::{count_warns, temp_store_db};
use common::{Harness, policy, sleepy_command, unique_temp_dir};
use yagura_daemon::store;
use yagura_daemon::{DaemonConfig, OpenWindowRequest, RequestedCwd, WindowRegistry};

fn close_capable_config() -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..DaemonConfig::default()
    }
}

/// Opens a long-lived window with its name, vendor and cwd set explicitly.
async fn open_described_window(
    registry: &Arc<WindowRegistry>,
    command: Vec<String>,
    name: &str,
    vendor: &str,
    cwd: &Path,
) -> String {
    registry
        .open(OpenWindowRequest {
            command,
            cwd: Some(RequestedCwd::from_caller(cwd.to_path_buf())),
            name: Some(name.to_string()),
            vendor: Some(vendor.to_string()),
            detection: Some("generic-shell".to_string()),
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open must spawn the child")
        .window_id
}

#[tokio::test]
async fn a_failed_store_init_warns_once_and_leaves_window_operations_working() {
    let db = temp_store_db("store-init-failure");
    // Make the database path itself a directory: SQLite cannot open a directory, so the
    // failure is deterministic -- an empty file would be accepted as an empty database.
    std::fs::create_dir_all(&db.path).expect("the injected directory must be creatable");

    let cwd = unique_temp_dir("store-init-failure-cwd");
    let harness = Harness::start("store-init-failure", close_capable_config()).await;

    let (warns, ()) = count_warns(|| store::wire_into(&harness.registry, Some(db.path.clone())));
    assert_eq!(
        warns, 1,
        "a failed store init must warn exactly once, then stay silent"
    );

    // Outside the observation scope on purpose: this call has warnings of its
    // own that have nothing to do with the store.
    let window_id = open_described_window(
        &harness.registry,
        sleepy_command("30"),
        "unaffected",
        "neutral",
        &cwd,
    )
    .await;
    assert!(
        !window_id.is_empty(),
        "window operations must keep working when persistence is disabled"
    );

    drop(harness);
}

// `open_store`'s permission asymmetry for an explicit db path: the file is narrowed
// unconditionally, its parent directory never is.
#[cfg(unix)]
#[test]
fn open_store_narrows_an_explicit_db_file_but_leaves_its_parent_directory_unnarrowed() {
    use std::os::unix::fs::PermissionsExt;

    let db = temp_store_db("store-explicit-path-permissions");
    let parent = db
        .path
        .parent()
        .expect("temp_store_db must return a path with a parent")
        .to_path_buf();
    // Set explicitly rather than relying on whatever the fixtures and umask gave these:
    // without a known starting mode, "the parent stayed 0755" or "the file became 0600"
    // could hold trivially by coincidence.
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
        .expect("chmod the parent dir to 0755 before the call");
    std::fs::write(&db.path, []).expect("create the db file as an empty file before the call");
    std::fs::set_permissions(&db.path, std::fs::Permissions::from_mode(0o644))
        .expect("chmod the db file to 0644 before the call");

    store::open_store(&db.path).expect("open_store must succeed on an explicit path");

    let parent_mode = std::fs::metadata(&parent)
        .expect("parent dir must still exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        parent_mode, 0o755,
        "open_store must never narrow the parent directory of an explicit db path -- that \
         narrowing is scoped to main.rs's default-layout call, not to open_store itself"
    );

    let file_mode = std::fs::metadata(&db.path)
        .expect("db file must still exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        file_mode, 0o600,
        "open_store must narrow the db file itself to 0600 unconditionally, regardless of \
         whether the path came from the default layout, config, or YAGURA_STORE_DB"
    );
}
