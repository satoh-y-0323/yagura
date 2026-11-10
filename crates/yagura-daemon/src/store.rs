//! Session store: the SQLite database a daemon of an instance keeps its
//! windows' rows, rooms, plans and tickets in.
//!
//! The schema never has per-vendor tables. The window table
//! ([`crate::window_table`]) holds one row per window a daemon opened and has
//! not closed; a row whose holder ended without the window being closed is
//! `lost`, and the `lost` rows are the restore candidates. The restore UI
//! lives in the GUI; this module only opens and migrates the store and exposes
//! [`open_readonly`] plus [`read_lost_windows`] as the non-destructive read
//! port.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};
use thiserror::Error;

use crate::sql::FixedSql;
use crate::window::WindowRegistry;

/// One restore candidate: the values a `lost` window's row keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostWindow {
    /// 0-based place among the `lost` rows, in the order their windows opened.
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
    /// stores and returns without interpreting. `None` for a window that never
    /// reported a layout, and for a stored value this layer degraded (see
    /// [`read_lost_windows`]).
    pub layout_json: Option<String>,
    /// The launch profile the window was opened from; `None` for a window
    /// opened from a raw argv.
    pub profile: Option<String>,
    /// The `model` launch option value the window was opened with, as the
    /// caller supplied it. `None` when none was supplied.
    pub model: Option<String>,
    /// The `effort` launch option value the window was opened with, as the
    /// caller supplied it. `None` when none was supplied.
    pub effort: Option<String>,
}

/// Upper bound on the rows a single [`read_lost_windows`] call returns: a
/// bound SQL parameter, never a cast, so a tampered or corrupted store cannot
/// make the read allocate without limit.
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

/// `busy_timeout` (milliseconds) applied to every connection this module
/// opens, so a lock held briefly by a concurrent writer does not fail a call
/// outright.
const BUSY_TIMEOUT_MS: i64 = 5000;

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
    /// A value could not be serialized to JSON. Cannot happen in practice for
    /// the string values stored; kept as a typed error so a future field type
    /// change fails through `?` instead of a hidden panic.
    #[error("failed to serialize a stored value: {0}")]
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
    include_str!("../migrations/017_windows.sql"),
    include_str!("../migrations/018_parcels.sql"),
    include_str!("../migrations/019_delivery_marks.sql"),
    include_str!("../migrations/020_unsaved_parcels.sql"),
    include_str!("../migrations/021_window_hooks.sql"),
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

/// Puts the database a read-write connection opened in WAL mode, so a reader
/// never stops a writer and a write waits only for another write. The mode is
/// kept in the file, so a read-only connection, which SQLite refuses to change
/// it from (`SQLITE_READONLY`), reads it in WAL mode all the same.
fn apply_write_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update_fixed("journal_mode", "WAL")?;
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
/// Non-destructive: it deletes nothing but the rows a migration retires
/// (kept parcels of a kind no longer kept), and a database from a newer
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
    apply_write_pragmas(&conn)?;
    apply_migrations(&mut conn, MIGRATIONS)?;
    Ok(conn)
}

/// [`open_migrated`], wrapped as the [`Store`] [`wire_into`] gives the
/// window table.
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

/// The current wall-clock time as Unix epoch seconds, degrading to `0` if the
/// clock cannot be read (fail-safe: a stamp must never panic on a clock
/// failure).
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

