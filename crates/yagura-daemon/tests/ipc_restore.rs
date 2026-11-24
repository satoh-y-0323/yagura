#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! IPC-level contracts for the restore flow's daemon side: `ListRestoreCandidates`,
//! `UpdateWindowLayout`, its reflection into the window's row, and `ListWindows`'s
//! `WindowInfo.layout_json`.
//!
//! `ListRestoreCandidates` is answered from `DaemonConfig::store_db_path_resolved`:
//! the candidates are the window table's `lost` rows. Tests needing candidates seed a
//! database with `rusqlite::Connection::open` and point a `DaemonConfig` at that field.
//! The table is untrusted input (another process of the same user can write it), so a
//! corrupt row costs only itself and a read warns once per kind of damage.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::real_daemon::Sandbox;
use common::store_fixtures::{
    insert_lost_window, insert_lost_window_with_invalid_utf8_layout, insert_window_table_row,
    temp_store_db,
};
use common::{Harness, expect_error, read_response, send, sleepy_command, unique_temp_dir};
use rusqlite::types::Value;
use yagura_daemon::DaemonConfig;
use yagura_daemon::store;
use yagura_protocol::{ClientMessage, DaemonMessage, RestoreCandidate};
use yagura_test_support::HANG_GUARD;

/// Creates the schema at `path` through the production entry point, releasing the store's
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

/// A window id of the shape the window table hands out, distinct per `index`.
fn seeded_id(index: usize) -> String {
    format!("w{index:08}")
}

async fn candidates(client: &mut yagura_test_support::ipc::Connection) -> Vec<RestoreCandidate> {
    send(client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(client).await {
        DaemonMessage::RestoreCandidateList { windows, .. } => windows,
        other => panic!("expected restore_candidate_list, got {other:?}"),
    }
}

fn names(candidates: &[RestoreCandidate]) -> Vec<&str> {
    candidates.iter().map(|c| c.name.as_str()).collect()
}

/// The candidates are the `lost` rows only, in the order the windows opened, and one read
/// returns at most the row limit, bounded by a bound SQL parameter rather than a cast.
#[tokio::test]
async fn list_restore_candidates_returns_the_lost_rows_in_open_order_up_to_the_limit() {
    let db = temp_store_db("ipc-restore-lost-rows");
    create_schema(&db.path);
    let limit = usize::try_from(store::MAX_SNAPSHOT_WINDOWS).unwrap();
    let seeded = limit + 44;
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        let command = || Value::Text("[\"cmd.exe\"]".to_owned());
        insert_window_table_row(
            &conn,
            &seeded_id(90_000_000),
            "an-opening-window",
            "opening",
            command(),
            Value::Null,
        );
        insert_window_table_row(
            &conn,
            &seeded_id(90_000_001),
            "a-live-window",
            "live",
            command(),
            Value::Null,
        );
        for index in 0..seeded {
            insert_lost_window(&conn, &seeded_id(index), &format!("lost-{index:03}"));
        }
    }

    let harness = Harness::start("ipc-restore-lost-rows", config_with_store(db.path.clone())).await;
    let mut client = harness.connect().await;
    let read = candidates(&mut client).await;
    let expected: Vec<String> = (0..limit).map(|index| format!("lost-{index:03}")).collect();
    assert_eq!(
        names(&read),
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "the candidates must be the first lost rows in the order the windows opened, at most \
         the row limit; an opening or live row is never one"
    );

    drop(harness);
}

#[tokio::test]
async fn list_restore_candidates_with_no_resolved_path_returns_an_empty_normal_response() {
    let harness = Harness::start("ipc-restore-no-path", DaemonConfig::default()).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut client).await {
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
}

