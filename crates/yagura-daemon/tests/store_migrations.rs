#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `store::rotate_slots` is called directly here: it is the unit under test.
// The database is also seeded and read with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Migration, schema and rotation contracts for the SQLite store. Nothing here spawns a
//! PTY, so the file is OS-independent.
//!
//! Seeding and preparation use `rusqlite::Connection::open` directly, since the store
//! deliberately exposes no query port. Every verification read goes through
//! `store::open_readonly`, the non-destructive read port. A `Store` returned by
//! `open_store` is always dropped before a second connection writes to the same file, so
//! the two never contend for the write lock.
//!
//! Every database lives in a per-test temporary directory, and the `resolve_db_path`
//! contract passes synthetic `OsString`s rather than reading the real environment.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use common::store_fixtures::{
    count_warns, insert_window_row, insert_window_row_with_blob_command,
    insert_window_row_with_command_json, read_meta, set_meta, slot_rows, temp_store_db,
};
use yagura_daemon::store;

/// The comparison query, fixed verbatim.
const SCHEMA_QUERY: &str =
    "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name";

/// Normalizes one `sqlite_master.sql` value: CRLF to LF, every run of
/// whitespace (newlines included) collapsed to a single space, trimmed.
fn normalize_sql(sql: &str) -> String {
    sql.replace("\r\n", "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `(type, name, normalized sql)` row set of a database.
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

/// Schema v2, derived mechanically in two steps:
///
/// 1. Schema v1 comes from the DDL fence: split the fenced DDL on `;`, drop each
///    statement's trailing semicolon (`sqlite_master.sql` does not store it), normalize
///    whitespace, and order the resulting `(type, name, sql)` rows by `(type, name)`.
/// 2. `002_layout.sql`'s `ALTER TABLE ... ADD COLUMN` is folded into
///    `snapshot_windows`'s `CREATE TABLE` text: `, layout_json TEXT` goes immediately
///    after the last column definition and before the table constraint. `snapshot_meta`
///    has no table constraint, so this fold rule is specific to `snapshot_windows`.
const EXPECTED_SCHEMA_V2: &[(&str, &str, &str)] = &[
    (
        "table",
        "snapshot_meta",
        "CREATE TABLE snapshot_meta ( id INTEGER PRIMARY KEY CHECK (id = 1), \
         taken_at_epoch_s INTEGER NOT NULL, prev_taken_at_epoch_s INTEGER )",
    ),
    (
        "table",
        "snapshot_windows",
        "CREATE TABLE snapshot_windows ( slot INTEGER NOT NULL CHECK (slot IN (0, 1)), \
         position INTEGER NOT NULL, command_json TEXT NOT NULL, cwd TEXT NOT NULL, \
         name TEXT NOT NULL, vendor TEXT, detection TEXT, layout_json TEXT, \
         PRIMARY KEY (slot, position) )",
    ),
];

fn expected_schema_v2() -> Vec<(String, String, String)> {
    EXPECTED_SCHEMA_V2
        .iter()
        .map(|(kind, name, sql)| ((*kind).to_string(), (*name).to_string(), (*sql).to_string()))
        .collect()
}

/// Schema v3, derived mechanically: `EXPECTED_SCHEMA_V2`'s two rows plus the three
/// `CREATE TABLE` statements fenced in the chat schema's design record, normalized the
/// same way as v1 was (split on `;`, drop the trailing semicolon, collapse whitespace),
/// sorted by `(type, name)`. The fenced `INSERT` is not schema and is checked separately
/// (see `production_migrations_seed_the_main_chat_room_row`).
const EXPECTED_SCHEMA_V3: &[(&str, &str, &str)] = &[
    (
        "table",
        "chat_delivery",
        "CREATE TABLE chat_delivery ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         window_id TEXT NOT NULL, state TEXT NOT NULL, at_ms INTEGER NOT NULL, \
         PRIMARY KEY (room_id, seq, window_id) )",
    ),
    (
        "table",
        "chat_message",
        "CREATE TABLE chat_message ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         at_ms INTEGER NOT NULL, from_kind TEXT NOT NULL, from_window_id TEXT, \
         from_name TEXT NOT NULL, body TEXT NOT NULL, mentions_json TEXT NOT NULL, \
         broadcast INTEGER NOT NULL DEFAULT 0, silence_noted INTEGER NOT NULL DEFAULT 0, \
         PRIMARY KEY (room_id, seq) )",
    ),
    (
        "table",
        "chat_room",
        "CREATE TABLE chat_room ( room_id TEXT PRIMARY KEY, halted_reason TEXT, \
         halted_at_ms INTEGER, needs_user INTEGER NOT NULL DEFAULT 0 )",
    ),
    (
        "table",
        "snapshot_meta",
        "CREATE TABLE snapshot_meta ( id INTEGER PRIMARY KEY CHECK (id = 1), \
         taken_at_epoch_s INTEGER NOT NULL, prev_taken_at_epoch_s INTEGER )",
    ),
    (
        "table",
        "snapshot_windows",
        "CREATE TABLE snapshot_windows ( slot INTEGER NOT NULL CHECK (slot IN (0, 1)), \
         position INTEGER NOT NULL, command_json TEXT NOT NULL, cwd TEXT NOT NULL, \
         name TEXT NOT NULL, vendor TEXT, detection TEXT, layout_json TEXT, \
         PRIMARY KEY (slot, position) )",
    ),
];

fn expected_schema_v3() -> Vec<(String, String, String)> {
    EXPECTED_SCHEMA_V3
        .iter()
        .map(|(kind, name, sql)| ((*kind).to_string(), (*name).to_string(), (*sql).to_string()))
        .collect()
}

/// Schema v4, derived mechanically from `EXPECTED_SCHEMA_V3`: `004_chat_rooms.sql` is three `ALTER TABLE chat_room ADD COLUMN`
/// statements plus one `CREATE UNIQUE INDEX`. There is no `display_name` column: the
/// display name a room shows is a read-time value the disambiguation function derives
/// from `(workspace_kind, workspace_key)`, so a stored copy would only be able to drift
/// from it. Applying the DDL to a fresh v3 database and reading `sqlite_master.sql` back
/// (verified against a live SQLite connection, not guessed) shows two changes relative to
/// v3: `chat_room`'s `CREATE TABLE` text gets `, workspace_kind TEXT, workspace_key TEXT,
/// created_at_ms INTEGER NOT NULL DEFAULT 0` folded in immediately before its closing
/// parenthesis (the same "ADD COLUMN text lands right before the closing paren" rule
/// `EXPECTED_SCHEMA_V2`'s doc comment already documents for `layout_json`), and a new
/// `chat_room_workspace` index row appears. `chat_delivery`, `chat_message`,
/// `snapshot_meta` and `snapshot_windows` are untouched. Sorted by `(type, name)`:
/// `"index" < "table"` lexically, so the new index row sorts before every table row.
const EXPECTED_SCHEMA_V4: &[(&str, &str, &str)] = &[
    (
        "index",
        "chat_room_workspace",
        "CREATE UNIQUE INDEX chat_room_workspace ON chat_room (workspace_kind, workspace_key) \
         WHERE workspace_key IS NOT NULL",
    ),
    (
        "table",
        "chat_delivery",
        "CREATE TABLE chat_delivery ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         window_id TEXT NOT NULL, state TEXT NOT NULL, at_ms INTEGER NOT NULL, \
         PRIMARY KEY (room_id, seq, window_id) )",
    ),
    (
        "table",
        "chat_message",
        "CREATE TABLE chat_message ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         at_ms INTEGER NOT NULL, from_kind TEXT NOT NULL, from_window_id TEXT, \
         from_name TEXT NOT NULL, body TEXT NOT NULL, mentions_json TEXT NOT NULL, \
         broadcast INTEGER NOT NULL DEFAULT 0, silence_noted INTEGER NOT NULL DEFAULT 0, \
         PRIMARY KEY (room_id, seq) )",
    ),
    (
        "table",
        "chat_room",
        "CREATE TABLE chat_room ( room_id TEXT PRIMARY KEY, halted_reason TEXT, \
         halted_at_ms INTEGER, needs_user INTEGER NOT NULL DEFAULT 0 , workspace_kind TEXT, \
         workspace_key TEXT, created_at_ms INTEGER NOT NULL DEFAULT 0)",
    ),
    (
        "table",
        "snapshot_meta",
        "CREATE TABLE snapshot_meta ( id INTEGER PRIMARY KEY CHECK (id = 1), \
         taken_at_epoch_s INTEGER NOT NULL, prev_taken_at_epoch_s INTEGER )",
    ),
    (
        "table",
        "snapshot_windows",
        "CREATE TABLE snapshot_windows ( slot INTEGER NOT NULL CHECK (slot IN (0, 1)), \
         position INTEGER NOT NULL, command_json TEXT NOT NULL, cwd TEXT NOT NULL, \
         name TEXT NOT NULL, vendor TEXT, detection TEXT, layout_json TEXT, \
         PRIMARY KEY (slot, position) )",
    ),
];

fn expected_schema_v4() -> Vec<(String, String, String)> {
    EXPECTED_SCHEMA_V4
        .iter()
        .map(|(kind, name, sql)| ((*kind).to_string(), (*name).to_string(), (*sql).to_string()))
        .collect()
}

/// Schema v5, derived mechanically from `EXPECTED_SCHEMA_V4`: the chat delivery reason
/// migration is one `ALTER TABLE chat_delivery ADD COLUMN unknown_reason TEXT`, no other
/// object. Applying that statement to a table with a trailing `PRIMARY KEY` constraint
/// (verified against a live SQLite connection, not guessed) folds the new column text in
/// immediately before the constraint, the same "ADD COLUMN text lands right before the
/// closing paren" rule `EXPECTED_SCHEMA_V2`'s doc comment documents for `layout_json`.
/// `chat_message`, `chat_room`, `chat_room_workspace`, `snapshot_meta` and
/// `snapshot_windows` are untouched.
const EXPECTED_SCHEMA_V5: &[(&str, &str, &str)] = &[
    (
        "index",
        "chat_room_workspace",
        "CREATE UNIQUE INDEX chat_room_workspace ON chat_room (workspace_kind, workspace_key) \
         WHERE workspace_key IS NOT NULL",
    ),
    (
        "table",
        "chat_delivery",
        "CREATE TABLE chat_delivery ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         window_id TEXT NOT NULL, state TEXT NOT NULL, at_ms INTEGER NOT NULL, \
         unknown_reason TEXT, PRIMARY KEY (room_id, seq, window_id) )",
    ),
    (
        "table",
        "chat_message",
        "CREATE TABLE chat_message ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         at_ms INTEGER NOT NULL, from_kind TEXT NOT NULL, from_window_id TEXT, \
         from_name TEXT NOT NULL, body TEXT NOT NULL, mentions_json TEXT NOT NULL, \
         broadcast INTEGER NOT NULL DEFAULT 0, silence_noted INTEGER NOT NULL DEFAULT 0, \
         PRIMARY KEY (room_id, seq) )",
    ),
    (
        "table",
        "chat_room",
        "CREATE TABLE chat_room ( room_id TEXT PRIMARY KEY, halted_reason TEXT, \
         halted_at_ms INTEGER, needs_user INTEGER NOT NULL DEFAULT 0 , workspace_kind TEXT, \
         workspace_key TEXT, created_at_ms INTEGER NOT NULL DEFAULT 0)",
    ),
    (
        "table",
        "snapshot_meta",
        "CREATE TABLE snapshot_meta ( id INTEGER PRIMARY KEY CHECK (id = 1), \
         taken_at_epoch_s INTEGER NOT NULL, prev_taken_at_epoch_s INTEGER )",
    ),
    (
        "table",
        "snapshot_windows",
        "CREATE TABLE snapshot_windows ( slot INTEGER NOT NULL CHECK (slot IN (0, 1)), \
         position INTEGER NOT NULL, command_json TEXT NOT NULL, cwd TEXT NOT NULL, \
         name TEXT NOT NULL, vendor TEXT, detection TEXT, layout_json TEXT, \
         PRIMARY KEY (slot, position) )",
    ),
];

fn expected_schema_v5() -> Vec<(String, String, String)> {
    EXPECTED_SCHEMA_V5
        .iter()
        .map(|(kind, name, sql)| ((*kind).to_string(), (*name).to_string(), (*sql).to_string()))
        .collect()
}

/// Schema v7, derived mechanically from `EXPECTED_SCHEMA_V5`: schema v6's own migration
/// (`006_chat_delivery_reason_check.sql`) rebuilds `chat_delivery` under a temporary name
/// and renames it back, which -- verified against a live SQLite connection, not guessed
/// -- makes `sqlite_master.sql` quote the table name (`CREATE TABLE "chat_delivery"`)
/// where the original `CREATE TABLE chat_delivery` was not quoted; every other v5 object
/// is untouched by that migration. Migration 007 (`007_plans.sql`) then adds four new
/// tables in one plan module: `plan` (one row per plan), `plan_task` (one row per task,
/// parent- and user-specified profile/model/effort each their own nullable column, plus
/// the task's bound `window_id`), `plan_task_dep` (one row per dependency edge) and
/// `plan_task_write` (one row per normalized write target). None of the four tables
/// existed before migration 007; none of the six v6 objects changes. Migration 008
/// (`008_task_report.sql`, this contract's own addition) then adds four nullable columns
/// to `plan_task` through four `ALTER TABLE ... ADD COLUMN` statements -- `report_outcome`
/// (with the `IN ('succeeded', 'failed')` CHECK), `report_summary`, `report_path`,
/// `reported_at` -- and touches no other table. `ALTER TABLE ADD COLUMN` folds each new
/// column definition into `sqlite_master.sql` immediately after the previous last column and
/// before `plan_task`'s trailing `PRIMARY KEY` clause (the same fold rule
/// `expected_schema_v2`'s doc comment records for `snapshot_windows.layout_json`,
/// confirmed here against SQLite 3.45.1 directly: four sequential
/// `ADD COLUMN` calls on `plan_task` land in declaration order, all before
/// the `PRIMARY KEY`). No table is added or renamed, so every row here besides
/// `plan_task` is unchanged from what migration 007 already produced.
const EXPECTED_SCHEMA_V8: &[(&str, &str, &str)] = &[
    (
        "index",
        "chat_room_workspace",
        "CREATE UNIQUE INDEX chat_room_workspace ON chat_room (workspace_kind, workspace_key) \
         WHERE workspace_key IS NOT NULL",
    ),
    (
        "table",
        "chat_delivery",
        "CREATE TABLE \"chat_delivery\" ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         window_id TEXT NOT NULL, state TEXT NOT NULL, at_ms INTEGER NOT NULL, \
         unknown_reason TEXT, PRIMARY KEY (room_id, seq, window_id), \
         CHECK (unknown_reason IS NULL OR state = 'undelivered_unknown') )",
    ),
    (
        "table",
        "chat_message",
        "CREATE TABLE chat_message ( room_id TEXT NOT NULL, seq INTEGER NOT NULL, \
         at_ms INTEGER NOT NULL, from_kind TEXT NOT NULL, from_window_id TEXT, \
         from_name TEXT NOT NULL, body TEXT NOT NULL, mentions_json TEXT NOT NULL, \
         broadcast INTEGER NOT NULL DEFAULT 0, silence_noted INTEGER NOT NULL DEFAULT 0, \
         PRIMARY KEY (room_id, seq) )",
    ),
    (
        "table",
        "chat_room",
        "CREATE TABLE chat_room ( room_id TEXT PRIMARY KEY, halted_reason TEXT, \
         halted_at_ms INTEGER, needs_user INTEGER NOT NULL DEFAULT 0 , workspace_kind TEXT, \
         workspace_key TEXT, created_at_ms INTEGER NOT NULL DEFAULT 0)",
    ),
    (
        "table",
        "plan",
        "CREATE TABLE plan ( plan_id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL, \
         owner_window TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL )",
    ),
    (
        "table",
        "plan_task",
        "CREATE TABLE plan_task ( plan_id TEXT NOT NULL, task_id TEXT NOT NULL, \
         seq INTEGER NOT NULL, title TEXT NOT NULL, role TEXT, parent_profile TEXT, \
         parent_model TEXT, parent_effort TEXT, user_profile TEXT, user_model TEXT, \
         user_effort TEXT, window_id TEXT, \
         report_outcome TEXT CHECK (report_outcome IN ('succeeded', 'failed')), \
         report_summary TEXT, report_path TEXT, reported_at INTEGER, \
         PRIMARY KEY (plan_id, task_id) )",
    ),
    (
        "table",
        "plan_task_dep",
        "CREATE TABLE plan_task_dep ( plan_id TEXT NOT NULL, task_id TEXT NOT NULL, \
         depends_on TEXT NOT NULL, PRIMARY KEY (plan_id, task_id, depends_on) )",
    ),
    (
        "table",
        "plan_task_write",
        "CREATE TABLE plan_task_write ( plan_id TEXT NOT NULL, task_id TEXT NOT NULL, \
         path TEXT NOT NULL, PRIMARY KEY (plan_id, task_id, path) )",
    ),
    (
        "table",
        "snapshot_meta",
        "CREATE TABLE snapshot_meta ( id INTEGER PRIMARY KEY CHECK (id = 1), \
         taken_at_epoch_s INTEGER NOT NULL, prev_taken_at_epoch_s INTEGER )",
    ),
    (
        "table",
        "snapshot_windows",
        "CREATE TABLE snapshot_windows ( slot INTEGER NOT NULL CHECK (slot IN (0, 1)), \
         position INTEGER NOT NULL, command_json TEXT NOT NULL, cwd TEXT NOT NULL, \
         name TEXT NOT NULL, vendor TEXT, detection TEXT, layout_json TEXT, \
         PRIMARY KEY (slot, position) )",
    ),
];

fn expected_schema_v8() -> Vec<(String, String, String)> {
    EXPECTED_SCHEMA_V8
        .iter()
        .map(|(kind, name, sql)| ((*kind).to_string(), (*name).to_string(), (*sql).to_string()))
        .collect()
}

// `sample_rows` cannot join the other seeding helpers in
// `tests/common/store_fixtures.rs`: that shared module must not name `store::`, and
// `SnapshotWindow` does.

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

#[test]
fn fresh_open_store_reaches_the_shipped_schema_version_with_the_same_object_names() {
    let db = temp_store_db("store-fresh-open");
    create_schema(&db.path);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        store::MIGRATIONS.len() as i64,
        "a fresh database must be migrated all the way to the end of the shipped series"
    );

    let names: Vec<String> = read_schema(&conn)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(
        names,
        vec![
            "chat_room_workspace".to_string(),
            "chat_delivery".to_string(),
            "chat_message".to_string(),
            "chat_room".to_string(),
            "plan".to_string(),
            "plan_task".to_string(),
            "plan_task_dep".to_string(),
            "plan_task_write".to_string(),
            "snapshot_meta".to_string(),
            "snapshot_windows".to_string(),
        ],
        "migration 007 (the plan module) adds four new tables on top of the six the chat and \
         session-store migrations already left in place; nothing after the fourth migration \
         changes any of those first six object names"
    );
}

/// The plan module's own schema plus the task-report columns migration 008 adds to
/// `plan_task`, checked field-for-field rather than by name alone (`expected_schema_v8`'s
/// doc comment records how each `plan*` table's DDL was derived). Kept meaningful
/// independently of whatever `MIGRATIONS` holds beyond index 7, the same way the v2
/// through v5 cases keep their versions meaningful.
#[test]
fn migrations_through_the_task_report_reach_schema_v8_matching_the_frozen_ddl() {
    let (version, schema) = apply_and_read("store-v8-ddl", &[&store::MIGRATIONS[..8]]);
    assert_eq!(
        version, 8,
        "applying the first eight migrations must set user_version to 8"
    );
    assert_eq!(
        schema,
        expected_schema_v8(),
        "the v8 schema (including plan_task's four task-report columns) must match the \
         frozen v8 DDL exactly, not merely carry the right object names"
    );
}

/// Schema v9: migration 009 (`009_task_instruction.sql`) adds one nullable `instruction
/// TEXT` column to `plan_task` through `ALTER TABLE ... ADD COLUMN`, folded into
/// `sqlite_master.sql` after `reported_at` and before the trailing `PRIMARY KEY` clause
/// (the fold rule `expected_schema_v8`'s doc comment records). Every other object is
/// unchanged from v8.
fn expected_schema_v9() -> Vec<(String, String, String)> {
    const V8_PLAN_TASK_TAIL: &str = "reported_at INTEGER, PRIMARY KEY (plan_id, task_id) )";
    const V9_PLAN_TASK_TAIL: &str =
        "reported_at INTEGER, instruction TEXT, PRIMARY KEY (plan_id, task_id) )";
    expected_schema_v8()
        .into_iter()
        .map(|(kind, name, sql)| {
            if name == "plan_task" {
                assert!(
                    sql.ends_with(V8_PLAN_TASK_TAIL),
                    "the v8 plan_task DDL must end with the tail the v9 fold replaces"
                );
                let sql = sql.replace(V8_PLAN_TASK_TAIL, V9_PLAN_TASK_TAIL);
                (kind, name, sql)
            } else {
                (kind, name, sql)
            }
        })
        .collect()
}

/// Applying the first nine migrations reaches `user_version` 9 with
/// `plan_task.instruction` as a nullable `TEXT` column and nothing else changed, matching
/// the frozen v9 DDL exactly -- kept meaningful independently of whatever `MIGRATIONS`
/// currently holds beyond index 8, the same way `migrations_through_the_task_report_reach_schema_v8_matching_the_frozen_ddl`
/// keeps v8 meaningful (that test's own `&store::MIGRATIONS[..8]` shape). Migration 010
/// (`instruction_notice_state` / `instruction_notice_at`) moved "the shipped series ends
/// here" off of v9 and onto v10 (`fresh_open_store_reaches_schema_v10_with_nullable_instruction_notice_columns`);
/// this contract only pins what v9 itself looks like.
#[test]
fn fresh_open_store_reaches_schema_v9_with_a_nullable_task_instruction_column() {
    let (_db, conn) = migrated_readonly("store-v9-ddl", &[&store::MIGRATIONS[..9]]);
    assert_eq!(
        user_version(&conn),
        9,
        "applying the first nine migrations must set user_version to 9"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v9(),
        "the v9 schema (plan_task.instruction TEXT on top of the v8 DDL) must match the \
         frozen v9 DDL exactly, not merely carry the right object names"
    );
    let instruction_column: Vec<_> = table_columns(&conn, "plan_task")
        .into_iter()
        .filter(|(_, name, ..)| name == "instruction")
        .map(|(_, _, kind, notnull, default, pk)| (kind, notnull, default, pk))
        .collect();
    assert_eq!(
        instruction_column,
        vec![("TEXT".to_string(), 0, None, 0)],
        "plan_task.instruction must be a single nullable TEXT column with no default"
    );
}

/// Migration 009 is additive -- applied on top of a v8 database it lands on the same
/// schema as the whole series applied at once.
#[test]
fn apply_migration_009_alone_reaches_the_same_schema_as_a_fresh_database() {
    assert!(
        store::MIGRATIONS.len() >= 9,
        "this contract is meaningless until migration 009 exists: with only eight migrations \
         shipped, applying the first eight and applying all of them are the same no-op, which \
         would make the equality assertions below vacuous"
    );

    let (fresh_version, fresh_schema) =
        apply_and_read("store-instruction-fresh", &[store::MIGRATIONS]);
    let (incremental_version, incremental_schema) = apply_and_read(
        "store-instruction-incremental",
        &[&store::MIGRATIONS[..8], store::MIGRATIONS],
    );
    assert_eq!(
        incremental_version, fresh_version,
        "applying migration 009 on top of a v8 database must reach the same version as a \
         fresh database migrated in one go"
    );
    assert_eq!(
        fresh_schema, incremental_schema,
        "a database migrated straight to v9 and one upgraded from v8 by migration 009 alone \
         must land on an identical schema"
    );
}

/// `(instruction, title, window_id, report_outcome, report_summary, report_path, reported_at)`.
type LegacyTaskRow = (
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// A plan task row written by a v8 database keeps every value it had once migration
/// 009 runs, and its new `instruction` reads as NULL.
#[test]
fn a_task_row_written_at_v8_reads_a_null_instruction_after_migration_009() {
    let db = temp_store_db("store-instruction-legacy-row");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..8])
            .expect("the first eight migrations must apply");
        conn.execute_batch(
            "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
             VALUES ('p-legacy', 'legacy', 'C:/work', NULL, 1000, 1000); \
             INSERT INTO plan_task (plan_id, task_id, seq, title, window_id, report_outcome, \
             report_summary, report_path, reported_at) \
             VALUES ('p-legacy', 't1', 0, 'old task', 'w-old', 'failed', 'old summary', \
             'C:/out/r.txt', 2000);",
        )
        .expect("seeding a v8 task row must succeed");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("the rest of the shipped series must apply on top of v8");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let row: LegacyTaskRow = conn
        .query_row(
            "SELECT instruction, title, window_id, report_outcome, report_summary, report_path, \
             reported_at FROM plan_task WHERE plan_id = 'p-legacy' AND task_id = 't1'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .expect("the v8 task row must be readable with its instruction column after migration");
    assert_eq!(
        row,
        (
            None,
            "old task".to_string(),
            Some("w-old".to_string()),
            Some("failed".to_string()),
            Some("old summary".to_string()),
            Some("C:/out/r.txt".to_string()),
            Some(2000),
        ),
        "migration 009 must leave a v8 row's values intact and its instruction NULL"
    );
}

/// Schema v10: migration 010 (`010_instruction_notice.sql`) adds two nullable columns to
/// `plan_task` through two `ALTER TABLE ... ADD COLUMN` statements --
/// `instruction_notice_state` (with the `IN ('pending', 'delivered', 'window_closed')`
/// CHECK) and `instruction_notice_at` -- folded into `sqlite_master.sql` after
/// `instruction` and before the trailing `PRIMARY KEY` clause (the same fold rule
/// `expected_schema_v8`'s doc comment records, confirmed here against a live SQLite
/// connection through `rusqlite` 0.40.2 / `libsqlite3-sys` 0.38.2, this workspace's pinned
/// version, rather than guessed: applying the two `ADD COLUMN` statements to a v9 database
/// and reading `sqlite_master.sql` back lands both new columns, in declaration order,
/// immediately before `PRIMARY KEY (plan_id, task_id)`). Every other object is unchanged
/// from v9.
fn expected_schema_v10() -> Vec<(String, String, String)> {
    const V9_PLAN_TASK_TAIL: &str = "instruction TEXT, PRIMARY KEY (plan_id, task_id) )";
    const V10_PLAN_TASK_TAIL: &str = "instruction TEXT, instruction_notice_state TEXT CHECK \
         (instruction_notice_state IN ('pending', 'delivered', 'window_closed')), \
         instruction_notice_at INTEGER, PRIMARY KEY (plan_id, task_id) )";
    expected_schema_v9()
        .into_iter()
        .map(|(kind, name, sql)| {
            if name == "plan_task" {
                assert!(
                    sql.ends_with(V9_PLAN_TASK_TAIL),
                    "the v9 plan_task DDL must end with the tail the v10 fold replaces"
                );
                let sql = sql.replace(V9_PLAN_TASK_TAIL, V10_PLAN_TASK_TAIL);
                (kind, name, sql)
            } else {
                (kind, name, sql)
            }
        })
        .collect()
}

/// Applying the first ten migrations reaches `user_version` 10 with
/// `plan_task.instruction_notice_state` and `plan_task.instruction_notice_at` as nullable
/// columns, the state column CHECK-constrained to exactly the three wire values, and
/// nothing else changed -- pinned to `&store::MIGRATIONS[..10]` so later migrations do
/// not move what v10 itself looks like.
#[test]
fn fresh_open_store_reaches_schema_v10_with_nullable_instruction_notice_columns() {
    let (_db, conn) = migrated_readonly("store-fresh-open-v10-ddl", &[&store::MIGRATIONS[..10]]);
    assert_eq!(
        user_version(&conn),
        10,
        "applying the first ten migrations must set user_version to 10"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v10(),
        "a fresh database's full schema must match the v9 DDL plus plan_task's two \
         instruction-notice columns"
    );
    let columns: Vec<_> = table_columns(&conn, "plan_task")
        .into_iter()
        .filter(|(_, name, ..)| {
            name == "instruction_notice_state" || name == "instruction_notice_at"
        })
        .map(|(_, name, kind, notnull, default, pk)| (name, kind, notnull, default, pk))
        .collect();
    assert_eq!(
        columns,
        vec![
            (
                "instruction_notice_state".to_string(),
                "TEXT".to_string(),
                0,
                None,
                0
            ),
            (
                "instruction_notice_at".to_string(),
                "INTEGER".to_string(),
                0,
                None,
                0
            ),
        ],
        "both instruction-notice columns must be nullable with no default"
    );
}

/// Migration 010 is additive -- applied on top of a v9 database it lands on the
/// same schema as the whole series applied at once.
#[test]
fn apply_migration_010_alone_reaches_the_same_schema_as_a_fresh_database() {
    assert!(
        store::MIGRATIONS.len() >= 10,
        "this contract is meaningless until migration 010 exists: with only nine migrations \
         shipped, applying the first nine and applying all of them are the same no-op, which \
         would make the equality assertions below vacuous"
    );

    let (fresh_version, fresh_schema) =
        apply_and_read("store-instruction-notice-fresh", &[store::MIGRATIONS]);
    let (incremental_version, incremental_schema) = apply_and_read(
        "store-instruction-notice-incremental",
        &[&store::MIGRATIONS[..9], store::MIGRATIONS],
    );
    assert_eq!(
        incremental_version, fresh_version,
        "applying migration 010 on top of a v9 database must reach the same version as a \
         fresh database migrated in one go"
    );
    assert_eq!(
        fresh_schema, incremental_schema,
        "a database migrated straight to v10 and one upgraded from v9 by migration 010 alone \
         must land on an identical schema"
    );
}

/// `(window_id, title, instruction, instruction_notice_state, instruction_notice_at,
/// report_outcome)`, mirroring `LegacyTaskRow`'s purpose for the v10 legacy-row contract
/// below: a named alias rather than an inline 6-element tuple type, so `clippy::type_complexity`
/// (`-D warnings`) has nothing to flag, the same reason `LegacyTaskRow` already exists for
/// the v9 case.
type LegacyTaskRowV10 = (
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

/// A plan task row written by a v9 database keeps every value it had once migration
/// 010 runs, and both new columns read as NULL.
#[test]
fn a_task_row_written_at_v9_reads_null_instruction_notice_columns_after_migration_010() {
    let db = temp_store_db("store-instruction-notice-legacy-row");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..9])
            .expect("the first nine migrations must apply");
        conn.execute_batch(
            "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
             VALUES ('p-legacy-v9', 'legacy', 'C:/work', NULL, 1000, 1000); \
             INSERT INTO plan_task (plan_id, task_id, seq, title, window_id, instruction) \
             VALUES ('p-legacy-v9', 't1', 0, 'old task', 'w-old', 'do the old thing');",
        )
        .expect("seeding a v9 task row must succeed");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("the rest of the shipped series must apply on top of v9");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let row: LegacyTaskRowV10 = conn
        .query_row(
            "SELECT window_id, title, instruction, instruction_notice_state, instruction_notice_at, \
             report_outcome FROM plan_task WHERE plan_id = 'p-legacy-v9' AND task_id = 't1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .expect("the v9 task row must be readable with its instruction-notice columns after migration");
    assert_eq!(
        row,
        (
            Some("w-old".to_string()),
            "old task".to_string(),
            Some("do the old thing".to_string()),
            None,
            None,
            None,
        ),
        "migration 010 must leave a v9 row's values intact and both new columns NULL"
    );
}

/// `plan_task.instruction_notice_state`'s CHECK constraint refuses any value
/// outside `{'pending', 'delivered', 'window_closed'}`, asserted by SQLite's structured
/// error code (`ErrorCode::ConstraintViolation`) rather than by message text, mirroring
/// `report_outcome_check_rejects_a_value_outside_the_allowed_set`.
#[test]
fn instruction_notice_state_check_rejects_a_value_outside_the_allowed_set() {
    let db = temp_store_db("store-instruction-notice-state-check");
    let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
    store::apply_migrations(&mut conn, store::MIGRATIONS)
        .expect("the shipped migrations must apply to a fresh database");

    conn.execute(
        "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
         VALUES ('p-notice-check', 'check fixture', 'C:/work', NULL, 1000, 1000)",
        [],
    )
    .expect("seeding the parent plan row must succeed");

    let refused = conn.execute(
        "INSERT INTO plan_task (plan_id, task_id, seq, title, instruction_notice_state) \
         VALUES ('p-notice-check', 't1', 0, 'do the thing', 'somehow_unknown')",
        [],
    );
    let Err(rusqlite::Error::SqliteFailure(ffi_error, _)) = refused else {
        panic!("expected a SqliteFailure carrying a constraint-violation code, got {refused:?}");
    };
    assert_eq!(
        ffi_error.code,
        rusqlite::ErrorCode::ConstraintViolation,
        "an instruction_notice_state value outside {{'pending', 'delivered', \
         'window_closed'}} must be refused as a CHECK constraint violation, not merely \
         rejected for some other reason"
    );
}

/// Schema v7 on its own, kept meaningful independently of whatever `MIGRATIONS` currently
/// holds beyond index 6 -- `apply_migration_008_alone_reaches_the_same_schema_as_a_fresh_database`
/// below needs a v7 starting point distinct from the full series to prove migration 008 is
/// additive rather than only ever exercised as part of the whole series at once.
#[test]
fn apply_migration_008_alone_reaches_the_same_schema_as_a_fresh_database() {
    assert!(
        store::MIGRATIONS.len() >= 8,
        "this contract is meaningless until migration 008 exists: with only seven migrations \
         shipped, applying the first seven and applying all of them are the same no-op, which \
         would make the equality assertions below vacuous"
    );

    let (fresh_version, fresh_schema) = apply_and_read("store-report-fresh", &[store::MIGRATIONS]);
    let (incremental_version, incremental_schema) = apply_and_read(
        "store-report-incremental",
        &[&store::MIGRATIONS[..7], store::MIGRATIONS],
    );

    assert_eq!(
        fresh_version,
        store::MIGRATIONS.len() as i64,
        "applying the whole shipped series in one go must reach the last version"
    );
    assert_eq!(
        incremental_version, fresh_version,
        "applying migration 008 on top of a v7 database must reach the same version as a \
         fresh database migrated in one go"
    );
    assert_eq!(
        fresh_schema, incremental_schema,
        "a database migrated straight to v8 and one upgraded from v7 by migration 008 alone \
         must land on an identical schema"
    );
}

/// `plan_task.report_outcome`'s CHECK constraint refuses any value outside
/// `{'succeeded', 'failed'}`. The refusal is asserted by SQLite's structured error code
/// (`ErrorCode::ConstraintViolation`, i.e. `SQLITE_CONSTRAINT`) rather than by message text,
/// so this contract cannot be satisfied by any other kind of failure (a missing column, for
/// instance, fails with a plain `SQLITE_ERROR` and would not carry this code).
#[test]
fn report_outcome_check_rejects_a_value_outside_the_allowed_set() {
    let db = temp_store_db("store-report-outcome-check");
    let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
    store::apply_migrations(&mut conn, store::MIGRATIONS)
        .expect("the shipped migrations must apply to a fresh database");

    conn.execute(
        "INSERT INTO plan (plan_id, title, cwd, owner_window, created_at, updated_at) \
         VALUES ('p-check', 'check fixture', 'C:/work', NULL, 1000, 1000)",
        [],
    )
    .expect("seeding the parent plan row must succeed");

    let refused = conn.execute(
        "INSERT INTO plan_task \
         (plan_id, task_id, seq, title, report_outcome, report_summary, report_path, \
          reported_at) \
         VALUES ('p-check', 't1', 0, 'do the thing', 'done', NULL, NULL, 1000)",
        [],
    );
    let Err(rusqlite::Error::SqliteFailure(ffi_error, _)) = refused else {
        panic!("expected a SqliteFailure carrying a constraint-violation code, got {refused:?}");
    };
    assert_eq!(
        ffi_error.code,
        rusqlite::ErrorCode::ConstraintViolation,
        "a report_outcome value outside {{'succeeded', 'failed'}} must be refused as a CHECK \
         constraint violation, not merely rejected for some other reason"
    );
}

/// Schema v6 on its own, kept meaningful independently of whatever `MIGRATIONS` currently
/// holds beyond index 5, the same way the v2 through v5 cases above keep their versions
/// meaningful -- `apply_and_migration_007_alone_reach_the_same_schema_as_a_fresh_database`
/// below needs a v6 starting point distinct from the full series to prove migration 007 is
/// additive rather than only ever exercised as part of the whole series at once.
#[test]
fn apply_and_migration_007_alone_reach_the_same_schema_as_a_fresh_database() {
    assert!(
        store::MIGRATIONS.len() >= 7,
        "this contract is meaningless until migration 007 exists: with only six migrations \
         shipped, applying the first six and applying all of them are the same no-op, which \
         would make the equality assertions below vacuous"
    );

    let (fresh_version, fresh_schema) = apply_and_read("store-plan-fresh", &[store::MIGRATIONS]);
    let (incremental_version, incremental_schema) = apply_and_read(
        "store-plan-incremental",
        &[&store::MIGRATIONS[..6], store::MIGRATIONS],
    );

    assert_eq!(
        fresh_version,
        store::MIGRATIONS.len() as i64,
        "applying the whole shipped series in one go must reach the last version"
    );
    assert_eq!(
        incremental_version, fresh_version,
        "applying migration 007 on top of a v6 database must reach the same version as a \
         fresh database migrated in one go"
    );
    assert_eq!(
        fresh_schema, incremental_schema,
        "a database migrated straight to v7 and one upgraded from v6 by migration 007 alone \
         must land on an identical schema"
    );
}

/// The v4 state on its own -- the schema as it stood before the fifth migration added
/// `chat_delivery.unknown_reason` -- kept meaningful independently of whatever `MIGRATIONS`
/// currently holds beyond index 3, the same way `migrations_through_chat_reach_schema_v3_...`
/// keeps v3 meaningful.
#[test]
fn migrations_through_rooms_reach_schema_v4_with_workspace_index() {
    let db = temp_store_db("store-v4-only");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..4])
            .expect("applying the first four migrations must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        4,
        "applying only the first four migrations must stop at schema version 4"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v4(),
        "schema v4 is still exactly the frozen six-object DDL; the chat_delivery reason \
         column only arrives with the fifth migration"
    );
}

