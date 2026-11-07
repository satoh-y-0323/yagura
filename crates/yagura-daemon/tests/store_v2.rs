#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! The production migration series lands on one schema whether it is applied in one go
//! or over an older database. Nothing here spawns a PTY, so the file is OS-independent.

use crate::common;

use common::store_fixtures::temp_store_db;
use yagura_daemon::store;

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