/// The columns of one `lost` row as they read back, before its command is
/// judged.
struct ReadRow {
    command_json: String,
    cwd: String,
    name: String,
    vendor: Option<String>,
    detection: Option<String>,
    layout_json: Option<String>,
    layout_degraded: bool,
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

/// Reads the window table's `lost` rows, in the order their windows opened,
/// bounded by [`MAX_SNAPSHOT_WINDOWS`].
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
/// * the columns a candidate cannot be restored without (`command_json`,
///   `cwd`, `name`, `vendor`, `detection`) skip the **whole row** -- a wrong
///   storage class, a `command_json` longer than [`MAX_COMMAND_JSON_BYTES`],
///   or a `command_json` that is not a JSON array of strings;
/// * `layout_json` loses **only that value** (it becomes `None`, the row
///   stays a candidate) -- a wrong storage class, a value longer than
///   [`MAX_LAYOUT_JSON_BYTES`], or text that is not valid UTF-8. A `NULL`
///   here is the ordinary "no layout saved" case and is not a degradation.
pub fn read_lost_windows(conn: &Connection) -> Result<Vec<LostWindow>, StoreError> {
    let mut stmt = conn.prepare_fixed(
        "SELECT command_json, cwd, name, vendor, detection, layout_json, profile, model, effort \
         FROM windows WHERE state = 'lost' ORDER BY opened_seq LIMIT ?1",
    )?;
    let mapped = stmt.query_map(params![MAX_SNAPSHOT_WINDOWS], |row| {
        let (layout_json, layout_degraded) = decode_layout_value(row.get_ref(5)?);
        Ok(ReadRow {
            command_json: row.get(0)?,
            cwd: row.get(1)?,
            name: row.get(2)?,
            vendor: row.get(3)?,
            detection: row.get(4)?,
            layout_json,
            layout_degraded,
            profile: row.get(6)?,
            model: row.get(7)?,
            effort: row.get(8)?,
        })
    })?;

    let mut rows = Vec::new();
    let mut skipped_a_corrupt_row = false;
    let mut degraded_a_layout_value = false;
    // Past this point nothing propagates: a failure in a restore-critical
    // column costs that row, one in layout_json costs only that value, and
    // the two are tracked separately so their warnings stay distinguishable.
    for (position, item) in (0i64..).zip(mapped) {
        let Ok(read) = item else {
            skipped_a_corrupt_row = true;
            continue;
        };
        degraded_a_layout_value |= read.layout_degraded;
        if read.command_json.len() > MAX_COMMAND_JSON_BYTES {
            skipped_a_corrupt_row = true;
            continue;
        }
        let Ok(command) = serde_json::from_str::<Vec<String>>(&read.command_json) else {
            skipped_a_corrupt_row = true;
            continue;
        };
        rows.push(LostWindow {
            position,
            command,
            cwd: read.cwd,
            name: read.name,
            vendor: read.vendor,
            detection: read.detection,
            layout_json: read.layout_json,
            profile: read.profile,
            model: read.model,
            effort: read.effort,
        });
    }
    if skipped_a_corrupt_row {
        tracing::warn!("skipped one or more corrupt lost window rows");
    }
    if degraded_a_layout_value {
        tracing::warn!("degraded one or more lost window layout values to none");
    }
    Ok(rows)
}

/// The session store, opened and migrated. Exposes no query port: verifying
/// a store's contents is done by opening a second connection to the same
/// path with [`open_readonly`], never through this type.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

/// Wires the store into `registry`: production's only entry point, called
/// once at start-up before any window can be opened.
///
/// Internal order: [`open_store`] -> the one move of a 0.3 store's last
/// windows into `lost` rows ([`crate::recovery::move_0_3_candidates`]) ->
/// the window table's writer on the store's connection
/// ([`crate::window::WindowRegistry::set_window_table`]) -> the writer of the
/// kept parcels on a connection of its own. `db_path` being
/// `None`, or [`open_store`] failing, disables persistence for the rest of
/// this process (warned once) without affecting window operations.
pub fn wire_into(registry: &Arc<WindowRegistry>, db_path: Option<PathBuf>) {
    let Some(db_path) = db_path else {
        // No persistent-data base directory could be resolved (e.g. neither
        // `LOCALAPPDATA` nor `XDG_DATA_HOME`/`HOME` is set); the daemon still
        // runs, without session persistence.
        tracing::warn!("no database path resolved; session persistence is disabled for this run");
        return;
    };
    let mut store = match open_store(&db_path) {
        Ok(store) => store,
        Err(err) => {
            tracing::warn!(
                "failed to initialize the session store ({err}); persistence is disabled"
            );
            return;
        }
    };
    let holders_dir = registry.holders_dir();
    if let Err(err) = crate::recovery::move_0_3_candidates(&mut store.conn, holders_dir.as_deref())
    {
        tracing::warn!("failed to move the 0.3 store's last windows into lost rows ({err})");
    }
    registry.set_window_table(crate::window_table::WindowTable::spawn(store.conn));
    match open_migrated(&db_path) {
        Ok(conn) => registry.set_parcel_store(crate::parcel_store::ParcelStore::spawn(
            conn,
            wake_shown(registry),
        )),
        Err(err) => tracing::warn!(
            "failed to open the store of kept parcels ({err}); parcels are kept in memory only"
        ),
    }
}

/// Wakes the delivery agent of a window whose inbox a written row showed
/// parcels in, as a deposit does, from the parcel writer's thread.
fn wake_shown(registry: &Arc<WindowRegistry>) -> crate::parcel_store::OnShown {
    let registry = Arc::downgrade(registry);
    // The agent runs on the daemon's runtime, which the writer's thread is not in.
    let runtime = tokio::runtime::Handle::try_current().ok();
    Box::new(move |window_id| {
        let (Some(registry), Some(runtime)) = (registry.upgrade(), runtime.as_ref()) else {
            return;
        };
        if let Some(entry) = registry.get(window_id) {
            let _entered = runtime.enter();
            registry.wake_notices(&entry);
        }
    })
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