/// The v3 state on its own -- the three chat tables in the shape they had before the
/// fourth migration added the workspace columns and index -- kept meaningful independently
/// of whatever `MIGRATIONS` currently holds beyond index 2, the same way
/// `migrations_through_layout_reach_schema_v2_with_two_tables` keeps v2 meaningful.
#[test]
fn migrations_through_chat_reach_schema_v3_with_five_tables() {
    let db = temp_store_db("store-v3-only");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..3])
            .expect("applying the first three migrations must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        3,
        "applying only the first three migrations must stop at schema version 3"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v3(),
        "schema v3 is still exactly the frozen five-table DDL; the workspace columns and \
         index only arrive with the fourth migration"
    );
}

/// The v2 state on its own -- the session-store tables only, before the chat migration
/// runs -- kept meaningful independently of whatever `MIGRATIONS` currently holds beyond
/// index 1.
#[test]
fn migrations_through_layout_reach_schema_v2_with_two_tables() {
    let db = temp_store_db("store-v2-only");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..2])
            .expect("applying the first two migrations must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        2,
        "applying only the first two migrations must stop at schema version 2"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v2(),
        "schema v2 is still exactly the frozen two-table DDL; the chat tables only arrive \
         with the third migration"
    );
}

/// A dummy migration series (never the production one), so this contract
/// stays meaningful at schema versions above 1 without depending on what
/// `MIGRATIONS` currently holds.
const DUMMY_MIGRATIONS: &[&str] = &[
    "CREATE TABLE dummy_first (id INTEGER PRIMARY KEY, label TEXT NOT NULL);",
    "CREATE TABLE dummy_second (id INTEGER PRIMARY KEY, amount INTEGER);",
];

