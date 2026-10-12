#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! IPC-level contracts for the restore flow's daemon side: `ListRestoreCandidates`,
//! `UpdateWindowLayout`, its reflection into the next snapshot row, and `ListWindows`'s
//! `WindowInfo.layout_json`.
//!
//! `ListRestoreCandidates` is answered from `DaemonConfig::store_db_path_resolved`
//! directly, not through the registry's store-writer channel, which carries only the
//! write path. Tests needing candidates therefore seed a database with
//! `rusqlite::Connection::open` and point a `DaemonConfig` at that field; the `Harness`
//! in this crate never wires the store itself.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::store_fixtures::{insert_window_row_with_command_json, set_meta, temp_store_db};
use common::{Harness, expect_error, read_response, send, sleepy_command, unique_temp_dir};
use yagura_daemon::DaemonConfig;
use yagura_daemon::store;
use yagura_protocol::{ClientMessage, DaemonMessage};

/// Creates schema v2 at `path` through the production entry point, releasing the store's
/// own connection before returning so a seeding connection can take the write lock.
fn create_schema(path: &Path) {
    store::open_store(path).expect("open_store must succeed on a fresh path");
}

/// A `DaemonConfig` pointed at `path` via `store_db_path_resolved`, the field
/// `ListRestoreCandidates`'s handler reads.
fn config_with_store(path: PathBuf) -> DaemonConfig {
    DaemonConfig {
        store_db_path_resolved: Some(path),
        ..DaemonConfig::default()
    }
}

