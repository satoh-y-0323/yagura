//! Taking back, as a daemon starts, the windows a previous daemon of its
//! instance left running, and removing the files of a holder that is gone.
//!
//! The start has two steps. Before the daemon accepts connections, the holders'
//! records, and whether the processes they name still run, decide which
//! windows are still there: each window whose holder runs (or whose holder the
//! OS cannot tell about) is registered at once, not connected and `Unknown`,
//! with the values its row keeps, or, without a row, with only its id until
//! its holder's launch record gives the values back. A window whose holder is
//! gone becomes `lost`, a restore candidate, and its holder's files are
//! removed. What the start sweeps away afterwards is decided by the
//! [`Remaining`] windows this step returns. Once the daemon accepts
//! connections, each window's own link lets itself into its holder; no window
//! waits for another, and nothing waits for them all.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::{Connection, TransactionBehavior, params};
use tokio::sync::watch;
use yagura_os::{Liveness, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{HolderRecord, holder_record_path, read_holder_record};

use crate::parcel_store::KeptReport;
use crate::sql::FixedSql;
use crate::store::StoreError;
use crate::window::WindowRegistry;
use crate::window_table::{RowState, StoredWindow};

/// The names of one holder's files.
pub(crate) struct HolderFiles<'a> {
    /// The instance argument the holder was started with: its id, empty for
    /// the default instance.
    // Read only where a holder's endpoint is a file to remove (Unix).
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) instance: &'a OsStr,
    pub(crate) holders_dir: &'a Path,
    pub(crate) window_id: &'a str,
}

/// Removes the files of a holder whose end is known (it was killed here, or
/// found gone): on Unix its socket and the socket's lock, under that lock so a
/// live listener's are never removed, and then its record. Paths are derived
/// from the holders directory and the window id, never read from the record;
/// a record naming another endpoint is left, as is a record that is a link.
pub(crate) fn remove_dead_holder_files(files: &HolderFiles<'_>) {
    let window_id = files.window_id;
    let Ok(record_path) = holder_record_path(files.holders_dir, window_id) else {
        return;
    };
    #[cfg(unix)]
    if !remove_dead_endpoint(files) {
        return;
    }
    match std::fs::symlink_metadata(&record_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(
                window_id,
                "a window holder's record is a link; it is left in place"
            );
        }
        Ok(_) => {
            if let Err(error) = std::fs::remove_file(&record_path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(window_id, %error, "cannot remove a window holder's record");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(window_id, %error, "cannot read a window holder's record"),
    }
}

/// Removes a gone holder's socket and lock; `false` when its record must stay
/// (the endpoint is still held, or the record names another one).
#[cfg(unix)]
fn remove_dead_endpoint(files: &HolderFiles<'_>) -> bool {
    use yagura_os::{OsError, PeerAuth as _};

    let window_id = files.window_id;
    let instance = (!files.instance.is_empty()).then_some(files.instance);
    let derived = yagura_os::resolve_instance_from(instance)
        .map_err(|error| error.to_string())
        .and_then(|namespace| {
            let user_id = yagura_os::PlatformIpc::default()
                .current_user_id()
                .map_err(|error| error.to_string())?;
            yagura_os::holder_endpoint(&user_id, &namespace, files.holders_dir, window_id)
                .map_err(|error| error.to_string())
        });
    let endpoint = match derived {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::warn!(window_id, %error, "cannot name a window holder's endpoint");
            return false;
        }
    };
    if let Ok(record) = read_holder_record(files.holders_dir, window_id)
        && record.endpoint != endpoint
    {
        tracing::warn!(
            window_id,
            "a window holder's record names another endpoint; its files are left in place"
        );
        return false;
    }
    match yagura_os::platform_unix::remove_abandoned_endpoint(Path::new(&endpoint)) {
        Ok(()) => true,
        Err(OsError::EndpointHeld(_)) => {
            tracing::warn!(
                window_id,
                "a window holder's endpoint is still held; its files are left in place"
            );
            false
        }
        Err(error) => {
            tracing::warn!(window_id, %error, "cannot remove a window holder's endpoint");
            true
        }
    }
}