/// Same series with one deliberate column-type difference in version 2. Used
/// as the control: the schema comparison must be able to tell it apart from
/// [`DUMMY_MIGRATIONS`], or the "fresh equals incremental" assertion above
/// would be vacuous.
const DUMMY_MIGRATIONS_DIVERGENT: &[&str] = &[
    "CREATE TABLE dummy_first (id INTEGER PRIMARY KEY, label TEXT NOT NULL);",
    "CREATE TABLE dummy_second (id INTEGER PRIMARY KEY, amount TEXT);",
];

/// Applies `series` to a brand-new database at `tag` and returns its
/// `(user_version, schema)` as read back through `open_readonly`.
fn apply_and_read(tag: &str, steps: &[&[&'static str]]) -> (i64, Vec<(String, String, String)>) {
    let db = temp_store_db(tag);
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        for series in steps {
            store::apply_migrations(&mut conn, series).expect("apply_migrations must succeed");
        }
    }
    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    (user_version(&conn), read_schema(&conn))
}

#[test]
fn apply_migrations_fresh_and_incremental_reach_the_same_schema() {
    let (fresh_version, fresh_schema) = apply_and_read("store-migrate-fresh", &[DUMMY_MIGRATIONS]);
    let (incremental_version, incremental_schema) = apply_and_read(
        "store-migrate-incremental",
        &[&DUMMY_MIGRATIONS[..1], DUMMY_MIGRATIONS],
    );

    assert_eq!(
        fresh_version, 2,
        "applying a two-version series must set user_version to 2"
    );
    assert_eq!(
        incremental_version, 2,
        "re-applying the full series over version 1 must advance to version 2 exactly once"
    );
    assert_eq!(
        fresh_schema, incremental_schema,
        "a database migrated in one go and one migrated version by version must be identical"
    );

    // Control: the comparison must actually be able to fail.
    let (_, divergent_schema) =
        apply_and_read("store-migrate-divergent", &[DUMMY_MIGRATIONS_DIVERGENT]);
    assert_ne!(
        fresh_schema, divergent_schema,
        "a deliberate column-type difference must be detected by the schema comparison, \
         otherwise the equality assertion above proves nothing"
    );
}

/// The v5 state on its own -- the chat delivery table with its reason column but before
/// any constraint tied the reason to the state -- kept meaningful independently of whatever
/// `MIGRATIONS` currently holds beyond index 4, the same way the v2 / v3 / v4 cases above
/// keep their versions meaningful.
#[test]
fn migrations_through_the_delivery_reason_column_reach_schema_v5() {
    let db = temp_store_db("store-v5-only");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..5])
            .expect("applying the first five migrations must succeed");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        user_version(&conn),
        5,
        "applying only the first five migrations must stop at schema version 5"
    );
    assert_eq!(
        read_schema(&conn),
        expected_schema_v5(),
        "schema v5 is still exactly the frozen six-object DDL with a plain, unconstrained \
         reason column"
    );
}

