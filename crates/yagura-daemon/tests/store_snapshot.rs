#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Wiring, fail-safe, read-degradation and rotation contracts for the SQLite store.
//!
//! The wiring contracts drive a real `WindowRegistry`, i.e. real PTY children, through
//! the `sleepy_command` / `Harness` fixtures portable across OSes; `sample_rows`' argv
//! values are stored fixture data only, never spawned.
//!
//! A contract asserting "exactly one warning" wraps only the call under observation with
//! `common::store_fixtures::count_warns`, which counts WARN-level events and ignores
//! target and message text so no log wording is frozen into a test. Wrapping an
//! `open_window` as well would make the count flaky, and warnings emitted on the store's
//! own thread fall outside the thread-local scope by construction.
//!
//! Seeding uses `rusqlite::Connection::open` directly; every verification read goes
//! through `store::open_readonly`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::store_fixtures::{count_warns, insert_window_row_with_command_json, temp_store_db};
use common::{Harness, policy, read_response, send, sleepy_command, unique_temp_dir};
use yagura_daemon::store;
use yagura_daemon::{DaemonConfig, OpenWindowRequest, RequestedCwd, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage};

const POLL_BUDGET: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Polls the current slot until `predicate` holds, then returns the rows that satisfied
/// it. A read error counts as "not converged yet" (the store thread may be mid-write),
/// never as a failure, and only the final state is asserted.
async fn wait_for_current_snapshot(
    db_path: &Path,
    label: &str,
    predicate: impl Fn(&[store::SnapshotWindow]) -> bool,
) -> Vec<store::SnapshotWindow> {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    let mut last: Option<Vec<store::SnapshotWindow>> = None;
    loop {
        if let Ok(conn) = store::open_readonly(db_path)
            && let Ok(rows) = store::read_snapshot_conn(&conn)
        {
            if predicate(&rows) {
                return rows;
            }
            last = Some(rows);
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last observed snapshot: {last:?}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn close_capable_config() -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        ..DaemonConfig::default()
    }
}

fn sample_rows() -> Vec<store::SnapshotWindow> {
    vec![
        store::SnapshotWindow {
            position: 0,
            command: vec!["cmd.exe".to_string(), "/c".to_string(), "ver".to_string()],
            cwd: "C:/work/alpha".to_string(),
            name: "alpha".to_string(),
            vendor: None,
            detection: Some("generic-shell".to_string()),
            layout_json: None,
            profile: None,
            model: None,
            effort: None,
        },
        store::SnapshotWindow {
            position: 1,
            command: vec!["pwsh.exe".to_string()],
            cwd: "C:/work/beta".to_string(),
            name: "beta".to_string(),
            vendor: Some("neutral".to_string()),
            detection: None,
            layout_json: None,
            profile: None,
            model: None,
            effort: None,
        },
    ]
}

/// Creates schema v1 at `path` through the production entry point, releasing the store's
/// own connection before returning so a seeding connection can take the write lock.
///
/// The returned `Store` is deliberately left unbound rather than passed to `drop(..)`:
/// whether `Store` implements `Drop` is an implementation detail, and an explicit
/// `drop(..)` of a type that does not would tie this file's lint status to it
/// (`clippy::drop_non_drop`).
fn create_schema(path: &Path) {
    // Kept per-binary: the shared fixtures must not name yagura_daemon::store.
    store::open_store(path).expect("open_store must succeed on a fresh path");
}

/// Opens a long-lived window with every snapshot-visible field set explicitly, so the
/// row that lands in the store can be checked field by field rather than only by count.
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

/// The resolved `cwd` the registry reports for `window_id`, used as the oracle for the
/// snapshot's `cwd`: the registry resolves the requested path before storing it.
fn registry_cwd(registry: &WindowRegistry, window_id: &str) -> String {
    registry
        .list()
        .into_iter()
        .find(|w| w.window_id == window_id)
        .unwrap_or_else(|| panic!("window {window_id} must be listed"))
        .cwd
}

#[tokio::test]
async fn opening_a_window_adds_a_fully_populated_row_to_the_current_slot() {
    let db = temp_store_db("store-open-follow");
    let cwd = unique_temp_dir("store-open-cwd");
    let harness = Harness::start("store-open-follow", close_capable_config()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));

    let command = sleepy_command("30");
    let window_id =
        open_described_window(&harness.registry, command.clone(), "alpha", "neutral", &cwd).await;

    let rows = wait_for_current_snapshot(&db.path, "the opened window to appear", |rows| {
        rows.len() == 1
    })
    .await;
    let row = &rows[0];
    assert_eq!(row.position, 0, "the only window must be at position 0");
    assert_eq!(row.command, command, "the argv must round-trip verbatim");
    assert_eq!(
        row.name.as_str(),
        "alpha",
        "the display name must be stored"
    );
    assert_eq!(
        row.vendor.as_deref(),
        Some("neutral"),
        "the vendor label must be stored as data"
    );
    assert_eq!(
        row.detection.as_deref(),
        Some("generic-shell"),
        "the detection definition name must be stored"
    );
    assert_eq!(
        row.cwd,
        registry_cwd(&harness.registry, &window_id),
        "the stored cwd must be the resolved absolute path the registry reports"
    );

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn renaming_a_window_updates_its_snapshot_row() {
    let db = temp_store_db("store-rename-follow");
    let cwd = unique_temp_dir("store-rename-cwd");
    let harness = Harness::start("store-rename-follow", close_capable_config()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));
    let mut client = harness.connect().await;

    let window_id = open_described_window(
        &harness.registry,
        sleepy_command("30"),
        "before-rename",
        "neutral",
        &cwd,
    )
    .await;
    wait_for_current_snapshot(&db.path, "the opened window to appear", |rows| {
        rows.len() == 1
    })
    .await;

    send(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id: window_id.clone(),
            name: "after-rename".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowRenamed { name, .. } => {
            assert_eq!(name, "after-rename", "the rename must be acknowledged")
        }
        other => panic!("expected window_renamed, got {other:?}"),
    }

    let rows = wait_for_current_snapshot(&db.path, "the renamed window to converge", |rows| {
        rows.len() == 1 && rows[0].name == "after-rename"
    })
    .await;
    assert_eq!(rows.len(), 1, "renaming must not add or drop rows");

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn closing_a_window_removes_it_from_the_snapshot() {
    let db = temp_store_db("store-close-follow");
    let cwd = unique_temp_dir("store-close-cwd");
    let harness = Harness::start("store-close-follow", close_capable_config()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));
    let mut client = harness.connect().await;

    let window_id = open_described_window(
        &harness.registry,
        sleepy_command("30"),
        "to-be-closed",
        "neutral",
        &cwd,
    )
    .await;
    wait_for_current_snapshot(&db.path, "the opened window to appear", |rows| {
        rows.len() == 1
    })
    .await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => {
            assert_eq!(acked, window_id, "window_closed must ack the closed window")
        }
        other => panic!("expected window_closed, got {other:?}"),
    }

    wait_for_current_snapshot(&db.path, "the closed window to disappear", |rows| {
        rows.is_empty()
    })
    .await;

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn a_naturally_exited_window_stays_in_the_snapshot() {
    // Only an explicit `close_window` removes a window from the restore candidates; a
    // child that exited on its own stays. The observation order is mandatory -- exit,
    // then trigger a write from another window, then observe -- because observing before
    // a later trigger could not tell this design apart from one filtering Exited windows.
    let db = temp_store_db("store-natural-exit");
    let cwd = unique_temp_dir("store-natural-exit-cwd");
    let harness = Harness::start("store-natural-exit", close_capable_config()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));
    let mut client = harness.connect().await;

    let short_lived = open_described_window(
        &harness.registry,
        sleepy_command("2"),
        "exits-on-its-own",
        "neutral",
        &cwd,
    )
    .await;
    let survivor = open_described_window(
        &harness.registry,
        sleepy_command("30"),
        "still-running",
        "neutral",
        &cwd,
    )
    .await;
    wait_for_current_snapshot(&db.path, "both windows to appear", |rows| rows.len() == 2).await;

    harness
        .wait_until_child_exited(&short_lived, Duration::from_secs(20))
        .await;

    send(
        &mut client,
        &ClientMessage::RenameWindow {
            window_id: survivor.clone(),
            name: "trigger-after-exit".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowRenamed { .. } => {}
        other => panic!("expected window_renamed, got {other:?}"),
    }

    let rows = wait_for_current_snapshot(&db.path, "the post-exit write to converge", |rows| {
        rows.iter().any(|r| r.name == "trigger-after-exit")
    })
    .await;
    assert!(
        rows.iter().any(|r| r.name == "exits-on-its-own"),
        "a window whose child exited naturally must remain a restore candidate; \
         only close_window removes one; found: {rows:?}"
    );

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
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
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[test]
fn the_warn_gate_reports_each_failure_axis_once() {
    let init_target = temp_store_db("store-gate-init");
    std::fs::create_dir_all(&init_target.path).expect("the injected directory must be creatable");

    let write_target = temp_store_db("store-gate-write");
    create_schema(&write_target.path);
    let mut read_only =
        store::open_readonly(&write_target.path).expect("open_readonly must succeed");

    // One gate for both axes: the two suppression flags must be independent,
    // so tripping one must not silence the other.
    let gate = store::WarnGate::new();

    let (init_warns, both_failed) = count_warns(|| {
        let first = store::init_store_or_warn(&gate, &init_target.path);
        let second = store::init_store_or_warn(&gate, &init_target.path);
        first.is_none() && second.is_none()
    });
    assert!(
        both_failed,
        "opening a directory as a database must fail both times"
    );
    assert_eq!(
        init_warns, 1,
        "two init failures behind one gate must produce exactly one warning"
    );

    let rows = sample_rows();
    let (write_warns, ()) = count_warns(|| {
        store::write_snapshot_or_warn(&gate, &mut read_only, &rows);
        store::write_snapshot_or_warn(&gate, &mut read_only, &rows);
    });
    assert_eq!(
        write_warns, 1,
        "two write failures must produce exactly one warning, and the already-tripped init \
         flag must not have silenced the write axis"
    );
}

#[test]
fn reading_a_snapshot_returns_at_most_the_row_limit_in_position_order() {
    let db = temp_store_db("store-read-limit");
    create_schema(&db.path);

    let seeded = 300i64;
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        for position in (0..seeded).rev() {
            insert_window_row_with_command_json(
                &conn,
                store::SLOT_CURRENT,
                position,
                &format!("window-{position}"),
                "[\"cmd.exe\"]",
            );
        }
    }
    assert!(
        seeded > store::MAX_SNAPSHOT_WINDOWS,
        "the fixture must exceed the limit for this contract to mean anything"
    );

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let rows = store::read_snapshot_conn(&conn).expect("reading a well-formed store must succeed");

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    let expected: Vec<i64> = (0..store::MAX_SNAPSHOT_WINDOWS).collect();
    assert_eq!(
        positions, expected,
        "the read must return the first MAX_SNAPSHOT_WINDOWS rows in ascending position order, \
         bounded by a bound SQL parameter rather than by a cast"
    );
}

