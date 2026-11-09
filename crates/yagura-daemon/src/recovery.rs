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

#[cfg(unix)]
impl HolderFiles<'_> {
    fn to_owned(&self) -> OwnedHolderFiles {
        OwnedHolderFiles {
            instance: self.instance.to_owned(),
            holders_dir: self.holders_dir.to_owned(),
            window_id: self.window_id.to_owned(),
        }
    }
}

/// [`HolderFiles`] owned, to be carried to a thread of their own.
#[cfg(unix)]
struct OwnedHolderFiles {
    instance: std::ffi::OsString,
    holders_dir: PathBuf,
    window_id: String,
}

#[cfg(unix)]
impl OwnedHolderFiles {
    fn borrow(&self) -> HolderFiles<'_> {
        HolderFiles {
            instance: &self.instance,
            holders_dir: &self.holders_dir,
            window_id: &self.window_id,
        }
    }
}

/// What follows the known end of a window's holder, the same for every way
/// it ended (closed, killed here, or found gone): on Unix, on a thread of its
/// own, the holder's endpoint lock is taken once every process holding it
/// (the holder and its watcher) has let it go; under that lock a record still
/// there has the child's tree ended from outside it, without waiting for any
/// of it, and the holder's files are removed. `keep` is held until then.
/// Nothing is waited for here: the caller has published what the window's
/// end makes known first.
///
/// The lock's wait has no bound and is never repeated: the kernel wakes it as
/// the last holder of the lock closes it. A watcher stopped from outside
/// keeps it, and the files, until it goes on (and removes them itself) or
/// dies. Paths are derived from the holders directory and the window id;
/// a record naming another endpoint, or that is a link, is left in place.
pub(crate) fn finish_after_holder<K: Send + 'static>(files: &HolderFiles<'_>, keep: K) {
    #[cfg(unix)]
    {
        let owned = files.to_owned();
        let started = std::thread::Builder::new()
            .name("holder-release".to_owned())
            .spawn(move || {
                finish_under_lock(&owned.borrow());
                drop(keep);
            });
        if let Err(error) = started {
            tracing::warn!(
                window_id = files.window_id,
                %error,
                "cannot start the thread that ends a gone holder's window; its files are left for \
                 the next start"
            );
        }
    }
    #[cfg(not(unix))]
    {
        // The holder's job ended the child's tree with it, and no lock guards
        // the record.
        if let Ok(record_path) = holder_record_path(files.holders_dir, files.window_id)
            && record_is_removable(files.window_id, &record_path)
            && let Err(error) = std::fs::remove_file(&record_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                window_id = files.window_id,
                %error,
                "cannot remove a window holder's record"
            );
        }
        drop(keep);
    }
}

/// Whether the record at `record_path` may be removed: it is not a link.
fn record_is_removable(window_id: &str, record_path: &Path) -> bool {
    match std::fs::symlink_metadata(record_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(
                window_id,
                "a window holder's record is a link; it is left in place"
            );
            false
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(window_id, %error, "cannot read a window holder's record");
            false
        }
    }
}

/// Blocking, without a bound: see [`finish_after_holder`].
#[cfg(unix)]
fn finish_under_lock(files: &HolderFiles<'_>) {
    use std::os::unix::fs::FileTypeExt as _;

    use yagura_os::platform_unix::{
        endpoint_lock_path, lock_endpoint_blocking, remove_endpoint_files_under_lock,
    };

    let window_id = files.window_id;
    let Ok(record_path) = holder_record_path(files.holders_dir, window_id) else {
        return;
    };
    let Some(endpoint) = derived_endpoint(files) else {
        return;
    };
    let socket = Path::new(&endpoint);
    let lock_path = endpoint_lock_path(socket);
    let left = [record_path.as_path(), socket, lock_path.as_path()]
        .iter()
        .any(|path| std::fs::symlink_metadata(path).is_ok());
    if !left {
        return;
    }
    let lock = match lock_endpoint_blocking(socket) {
        Ok(Some(lock)) => lock,
        // Its last holder removed the lock file on the way out, after the rest.
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(window_id, %error, "cannot take a gone holder's endpoint lock");
            return;
        }
    };
    if !record_is_removable(window_id, &record_path) {
        return;
    }
    match read_holder_record(files.holders_dir, window_id) {
        Ok(record) if record.window_id != window_id || record.endpoint != endpoint => {
            tracing::warn!(
                window_id,
                "a window holder's record names another window or endpoint; its files are left in \
                 place"
            );
            return;
        }
        Ok(record) => end_recorded_tree(window_id, &record),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(window_id, %error, "cannot read a window holder's record"),
    }
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if !meta.file_type().is_socket() => {
            tracing::warn!(
                window_id,
                "a window holder's endpoint is not a socket; its files are left in place"
            );
            return;
        }
        _ => {}
    }
    if let Err(error) = remove_endpoint_files_under_lock(&record_path, socket, &lock_path) {
        tracing::warn!(window_id, %error, "cannot remove a gone holder's files");
    }
    drop(lock);
}

