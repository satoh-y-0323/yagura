#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! The latest schema's `layout_json` contract for the SQLite store (the column arrived in
//! v2 and every later migration must go on honoring it). Nothing here spawns a PTY, so
//! the file is OS-independent.
//!
//! Seeding uses `rusqlite::Connection::open` directly, through `common::store_fixtures`
//! or inline SQL; every verification read goes through the non-destructive read port
//! (`store::open_readonly` and the `read_*_conn` functions) that the restore flow uses.

mod common;

use std::path::Path;

use common::store_fixtures::{
    capture_warns, count_warns, insert_window_row, insert_window_row_with_command_json,
    insert_window_row_with_invalid_utf8_layout, insert_window_row_with_layout_value, set_meta,
    temp_store_db,
};
use rusqlite::types::Value as SqlValue;
use yagura_daemon::store;

/// Creates schema v2 at `path` through the production entry point, releasing the store's
/// own connection before returning so a seeding connection can take the write lock. Kept
/// per-binary so `Store`'s `Drop`-ness cannot leak into this file's lint status.
fn create_schema(path: &Path) {
    store::open_store(path).expect("open_store must succeed on a fresh path");
}

fn sample_rows_with_layout() -> Vec<store::SnapshotWindow> {
    vec![
        store::SnapshotWindow {
            position: 0,
            command: vec!["cmd.exe".to_string()],
            cwd: "C:/work/alpha".to_string(),
            name: "alpha".to_string(),
            vendor: None,
            detection: None,
            layout_json: Some("{\"v\":1,\"pos\":{\"x\":1,\"y\":2}}".to_string()),
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

/// A JSON-shaped string of exactly `total_len` bytes. The read side never JSON-parses
/// `layout_json` -- only its byte length and UTF-8-ness are checked at this layer -- so a
/// plain padded string is a faithful boundary fixture.
fn layout_json_of_exact_length(total_len: usize) -> String {
    "x".repeat(total_len)
}

const SCHEMA_QUERY: &str =
    "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name";

fn normalize_sql(sql: &str) -> String {
    sql.replace("\r\n", "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_schema(conn: &rusqlite::Connection) -> Vec<(String, String, String)> {
    let mut stmt = conn
        .prepare(SCHEMA_QUERY)
        .expect("the schema comparison query must prepare");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })
        .expect("the schema comparison query must run");
    rows.map(|row| {
        let (kind, name, sql) = row.expect("schema row must decode");
        (kind, name, normalize_sql(&sql))
    })
    .collect()
}

fn user_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version must be readable")
}

#[test]
fn open_store_on_a_fresh_path_reaches_the_latest_schema() {
    let db = temp_store_db("v2-fresh-open");
    create_schema(&db.path);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        store::MIGRATIONS.len() as i64,
        "a fresh open_store must migrate all the way to the latest schema version (the full \
         series)"
    );
}

#[test]
fn production_migrations_applied_incrementally_match_a_fresh_application() {
    let fresh_db = temp_store_db("v2-migrate-fresh");
    {
        let mut conn =
            rusqlite::Connection::open(&fresh_db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("applying the full production series in one go must succeed");
    }

    let incremental_db = temp_store_db("v2-migrate-incremental");
    {
        let mut conn =
            rusqlite::Connection::open(&incremental_db.path).expect("a seed connection must open");
        // Simulates a real upgrade: a database already at v1 is brought up to date by
        // re-running the full series.
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..1])
            .expect("applying only 001 must succeed");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("applying the full series over an existing v1 database must succeed");
    }

    let fresh_conn = store::open_readonly(&fresh_db.path).expect("open_readonly must succeed");
    let incremental_conn =
        store::open_readonly(&incremental_db.path).expect("open_readonly must succeed");

    assert_eq!(
        user_version(&fresh_conn),
        store::MIGRATIONS.len() as i64,
        "the fresh path must reach the latest schema version"
    );
    assert_eq!(
        user_version(&incremental_conn),
        store::MIGRATIONS.len() as i64,
        "the incremental (upgrade) path must also reach the latest schema version, not stall \
         partway or double-apply the last migration"
    );
    assert_eq!(
        read_schema(&fresh_conn),
        read_schema(&incremental_conn),
        "a database migrated in one go and one upgraded incrementally from v1 must land on the \
         identical schema"
    );
}