#[test]
fn reading_a_snapshot_skips_corrupt_rows_and_warns_once_per_call() {
    let db = temp_store_db("store-read-corrupt");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "ok-a", "[\"cmd.exe\"]");
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_CURRENT,
            1,
            "not-json",
            "this is not json",
        );
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_CURRENT,
            2,
            "ok-b",
            "[\"pwsh.exe\"]",
        );
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_CURRENT,
            3,
            "not-an-array",
            "{\"argv\": [\"cmd.exe\"]}",
        );
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 4, "ok-c", "[\"ver\"]");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, rows) = count_warns(|| {
        store::read_snapshot_conn(&conn)
            .expect("a store with corrupt rows must degrade, not fail the read")
    });

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0, 2, 4],
        "rows whose command_json is not a JSON array must be skipped, and the rest returned \
         (the store is untrusted: another process of the same user can write it)"
    );
    assert_eq!(
        warns, 1,
        "a read must warn at most once no matter how many rows were corrupt, so a tampered \
         store cannot flood the log"
    );
}

#[test]
fn a_written_snapshot_round_trips_through_a_reopened_connection() {
    let db = temp_store_db("store-round-trip");
    create_schema(&db.path);

    let written = sample_rows();
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        store::write_snapshot_once(&mut conn, &written).expect("write_snapshot_once must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let read_back = store::read_snapshot_conn(&conn).expect("the round trip must read back");
    assert_eq!(
        read_back, written,
        "every field and the 0-based position order must survive a write, a connection drop \
         and a fresh read-only open"
    );
}

#[tokio::test]
async fn wire_into_makes_a_registry_open_land_in_the_current_slot() {
    let db = temp_store_db("store-wire-poll");
    let cwd = unique_temp_dir("store-wire-poll-cwd");
    let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
    store::wire_into(&registry, Some(db.path.clone()));

    let window_id =
        open_described_window(&registry, sleepy_command("30"), "wired", "neutral", &cwd).await;
    let rows = wait_for_current_snapshot(&db.path, "the wired open to reach slot 0", |rows| {
        rows.len() == 1
    })
    .await;
    assert_eq!(
        rows[0].name.as_str(),
        "wired",
        "the row that appeared must be the window that was opened ({window_id})"
    );

    drop(registry);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn wire_into_rotates_the_previous_configuration_into_prev() {
    let db = temp_store_db("store-wire-rotate");
    create_schema(&db.path);

    let configuration_a = sample_rows();
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        store::write_snapshot_once(&mut conn, &configuration_a)
            .expect("write_snapshot_once must succeed");
    }

    let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
    store::wire_into(&registry, Some(db.path.clone()));

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let prev = store::read_prev_snapshot_conn(&conn).expect("prev must be readable");
    assert_eq!(
        prev, configuration_a,
        "wiring the store at start-up must rotate the last session's configuration into prev \
         before anything writes to current; this is the only place rotation happens"
    );

    drop(registry);
    tokio::time::sleep(Duration::from_millis(200)).await;
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

// Manual verification helper, ignored by default: it reads a real database chosen by
// YAGURA_STORE_DB. It must never open the store for writing -- `open_store` migrates and
// `rotate_slots` is destructive, either of which would change the state being observed.
#[test]
#[ignore = "manual verification helper: requires YAGURA_STORE_DB and a real database"]
fn dump_store_for_manual_verification() {
    let raw = std::env::var_os("YAGURA_STORE_DB").unwrap_or_else(|| {
        panic!("YAGURA_STORE_DB must be set to the database to dump; refusing to guess a path")
    });
    let path = PathBuf::from(raw);

    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        match dump_once(&path) {
            Ok(text) => {
                println!("{text}");
                return;
            }
            Err(reason) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "could not read {} within the budget: {reason}",
                    path.display()
                );
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn dump_once(path: &Path) -> Result<String, String> {
    let conn = store::open_readonly(path).map_err(|e| e.to_string())?;
    let current = store::read_snapshot_conn(&conn).map_err(|e| e.to_string())?;
    let prev = store::read_prev_snapshot_conn(&conn).map_err(|e| e.to_string())?;
    let meta = conn
        .query_row(
            "SELECT taken_at_epoch_s, prev_taken_at_epoch_s FROM snapshot_meta WHERE id = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let mut out = String::new();
    out.push_str(&format!("db: {}\n", path.display()));
    out.push_str(&format!(
        "taken_at_epoch_s: {}  prev_taken_at_epoch_s: {:?}\n",
        meta.0, meta.1
    ));
    out.push_str(&format!("slot 0 (current): {} row(s)\n", current.len()));
    for row in &current {
        out.push_str(&format!("  {row:?}\n"));
    }
    out.push_str(&format!("slot 1 (prev): {} row(s)\n", prev.len()));
    for row in &prev {
        out.push_str(&format!("  {row:?}\n"));
    }
    Ok(out)
}
