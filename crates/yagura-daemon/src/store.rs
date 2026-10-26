//! Session store: a SQLite-backed record of the current and previous window
//! configuration.
//!
//! The schema is one table pair, never per-vendor tables:
//! `snapshot_windows` (a `slot` column distinguishes `SLOT_CURRENT` from
//! `SLOT_PREV`) and `snapshot_meta` (one row, timestamps). Schema v2 adds
//! `snapshot_windows.layout_json`, an opaque presentation string this layer
//! stores and returns without interpreting. The restore UI lives in the GUI;
//! this module only maintains the store and exposes [`open_readonly`] plus
//! the `read_*_conn` functions as the non-destructive read port.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};
use thiserror::Error;

use crate::sql::FixedSql;
use crate::window::WindowRegistry;

/// One restore candidate: a window's snapshot-visible fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotWindow {
    /// 0-based position within its slot, assigned by the caller that builds
    /// the snapshot (registration order, closed windows already excluded).
    pub position: i64,
    /// Argument vector the window was spawned with.
    pub command: Vec<String>,
    /// Working directory, already resolved to an absolute path.
    pub cwd: String,
    /// Display name.
    pub name: String,
    /// Vendor label: a data value, never a type variant.
    pub vendor: Option<String>,
    /// Detection definition name, verbatim from the window's open request.
    pub detection: Option<String>,
    /// The window's presentation layout, an opaque JSON string this layer
    /// stores and returns without interpreting (schema v2). `None` for a row
    /// written before the column existed, for a window that never reported a
    /// layout, and for a stored value this layer degraded (see
    /// [`read_snapshot_conn`]).
    pub layout_json: Option<String>,
    /// The launch profile the window was opened from (schema v11). `None`
    /// for a window opened from a raw argv, and for a row written before the
    /// column existed.
    pub profile: Option<String>,
    /// The `model` launch option value the window was opened with, as the
    /// caller supplied it. `None` when none was supplied.
    pub model: Option<String>,
    /// The `effort` launch option value the window was opened with, as the
    /// caller supplied it. `None` when none was supplied.
    pub effort: Option<String>,
}

/// Upper bound on the rows a single [`read_snapshot_conn`] /
/// [`read_prev_snapshot_conn`] call returns: a bound SQL parameter, never a
/// cast, so a tampered or corrupted store cannot make the read allocate
/// without limit.
pub const MAX_SNAPSHOT_WINDOWS: i64 = 256;

/// Upper bound on the byte length of one row's `command_json`. 1 MiB sits
/// far above any realistic spawn argv, and a row over the bound is skipped
/// exactly like a malformed one rather than reaching a caller.
///
/// The comparison runs on the already-materialized value, so it bounds what
/// a read *retains*, not the transient allocation of one oversized row. The
/// write path carries no matching bound: this fail-safe belongs to the read
/// side.
pub const MAX_COMMAND_JSON_BYTES: usize = 1_048_576;

/// Upper bound on the byte length of one row's `layout_json`, enforced on
/// both sides: the IPC layer refuses a larger value outright, and a stored
/// value over the bound degrades to `None` on read.
///
/// Unlike `MAX_COMMAND_JSON_BYTES`, exceeding this bound never costs the
/// row: a layout is presentation state, so losing it leaves a usable restore
/// candidate, while losing the command would leave one that cannot be
/// restored at all.
///
/// Defined in `yagura_protocol` and re-exported here, so the store's
/// enforcement and the GUI-side pre-send validation share one definition.
pub use yagura_protocol::MAX_LAYOUT_JSON_BYTES;

/// The slot holding the daemon's present window configuration.
pub const SLOT_CURRENT: i64 = 0;
/// The slot holding the configuration [`rotate_slots`] most recently rotated
/// out of `SLOT_CURRENT`.
pub const SLOT_PREV: i64 = 1;

/// `busy_timeout` (milliseconds) applied to every connection this module
/// opens, so a lock held briefly by a concurrent writer does not fail a call
/// outright.
const BUSY_TIMEOUT_MS: i64 = 5000;

/// Empties one slot. Shared by the rotation and by the wholesale replacement
/// of `SLOT_CURRENT`, which must delete by exactly the same rule.
const DELETE_SLOT_SQL: &str = "DELETE FROM snapshot_windows WHERE slot = ?1";