#[tokio::test]
async fn list_restore_candidates_with_a_nonexistent_database_returns_an_internal_error() {
    let missing_dir = unique_temp_dir("ipc-restore-missing-db");
    let missing = missing_dir.join("does-not-exist.db");
    let harness = Harness::start("ipc-restore-missing-db", config_with_store(missing)).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    let msg = read_response(&mut client).await;
    let _ = expect_error(msg, "internal");

    drop(harness);
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
    match read_response(&mut client).await {
        DaemonMessage::WindowLayoutUpdated { window_id: acked } => {
            assert_eq!(
                acked, window_id,
                "the ack must name the window that was updated"
            )
        }
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    drop(harness);
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
    let msg = read_response(&mut client).await;
    let _ = expect_error(msg, "window_not_found");

    drop(harness);
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
    let msg = read_response(&mut client).await;
    let _ = expect_error(msg, "invalid_argument");

    drop(harness);
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
    match read_response(&mut client).await {
        DaemonMessage::WindowLayoutUpdated { window_id: acked } => {
            assert_eq!(acked, window_id)
        }
        other => {
            panic!("a layout_json of exactly MAX_LAYOUT_JSON_BYTES must be accepted, got {other:?}")
        }
    }

    drop(harness);
}

/// The layout `window_id`'s row in the window table at `db` keeps; `None` while the row
/// is not there or keeps none.
fn row_layout(db: &Path, window_id: &str) -> Option<String> {
    let conn = store::open_readonly(db).ok()?;
    conn.query_row(
        "SELECT layout_json FROM windows WHERE window_id = ?1",
        [window_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

#[tokio::test]
async fn update_window_layout_is_reflected_in_the_windows_row() {
    let db = temp_store_db("ipc-layout-reflected");
    let harness = Harness::start("ipc-layout-reflected", DaemonConfig::default()).await;
    store::wire_into(&harness.registry, Some(db.path.clone()));

    let window_id = harness.open(sleepy_command("30"), None).await;
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
    match read_response(&mut client).await {
        DaemonMessage::WindowLayoutUpdated { .. } => {}
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while row_layout(&db.path, &window_id).as_deref() != Some(layout_json.as_str()) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the updated layout never reached the window's row: {:?}",
            row_layout(&db.path, &window_id)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    drop(harness);
}

#[tokio::test]
async fn list_windows_reports_layout_json_after_it_is_set() {
    let harness = Harness::start("ipc-listwindows-layout", DaemonConfig::default()).await;
    let window_id = harness.open(sleepy_command("30"), None).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListWindows).await;
    match read_response(&mut client).await {
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
    match read_response(&mut client).await {
        DaemonMessage::WindowLayoutUpdated { .. } => {}
        other => panic!("expected window_layout_updated, got {other:?}"),
    }

    send(&mut client, &ClientMessage::ListWindows).await;
    match read_response(&mut client).await {
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
        for index in 0..3usize {
            insert_window_table_row(
                &conn,
                &seeded_id(index),
                &format!("row-{index}"),
                "lost",
                Value::Text(command_json_of_len(ROW_COMMAND_JSON_LEN)),
                Value::Null,
            );
        }
    }

    let harness = Harness::start("ipc-restore-budget", config_with_store(db.path.clone())).await;
    let mut client = harness.connect().await;

    send(&mut client, &ClientMessage::ListRestoreCandidates).await;
    match read_response(&mut client).await {
        DaemonMessage::RestoreCandidateList { windows, .. } => {
            assert_eq!(
                windows.len(),
                2,
                "3 rows of ~950 KiB each must exceed the 2 MiB reply budget after the third, \
                 leaving exactly the first two"
            );
            assert_eq!(
                names(&windows),
                vec!["row-0", "row-1"],
                "truncation must cut off from the row that exceeds the budget, in open \
                 order, keeping a contiguous prefix"
            );
        }
        other => panic!("expected restore_candidate_list, got {other:?}"),
    }

    // The connection must remain usable after a truncated response.
    send(&mut client, &ClientMessage::Ping).await;
    match read_response(&mut client).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong after a truncated restore_candidate_list, got {other:?}"),
    }

    drop(harness);
}

/// The `WARN` lines `log` gained past its first `from` bytes.
fn warns_after(log: &str, from: usize) -> usize {
    log.get(from..)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(" WARN "))
        .count()
}

/// A corrupt `lost` row costs only itself: one whose command does not read back as an
/// argv (not JSON, not an array, a `BLOB`, longer than `MAX_COMMAND_JSON_BYTES`) is
/// skipped, and a layout that does not read back as text within `MAX_LAYOUT_JSON_BYTES`
/// becomes `None` while its row stays. Each read warns once for skipped rows and once for
/// degraded layouts however many there were, so a tampered table cannot flood the log.
#[tokio::test]
async fn a_corrupt_lost_row_costs_only_itself_and_each_read_warns_once_per_kind() {
    let sandbox = Sandbox::new("restore-corrupt");
    let data_root = sandbox.data_root();
    std::fs::create_dir_all(&data_root).unwrap();
    let db = data_root.join("yagura.db");
    create_schema(&db);
    let at_limit = "x".repeat(store::MAX_LAYOUT_JSON_BYTES);
    {
        let conn = rusqlite::Connection::open(&db).expect("a seed connection must open");
        let command = || Value::Text("[\"cmd.exe\"]".to_owned());
        let mut index = 0;
        let mut row = |name: &str, command_json: Value, layout_json: Value| {
            insert_window_table_row(
                &conn,
                &seeded_id(index),
                name,
                "lost",
                command_json,
                layout_json,
            );
            index += 1;
        };
        row("good-first", command(), Value::Null);
        row(
            "not-json",
            Value::Text("this is not json".to_owned()),
            Value::Null,
        );
        row(
            "blob-command",
            Value::Blob(b"[\"cmd.exe\"]".to_vec()),
            Value::Null,
        );
        row(
            "not-an-array",
            Value::Text("{\"argv\": [\"cmd.exe\"]}".to_owned()),
            Value::Null,
        );
        row(
            "oversized-command",
            Value::Text(format!(
                "[\"{}\"]",
                "x".repeat(store::MAX_COMMAND_JSON_BYTES)
            )),
            Value::Null,
        );
        row("layout-at-limit", command(), Value::Text(at_limit.clone()));
        row(
            "layout-over-limit",
            command(),
            Value::Text("x".repeat(store::MAX_LAYOUT_JSON_BYTES + 1)),
        );
        row("layout-blob", command(), Value::Blob(b"{\"v\":1}".to_vec()));
        insert_lost_window_with_invalid_utf8_layout(
            &conn,
            &seeded_id(90_000_000),
            "layout-invalid-utf8",
            &[0x7b, 0xff, 0xfe, 0x7d],
        );
        insert_lost_window(&conn, &seeded_id(90_000_001), "good-last");
    }

    let mut daemon = sandbox.start().await;
    // The daemon logs its start-up before it answers anything; connecting alone is not
    // ordered after that.
    common::call_list_windows(&mut daemon.client).await;
    let log_path = sandbox.state_root().join("logs").join("daemon.log");
    for call in ["first", "second"] {
        let before = std::fs::read_to_string(&log_path).unwrap_or_default().len();
        let read = candidates(&mut daemon.client).await;
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert_eq!(
            names(&read),
            vec![
                "good-first",
                "layout-at-limit",
                "layout-over-limit",
                "layout-blob",
                "layout-invalid-utf8",
                "good-last"
            ],
            "the {call} read must skip exactly the rows whose command does not read back"
        );
        let layouts: Vec<Option<&str>> = read.iter().map(|c| c.layout_json.as_deref()).collect();
        assert_eq!(
            layouts,
            vec![None, Some(at_limit.as_str()), None, None, None, None],
            "a layout within the limit must come back unchanged, and one that does not read \
             back as text within it must become None"
        );
        assert_eq!(
            warns_after(&log, before),
            2,
            "the {call} read must warn once for the skipped rows and once for the degraded \
             layouts; it logged:\n{}",
            log.get(before..).unwrap_or_default()
        );
    }
    drop(daemon);
}

/// A saved name comes back as a candidate only in a form the window-name rule accepts,
/// since the GUI sends it back as the name of the window it opens: blank edges are
/// trimmed, a name with nothing visible left takes its command's file stem, and one
/// whose file stem shows nothing either takes the fixed fallback.
#[tokio::test]
async fn a_saved_name_comes_back_trimmed_or_replaced_when_nothing_visible_is_left() {
    let db = temp_store_db("ipc-restore-settled-names");
    create_schema(&db.path);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        let rows = [
            (" lost ", "[\"cmd.exe\"]"),
            ("\u{3000}", "[\"stemname.exe\"]"),
            ("\u{2800}", "[\"\u{3000}.exe\"]"),
        ];
        for (index, (name, command_json)) in rows.into_iter().enumerate() {
            insert_window_table_row(
                &conn,
                &seeded_id(index),
                name,
                "lost",
                Value::Text(command_json.to_owned()),
                Value::Null,
            );
        }
    }

    let harness = Harness::start(
        "ipc-restore-settled-names",
        config_with_store(db.path.clone()),
    )
    .await;
    let mut client = harness.connect().await;
    let read = candidates(&mut client).await;
    assert_eq!(
        names(&read),
        vec!["lost", "stemname", "window"],
        "every candidate's name must already satisfy validate_window_name"
    );

    drop(harness);
}
