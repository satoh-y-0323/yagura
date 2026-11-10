// Seeds and reads the database with raw SQL, which only tests may do.
#![allow(clippy::disallowed_methods)]

//! Fixtures shared by the store tests.
//!
//! Also carries the WARN-capturing subscribers ([`WarnCounter`] / [`count_warns`] for a
//! bare count, [`capture_warns`] for wording, [`capture_warn_events`] for wording plus
//! fields), which several of the store test binaries need in the same axis-independent
//! form.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use yagura_test_support::paths::TempDir;

/// A temporary directory plus the path of a database file inside it.
///
/// The directory is removed when this value is dropped, so tests must hold it for as long
/// as they use [`TempStoreDb::path`]. The file itself is *not* created: the path is
/// handed to `open_store`, which creates it, or turned into a directory on purpose for
/// the init-failure injection.
pub struct TempStoreDb {
    /// Owns the temporary tree; only meaningful for its `Drop`.
    _tree: TempDir,
    /// Path of the database file, inside a directory that already exists.
    pub path: PathBuf,
}

/// Creates a uniquely named temporary directory and returns the path of a `yagura.db`
/// inside it.
///
/// Never touches the real per-user data directory: the store tests must not read or write
/// the user's actual database.
pub fn temp_store_db(tag: &str) -> TempStoreDb {
    let tree = TempDir::new("yagura-cli-test", tag);
    let dir = tree.make_dir("store");
    let path = dir.join("yagura.db");
    TempStoreDb { _tree: tree, path }
}

/// Seeds (or updates) the single `snapshot_meta` row.
pub fn set_meta(conn: &rusqlite::Connection, taken_at: i64, prev_taken_at: Option<i64>) {
    conn.execute(
        "INSERT INTO snapshot_meta (id, taken_at_epoch_s, prev_taken_at_epoch_s) \
         VALUES (1, ?1, ?2) \
         ON CONFLICT(id) DO UPDATE SET taken_at_epoch_s = excluded.taken_at_epoch_s, \
         prev_taken_at_epoch_s = excluded.prev_taken_at_epoch_s",
        rusqlite::params![taken_at, prev_taken_at],
    )
    .expect("seeding the snapshot_meta row must succeed");
}

/// A `tracing` subscriber that does nothing except count WARN-level events.
pub struct WarnCounter {
    count: Arc<AtomicUsize>,
}

impl tracing::Subscriber for WarnCounter {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Runs `body` with a fresh WARN-counting subscriber installed as the
/// thread-local default, returning `(warn count, body's value)`.
pub fn count_warns<T>(body: impl FnOnce() -> T) -> (usize, T) {
    super::ensure_tracing_default_installed();
    let count = Arc::new(AtomicUsize::new(0));
    let subscriber = WarnCounter {
        count: Arc::clone(&count),
    };
    let value = tracing::subscriber::with_default(subscriber, body);
    (count.load(Ordering::SeqCst), value)
}

/// The name `tracing` gives the formatted text of an event's message.
const MESSAGE_FIELD: &str = "message";

/// One captured WARN-level event: the rendered text of its message plus every other
/// field the call site attached, by field name.
///
/// Field values are kept as text so one map can hold values of any `tracing` value type.
/// What the contracts need is which names a call site carries and what they render as,
/// not the Rust types they had before being recorded.
pub struct WarnEvent {
    /// The rendered `message` text: what a human-readable log line would show.
    pub message: String,
    /// Every field other than `message`, by name.
    pub fields: BTreeMap<String, String>,
}

/// Collects one event's message text and its remaining fields.
///
/// `record_debug` renders the message because `tracing::warn!` records its formatted
/// message through `Value::record_debug`, and `std::fmt::Arguments`'s `Debug` forwards to
/// `Display`, so the captured text is exactly what a log line would show. The typed
/// `record_*` methods are implemented as well so a string field arrives without the
/// quoting `Debug` would add and an integer field arrives as its digits; without them
/// `tracing`'s default `Visit` methods would route those values through `record_debug`.
struct WarnEventVisitor<'a> {
    message: &'a mut Option<String>,
    fields: &'a mut BTreeMap<String, String>,
}