/// A window whose holder may still run, as its record and the table name it.
pub(crate) struct Survivor {
    pub(crate) window_id: String,
    /// The window's row; `None` when the table has none for it.
    pub(crate) stored: Option<StoredWindow>,
    pub(crate) record: HolderRecord,
    /// The holder's process; `None` when the OS would not tell whether it
    /// runs, which is not taken for gone.
    pub(crate) holder: Option<ProcessHandle>,
    /// The window's credentials folder, which the window taken back owns.
    pub(crate) credentials: Option<PathBuf>,
}

/// The windows the first step of a start found still there: what the
/// start-up sweep keeps of theirs, while it clears what names any other
/// window.
#[derive(Debug, Default, Clone)]
pub struct Remaining {
    /// Windows whose holder runs or cannot be told about: taken back, being
    /// taken back, or left as they are.
    running: BTreeSet<String>,
    /// Windows whose row is `lost`: the restore candidates.
    lost: BTreeSet<String>,
}

impl Remaining {
    /// Whether the credentials file of `window_id` is kept: only a window
    /// whose holder may still run can use it.
    #[must_use]
    pub fn keeps_credentials_of(&self, window_id: &str) -> bool {
        self.running.contains(window_id)
    }

    /// Every window that remains, running or lost, in id order: whose plan
    /// ownership, task bindings and pending deliveries are kept.
    #[must_use]
    pub fn window_ids(&self) -> Vec<String> {
        self.running.union(&self.lost).cloned().collect()
    }

    /// Every row's window counted as remaining as it is, for a start that
    /// cannot judge any of them.
    fn unjudged(rows: &[StoredWindow]) -> Self {
        let mut remaining = Self::default();
        for row in rows {
            let ids = if row.state == RowState::Lost {
                &mut remaining.lost
            } else {
                &mut remaining.running
            };
            ids.insert(row.window_id.clone());
        }
        remaining
    }
}

/// The first step of the start: judges every window a previous daemon of the
/// instance left from its row and its holder's record, before the daemon
/// accepts connections. A window whose holder runs is registered, not
/// connected and `Unknown`, and its link lets itself into the holder once
/// `begin` turns `true`; a window whose holder is gone becomes `lost` and its
/// holder's files are removed; an `opening` row whose holder never started
/// the child is removed. Returns the windows that remain.
pub async fn take_back_windows(
    registry: &Arc<WindowRegistry>,
    begin: watch::Receiver<bool>,
) -> Remaining {
    let table = registry.window_table();
    let rows = match &table {
        Some(table) => table.rows().await,
        None => Vec::new(),
    };
    let unjudged = Remaining::unjudged(&rows);
    let Some(context) = registry.take_back_context() else {
        // Holders end with this daemon, so no record tells whether a row's
        // window still runs; every row stays as it is.
        return unjudged;
    };
    let credentials_dir = registry.config().mcp_config_dir.clone();
    let judged = tokio::task::spawn_blocking(move || {
        let judge = Judge {
            holders_dir: &context.holders_dir,
            instance: &context.instance,
        };
        let mut judged = judge.all(rows);
        if let Some(dir) = credentials_dir {
            give_credentials(&mut judged.survivors, &dir);
        }
        judged
    })
    .await;
    let judged = match judged {
        Ok(judged) => judged,
        Err(error) => {
            tracing::error!(%error, "cannot read the holders' records; no window is taken back");
            return unjudged;
        }
    };
    if let Some(table) = &table {
        table
            .settle_start(judged.lost.clone(), judged.removed.clone())
            .await;
    }
    let mut remaining = Remaining {
        running: judged.left.into_iter().collect(),
        lost: judged.lost.into_iter().chain(judged.already_lost).collect(),
    };
    for survivor in judged.survivors {
        let window_id = survivor.window_id.clone();
        remaining.running.insert(window_id.clone());
        match registry.take_back(survivor, begin.clone()).await {
            Ok(()) => tracing::info!(window_id, "taking the window back"),
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot take the window back; it is left as it is");
            }
        }
    }
    remaining
}