/// Polls the current slot until `predicate` holds. Duplicated locally because each
/// integration-test binary is its own compilation unit and cannot import another's
/// private items.
async fn wait_for_current_snapshot(
    db_path: &Path,
    label: &str,
    predicate: impl Fn(&[store::SnapshotWindow]) -> bool,
) -> Vec<store::SnapshotWindow> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn list_restore_candidates_returns_the_prev_slot_not_current() {
    let db = temp_store_db("ipc-restore-prev-vs-current");
    create_schema(&db.path);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_PREV,
            0,
            "prev-alpha",
            "[\"cmd.exe\"]",
        );
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_CURRENT,
            0,
            "current-only",
            "[\"pwsh.exe\"]",
        );
        set_meta(&conn, 2000, Some(1000));
    }

    let harness = Harness::start(
        "ipc-restore-prev-vs-current",
        config_with_store(db.path.clone()),
    )
    .await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::RestoreCandidateList {
            windows,
            taken_at_epoch_s,
        } => {
            assert_eq!(
                taken_at_epoch_s,
                Some(1000),
                "the reported timestamp must be prev_taken_at_epoch_s, not taken_at_epoch_s"
            );
            let names: Vec<&str> = windows.iter().map(|w| w.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["prev-alpha"],
                "only the prev slot's rows must be returned; current must never leak in"
            );
        }
        other => panic!("expected restore_candidate_list, got {other:?}"),
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn list_restore_candidates_with_no_resolved_path_returns_an_empty_normal_response() {
    let harness = Harness::start("ipc-restore-no-path", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::RestoreCandidateList {
            windows,
            taken_at_epoch_s,
        } => {
            assert!(
                windows.is_empty(),
                "no resolved store path must answer an empty, non-error response"
            );
            assert_eq!(taken_at_epoch_s, None);
        }
        other => panic!("expected a normal (empty) restore_candidate_list, got {other:?}"),
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn list_restore_candidates_with_a_nonexistent_database_returns_an_internal_error() {
    let missing_dir = unique_temp_dir("ipc-restore-missing-db");
    let missing = missing_dir.join("does-not-exist.db");
    let harness = Harness::start("ipc-restore-missing-db", config_with_store(missing)).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let _ = expect_error(msg, "internal");

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_window_layout_acks_a_valid_layout() {
    let harness = Harness::start("ipc-layout-ack", DaemonConfig::default()).await;
    let window_id = harness.open(sleepy_command("30"), None).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id: window_id.clone(),
            layout_json: "{\"v\":1}".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowLayoutUpdated { window_id: acked } => {
            assert_eq!(
                acked, window_id,
                "the ack must name the window that was updated"
            )
        }
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_window_layout_returns_window_not_found_for_an_unknown_window() {
    let harness = Harness::start("ipc-layout-not-found", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id: "does-not-exist".to_string(),
            layout_json: "{\"v\":1}".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let _ = expect_error(msg, "window_not_found");

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_window_layout_rejects_a_layout_json_over_the_byte_limit() {
    let harness = Harness::start("ipc-layout-over-limit", DaemonConfig::default()).await;
    let window_id = harness.open(sleepy_command("30"), None).await;
    let mut client = harness.connect().await;

    let oversized = "x".repeat(store::MAX_LAYOUT_JSON_BYTES + 1);
    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id,
            layout_json: oversized,
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    let _ = expect_error(msg, "invalid_argument");

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_window_layout_accepts_a_layout_json_at_exactly_the_byte_limit() {
    let harness = Harness::start("ipc-layout-at-limit", DaemonConfig::default()).await;
    let window_id = harness.open(sleepy_command("30"), None).await;
    let mut client = harness.connect().await;

    let at_limit = "x".repeat(store::MAX_LAYOUT_JSON_BYTES);
    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id: window_id.clone(),
            layout_json: at_limit,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowLayoutUpdated { window_id: acked } => {
            assert_eq!(acked, window_id)
        }
        other => {
            panic!("a layout_json of exactly MAX_LAYOUT_JSON_BYTES must be accepted, got {other:?}")
        }
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_window_layout_is_reflected_in_the_next_snapshot_row() {
    let db = temp_store_db("ipc-layout-reflected");
    let harness = Harness::start("ipc-layout-reflected", DaemonConfig::default()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));

    let window_id = harness.open(sleepy_command("30"), None).await;
    wait_for_current_snapshot(
        &db.path,
        "the window to appear with no layout yet",
        |rows| rows.len() == 1 && rows[0].layout_json.is_none(),
    )
    .await;

    let mut client = harness.connect().await;
    let layout_json = "{\"v\":1,\"pos\":{\"x\":10,\"y\":20}}".to_string();
    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id: window_id.clone(),
            layout_json: layout_json.clone(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowLayoutUpdated { .. } => {}
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    wait_for_current_snapshot(
        &db.path,
        "the updated layout to reach the current snapshot",
        |rows| rows.len() == 1 && rows[0].layout_json.as_deref() == Some(layout_json.as_str()),
    )
    .await;

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn list_windows_reports_layout_json_after_it_is_set() {
    let harness = Harness::start("ipc-listwindows-layout", DaemonConfig::default()).await;
    let window_id = harness.open(sleepy_command("30"), None).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListWindows).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowList { windows } => {
            let entry = windows
                .iter()
                .find(|w| w.window_id == window_id)
                .expect("window must be listed");
            assert_eq!(
                entry.layout_json, None,
                "before any layout is set, layout_json must be None"
            );
        }
        other => panic!("expected window_list, got {other:?}"),
    }

    let layout_json = "{\"v\":1,\"pos\":{\"x\":3,\"y\":4}}".to_string();
    send(
        &mut client,
        &ClientMessage::UpdateWindowLayout {
            window_id: window_id.clone(),
            layout_json: layout_json.clone(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowLayoutUpdated { .. } => {}
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    send(&mut client, &ClientMessage::ListWindows).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowList { windows } => {
            let entry = windows
                .iter()
                .find(|w| w.window_id == window_id)
                .expect("window must still be listed");
            assert_eq!(
                entry.layout_json.as_deref(),
                Some(layout_json.as_str()),
                "after a successful layout update, list_windows's WindowInfo must report it"
            );
        }
        other => panic!("expected window_list, got {other:?}"),
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// A syntactically valid JSON array of one string whose encoded byte length is exactly
/// `total_len`.
fn command_json_of_len(total_len: usize) -> String {
    const OVERHEAD: usize = 4; // the two brackets and two quotes
    let padding_len = total_len
        .checked_sub(OVERHEAD)
        .expect("total_len must be at least 4");
    format!("[\"{}\"]", "x".repeat(padding_len))
}

#[tokio::test]
async fn list_restore_candidates_truncates_from_the_row_that_exceeds_the_reply_budget() {
    let db = temp_store_db("ipc-restore-budget");
    create_schema(&db.path);
    // Comfortably under MAX_COMMAND_JSON_BYTES, so no row is skipped at the store's own
    // read layer, while three rows together overrun RESTORE_REPLY_BUDGET_BYTES with
    // margin for the other fields' serialization overhead.
    const ROW_COMMAND_JSON_LEN: usize = 950_000;
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        for position in 0..3i64 {
            insert_window_row_with_command_json(
                &conn,
                store::SLOT_PREV,
                position,
                &format!("row-{position}"),
                &command_json_of_len(ROW_COMMAND_JSON_LEN),
            );
        }
        set_meta(&conn, 5000, Some(4000));
    }

    let harness = Harness::start("ipc-restore-budget", config_with_store(db.path.clone())).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::RestoreCandidateList { windows, .. } => {
            assert_eq!(
                windows.len(),
                2,
                "3 rows of ~950 KiB each must exceed the 2 MiB reply budget after the third, \
                 leaving exactly the first two"
            );
            let positions: Vec<i64> = windows.iter().map(|w| w.position).collect();
            assert_eq!(
                positions,
                vec![0, 1],
                "truncation must cut off from the row that exceeds the budget, in position \
                 order, keeping a contiguous low-position prefix"
            );
        }
        other => panic!("expected restore_candidate_list, got {other:?}"),
    }

    // The connection must remain usable after a truncated response.
    send(&mut client, &ClientMessage::Ping).await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong after a truncated restore_candidate_list, got {other:?}"),
    }

    drop(harness);
    tokio::time::sleep(Duration::from_millis(200)).await;
}