/// The columns of one table, as SQLite itself reports them: `(cid, name, type, notnull,
/// dflt_value, pk)`. Used to compare two live databases against each other, never against a
/// DDL string written by hand: a migration that rebuilds a table must land on the same
/// columns, and the way to know that is to read both.
fn table_columns(
    conn: &rusqlite::Connection,
    table: &str,
) -> Vec<(i64, String, String, i64, Option<String>, i64)> {
    let mut stmt = conn
        .prepare("SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")
        .expect("the column query must prepare");
    let rows = stmt
        .query_map(rusqlite::params![table], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("the column query must run");
    rows.map(|row| row.expect("a column row must decode"))
        .collect()
}

/// Applies `series` to a brand-new database at `tag` and hands the caller a read-only
/// connection to it, with the seeding connection already closed.
fn migrated_readonly(
    tag: &str,
    steps: &[&[&'static str]],
) -> (common::store_fixtures::TempStoreDb, rusqlite::Connection) {
    let db = temp_store_db(tag);
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        for series in steps {
            store::apply_migrations(&mut conn, series).expect("apply_migrations must succeed");
        }
    }
    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    (db, conn)
}

/// What a migration that rebuilds `chat_delivery` must not disturb: a database built by
/// running the whole shipped series at once and one upgraded onto it from the version
/// before must be indistinguishable, and the rebuilt table must keep the columns it had.
/// Both sides of every comparison are read from a live database; nothing here repeats a
/// DDL string from a design document.
#[test]
fn rebuilding_the_delivery_table_leaves_the_schema_and_its_columns_as_they_were() {
    let (_fresh_db, fresh) = migrated_readonly("store-rebuild-fresh", &[store::MIGRATIONS]);
    let (_upgraded_db, upgraded) = migrated_readonly(
        "store-rebuild-upgraded",
        &[&store::MIGRATIONS[..5], store::MIGRATIONS],
    );
    let (_previous_db, previous) =
        migrated_readonly("store-rebuild-previous", &[&store::MIGRATIONS[..5]]);

    assert_eq!(
        user_version(&fresh),
        user_version(&upgraded),
        "both paths must end at the same schema version"
    );
    assert_eq!(
        read_schema(&fresh),
        read_schema(&upgraded),
        "a database built in one go and one upgraded from the previous version must land on \
         the identical schema"
    );
    assert_eq!(
        table_columns(&previous, "chat_delivery"),
        table_columns(&fresh, "chat_delivery"),
        "rebuilding the delivery table must carry every column across unchanged -- same \
         names, same order, same types, same nullability, same primary key"
    );
}

/// A reason may only sit next to the one state that means "not delivered because the
/// destination's status was unknown". The database itself refuses any other pairing, and the
/// control shows the same write going through on the version before the constraint existed,
/// so the refusal is the constraint's doing and not the fixture's.
#[test]
fn a_reason_next_to_any_other_delivery_state_is_refused_by_the_shipped_schema() {
    const CONTRADICTORY: &str = "INSERT INTO chat_delivery \
         (room_id, seq, window_id, state, at_ms, unknown_reason) \
         VALUES ('main', 1, 'w-a', 'confirmed', 1000, 'screen_lost')";

    let control_db = temp_store_db("store-delivery-check-control");
    {
        let mut conn =
            rusqlite::Connection::open(&control_db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..5])
            .expect("applying the first five migrations must succeed");
        conn.execute(CONTRADICTORY, []).expect(
            "without the constraint the same write must be accepted, or this case \
                     proves nothing about the constraint",
        );
    }

    let db = temp_store_db("store-delivery-check");
    let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
    store::apply_migrations(&mut conn, store::MIGRATIONS)
        .expect("the shipped migrations must apply to a fresh database");

    let refused = conn.execute(CONTRADICTORY, []);
    assert!(
        refused.is_err(),
        "a reason recorded next to a state other than undelivered_unknown must be refused \
         by the database, but the write was accepted"
    );

    conn.execute(
        "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
         VALUES ('main', 1, 'w-b', 'undelivered_unknown', 1000, 'screen_lost')",
        [],
    )
    .expect("a reason next to undelivered_unknown must still be accepted");
    conn.execute(
        "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
         VALUES ('main', 1, 'w-c', 'confirmed', 1000, NULL)",
        [],
    )
    .expect("a row with no reason must be accepted whatever its state");
    conn.execute(
        "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
         VALUES ('main', 1, 'w-d', 'undelivered_unknown', 1000, NULL)",
        [],
    )
    .expect("undelivered_unknown with no reason must be accepted: the rule is one-way");
}

/// A database written before the constraint existed can hold rows the constraint forbids.
/// Migrating it must still succeed -- a daemon that cannot open its own database cannot
/// start -- and each offending row must lose only its reason, never the row.
#[test]
fn migrating_a_database_whose_reason_contradicts_its_state_keeps_the_row_without_the_reason() {
    let db = temp_store_db("store-delivery-check-repair");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..5])
            .expect("applying the first five migrations must succeed");
        for sql in [
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
             VALUES ('main', 1, 'w-contradicting', 'confirmed', 1000, 'screen_lost')",
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
             VALUES ('main', 1, 'w-conforming', 'undelivered_unknown', 1100, 'no_rule_matched')",
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
             VALUES ('main', 1, 'w-plain', 'pending', 1200, NULL)",
        ] {
            conn.execute(sql, [])
                .expect("seeding a v5-era row must succeed");
        }

        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("a database holding rows the new constraint forbids must still migrate");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let mut stmt = conn
        .prepare(
            "SELECT window_id, state, at_ms, unknown_reason FROM chat_delivery \
             WHERE room_id = 'main' AND seq = 1 ORDER BY window_id",
        )
        .expect("the delivery query must prepare");
    let rows: Vec<(String, String, i64, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("the delivery query must run")
        .map(|row| row.expect("a delivery row must decode"))
        .collect();

    assert_eq!(
        rows,
        vec![
            (
                "w-conforming".to_string(),
                "undelivered_unknown".to_string(),
                1100,
                Some("no_rule_matched".to_string())
            ),
            (
                "w-contradicting".to_string(),
                "confirmed".to_string(),
                1000,
                None
            ),
            ("w-plain".to_string(), "pending".to_string(), 1200, None),
        ],
        "the contradicting row must survive the migration with its state and timestamp \
         intact and only its reason dropped, while the conforming and reason-less rows are \
         carried across untouched"
    );
}