/// The reports the store kept for each window taken back, read once as the
/// daemon starts; a `lost` window's stay in the store with its row. A
/// window's inbox reads its own parcels as it is built.
pub(crate) async fn kept_reports(registry: &WindowRegistry) -> Vec<(String, Vec<KeptReport>)> {
    let Some(store) = registry.parcel_store() else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for info in registry.list() {
        let reports = store.load_reports(&info.window_id).await;
        if !reports.is_empty() {
            kept.push((info.window_id, reports));
        }
    }
    kept
}

/// What judging the rows and records came to.
#[derive(Default)]
struct Judged {
    /// Windows whose holder runs or cannot be told about, to take back.
    survivors: Vec<Survivor>,
    /// Rows to mark `lost`: their holder is gone.
    lost: Vec<String>,
    /// Rows already `lost`.
    already_lost: Vec<String>,
    /// `opening` rows to remove: their window never started.
    removed: Vec<String>,
    /// Rows left as they are, since their record could not be read.
    left: Vec<String>,
}

/// Whether a record's holder runs.
enum Holder {
    Gone,
    /// It runs, or the OS cannot tell; `None` when it cannot be named.
    Runs(Option<ProcessHandle>),
}

/// The holders directory the records are judged in.
struct Judge<'a> {
    holders_dir: &'a Path,
    instance: &'a OsStr,
}

impl Judge<'_> {
    /// Judges `rows`, in the order their windows opened, then the records no
    /// row names, in name order.
    fn all(&self, rows: Vec<StoredWindow>) -> Judged {
        let named: HashSet<String> = rows.iter().map(|row| row.window_id.clone()).collect();
        let rowless = records_without_rows(self.holders_dir, &named);
        let mut judged = Judged::default();
        for row in rows {
            self.row(row, &mut judged);
        }
        for window_id in rowless {
            self.rowless(window_id, &mut judged);
        }
        judged
    }

    fn row(&self, row: StoredWindow, judged: &mut Judged) {
        let window_id = row.window_id.clone();
        let record = match self.record(&window_id) {
            Ok(Some(record)) => record,
            // A row without a record: a window that never started its holder
            // (`opening`) or whose holder ended and removed its record.
            Ok(None) => {
                match row.state {
                    RowState::Opening => judged.removed.push(window_id),
                    RowState::Live => judged.lost.push(window_id),
                    RowState::Lost => judged.already_lost.push(window_id),
                }
                return;
            }
            Err(()) => {
                if row.state == RowState::Lost {
                    judged.already_lost.push(window_id);
                } else {
                    judged.left.push(window_id);
                }
                return;
            }
        };
        let holder = holder_of(&window_id, &record);
        match (row.state, holder) {
            (RowState::Lost, Holder::Gone) => {
                self.end_gone(&window_id, &record, Some(&row));
                judged.already_lost.push(window_id);
            }
            (RowState::Lost, Holder::Runs(_)) => judged.already_lost.push(window_id),
            // A holder that has not started the child would start it on a
            // `Hello`; it ends by itself once its first `Hello` does not come.
            (RowState::Opening, holder) if record.child_pid.is_none() => {
                if matches!(holder, Holder::Gone) {
                    self.end_gone(&window_id, &record, Some(&row));
                }
                judged.removed.push(window_id);
            }
            (_, Holder::Gone) => {
                self.end_gone(&window_id, &record, Some(&row));
                judged.lost.push(window_id);
            }
            (_, Holder::Runs(_)) if record.child_pid.is_none() => judged.left.push(window_id),
            (_, Holder::Runs(holder)) => judged.survivors.push(Survivor {
                window_id,
                stored: Some(row),
                record,
                holder,
                credentials: None,
            }),
        }
    }

    fn rowless(&self, window_id: String, judged: &mut Judged) {
        let Ok(Some(record)) = self.record(&window_id) else {
            return;
        };
        match holder_of(&window_id, &record) {
            Holder::Gone => self.end_gone(&window_id, &record, None),
            Holder::Runs(_) if record.child_pid.is_none() => {}
            Holder::Runs(holder) => judged.survivors.push(Survivor {
                window_id,
                stored: None,
                record,
                holder,
                credentials: None,
            }),
        }
    }

    /// `window_id`'s holder record; `None` when there is none, `Err` when it
    /// cannot be read or names another window.
    fn record(&self, window_id: &str) -> Result<Option<HolderRecord>, ()> {
        match read_holder_record(self.holders_dir, window_id) {
            Ok(record) if record.window_id == window_id => Ok(Some(record)),
            Ok(_) => {
                tracing::warn!(window_id, "a window holder's record names another window");
                Err(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot read a window holder's record");
                Err(())
            }
        }
    }

    /// What is left of a window whose holder is gone: on Linux the child's
    /// scope, by the name its row (or, without a row, its record) keeps; on
    /// Unix the child's process group, when the child its record names still
    /// runs; then the holder's files. Removing the files first leaves, if the
    /// daemon dies here, a `live` row without a record, which the next start
    /// marks lost.
    fn end_gone(&self, window_id: &str, record: &HolderRecord, row: Option<&StoredWindow>) {
        #[cfg(unix)]
        end_recorded_child_group(
            window_id,
            record,
            row.and_then(|row| row.values.as_ref())
                .and_then(|values| values.command.first())
                .map(String::as_str),
            self.recorded_scope(window_id, record, row).as_deref(),
        );
        #[cfg(not(unix))]
        let _ = (record, row);
        remove_dead_holder_files(&HolderFiles {
            instance: self.instance,
            holders_dir: self.holders_dir,
            window_id,
        });
    }
}