/// The endpoint a window's holder serves, derived from the instance, the
/// user and the holders directory, never read from its record.
#[cfg(unix)]
fn derived_endpoint(files: &HolderFiles<'_>) -> Option<String> {
    use yagura_os::PeerAuth as _;

    let instance = (!files.instance.is_empty()).then_some(files.instance);
    let derived = yagura_os::resolve_instance_from(instance)
        .map_err(|error| error.to_string())
        .and_then(|namespace| {
            let user_id = yagura_os::PlatformIpc::default()
                .current_user_id()
                .map_err(|error| error.to_string())?;
            yagura_os::holder_endpoint(&user_id, &namespace, files.holders_dir, files.window_id)
                .map_err(|error| error.to_string())
        });
    match derived {
        Ok(endpoint) => Some(endpoint),
        Err(error) => {
            tracing::warn!(
                window_id = files.window_id,
                %error,
                "cannot name a window holder's endpoint"
            );
            None
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
    let context_files = (context.instance.clone(), context.holders_dir.clone());
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
    finish_gone_once_accepting(context_files, judged.gone, begin.clone());
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
    /// Windows whose holder is gone, what follows whose end is left until the
    /// daemon accepts connections.
    gone: Vec<String>,
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
    // Read only to check a recorded scope's name (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
                self.end_gone(&window_id, &record, Some(&row), judged);
                judged.already_lost.push(window_id);
            }
            (RowState::Lost, Holder::Runs(_)) => judged.already_lost.push(window_id),
            // A holder that has not started the child would start it on a
            // `Hello`; it ends by itself once its first `Hello` does not come.
            (RowState::Opening, holder) if record.child_pid.is_none() => {
                if matches!(holder, Holder::Gone) {
                    self.end_gone(&window_id, &record, Some(&row), judged);
                }
                judged.removed.push(window_id);
            }
            (_, Holder::Gone) => {
                self.end_gone(&window_id, &record, Some(&row), judged);
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
            Holder::Gone => self.end_gone(&window_id, &record, None, judged),
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

    /// What is left of a window whose holder is gone: on Linux the user
    /// manager is asked to kill the child's scope, by the name its row (or,
    /// without a row, its record) keeps; then the window is handed to
    /// [`finish_after_holder`] once the daemon accepts connections. Its files
    /// go only after its tree is ended, so a daemon that dies first leaves
    /// them to the next start's judging.
    fn end_gone(
        &self,
        window_id: &str,
        record: &HolderRecord,
        row: Option<&StoredWindow>,
        judged: &mut Judged,
    ) {
        #[cfg(target_os = "linux")]
        if let Some(scope) = self.recorded_scope(window_id, record, row) {
            kill_scope(window_id, &scope);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (record, row);
        judged.gone.push(window_id.to_owned());
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

/// Ends, from outside it, the tree whose leader is the child a holder's
/// `record` names, without waiting for any of it: only while that child
/// still runs as the process the record names, so nothing outside the
/// window is signalled.
#[cfg(unix)]
pub(crate) fn end_recorded_tree(window_id: &str, record: &HolderRecord) {
    let Some(child_pid) = record.child_pid.filter(|pid| *pid != 0) else {
        return;
    };
    let Some(start) = record
        .child_start
        .as_deref()
        .and_then(|start| start.parse::<ProcessStart>().ok())
    else {
        tracing::warn!(
            window_id,
            pid = child_pid,
            "a window holder's record carries no start of its child; its tree is not ended"
        );
        return;
    };
    match yagura_os::platform_unix::TreeEnd::not_parent(child_pid, start).run(&mut PlainSends) {
        Ok(rest) => {
            let left = rest.unsent.len() + rest.unended.len() + rest.unwaited;
            if left > 0 {
                tracing::warn!(
                    window_id,
                    child_pid,
                    left,
                    "part of a window's tree could not be signalled"
                );
            }
        }
        Err(error) => tracing::warn!(window_id, child_pid, %error, "cannot end a window's tree"),
    }
}

/// Sends as they are.
#[cfg(unix)]
struct PlainSends;

#[cfg(unix)]
impl yagura_os::platform_unix::TreeEndHooks for PlainSends {}

/// Hands each of the `gone` windows of the holders directory `files` names
/// (with its instance) to [`finish_after_holder`] once `begin` turns `true`.
fn finish_gone_once_accepting(
    (instance, holders_dir): (std::ffi::OsString, PathBuf),
    gone: Vec<String>,
    mut begin: watch::Receiver<bool>,
) {
    if gone.is_empty() {
        return;
    }
    tokio::spawn(async move {
        if begin.wait_for(|began| *began).await.is_err() {
            return;
        }
        for window_id in &gone {
            finish_after_holder(
                &HolderFiles {
                    instance: &instance,
                    holders_dir: &holders_dir,
                    window_id,
                },
                (),
            );
        }
    });
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