/// The chat delivery reason column the fifth migration adds must be nullable, and every
/// row a database created before the fifth migration already carries must read back with
/// no reason: a migration adds a column, it does not retroactively invent
/// data for rows that predate it.
#[test]
fn migrating_to_v5_leaves_pre_existing_delivery_rows_with_no_reason() {
    let db = temp_store_db("store-migrate-delivery-reason-null");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, &store::MIGRATIONS[..4])
            .expect("applying the first four migrations must reach schema v4");
        conn.execute(
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms) \
             VALUES ('main', 1, 'w-a', 'pending', 1000)",
            [],
        )
        .expect("seeding a v4-era chat_delivery row must succeed");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("applying the remaining migrations must reach the latest schema");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let reason: Option<String> = conn
        .query_row(
            "SELECT unknown_reason FROM chat_delivery WHERE room_id = 'main' AND seq = 1 \
             AND window_id = 'w-a'",
            [],
            |row| row.get(0),
        )
        .expect("the pre-existing chat_delivery row must still be readable after migrating");
    assert_eq!(
        reason, None,
        "a chat_delivery row written before the reason column existed must read back with \
         a NULL reason, not a guessed value"
    );
}

/// The chat migration's own `INSERT`, checked separately from the `CREATE TABLE` schema
/// comparison above: it is data, not schema, and does not appear in `sqlite_master`.
#[test]
fn production_migrations_seed_the_main_chat_room_row() {
    let db = temp_store_db("store-migrate-chat-room-seed");
    {
        let mut conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        store::apply_migrations(&mut conn, store::MIGRATIONS)
            .expect("the production migrations must apply to a fresh database");
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let room_ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT room_id FROM chat_room")
            .expect("the chat_room query must prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("the chat_room query must run")
            .map(|row| row.expect("chat_room row must decode"))
            .collect()
    };
    assert_eq!(
        room_ids,
        vec!["main".to_string()],
        "the chat schema's own INSERT must seed exactly one chat_room row, id 'main'"
    );
}

#[test]
fn a_future_schema_version_is_refused_and_left_untouched() {
    let db = temp_store_db("store-future-version");
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a seed connection must open");
        conn.execute_batch(
            "CREATE TABLE future_marker (id INTEGER PRIMARY KEY, note TEXT); \
             PRAGMA user_version = 99;",
        )
        .expect("seeding a future-version database must succeed");
    }

    let before = {
        let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
        (user_version(&conn), read_schema(&conn))
    };
    assert_eq!(before.0, 99, "the fixture must really be at version 99");

    // Left unbound so the refused `Store` is released before the verification read below,
    // and for the `clippy::drop_non_drop` reason documented on `create_schema`.
    assert!(
        store::open_store(&db.path).is_err(),
        "a database written by a future version of yagura must be refused rather than \
         migrated or written to (fail-safe)"
    );

    let after = {
        let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
        (user_version(&conn), read_schema(&conn))
    };
    assert_eq!(
        before, after,
        "refusing a future-version database must leave its user_version and schema untouched"
    );
}