/// Errors returned by this module's fallible operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The store's parent directory could not be created.
    #[error("failed to create the store's parent directory: {0}")]
    CreateDir(#[source] std::io::Error),
    /// Any SQLite-level failure: a connection could not open, a statement
    /// could not prepare, or a query could not run.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database's `user_version` is newer than the migrations this build
    /// knows about: opening is refused rather than risking a write this build
    /// cannot interpret correctly (fail-safe).
    #[error("the store is at schema version {found}, newer than this build supports ({max})")]
    FutureSchemaVersion { found: i64, max: i64 },
    /// A row's `command` could not be serialized to `command_json`. Cannot
    /// happen in practice for a `Vec<String>`; kept as a typed error so a
    /// future field type change fails through `?` instead of a hidden panic.
    #[error("failed to serialize a snapshot row: {0}")]
    Serialize(#[from] serde_json::Error),
    /// [`crate::chat_store::ChatStore::ensure_room`] was called for an
    /// existing `room_id` whose stored `(workspace_kind, workspace_key)`
    /// does not match the pair just passed in.
    #[error("room {room_id} is already bound to a different workspace key")]
    RoomKeyMismatch { room_id: String },
    /// A message was posted to a room that has no `chat_room` row: it was
    /// deleted, or never existed.
    #[error("room {room_id} does not exist")]
    RoomGone { room_id: String },
    /// [`crate::chat_store::ChatStore::delete_room`] was asked to delete
    /// `main`, which every window shares and which is never deleted.
    #[error("the main room cannot be deleted")]
    MainRoom,
}

/// The production migration series, `index = schema version - 1`. Every
/// version is `include_str!`-embedded so [`apply_migrations`] always runs the
/// exact text this build shipped with.
pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_layout.sql"),
    include_str!("../migrations/003_chat.sql"),
    include_str!("../migrations/004_chat_rooms.sql"),
    include_str!("../migrations/005_chat_delivery_reason.sql"),
    include_str!("../migrations/006_chat_delivery_reason_check.sql"),
    include_str!("../migrations/007_plans.sql"),
    include_str!("../migrations/008_task_report.sql"),
    include_str!("../migrations/009_task_instruction.sql"),
    include_str!("../migrations/010_instruction_notice.sql"),
    include_str!("../migrations/011_snapshot_profile.sql"),
    include_str!("../migrations/012_plan_review.sql"),
    include_str!("../migrations/013_chat_delivery_state_check.sql"),
    include_str!("../migrations/014_chat_seq.sql"),
    include_str!("../migrations/015_default_palettes.sql"),
    include_str!("../migrations/016_tickets.sql"),
];

/// The schema version recorded in the database's `user_version`: `0` for a
/// database no migration has been applied to yet.
fn schema_version(conn: &Connection) -> Result<i64, StoreError> {
    let version: i64 = conn.query_row_fixed("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version)
}

/// Applies the `PRAGMA`s every connection this module hands out must carry:
/// one call site, so the read-only and read-write ports cannot drift apart.
fn apply_connection_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update_fixed("busy_timeout", BUSY_TIMEOUT_MS)?;
    Ok(())
}