// A v1-origin row's `layout_json` is NULL because it predates the 002 migration's ADD
// COLUMN, which is nullable, so no backfill is needed or performed.
#[test]
fn write_snapshot_then_read_round_trips_layout_json_when_present() {
    let db = temp_store_db("v2-round-trip-some");
    create_schema(&db.path);

    let written = sample_rows_with_layout();
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        store::write_snapshot_once(&mut conn, &written).expect("write_snapshot_once must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let read_back = store::read_snapshot_conn(&conn).expect("the round trip must read back");
    assert_eq!(
        read_back, written,
        "layout_json (Some and None across the two seeded rows) must round-trip verbatim"
    );
}

#[test]
fn a_v1_origin_row_predating_the_layout_column_reads_back_as_none() {
    let db = temp_store_db("v2-round-trip-v1-origin");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        // The insert never mentions layout_json at all -- the shape a row written before
        // the column existed would have, since the ADD COLUMN default is NULL.
        insert_window_row(&conn, store::SLOT_CURRENT, 0, "legacy-row");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, rows) = count_warns(|| {
        store::read_snapshot_conn(&conn).expect("a v1-origin row must read back, not fail")
    });
    assert_eq!(
        rows.len(),
        1,
        "the v1-origin row must still be a restore candidate"
    );
    assert_eq!(
        rows[0].layout_json, None,
        "a row predating the layout_json column must read back as None, not empty or an error"
    );
    assert_eq!(
        warns, 0,
        "a NULL layout_json must never warn (NULL reads as None silently)"
    );
}

#[test]
// Exercises rotate_slots itself, outside the startup path that owns it.
#[allow(clippy::disallowed_methods)]
fn rotate_slots_preserves_layout_json_in_the_rotated_prev_row() {
    let db = temp_store_db("v2-rotate-preserves-layout");
    create_schema(&db.path);

    let written = sample_rows_with_layout();
    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    store::write_snapshot_once(&mut conn, &written).expect("write_snapshot_once must succeed");
    store::rotate_slots(&mut conn).expect("rotate_slots must succeed");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let prev = store::read_prev_snapshot_conn(&conn).expect("prev must be readable");
    assert_eq!(
        prev, written,
        "rotate_slots moves rows wholesale (UPDATE ... SET slot), so layout_json must survive \
         into the rotated prev row unchanged"
    );
}

const DEGRADED_LAYOUT_WARNING: &str = "degraded one or more snapshot_windows layout values to none";
const SKIPPED_ROW_WARNING: &str = "skipped one or more corrupt snapshot_windows rows";

#[test]
fn a_layout_json_at_exactly_the_byte_limit_is_returned_unchanged() {
    let db = temp_store_db("v2-layout-at-limit");
    create_schema(&db.path);

    let at_limit = layout_json_of_exact_length(store::MAX_LAYOUT_JSON_BYTES);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            0,
            "at-limit",
            SqlValue::Text(at_limit.clone()),
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, rows) =
        count_warns(|| store::read_snapshot_conn(&conn).expect("a well-formed store must read"));
    assert_eq!(
        rows.first().and_then(|row| row.layout_json.as_deref()),
        Some(at_limit.as_str()),
        "a layout_json of exactly MAX_LAYOUT_JSON_BYTES must be returned, not degraded (boundary \
         regression guard against an off-by-one)"
    );
    assert_eq!(warns, 0, "a well-formed row at the boundary must not warn");
}

#[test]
fn a_layout_json_one_byte_over_the_limit_degrades_to_none_and_warns() {
    let db = temp_store_db("v2-layout-over-limit");
    create_schema(&db.path);

    let oversized = layout_json_of_exact_length(store::MAX_LAYOUT_JSON_BYTES + 1);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            0,
            "over-limit",
            SqlValue::Text(oversized),
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn).expect("an oversized layout_json must degrade, not fail")
    });
    assert_eq!(
        rows.len(),
        1,
        "unlike an oversized command_json, an oversized layout_json must not drop the row"
    );
    assert_eq!(
        rows[0].layout_json, None,
        "a layout_json exceeding MAX_LAYOUT_JSON_BYTES must degrade to None"
    );
    assert_eq!(
        messages,
        vec![DEGRADED_LAYOUT_WARNING.to_string()],
        "the degradation must warn exactly once, with the dedicated layout wording"
    );
}

#[test]
fn a_blob_layout_json_degrades_to_none_and_warns() {
    let db = temp_store_db("v2-layout-blob");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            0,
            "blob-layout",
            SqlValue::Blob(vec![0xff, 0xfe, 0x00, 0x01]),
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn).expect("a BLOB layout_json must degrade, not fail")
    });
    assert_eq!(rows.len(), 1, "a BLOB layout_json must not drop the row");
    assert_eq!(
        rows[0].layout_json, None,
        "a BLOB layout_json must degrade to None"
    );
    assert_eq!(messages, vec![DEGRADED_LAYOUT_WARNING.to_string()]);
}

