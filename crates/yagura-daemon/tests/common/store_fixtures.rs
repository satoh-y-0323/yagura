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

/// Inserts one `snapshot_windows` row with the given `command_json` verbatim, including
/// the deliberately malformed values the corrupt-row degradation contracts need.
pub fn insert_window_row_with_command_json(
    conn: &rusqlite::Connection,
    slot: i64,
    position: i64,
    name: &str,
    command_json: &str,
) {
    conn.execute(
        "INSERT INTO snapshot_windows \
         (slot, position, command_json, cwd, name, vendor, detection) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            slot,
            position,
            command_json,
            "C:/work",
            name,
            None::<String>,
            None::<String>
        ],
    )
    .expect("seeding a snapshot_windows row must succeed");
}

/// Inserts one well-formed `snapshot_windows` row.
pub fn insert_window_row(conn: &rusqlite::Connection, slot: i64, position: i64, name: &str) {
    insert_window_row_with_command_json(conn, slot, position, name, "[\"cmd.exe\"]");
}

/// Inserts one `snapshot_windows` row whose `command_json` value is a raw `BLOB` rather
/// than `TEXT`: SQLite's TEXT-affinity conversion applies to INTEGER/REAL values, but a
/// bound `BLOB` is stored as-is, which makes this a *storage-class* corruption distinct
/// from the JSON-content corruption `insert_window_row_with_command_json` can plant.
pub fn insert_window_row_with_blob_command(
    conn: &rusqlite::Connection,
    slot: i64,
    position: i64,
    name: &str,
    blob: &[u8],
) {
    conn.execute(
        "INSERT INTO snapshot_windows \
         (slot, position, command_json, cwd, name, vendor, detection) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            slot,
            position,
            blob,
            "C:/work",
            name,
            None::<String>,
            None::<String>
        ],
    )
    .expect("seeding a BLOB-command_json snapshot_windows row must succeed");
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

/// Reads `(taken_at_epoch_s, prev_taken_at_epoch_s)` from the meta row.
pub fn read_meta(conn: &rusqlite::Connection) -> (i64, Option<i64>) {
    conn.query_row(
        "SELECT taken_at_epoch_s, prev_taken_at_epoch_s FROM snapshot_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("the meta row must exist and decode")
}

/// Reads `(position, name)` of every row in `slot`, ordered by position.
pub fn slot_rows(conn: &rusqlite::Connection, slot: i64) -> Vec<(i64, String)> {
    let mut stmt = conn
        .prepare("SELECT position, name FROM snapshot_windows WHERE slot = ?1 ORDER BY position")
        .expect("the slot query must prepare");
    let rows = stmt
        .query_map([slot], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("the slot query must run");
    rows.map(|row| row.expect("slot row must decode")).collect()
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

/// Inserts one `snapshot_windows` row whose `layout_json` value is `TEXT` storage class
/// but not valid UTF-8 -- distinct from [`insert_window_row_with_layout_value`]'s `Blob`
/// variant, a different SQLite storage class entirely. `CAST(x'..' AS TEXT)` forces the
/// raw bytes to be stored under the `Text` storage class without going through Rust's
/// UTF-8-checked string types, which could not represent this value at all.
///
/// The SQL text is built with `format!` rather than a bound parameter because
/// `CAST(x'..' AS TEXT)` is a SQL expression, not a value a placeholder can carry; `hex`
/// is derived here from `invalid_utf8`, never from untrusted input.
pub fn insert_window_row_with_invalid_utf8_layout(
    conn: &rusqlite::Connection,
    slot: i64,
    position: i64,
    name: &str,
    invalid_utf8: &[u8],
) {
    let hex: String = invalid_utf8.iter().map(|b| format!("{b:02x}")).collect();
    let sql = format!(
        "INSERT INTO snapshot_windows \
         (slot, position, command_json, cwd, name, vendor, detection, layout_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, CAST(x'{hex}' AS TEXT))"
    );
    conn.execute(
        &sql,
        rusqlite::params![slot, position, "[\"cmd.exe\"]", "C:/work", name],
    )
    .expect("seeding a snapshot_windows row with an invalid-UTF-8 layout_json must succeed");
}