impl WarnEventVisitor<'_> {
    fn put(&mut self, field: &tracing::field::Field, value: String) {
        if field.name() == MESSAGE_FIELD {
            *self.message = Some(value);
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl tracing::field::Visit for WarnEventVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put(field, value.to_string());
    }
}

/// A `tracing` subscriber that collects every WARN-level event carrying a message, in
/// emission order.
struct WarnEventCollector {
    events: Arc<Mutex<Vec<WarnEvent>>>,
}

impl tracing::Subscriber for WarnEventCollector {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut message = None;
        let mut fields = BTreeMap::new();
        event.record(&mut WarnEventVisitor {
            message: &mut message,
            fields: &mut fields,
        });
        if let Some(message) = message {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(WarnEvent { message, fields });
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Runs `body` with a fresh event-collecting WARN subscriber installed as the
/// thread-local default, returning `(the WARN events in emission order, body's value)`.
///
/// This is the widest of the three WARN helpers: it is for contracts that must tell two
/// warnings apart by the fields they carry, not only by their wording. Events carrying no
/// message field at all are not collected, which is what [`capture_warns`] -- implemented
/// on top of this -- has always done.
pub fn capture_warn_events<T>(body: impl FnOnce() -> T) -> (Vec<WarnEvent>, T) {
    super::ensure_tracing_default_installed();
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = WarnEventCollector {
        events: Arc::clone(&events),
    };
    let value = tracing::subscriber::with_default(subscriber, body);
    let collected = std::mem::take(
        &mut *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    (collected, value)
}

/// Runs `body` with a fresh WARN capture installed as the thread-local default,
/// returning `(warn message texts in emission order, body's value)`. Kept alongside
/// [`count_warns`] rather than replacing it: most contracts need only a count, and this
/// is for the ones that must tell two warnings apart by wording.
pub fn capture_warns<T>(body: impl FnOnce() -> T) -> (Vec<String>, T) {
    let (events, value) = capture_warn_events(body);
    (
        events.into_iter().map(|event| event.message).collect(),
        value,
    )
}

/// Inserts one `snapshot_windows` row with an explicit, arbitrary `layout_json` storage
/// value -- a `BLOB`, an oversized `TEXT`, an `Integer`/`Real`, or `NULL` -- so a single
/// seeding function covers every storage class the read side must degrade.
pub fn insert_window_row_with_layout_value(
    conn: &rusqlite::Connection,
    slot: i64,
    position: i64,
    name: &str,
    layout_json: rusqlite::types::Value,
) {
    conn.execute(
        "INSERT INTO snapshot_windows \
         (slot, position, command_json, cwd, name, vendor, detection, layout_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            slot,
            position,
            "[\"cmd.exe\"]",
            "C:/work",
            name,
            None::<String>,
            None::<String>,
            layout_json
        ],
    )
    .expect("seeding a snapshot_windows row with an explicit layout_json value must succeed");
}

/// One row of the window table, as the values a window was opened with.
#[derive(Debug, Clone)]
pub struct TableRow {
    pub name: String,
    pub command: Vec<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The rows of the window table at `db_path`, in the order the windows opened; `None`
/// while the table cannot be read.
pub fn window_rows(db_path: &std::path::Path) -> Option<Vec<TableRow>> {
    let conn = yagura_daemon::store::open_readonly(db_path).ok()?;
    let mut statement = conn
        .prepare(
            "SELECT name, command_json, profile, model, effort FROM windows ORDER BY opened_seq",
        )
        .ok()?;
    let rows = statement
        .query_map([], |row| {
            let command_json: String = row.get(1)?;
            Ok(TableRow {
                name: row.get(0)?,
                command: serde_json::from_str(&command_json).unwrap_or_default(),
                profile: row.get(2)?,
                model: row.get(3)?,
                effort: row.get(4)?,
            })
        })
        .ok()?;
    rows.collect::<Result<_, _>>().ok()
}

/// Whether any value of any table in the session store at `db` holds `needle`; which
/// table keeps it is not named. `false` while the store cannot be read.
pub fn store_holds(db: &std::path::Path, needle: &str) -> bool {
    let Ok(conn) = yagura_daemon::store::open_readonly(db) else {
        return false;
    };
    let Ok(mut tables) = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'") else {
        return false;
    };
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    names.iter().any(|table| {
        let Ok(mut rows) = conn.prepare(&format!("SELECT * FROM \"{table}\"")) else {
            return false;
        };
        let columns = rows.column_count();
        let Ok(mut rows) = rows.query([]) else {
            return false;
        };
        while let Ok(Some(row)) = rows.next() {
            for column in 0..columns {
                let bytes = match row.get_ref(column) {
                    Ok(
                        rusqlite::types::ValueRef::Text(bytes)
                        | rusqlite::types::ValueRef::Blob(bytes),
                    ) => bytes,
                    _ => continue,
                };
                if String::from_utf8_lossy(bytes).contains(needle) {
                    return true;
                }
            }
        }
        false
    })
}

/// Polls the window table of the store at `db_path` until `predicate` holds, then returns
/// the rows that satisfied it. A read error counts as "not converged yet" (the table's
/// writer may be mid-write), never as a failure, and only the final state is asserted.
pub async fn wait_for_window_rows(
    db_path: &std::path::Path,
    label: &str,
    predicate: impl Fn(&[TableRow]) -> bool,
) -> Vec<TableRow> {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    let mut last: Option<Vec<TableRow>> = None;
    loop {
        if let Some(rows) = window_rows(db_path) {
            if predicate(&rows) {
                return rows;
            }
            last = Some(rows);
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last observed rows: {last:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Creates the schema at `path` through the production entry point, releasing the
/// store's own connection before returning so a seeding connection can take the write
/// lock. The `Store` is left unbound rather than passed to `drop(..)`, so whether it
/// implements `Drop` cannot reach this file's lint status (`clippy::drop_non_drop`).
pub fn create_schema(path: &std::path::Path) {
    yagura_daemon::store::open_store(path).expect("open_store must succeed on a fresh path");
}

/// Wires a fresh database under `dir` into `registry`, so every window it opens from
/// then on reserves a row in the window table; returns the database's path.
pub fn wire_window_table(
    registry: &Arc<yagura_daemon::window::WindowRegistry>,
    dir: &std::path::Path,
) -> PathBuf {
    let db = dir.join("window-table.db");
    yagura_daemon::store::wire_into(registry, Some(db.clone()));
    db
}

/// The last open order the window table of `db` handed out, 0 when it never handed
/// one out. Every open the registry starts takes the next one, a failed open
/// included, and no order is handed out twice.
pub fn last_opened_seq(db: &std::path::Path) -> i64 {
    let conn = yagura_daemon::store::open_readonly(db).expect("open the window table");
    conn.query_row(
        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'windows'), 0)",
        [],
        |row| row.get(0),
    )
    .expect("read the last open order")
}

/// The open order of `window_id`'s row in the window table of `db`.
pub fn opened_seq(db: &std::path::Path, window_id: &str) -> i64 {
    let conn = yagura_daemon::store::open_readonly(db).expect("open the window table");
    conn.query_row(
        "SELECT opened_seq FROM windows WHERE window_id = ?1",
        [window_id],
        |row| row.get(0),
    )
    .unwrap_or_else(|err| panic!("the row of {window_id} must hold its open order: {err}"))
}

/// Inserts one row of the window table with `command_json` and `layout_json` stored
/// verbatim, including the malformed values a reader of the table must degrade.
pub fn insert_window_table_row(
    conn: &rusqlite::Connection,
    window_id: &str,
    name: &str,
    state: &str,
    command_json: rusqlite::types::Value,
    layout_json: rusqlite::types::Value,
) {
    conn.execute(
        "INSERT INTO windows (window_id, name, cwd, command_json, layout_json, credentials, state) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'env', ?6)",
        rusqlite::params![window_id, name, "C:/work", command_json, layout_json, state],
    )
    .expect("seeding a window table row must succeed");
}

/// Inserts one well-formed `lost` row of the window table.
pub fn insert_lost_window(conn: &rusqlite::Connection, window_id: &str, name: &str) {
    insert_window_table_row(
        conn,
        window_id,
        name,
        "lost",
        rusqlite::types::Value::Text("[\"cmd.exe\"]".to_owned()),
        rusqlite::types::Value::Null,
    );
}

/// Inserts one `lost` row of the window table whose `layout_json` is `TEXT` storage
/// class but not valid UTF-8. `CAST(x'..' AS TEXT)` stores the raw bytes under the `Text`
/// storage class, which Rust's UTF-8-checked strings could not carry; `hex` is derived
/// here from `invalid_utf8`, never from untrusted input.
pub fn insert_lost_window_with_invalid_utf8_layout(
    conn: &rusqlite::Connection,
    window_id: &str,
    name: &str,
    invalid_utf8: &[u8],
) {
    let hex: String = invalid_utf8.iter().map(|b| format!("{b:02x}")).collect();
    let sql = format!(
        "INSERT INTO windows (window_id, name, cwd, command_json, layout_json, credentials, state) \
         VALUES (?1, ?2, ?3, ?4, CAST(x'{hex}' AS TEXT), 'env', 'lost')"
    );
    conn.execute(
        &sql,
        rusqlite::params![window_id, name, "C:/work", "[\"cmd.exe\"]"],
    )
    .expect("seeding a window table row with an invalid-UTF-8 layout_json must succeed");
}

/// The `user_version` of a session store a daemon that kept handoff envelopes and report
/// notices in `inbox_parcels` left behind: the last schema before they stopped being kept.
const SCHEMA_KEEPING_UNSAVED_PARCELS: i64 = 19;

/// Rewrites the session store at `db` (no daemon running on it) into what a daemon that
/// kept every parcel left for `window_id`: a handoff's envelope carrying `envelope` and a
/// report notice carrying `notice`, in place of whatever notice the store holds, and the
/// schema version it had before those were dropped.
pub fn seed_parcels_an_older_daemon_kept(
    db: &std::path::Path,
    window_id: &str,
    envelope: &str,
    notice: &str,
) {
    let conn = rusqlite::Connection::open(db).expect("the session store must open");
    conn.execute(
        "DELETE FROM inbox_parcels WHERE window_id = ?1 AND key = 'report'",
        [window_id],
    )
    .expect("clearing the report notice row must succeed");
    let next_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM inbox_parcels WHERE window_id = ?1",
            [window_id],
            |row| row.get(0),
        )
        .expect("the next parcel seq must be readable");
    for (offset, key, notice_flag, body) in [
        (0, "handoff:seeded:1", 0, envelope),
        (1, "report", 1, notice),
    ] {
        conn.execute(
            "INSERT INTO inbox_parcels (window_id, seq, key, notice, body)              VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![window_id, next_seq + offset, key, notice_flag, body],
        )
        .expect("seeding an inbox parcel row must succeed");
    }
    conn.execute("ALTER TABLE windows DROP COLUMN hooks", [])
        .expect("dropping the column a later schema added must succeed");
    conn.pragma_update(None, "user_version", SCHEMA_KEEPING_UNSAVED_PARCELS)
        .expect("rewinding the schema version must succeed");
}

/// The keys of the `inbox_parcels` rows kept for `window_id`, by seq; `None` while the
/// store cannot be read.
pub fn inbox_parcel_keys(db: &std::path::Path, window_id: &str) -> Option<Vec<String>> {
    let conn = yagura_daemon::store::open_readonly(db).ok()?;
    let mut statement = conn
        .prepare("SELECT key FROM inbox_parcels WHERE window_id = ?1 ORDER BY seq")
        .ok()?;
    let rows = statement.query_map([window_id], |row| row.get(0)).ok()?;
    rows.collect::<Result<_, _>>().ok()
}