#[test]
fn an_invalid_utf8_layout_json_degrades_to_none_and_warns() {
    let db = temp_store_db("v2-layout-invalid-utf8");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_invalid_utf8_layout(
            &conn,
            store::SLOT_CURRENT,
            0,
            "invalid-utf8-layout",
            &[0xff, 0xfe, 0xfd],
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn)
            .expect("an invalid-UTF-8 layout_json must degrade, not fail")
    });
    assert_eq!(
        rows.len(),
        1,
        "an invalid-UTF-8 layout_json must not drop the row"
    );
    assert_eq!(
        rows[0].layout_json, None,
        "an invalid-UTF-8 layout_json must degrade to None"
    );
    assert_eq!(messages, vec![DEGRADED_LAYOUT_WARNING.to_string()]);
}

#[test]
fn two_degraded_layout_rows_in_one_call_still_warn_only_once() {
    let db = temp_store_db("v2-layout-multiple-degraded-rows");
    create_schema(&db.path);

    let oversized = layout_json_of_exact_length(store::MAX_LAYOUT_JSON_BYTES + 1);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            0,
            "oversized-layout",
            SqlValue::Text(oversized),
        );
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            1,
            "blob-layout",
            SqlValue::Blob(vec![0xff, 0xfe, 0x00, 0x01]),
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn)
            .expect("multiple degraded layout rows must not fail the read")
    });

    assert_eq!(
        rows.len(),
        2,
        "both rows must survive (layout degradation is field-level, not a row skip)"
    );
    assert!(
        rows.iter().all(|row| row.layout_json.is_none()),
        "both rows' layout_json must degrade to None"
    );
    assert_eq!(
        messages,
        vec![DEGRADED_LAYOUT_WARNING.to_string()],
        "one read call degrading two (or more) layout rows must still emit the dedicated layout \
         warning exactly once, aggregated per call just like command_json's skip warning \
         (reading_a_snapshot_skips_corrupt_rows_and_warns_once_per_call in store_snapshot.rs), \
         not once per degraded row"
    );
}

#[test]
fn a_null_layout_json_degrades_to_none_without_warning() {
    let db = temp_store_db("v2-layout-null");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            0,
            "null-layout",
            SqlValue::Null,
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn).expect("a NULL layout_json must read back, not fail")
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].layout_json, None);
    assert!(
        messages.is_empty(),
        "NULL is the ordinary \"no layout saved yet\" case and must never warn, got: {messages:?}"
    );
}

#[test]
fn command_json_skip_and_layout_json_degradation_warn_independently_in_the_same_call() {
    // Both degradation axes in one read: the corrupt command_json row must be skipped and
    // the oversized layout_json on a different, otherwise well-formed row must degrade,
    // each warning exactly once and distinctly from the other.
    let db = temp_store_db("v2-layout-and-command-both-warn");
    create_schema(&db.path);

    let oversized_layout = layout_json_of_exact_length(store::MAX_LAYOUT_JSON_BYTES + 1);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "ok", "[\"cmd.exe\"]");
        insert_window_row_with_command_json(
            &conn,
            store::SLOT_CURRENT,
            1,
            "corrupt-command",
            "this is not json",
        );
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            2,
            "oversized-layout",
            SqlValue::Text(oversized_layout),
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, rows) = capture_warns(|| {
        store::read_snapshot_conn(&conn).expect("both degradation axes must not fail the read")
    });

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0, 2],
        "the corrupt-command row (position 1) must be skipped; the oversized-layout row \
         (position 2) must survive with layout degraded"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.position == 2)
            .and_then(|row| row.layout_json.as_ref()),
        None
    );

    let mut sorted = messages.clone();
    sorted.sort_unstable();
    let mut expected = vec![
        SKIPPED_ROW_WARNING.to_string(),
        DEGRADED_LAYOUT_WARNING.to_string(),
    ];
    expected.sort_unstable();
    assert_eq!(
        sorted, expected,
        "both warnings must fire, each exactly once, with distinct wording; got: {messages:?}"
    );
}