impl Judge<'_> {
    /// The child's scope name a gone holder's window keeps: its row's, or its
    /// record's when it has no row, and only when it has the form of this
    /// instance's names, since any process of the user can write either.
    #[cfg(target_os = "linux")]
    fn recorded_scope(
        &self,
        window_id: &str,
        record: &HolderRecord,
        row: Option<&StoredWindow>,
    ) -> Option<String> {
        let scope = match row {
            Some(row) => row.child_scope.clone(),
            None => record.child_scope.clone(),
        }?;
        let instance = (!self.instance.is_empty()).then_some(self.instance);
        let ours = yagura_os::resolve_instance_from(instance).is_ok_and(|namespace| {
            yagura_os::platform_unix::is_window_scope_name(&namespace, &scope)
        });
        if !ours {
            tracing::warn!(
                window_id,
                "a window's recorded scope is not a name this instance makes; it is not ended by it"
            );
            return None;
        }
        Some(scope)
    }

    /// Windows have no scope outside Linux.
    #[cfg(all(unix, not(target_os = "linux")))]
    #[allow(clippy::unused_self)]
    fn recorded_scope(
        &self,
        _window_id: &str,
        _record: &HolderRecord,
        _row: Option<&StoredWindow>,
    ) -> Option<String> {
        None
    }
}

/// Blocking: hands each of `survivors` the credentials folder in `config_dir`
/// whose file names its window.
fn give_credentials(survivors: &mut [Survivor], config_dir: &Path) {
    for (dir, window_id) in crate::window::credentials_folders(config_dir) {
        let survivor = survivors
            .iter_mut()
            .find(|survivor| window_id.as_deref() == Some(survivor.window_id.as_str()));
        if let Some(survivor) = survivor {
            survivor.credentials = Some(dir);
        }
    }
}

