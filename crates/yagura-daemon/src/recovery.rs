//! Taking back, as a daemon starts, the windows a previous daemon of its
//! instance left running, and removing the files of a holder that is gone.
//!
//! The start has two steps. Before the daemon accepts connections, the holders'
//! records and the window table decide which windows are still there: each
//! window whose holder runs (or whose holder the OS cannot tell about) is
//! registered at once, not connected and `Unknown`, with the values its row
//! keeps, or, without a row, with only its id until its holder's launch record
//! gives the values back. Once the daemon accepts
//! connections, each window's own link lets itself into its holder; no window
//! waits for another, and nothing waits for them all.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::watch;
use yagura_os::{Liveness, ProcessHandle, ProcessStart};
use yagura_protocol::holder::{HolderRecord, holder_record_path, read_holder_record};

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
}

/// The first step of the start: registers every window a previous daemon of
/// the instance left whose holder still runs, not connected and `Unknown`,
/// before the daemon accepts connections. Each window's link lets itself into
/// its holder once `begin` turns `true`.
///
/// A window whose holder is gone is left as it is here.
pub async fn take_back_windows(registry: &Arc<WindowRegistry>, begin: watch::Receiver<bool>) {
    let (Some(table), Some(holders_dir)) =
        (registry.window_table(), registry.lasting_holders_dir())
    else {
        return;
    };
    let rows = table.rows().await;
    let found = tokio::task::spawn_blocking(move || survivors(&holders_dir, rows)).await;
    let found = match found {
        Ok(found) => found,
        Err(error) => {
            tracing::error!(%error, "cannot read the holders' records; no window is taken back");
            return;
        }
    };
    for survivor in found {
        let window_id = survivor.window_id.clone();
        match registry.take_back(survivor, begin.clone()).await {
            Ok(()) => tracing::info!(window_id, "taking the window back"),
            Err(error) => {
                tracing::warn!(window_id, %error, "cannot take the window back; it is left as it is");
            }
        }
    }
}

/// The windows whose holder's record is in `holders_dir` and whose holder
/// runs, or whose holder the OS cannot tell about: those of `rows` first, in
/// the order they were opened, then those without a row.
fn survivors(holders_dir: &Path, rows: Vec<StoredWindow>) -> Vec<Survivor> {
    let named: HashSet<String> = rows.iter().map(|row| row.window_id.clone()).collect();
    let rowless = records_without_rows(holders_dir, &named);
    rows.into_iter()
        .filter(|stored| stored.state != RowState::Lost)
        .map(|stored| (stored.window_id.clone(), Some(stored)))
        .chain(rowless.into_iter().map(|window_id| (window_id, None)))
        .filter_map(|(window_id, stored)| survivor(holders_dir, window_id, stored))
        .collect()
}

fn survivor(holders_dir: &Path, id: String, stored: Option<StoredWindow>) -> Option<Survivor> {
    let window_id = id.as_str();
    let record = match read_holder_record(holders_dir, window_id) {
        Ok(record) => record,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(window_id, %error, "cannot read a window holder's record");
            return None;
        }
    };
    if record.window_id != window_id {
        tracing::warn!(window_id, "a window holder's record names another window");
        return None;
    }
    // A holder that has not started the child would start it on a `Hello`;
    // it ends by itself once its first `Hello` does not come.
    record.child_pid?;
    let Some(start) = record
        .holder_start
        .as_deref()
        .and_then(|start| start.parse::<ProcessStart>().ok())
    else {
        tracing::warn!(
            window_id,
            "a window holder's record carries no start, so whether it runs cannot be told; \
             it is left as it is"
        );
        return None;
    };
    let holder = match ProcessHandle::open(record.holder_pid, &start) {
        Ok(Some(holder)) if holder.liveness() == Liveness::Dead => return None,
        Ok(Some(holder)) => Some(holder),
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                window_id,
                %error,
                "whether a window's holder runs cannot be told; the window is kept"
            );
            None
        }
    };
    Some(Survivor {
        window_id: id.clone(),
        stored,
        record,
        holder,
    })
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