// A corrupt command_json row must still be skipped alongside layout_json, with the
// original wording, even when its own layout_json is perfectly well-formed.
#[test]
fn a_corrupt_command_json_row_is_still_skipped_when_its_layout_json_is_well_formed() {
    let db = temp_store_db("v2-command-skip-non-regression");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "ok", "[\"cmd.exe\"]");
        insert_window_row_with_layout_value(
            &conn,
            store::SLOT_CURRENT,
            1,
            "corrupt-command-good-layout",
            SqlValue::Text("{\"v\":1}".to_string()),
        );
        // Overwrite row 1's command_json directly, since
        // insert_window_row_with_layout_value's own command_json is well-formed.
        conn.execute(
            "UPDATE snapshot_windows SET command_json = ?1 WHERE slot = ?2 AND position = ?3",
            rusqlite::params!["not json at all", store::SLOT_CURRENT, 1],
        )
        .expect("seeding the corrupt command_json must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, rows) = count_warns(|| {
        store::read_snapshot_conn(&conn).expect("a corrupt row must degrade, not fail the read")
    });

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0],
        "the row with a well-formed layout_json but corrupt command_json must still be skipped \
         entirely (command_json corruption is a whole-row skip, not a field-level degradation)"
    );
    assert_eq!(
        warns, 1,
        "the skip must warn exactly once, the pre-existing (unchanged) command_json contract"
    );
}

// No meta row, a rotated Some, an explicit NULL and a non-Integer storage class all fall
// to Ok(None), and the storage-class case never warns.
#[test]
fn read_prev_taken_at_conn_returns_none_when_no_meta_row_exists() {
    let db = temp_store_db("v2-prev-taken-at-no-row");
    create_schema(&db.path);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let result = store::read_prev_taken_at_conn(&conn)
        .expect("a fresh schema with no meta row must not error");
    assert_eq!(result, None, "no meta row at all must read as Ok(None)");
}

#[test]
// Exercises rotate_slots itself, outside the startup path that owns it.
#[allow(clippy::disallowed_methods)]
fn read_prev_taken_at_conn_returns_some_after_rotation() {
    let db = temp_store_db("v2-prev-taken-at-rotated");
    create_schema(&db.path);

    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    insert_window_row(&conn, store::SLOT_CURRENT, 0, "current-a");
    set_meta(&conn, 555, None);
    store::rotate_slots(&mut conn).expect("rotate_slots must succeed");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let result = store::read_prev_taken_at_conn(&conn).expect("a rotated meta row must not error");
    assert_eq!(
        result,
        Some(555),
        "after rotate_slots stamps prev_taken_at_epoch_s, it must read back as Ok(Some(..))"
    );
}

#[test]
fn read_prev_taken_at_conn_returns_none_for_an_explicit_null() {
    let db = temp_store_db("v2-prev-taken-at-null");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        // A meta row exists (from a single write) but prev_taken_at_epoch_s
        // has never been set (no rotation has happened yet), so it is NULL.
        set_meta(&conn, 100, None);
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let result = store::read_prev_taken_at_conn(&conn)
        .expect("an explicit NULL prev_taken_at_epoch_s must not error");
    assert_eq!(
        result, None,
        "a meta row with a NULL prev_taken_at_epoch_s must be Ok(None)"
    );
}

#[test]
fn read_prev_taken_at_conn_degrades_a_non_integer_cell_without_warning() {
    let db = temp_store_db("v2-prev-taken-at-non-integer");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        set_meta(&conn, 100, None);
        conn.execute(
            "UPDATE snapshot_meta SET prev_taken_at_epoch_s = ?1 WHERE id = 1",
            rusqlite::params![SqlValue::Text("not-a-number".to_string())],
        )
        .expect("seeding a TEXT prev_taken_at_epoch_s must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (messages, result) = capture_warns(|| store::read_prev_taken_at_conn(&conn));
    assert_eq!(
        result.expect("a non-Integer cell must degrade, not error"),
        None,
        "a TEXT prev_taken_at_epoch_s cell must read as Ok(None)"
    );
    assert!(
        messages.is_empty(),
        "this degradation must never warn, got: {messages:?}"
    );

    // Same degradation, different non-Integer storage class (BLOB).
    let blob_conn =
        rusqlite::Connection::open(&db.path).expect("a second write connection must open");
    blob_conn
        .execute(
            "UPDATE snapshot_meta SET prev_taken_at_epoch_s = ?1 WHERE id = 1",
            rusqlite::params![SqlValue::Blob(vec![1, 2, 3])],
        )
        .expect("seeding a BLOB prev_taken_at_epoch_s must succeed");
    drop(blob_conn);

    let (messages, result) = capture_warns(|| store::read_prev_taken_at_conn(&conn));
    assert_eq!(result.expect("a BLOB cell must degrade, not error"), None);
    assert!(
        messages.is_empty(),
        "a BLOB cell must not warn either, got: {messages:?}"
    );
}