/// Applies every migration in `migrations` whose version is newer than the
/// database's current `user_version`, each in its own transaction: the DDL
/// runs through `execute_batch` (`PRAGMA`s cannot bind parameters, so
/// `user_version` is set through `pragma_update` afterward, inside the same
/// transaction). Calling this again with a series whose earlier versions
/// were already applied re-runs only the newer ones.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from reading `user_version`, running a
/// migration's DDL, or committing.
pub fn apply_migrations(
    conn: &mut Connection,
    migrations: &[&'static str],
) -> Result<(), StoreError> {
    let start = usize::try_from(schema_version(conn)?).unwrap_or(0);
    for (index, sql) in migrations.iter().enumerate().skip(start) {
        let tx = conn.transaction()?;
        tx.execute_batch_fixed(sql)?;
        let version = i64::try_from(index + 1).unwrap_or(i64::MAX);
        tx.pragma_update_fixed("user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// Opens (creating the parent directory and the file if needed) and migrates
/// `db_path` to the schema this build ships with: the shared entry point
/// behind both [`open_store`] and `ChatStore::open`, so the pragma
/// application, ownership narrowing, version check and migration order never
/// drift apart between the two.
///
/// Non-destructive: it never deletes data, and a database from a newer
/// schema version is refused untouched rather than migrated or written to
/// (fail-safe) -- the only case where this returns
/// [`StoreError::FutureSchemaVersion`].
///
/// # Errors
///
/// [`StoreError::CreateDir`] if the parent directory cannot be created,
/// [`StoreError::FutureSchemaVersion`] for a database newer than this build
/// supports, or [`StoreError::Sqlite`] for any other open/migrate failure.
pub fn open_migrated(db_path: &Path) -> Result<Connection, StoreError> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(StoreError::CreateDir)?;
    }
    let mut conn = Connection::open(db_path)?;
    // `Connection::open` has no creation-mode argument, so the file is
    // narrowed to owner-only right afterwards. A failure is warned rather
    // than propagated, since the store stays usable.
    if let Err(reason) = yagura_os::secure_owned_file(db_path) {
        tracing::warn!("{reason}; the session database file may not be owner-restricted");
    }
    apply_connection_pragmas(&conn)?;

    let current = schema_version(&conn)?;
    let max = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    if current > max {
        return Err(StoreError::FutureSchemaVersion {
            found: current,
            max,
        });
    }
    apply_migrations(&mut conn, MIGRATIONS)?;
    Ok(conn)
}

/// [`open_migrated`], wrapped as a [`Store`] for the session-snapshot writer.
///
/// # Errors
///
/// Same as [`open_migrated`].
pub fn open_store(db_path: &Path) -> Result<Store, StoreError> {
    let conn = open_migrated(db_path)?;
    Ok(Store { conn })
}

/// Opens `db_path` read-only (`SQLITE_OPEN_READ_ONLY` plus the same
/// `busy_timeout`): the non-destructive read port every verification read,
/// dump and restore flow uses in place of [`open_store`].
///
/// # Errors
///
/// [`StoreError::Sqlite`] if `db_path` does not exist or cannot be opened.
pub fn open_readonly(db_path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    apply_connection_pragmas(&conn)?;
    Ok(conn)
}

/// Rotates `SLOT_CURRENT` into `SLOT_PREV`, all in one transaction.
///
/// Destructive: it drops whatever `SLOT_PREV` held. When `SLOT_CURRENT` is
/// empty this is a no-op that leaves `SLOT_PREV` and
/// `prev_taken_at_epoch_s` untouched, so a start-up that has not written a
/// snapshot yet can never destroy the last recoverable session.
///
/// **Wiring-exclusive**: production calls this exactly once, from
/// [`wire_into`], before the writer thread starts -- never on a live store.
/// Calling it at any other time destroys `SLOT_PREV` outside a start-up
/// rotation, so `disallowed-methods` in `clippy.toml` holds it to that one
/// caller.
///
/// # Errors
///
/// Propagates any [`rusqlite::Error`] from the transaction.
pub fn rotate_slots(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    let current_count: i64 = tx.query_row_fixed(
        "SELECT COUNT(*) FROM snapshot_windows WHERE slot = ?1",
        params![SLOT_CURRENT],
        |row| row.get(0),
    )?;
    if current_count > 0 {
        tx.execute_fixed(DELETE_SLOT_SQL, params![SLOT_PREV])?;
        tx.execute_fixed(
            "UPDATE snapshot_windows SET slot = ?1 WHERE slot = ?2",
            params![SLOT_PREV, SLOT_CURRENT],
        )?;
        tx.execute_fixed(
            "UPDATE snapshot_meta SET prev_taken_at_epoch_s = taken_at_epoch_s WHERE id = 1",
            [],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Replaces `SLOT_CURRENT` wholesale with `rows` and stamps
/// `taken_at_epoch_s`, in one transaction.
///
/// The meta row is updated through an `INSERT ... ON CONFLICT(id) DO UPDATE`
/// (an UPSERT) that touches only `taken_at_epoch_s`; a `REPLACE` /
/// `INSERT OR REPLACE` is never used here because it deletes the row first,
/// which would silently null `prev_taken_at_epoch_s`.
/// `taken_at_epoch_s` stays `NOT NULL`: right after [`rotate_slots`] it keeps
/// its old value rather than being cleared, so "meaningless until the next
/// write" is documented, not enforced by nulling it. "When was the previous
/// session" is answered by `prev_taken_at_epoch_s`, not this column.
///
/// # Errors
///
/// [`StoreError::Serialize`] if a row's `command` cannot be encoded as JSON
/// (in practice, never, for `Vec<String>`), or [`StoreError::Sqlite`] for any
/// other failure in the transaction.
pub fn write_snapshot_once(
    conn: &mut Connection,
    rows: &[SnapshotWindow],
) -> Result<(), StoreError> {
    let now = current_epoch_secs();
    let tx = conn.transaction()?;
    tx.execute_fixed(DELETE_SLOT_SQL, params![SLOT_CURRENT])?;
    for row in rows {
        let command_json = serde_json::to_string(&row.command)?;
        tx.execute_fixed(
            "INSERT INTO snapshot_windows \
             (slot, position, command_json, cwd, name, vendor, detection, layout_json, \
             profile, model, effort) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                SLOT_CURRENT,
                row.position,
                command_json,
                row.cwd,
                row.name,
                row.vendor,
                row.detection,
                row.layout_json,
                row.profile,
                row.model,
                row.effort
            ],
        )?;
    }
    tx.execute_fixed(
        "INSERT INTO snapshot_meta (id, taken_at_epoch_s) VALUES (1, ?1) \
         ON CONFLICT(id) DO UPDATE SET taken_at_epoch_s = excluded.taken_at_epoch_s",
        params![now],
    )?;
    tx.commit()?;
    Ok(())
}

/// The current wall-clock time as Unix epoch seconds, degrading to `0` if the
/// clock cannot be read (fail-safe: a snapshot write must never panic on a
/// clock failure).
///
/// `pub(crate)` so [`crate::plan_store`] can stamp its own rows with the same
/// clock reading rather than duplicating this degrade-to-`0` idiom.
pub(crate) fn current_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// [`current_epoch_secs`] in milliseconds, with the same degrade-to-`0` rule.
pub(crate) fn current_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Reads `SLOT_CURRENT`, in ascending `position` order, bounded by
/// [`MAX_SNAPSHOT_WINDOWS`].
///
/// **Untrusted input**: another process running as the same user can write
/// this file. Nothing read here fails the whole call: the damage is contained
/// per column, and each containment stage logs at most one `WARN` for the
/// entire call, so a tampered store cannot flood the log.
///
/// # Errors
///
/// Returns [`StoreError::Sqlite`] only when the query itself cannot be
/// prepared or run. From the point rows are pulled onward, failures degrade
/// rather than propagate, in two units that differ by column:
///
/// * the columns a candidate cannot be restored without (`position`,
///   `command_json`, `cwd`, `name`, `vendor`, `detection`) skip the **whole
///   row** -- a wrong storage class, a `command_json` longer than
///   [`MAX_COMMAND_JSON_BYTES`], or a `command_json` that is not a JSON array
///   of strings;
/// * `layout_json` loses **only that value** (it becomes `None`, the row
///   stays a candidate) -- a wrong storage class, a value longer than
///   [`MAX_LAYOUT_JSON_BYTES`], or text that is not valid UTF-8. A `NULL`
///   here is the ordinary "no layout saved" case and is not a degradation.
pub fn read_snapshot_conn(conn: &Connection) -> Result<Vec<SnapshotWindow>, StoreError> {
    read_slot(conn, SLOT_CURRENT)
}

/// [`read_snapshot_conn`], reading `SLOT_PREV` instead.
///
/// # Errors
///
/// Same as [`read_snapshot_conn`].
pub fn read_prev_snapshot_conn(conn: &Connection) -> Result<Vec<SnapshotWindow>, StoreError> {
    read_slot(conn, SLOT_PREV)
}

/// Decides what one row's `layout_json` cell reads back as, returning the
/// value together with whether the cell was degraded.
///
/// Judged on the [`ValueRef`] rather than through `row.get`, so a value of
/// the wrong storage class, an oversized one, and one that is not valid
/// UTF-8 are all decided before anything is materialized -- and none of them
/// can turn into a row-level decode failure.
fn decode_layout_value(cell: ValueRef<'_>) -> (Option<String>, bool) {
    match cell {
        ValueRef::Null => (None, false),
        ValueRef::Text(bytes) if bytes.len() <= MAX_LAYOUT_JSON_BYTES => {
            match std::str::from_utf8(bytes) {
                Ok(text) => (Some(text.to_string()), false),
                Err(_) => (None, true),
            }
        }
        _ => (None, true),
    }
}

/// The three launch columns of one stored row, read together.
struct SnapshotLaunch {
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

/// Shared implementation of [`read_snapshot_conn`] / [`read_prev_snapshot_conn`].
fn read_slot(conn: &Connection, slot: i64) -> Result<Vec<SnapshotWindow>, StoreError> {
    let mut stmt = conn.prepare_fixed(
        "SELECT position, command_json, cwd, name, vendor, detection, layout_json, profile, \
         model, effort FROM snapshot_windows WHERE slot = ?1 ORDER BY position LIMIT ?2",
    )?;
    let mapped = stmt.query_map(params![slot, MAX_SNAPSHOT_WINDOWS], |row| {
        let (layout_json, layout_degraded) = decode_layout_value(row.get_ref(6)?);
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            layout_json,
            layout_degraded,
            SnapshotLaunch {
                profile: row.get::<_, Option<String>>(7)?,
                model: row.get::<_, Option<String>>(8)?,
                effort: row.get::<_, Option<String>>(9)?,
            },
        ))
    })?;

    let mut rows = Vec::new();
    let mut skipped_a_corrupt_row = false;
    let mut degraded_a_layout_value = false;
    // Past this point nothing propagates: a failure in a restore-critical
    // column costs that row, one in layout_json costs only that value, and
    // the two are tracked separately so their warnings stay distinguishable.
    for item in mapped {
        let Ok((
            position,
            command_json,
            cwd,
            name,
            vendor,
            detection,
            layout_json,
            layout_degraded,
            launch,
        )) = item
        else {
            skipped_a_corrupt_row = true;
            continue;
        };
        degraded_a_layout_value |= layout_degraded;
        if command_json.len() > MAX_COMMAND_JSON_BYTES {
            skipped_a_corrupt_row = true;
            continue;
        }
        let Ok(command) = serde_json::from_str::<Vec<String>>(&command_json) else {
            skipped_a_corrupt_row = true;
            continue;
        };
        rows.push(SnapshotWindow {
            position,
            command,
            cwd,
            name,
            vendor,
            detection,
            layout_json,
            profile: launch.profile,
            model: launch.model,
            effort: launch.effort,
        });
    }
    if skipped_a_corrupt_row {
        tracing::warn!(slot, "skipped one or more corrupt snapshot_windows rows");
    }
    if degraded_a_layout_value {
        tracing::warn!(
            slot,
            "degraded one or more snapshot_windows layout values to none"
        );
    }
    Ok(rows)
}

/// Reads when the *previous* configuration was recorded, in Unix epoch
/// seconds, or `None` when the store cannot answer that question.
///
/// **Untrusted input**, and answered fail-safe: a missing meta row, a
/// `NULL`, and a cell of any storage class other than `Integer` all
/// read as `Ok(None)`. That degradation is silent -- a missing timestamp is
/// already visible to the user as the restore banner's "no time" wording, so
/// a warning would add nothing -- and it is what keeps a single tampered
/// cell from failing the whole restore-candidate read.
///
/// # Errors
///
/// Returns [`StoreError::Sqlite`] only when the query itself cannot be
/// prepared or run.
pub fn read_prev_taken_at_conn(conn: &Connection) -> Result<Option<i64>, StoreError> {
    let mut stmt =
        conn.prepare_fixed("SELECT prev_taken_at_epoch_s FROM snapshot_meta WHERE id = 1")?;
    let mut rows = stmt.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(match row.get_ref(0) {
        Ok(ValueRef::Integer(value)) => Some(value),
        _ => None,
    })
}

/// Suppresses repeat `WARN`s per failure axis (init / write), independently
/// of one another: the store must not flood the log every time a disabled or
/// failing persistence path is exercised again.
#[derive(Debug, Default)]
pub struct WarnGate {
    init_warned: AtomicBool,
    write_warned: AtomicBool,
}

impl WarnGate {
    /// A gate with both axes un-tripped.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` for the first failure recorded on `axis` and `false` for every
    /// later one: the single definition of "warn once per axis" that the
    /// `*_or_warn` entry points gate their `WARN` on.
    fn first_failure(axis: &AtomicBool) -> bool {
        !axis.swap(true, Ordering::SeqCst)
    }
}

/// [`open_store`], warning at most once per `gate` on failure and returning
/// `None` instead of propagating the error: store initialization must never
/// take the daemon down, and repeated failures (every wiring attempt against
/// a still-broken path) must not repeat the warning.
pub fn init_store_or_warn(gate: &WarnGate, db_path: &Path) -> Option<Store> {
    match open_store(db_path) {
        Ok(store) => Some(store),
        Err(err) => {
            if WarnGate::first_failure(&gate.init_warned) {
                tracing::warn!(
                    "failed to initialize the session store ({err}); persistence is disabled"
                );
            }
            None
        }
    }
}

/// [`write_snapshot_once`], warning at most once per `gate` on failure and
/// swallowing the error: a write failure must never disturb the window
/// operation that triggered it, and the store's writer thread retries on the
/// next trigger indefinitely, so repeated failures stay silent after the
/// first warning.
pub fn write_snapshot_or_warn(gate: &WarnGate, conn: &mut Connection, rows: &[SnapshotWindow]) {
    if let Err(err) = write_snapshot_once(conn, rows)
        && WarnGate::first_failure(&gate.write_warned)
    {
        tracing::warn!("failed to write a session snapshot ({err}); will retry soon");
    }
}

/// A store connection ready for [`spawn_store_thread`]. Exposes no query
/// port: verifying a store's contents is done by opening a second connection
/// to the same path with [`open_readonly`], never through this type.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

/// Starts the store's writer thread, which owns `store` for the rest of its
/// life, and returns the channel [`crate::window::WindowRegistry::request_snapshot`]
/// sends restore-candidate rows on.
///
/// Every request received while a write is in flight is coalesced: the
/// channel is drained down to the most recent request before each write,
/// since only the latest full configuration is ever meaningful (a dump is
/// not incremental). The thread stops once every [`Sender`] clone is
/// dropped.
pub fn spawn_store_thread(mut store: Store) -> Sender<Vec<SnapshotWindow>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<SnapshotWindow>>();
    std::thread::spawn(move || {
        let gate = WarnGate::new();
        while let Ok(mut rows) = rx.recv() {
            while let Ok(next) = rx.try_recv() {
                rows = next;
            }
            write_snapshot_or_warn(&gate, &mut store.conn, &rows);
        }
    });
    tx
}

/// Wires the store into `registry`: production's only entry point, called
/// once at start-up before any window can be opened.
///
/// Internal order: [`open_store`] -> [`rotate_slots`] ->
/// [`spawn_store_thread`] -> [`crate::window::WindowRegistry::set_store_sender`].
/// `db_path` being `None`, or [`open_store`] failing, disables persistence
/// for the rest of this process (warned once through this call's own
/// [`WarnGate`]) without affecting window operations.
pub fn wire_into(registry: &Arc<WindowRegistry>, db_path: Option<PathBuf>) {
    let Some(db_path) = db_path else {
        // No persistent-data base directory could be resolved (e.g. neither
        // `LOCALAPPDATA` nor `XDG_DATA_HOME`/`HOME` is set); the daemon still
        // runs, without session persistence.
        tracing::warn!("no database path resolved; session persistence is disabled for this run");
        return;
    };
    let gate = WarnGate::new();
    let Some(mut store) = init_store_or_warn(&gate, &db_path) else {
        return;
    };
    // Rotating anywhere else would overwrite the previous session; `clippy.toml` bans every other call.
    #[allow(clippy::disallowed_methods)]
    let rotated = rotate_slots(&mut store.conn);
    if let Err(err) = rotated {
        tracing::warn!("failed to rotate the session store's slots ({err}); continuing as-is");
    }
    let tx = spawn_store_thread(store);
    registry.set_store_sender(tx);
}

/// Resolves the database path: `explicit` > `YAGURA_STORE_DB` >
/// `<data_base>/<namespace>/yagura.db` > `None` (persistence disabled), where
/// `data_base` is `local_app_data` as resolved by `main.rs` through
/// [`yagura_os::default_data_base_dir`]: `LOCALAPPDATA` on Windows,
/// `HOME/Library/Application Support` on macOS, `XDG_DATA_HOME` falling back
/// to `HOME/.local/share` on other Unix. The `yagura.db` leaf name itself
/// never varies with the instance; only the namespace directory it sits
/// under does.
///
/// `local_app_data` keeps its Windows-flavoured name for API stability; it
/// carries the resolved data-base directory on every platform. A pure
/// function -- `main.rs` is the only caller that reads the real environment,
/// every other caller passes synthetic values. `YAGURA_STORE_DB` combined
/// with a named instance is refused before this function is ever reached
/// (`yagura_os::store_override_conflicts`), so this function itself does not
/// judge that combination.
#[must_use]
pub fn resolve_db_path(
    explicit: Option<PathBuf>,
    env_store_db: Option<OsString>,
    local_app_data: Option<OsString>,
    namespace: &yagura_os::InstanceNamespace,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path);
    }
    if let Some(env_value) = env_store_db {
        return Some(PathBuf::from(env_value));
    }
    local_app_data
        .map(PathBuf::from)
        .map(|base| yagura_os::instance_dir(&base, namespace).join("yagura.db"))
}