/// Whether the holder `record` names runs. A record from before the holder's
/// start was kept has only its pid: a pid no process has is gone, and one a
/// process has cannot be told from a reused one, so that holder is taken for
/// running without a handle.
fn holder_of(window_id: &str, record: &HolderRecord) -> Holder {
    let start = record
        .holder_start
        .as_deref()
        .and_then(|start| start.parse::<ProcessStart>().ok());
    let Some(start) = start else {
        return match yagura_os::process_start(record.holder_pid) {
            Ok(None) => Holder::Gone,
            Ok(Some(_)) => {
                tracing::warn!(
                    window_id,
                    pid = record.holder_pid,
                    "a window holder's record carries no start and its pid runs, so whether it is \
                     the holder cannot be told; the window is kept, unconnected"
                );
                Holder::Runs(None)
            }
            Err(error) => {
                tracing::warn!(window_id, %error, "whether a window's holder runs cannot be told");
                Holder::Runs(None)
            }
        };
    };
    match ProcessHandle::open(record.holder_pid, &start) {
        Ok(Some(holder)) if holder.liveness() == Liveness::Dead => Holder::Gone,
        Ok(Some(holder)) => Holder::Runs(Some(holder)),
        Ok(None) => Holder::Gone,
        Err(error) => {
            tracing::warn!(
                window_id,
                %error,
                "whether a window's holder runs cannot be told; the window is kept"
            );
            Holder::Runs(None)
        }
    }
}

/// Ends the process group the child named by a holder's `record` leads, for a
/// holder that is gone or is about to be killed outright (neither can end the
/// group itself): only while that child still runs and is the process its
/// record names, since while its leader runs the group id names no other
/// group. Otherwise nothing is ended, and the group the record names is
/// logged with `command`, the window's argv[0] when known; nothing is read
/// from any other process. On Linux, for a holder that is gone, `child_scope`
/// is the child's scope name as the window's row keeps it: the user manager
/// is asked to kill every process in it, which reaches a child that left its
/// group, and its answer is logged, not waited for. The one place a window's
/// child group is ended without its holder.
#[cfg(unix)]
pub(crate) fn end_recorded_child_group(
    window_id: &str,
    record: &HolderRecord,
    command: Option<&str>,
    child_scope: Option<&str>,
) {
    #[cfg(target_os = "linux")]
    if let Some(scope) = child_scope {
        kill_scope(window_id, scope);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = child_scope;
    let Some(child_pid) = record.child_pid.filter(|pid| *pid != 0) else {
        return;
    };
    let child = record
        .child_start
        .as_deref()
        .and_then(|start| start.parse::<ProcessStart>().ok())
        .and_then(|start| ProcessHandle::open(child_pid, &start).ok().flatten())
        .filter(|child| child.liveness() == Liveness::Alive);
    let Some(_child) = child else {
        tracing::warn!(
            window_id,
            pgid = child_pid,
            pid = child_pid,
            command = command.unwrap_or_default(),
            "the child a window holder's record names has ended or is another process now; its \
             process group is not ended"
        );
        return;
    };
    let ended = yagura_os::ProcessGroup::led_by(child_pid, yagura_os::PROCESS_GROUP_TERM_GRACE)
        .and_then(|group| group.terminate());
    if let Err(error) = ended {
        tracing::warn!(window_id, child_pid, %error, "cannot end a window's process group");
    }
}

/// Asks the user manager, on a task of its own, to kill every process of the
/// scope `scope`; a scope already gone makes the answer an error, which is
/// only logged.
#[cfg(target_os = "linux")]
fn kill_scope(window_id: &str, scope: &str) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            window_id,
            "no runtime to ask the user manager to end a window's scope"
        );
        return;
    };
    let window_id = window_id.to_owned();
    let scope = scope.to_owned();
    runtime.spawn(async move {
        let answer = match yagura_os::platform_unix::UserManager::connect().await {
            Ok(manager) => manager.kill_unit(&scope).await,
            Err(error) => Err(error),
        };
        match answer {
            Ok(()) => tracing::info!(
                window_id,
                scope,
                "asked the user manager to kill the scope of a window whose holder is gone"
            ),
            Err(error) => tracing::info!(
                window_id,
                scope,
                %error,
                "the user manager did not kill the scope of a window whose holder is gone"
            ),
        }
    });
}