#[test]
fn writing_a_snapshot_updates_the_single_meta_row() {
    let db = temp_store_db("store-meta-update");
    create_schema(&db.path);

    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    set_meta(&conn, 0, None);
    store::write_snapshot_once(&mut conn, &sample_rows())
        .expect("write_snapshot_once must succeed");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM snapshot_meta", [], |row| row.get(0))
        .expect("the meta count must be readable");
    assert_eq!(
        count, 1,
        "snapshot_meta must hold exactly one row (id = 1 is CHECK-constrained)"
    );
    let (taken_at, _) = read_meta(&conn);
    assert_ne!(
        taken_at, 0,
        "writing a snapshot must stamp taken_at_epoch_s, replacing the seeded 0"
    );
}

/// The default instance's namespace, for the precedence cases below that are
/// not about instance naming at all.
fn default_namespace() -> yagura_os::InstanceNamespace {
    yagura_os::resolve_instance_from(None).expect("the default instance always resolves")
}

#[test]
fn resolve_db_path_follows_the_documented_precedence() {
    let explicit = PathBuf::from("explicit-store.db");
    let env_value = OsString::from("env-store.db");
    let local_app_data = OsString::from("local-app-data-root");
    let namespace = default_namespace();

    assert_eq!(
        store::resolve_db_path(
            Some(explicit.clone()),
            Some(env_value.clone()),
            Some(local_app_data.clone()),
            &namespace,
        ),
        Some(explicit.clone()),
        "an explicit path wins over both the environment and LOCALAPPDATA"
    );
    assert_eq!(
        store::resolve_db_path(
            None,
            Some(env_value.clone()),
            Some(local_app_data.clone()),
            &namespace,
        ),
        Some(PathBuf::from(&env_value)),
        "YAGURA_STORE_DB wins over the LOCALAPPDATA default"
    );
    assert_eq!(
        store::resolve_db_path(None, None, Some(local_app_data.clone()), &namespace),
        Some(
            yagura_os::instance_dir(&PathBuf::from(&local_app_data), &namespace).join("yagura.db")
        ),
        "with neither override, the default is yagura.db in the instance directory"
    );
    assert_eq!(
        store::resolve_db_path(None, None, None, &namespace),
        None,
        "with no path at all, persistence degrades to disabled rather than guessing one"
    );
}