/// The window ids of the records in `holders_dir` that no row of the table
/// names, in name order.
fn records_without_rows(holders_dir: &Path, named: &HashSet<String>) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(holders_dir) else {
        return Vec::new();
    };
    let mut rowless: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension() != Some(OsStr::new("json")) {
                return None;
            }
            path.file_stem()
                .and_then(OsStr::to_str)
                .filter(|window_id| !named.contains(*window_id))
                .map(str::to_owned)
        })
        .collect();
    rowless.sort();
    rowless
}

/// The slots a 0.3 store kept windows in: those open at its last write, and
/// those its last start rotated out of them.
const V0_3_SLOT_CURRENT: i64 = 0;
const V0_3_SLOT_PREV: i64 = 1;

/// The values of one 0.3 slot row, carried over verbatim: the reader of the
/// `lost` rows judges them as the untrusted input they are.
struct SlotRow {
    command_json: Value,
    cwd: Value,
    name: Value,
    vendor: Value,
    detection: Value,
    layout_json: Value,
    profile: Value,
    model: Value,
    effort: Value,
}

/// Moves, once, the windows a 0.3 store would have offered next for restoring
/// (its `current` slot when that holds a window, its `prev` slot otherwise)
/// into `lost` rows of the window table, each under a new window id no row and
/// no holder's files in `holders_dir` have, and drops the slot tables in the
/// same transaction: a store without them has been moved. Returns how many
/// windows were moved.
///
/// # Errors
///
/// [`StoreError::Sqlite`] when the store cannot be read or written; nothing is
/// moved then, and the next start tries again.
pub fn move_0_3_candidates(
    conn: &mut Connection,
    holders_dir: Option<&Path>,
) -> Result<usize, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let slot_tables: i64 = tx.query_row_fixed(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'snapshot_windows'",
        [],
        |row| row.get(0),
    )?;
    if slot_tables == 0 {
        return Ok(0);
    }
    let current: i64 = tx.query_row_fixed(
        "SELECT COUNT(*) FROM snapshot_windows WHERE slot = ?1",
        params![V0_3_SLOT_CURRENT],
        |row| row.get(0),
    )?;
    let slot = if current > 0 {
        V0_3_SLOT_CURRENT
    } else {
        V0_3_SLOT_PREV
    };
    let rows: Vec<SlotRow> = {
        let mut statement = tx.prepare_fixed(
            "SELECT command_json, cwd, name, vendor, detection, layout_json, profile, model, \
             effort FROM snapshot_windows WHERE slot = ?1 ORDER BY position",
        )?;
        let mapped = statement.query_map(params![slot], |row| {
            Ok(SlotRow {
                command_json: row.get(0)?,
                cwd: row.get(1)?,
                name: row.get(2)?,
                vendor: row.get(3)?,
                detection: row.get(4)?,
                layout_json: row.get(5)?,
                profile: row.get(6)?,
                model: row.get(7)?,
                effort: row.get(8)?,
            })
        })?;
        mapped.collect::<Result<_, _>>()?
    };
    let mut moved = 0;
    for row in rows {
        let has_row = |window_id: &str| {
            let count = tx.query_row_fixed(
                "SELECT COUNT(*) FROM windows WHERE window_id = ?1",
                params![window_id],
                |row| row.get::<_, i64>(0),
            );
            // An id that cannot be checked is treated as taken.
            !matches!(count, Ok(0))
        };
        let Some(window_id) = crate::window::free_window_id(holders_dir, has_row) else {
            tracing::warn!("no free window id for a window of the 0.3 store; it is not moved");
            continue;
        };
        tx.execute_fixed(
            "INSERT INTO windows (window_id, name, cwd, vendor, command_json, detection, profile, \
             model, effort, layout_json, credentials, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'env', 'lost')",
            params![
                window_id,
                row.name,
                row.cwd,
                row.vendor,
                row.command_json,
                row.detection,
                row.profile,
                row.model,
                row.effort,
                row.layout_json,
            ],
        )?;
        moved += 1;
    }
    tx.execute_batch_fixed(
        "DROP TABLE snapshot_windows; \
         DROP TABLE IF EXISTS snapshot_meta;",
    )?;
    tx.commit()?;
    Ok(moved)
}