// The empty-current no-op is what keeps a failed startup from destroying the previous
// session.

#[test]
fn rotate_slots_moves_current_into_prev_and_stamps_prev_taken_at() {
    let db = temp_store_db("store-rotate-nonempty");
    create_schema(&db.path);

    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    insert_window_row(&conn, store::SLOT_PREV, 0, "stale-prev");
    insert_window_row(&conn, store::SLOT_CURRENT, 0, "current-a");
    insert_window_row(&conn, store::SLOT_CURRENT, 1, "current-b");
    set_meta(&conn, 111, None);

    store::rotate_slots(&mut conn).expect("rotate_slots must succeed");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        slot_rows(&conn, store::SLOT_PREV),
        vec![(0, "current-a".to_string()), (1, "current-b".to_string())],
        "prev must be replaced wholesale by what current held (the stale prev row is gone)"
    );
    assert!(
        slot_rows(&conn, store::SLOT_CURRENT).is_empty(),
        "current must be empty after rotation: its rows moved to prev, they were not copied"
    );
    let (taken_at, prev_taken_at) = read_meta(&conn);
    assert_eq!(
        prev_taken_at,
        Some(111),
        "prev_taken_at_epoch_s must take over the timestamp current was carrying"
    );
    assert_eq!(
        taken_at, 111,
        "taken_at_epoch_s stays NOT NULL and keeps its old value until the next write \
         (it is documented as meaningless in this window, not nulled)"
    );
}

#[test]
fn rotate_slots_is_a_no_op_when_current_is_empty() {
    let db = temp_store_db("store-rotate-empty");
    create_schema(&db.path);

    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    insert_window_row(&conn, store::SLOT_PREV, 0, "keep-me-a");
    insert_window_row(&conn, store::SLOT_PREV, 1, "keep-me-b");
    set_meta(&conn, 222, Some(111));

    store::rotate_slots(&mut conn).expect("rotate_slots must succeed on an empty current");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert_eq!(
        slot_rows(&conn, store::SLOT_PREV),
        vec![(0, "keep-me-a".to_string()), (1, "keep-me-b".to_string())],
        "an empty current must not wipe prev: a start-up that never wrote a snapshot would \
         otherwise destroy the last recoverable session"
    );
    assert_eq!(
        read_meta(&conn),
        (222, Some(111)),
        "the empty-current no-op must leave prev_taken_at_epoch_s untouched as well"
    );
}

#[test]
fn writing_after_rotation_leaves_prev_taken_at_untouched() {
    let db = temp_store_db("store-rotate-then-write");
    create_schema(&db.path);

    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    insert_window_row(&conn, store::SLOT_CURRENT, 0, "before-rotation");
    set_meta(&conn, 111, None);
    store::rotate_slots(&mut conn).expect("rotate_slots must succeed");
    store::write_snapshot_once(&mut conn, &sample_rows())
        .expect("write_snapshot_once must succeed after rotation");
    drop(conn);

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (taken_at, prev_taken_at) = read_meta(&conn);
    assert_eq!(
        prev_taken_at,
        Some(111),
        "the meta update must be an UPSERT that touches taken_at_epoch_s only; a REPLACE / \
         INSERT OR REPLACE deletes the row first and silently nulls prev_taken_at_epoch_s"
    );
    assert_ne!(
        taken_at, 111,
        "the write must still refresh taken_at_epoch_s"
    );
    assert_eq!(
        slot_rows(&conn, store::SLOT_PREV),
        vec![(0, "before-rotation".to_string())],
        "the rotated prev rows must survive the following write"
    );
}

/// The size boundary these tests assert against, kept as a local literal so a change to
/// `store::MAX_COMMAND_JSON_BYTES` has to be mirrored here deliberately.
const MAX_COMMAND_JSON_BYTES_EXPECTED: usize = 1_048_576;

/// Builds a syntactically valid JSON array of one string whose encoded byte length is
/// exactly `total_len`; no escaping is needed since the padding is plain ASCII.
fn command_json_of_exact_length(total_len: usize) -> String {
    const OVERHEAD: usize = 4; // the two brackets and two quotes
    let padding_len = total_len
        .checked_sub(OVERHEAD)
        .expect("total_len must be at least 4 to hold an empty JSON string");
    format!("[\"{}\"]", "x".repeat(padding_len))
}

// A storage-class corrupt row (a BLOB written into a TEXT-affinity column) must not fail
// the whole read: only that row is skipped, and the call still warns exactly once, the
// same fail-safe contract JSON-content corruption already has.

#[test]
fn a_storage_class_corrupt_row_in_the_current_slot_is_skipped_not_fatal() {
    let db = temp_store_db("store-blob-current");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "ok", "[\"cmd.exe\"]");
        insert_window_row_with_blob_command(
            &conn,
            store::SLOT_CURRENT,
            1,
            "blob-corrupt",
            &[0xff, 0xfe, 0x00, 0x01],
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, result) = count_warns(|| store::read_snapshot_conn(&conn));
    let rows = result.expect(
        "a storage-class corrupt row (e.g. a BLOB written into command_json) must not fail \
         the whole read; it must be skipped like any other corrupt row",
    );

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0],
        "only the well-formed row must survive; the BLOB row must be skipped, not returned"
    );
    assert_eq!(
        warns, 1,
        "a storage-class corrupt row must warn exactly once, the same as a JSON-content \
         corrupt row (contract (6b))"
    );
}

#[test]
fn a_storage_class_corrupt_row_in_the_prev_slot_is_skipped_not_fatal() {
    let db = temp_store_db("store-blob-prev");
    create_schema(&db.path);

    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_PREV, 0, "ok", "[\"pwsh.exe\"]");
        insert_window_row_with_blob_command(
            &conn,
            store::SLOT_PREV,
            1,
            "blob-corrupt",
            &[0xde, 0xad, 0xbe, 0xef],
        );
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, result) = count_warns(|| store::read_prev_snapshot_conn(&conn));
    let rows = result.expect(
        "read_prev_snapshot_conn must degrade the same way read_snapshot_conn does: a \
         storage-class corrupt row is skipped, not fatal to the whole call",
    );

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0],
        "only the well-formed prev row must survive; the BLOB row must be skipped"
    );
    assert_eq!(
        warns, 1,
        "a storage-class corrupt row must warn exactly once"
    );
}

// A `command_json` value's own byte length is bounded: at or below the boundary the row
// is a normal restore candidate, above it the row is skipped like any other corrupt row.

#[test]
fn a_command_json_row_over_the_byte_limit_is_skipped_and_warns_once() {
    let db = temp_store_db("store-oversized-json");
    create_schema(&db.path);

    let oversized = command_json_of_exact_length(MAX_COMMAND_JSON_BYTES_EXPECTED + 1);
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "ok", "[\"cmd.exe\"]");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 1, "oversized", &oversized);
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (warns, rows) = count_warns(|| {
        store::read_snapshot_conn(&conn)
            .expect("an oversized command_json row must degrade, not fail the whole read")
    });

    let positions: Vec<i64> = rows.iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0],
        "a command_json value exceeding {MAX_COMMAND_JSON_BYTES_EXPECTED} bytes must be \
         skipped and excluded from the restore candidates, even though it is syntactically \
         valid JSON; found rows: {rows:?}",
    );
    assert_eq!(
        warns, 1,
        "skipping an oversized row must warn exactly once, the same as any other corrupt row"
    );
}

#[test]
fn a_command_json_row_at_exactly_the_byte_limit_is_still_returned() {
    let db = temp_store_db("store-boundary-json");
    create_schema(&db.path);

    let at_limit = command_json_of_exact_length(MAX_COMMAND_JSON_BYTES_EXPECTED);
    assert_eq!(
        at_limit.len(),
        MAX_COMMAND_JSON_BYTES_EXPECTED,
        "the fixture itself must be exactly at the boundary for this contract to mean anything"
    );
    {
        let conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
        insert_window_row_with_command_json(&conn, store::SLOT_CURRENT, 0, "at-limit", &at_limit);
    }

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let rows = store::read_snapshot_conn(&conn)
        .expect("a well-formed row at exactly the byte limit must read back normally");

    assert_eq!(
        rows.len(),
        1,
        "a command_json value of exactly {MAX_COMMAND_JSON_BYTES_EXPECTED} bytes must remain a \
         restore candidate: the limit rejects rows that exceed it, not rows that meet it \
         (boundary regression guard against an off-by-one `>=` rewrite)",
    );
}

#[test]
fn opening_a_store_creates_its_missing_parent_directories() {
    let db = temp_store_db("store-missing-parents");
    let nested = db
        .path
        .parent()
        .expect("the fixture database has a parent directory")
        .join("first-run")
        .join("data")
        .join("yagura.db");

    store::open_store(&nested).expect("open_store must create the missing parent directories");

    assert!(
        nested.is_file(),
        "the database file must exist at {nested:?}"
    );
}

// A connection that does not wait on a lock fails at once when another
// connection (a second daemon, the snapshot writer) is writing the same file.
#[test]
fn every_store_connection_waits_on_a_locked_database_instead_of_failing_at_once() {
    let db = temp_store_db("store-busy-timeout");
    let busy_timeout = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy_timeout must be readable")
    };

    let read_write = store::open_migrated(&db.path).expect("open_migrated must succeed");
    assert!(busy_timeout(&read_write) > 0);
    drop(read_write);

    let read_only = store::open_readonly(&db.path).expect("open_readonly must succeed");
    assert!(busy_timeout(&read_only) > 0);
}

#[test]
fn writing_a_snapshot_stamps_the_current_wall_clock_time() {
    let db = temp_store_db("store-meta-wall-clock");
    create_schema(&db.path);
    let epoch_secs = || -> i64 {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch");
        i64::try_from(elapsed.as_secs()).expect("seconds fit in i64")
    };

    let before = epoch_secs();
    let mut conn = rusqlite::Connection::open(&db.path).expect("a write connection must open");
    store::write_snapshot_once(&mut conn, &sample_rows())
        .expect("write_snapshot_once must succeed");
    drop(conn);
    let after = epoch_secs();

    let conn = store::open_readonly(&db.path).expect("open_readonly must succeed");
    let (taken_at, _) = read_meta(&conn);
    assert!(
        (before..=after).contains(&taken_at),
        "taken_at_epoch_s must be the time of the write: {taken_at} not in {before}..={after}"
    );
}
