//! The window table in `yagura.db`: one row for each window this daemon opened
//! and has not closed, so a next daemon can tell which windows should exist.
//!
//! A row is written `opening` before the window's holder starts, turns `live`
//! once the holder let the daemon in, follows the window's values, and is
//! removed before the holder is told to end. [`OpeningRow::open`] and
//! [`WindowRow::remove_then`] are the only ways to start and end a window
//! around its row, so neither order can be written the other way round.
//!
//! The rows are written on a thread of their own, over a connection of their
//! own; nothing here reads them back.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

use rusqlite::{Connection, params};
use serde::Serialize;
use tokio::sync::oneshot;

use crate::profile::CredentialsMode;
use crate::pty::lock_recover;
use crate::sql::FixedSql;

/// A window's values as its row and its holder's launch record carry them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WindowValues {
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) vendor: Option<String>,
    pub(crate) command: Vec<String>,
    pub(crate) detection: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) layout_json: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) opener: Option<String>,
    pub(crate) task_plan_id: Option<String>,
    pub(crate) task_id: Option<String>,
    pub(crate) credentials: &'static str,
}

/// The spelling of `mode` in the table and the launch record.
pub(crate) fn credentials_kind(mode: CredentialsMode) -> &'static str {
    match mode {
        CredentialsMode::Env => "env",
        CredentialsMode::Launch => "launch",
    }
}

/// The launch record a window's holder keeps in memory and returns in its
/// `Welcome`: enough to write the window's row again, and the window's token.
/// It is never written to a file.
#[derive(Serialize)]
struct LaunchRecord<'a> {
    window_id: &'a str,
    token: Option<&'a str>,
    window: &'a WindowValues,
}

/// Encodes the launch record of `window_id`.
pub(crate) fn launch_record(window_id: &str, token: Option<&str>, values: &WindowValues) -> String {
    // Strings only, which serde_json always encodes.
    serde_json::to_string(&LaunchRecord {
        window_id,
        token,
        window: values,
    })
    .unwrap_or_default()
}

enum Command {
    Reserve {
        window_id: String,
        values: WindowValues,
        reply: oneshot::Sender<Reserved>,
    },
    Live {
        window_id: String,
    },
    Update {
        window_id: String,
        values: WindowValues,
    },
    Remove {
        window_id: String,
        reply: Option<oneshot::Sender<()>>,
    },
}

/// What writing a window's `opening` row came to.
enum Reserved {
    Written,
    /// A row with that id is already in the table.
    Taken,
    /// The row could not be written; the window opens without one.
    Unwritten,
}

/// The writer of one database's window table.
#[derive(Clone)]
pub(crate) struct WindowTable {
    commands: mpsc::Sender<Command>,
}

impl WindowTable {
    /// Starts the thread that writes the table over `conn`, which must already
    /// be migrated. The thread ends once every handle and row is dropped.
    pub(crate) fn spawn(conn: Connection) -> Self {
        let (commands, received) = mpsc::channel::<Command>();
        std::thread::spawn(move || write_rows(&conn, &received));
        Self { commands }
    }

    fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }
}

fn write_rows(conn: &Connection, received: &mpsc::Receiver<Command>) {
    // One warning for the whole run: a broken database must not flood the log.
    let mut warned = false;
    let mut warn = |error: rusqlite::Error| {
        if !std::mem::replace(&mut warned, true) {
            tracing::warn!(%error, "failed to write the window table; windows open without rows");
        }
    };
    while let Ok(command) = received.recv() {
        match command {
            Command::Reserve {
                window_id,
                values,
                reply,
            } => {
                let reserved = match insert_opening(conn, &window_id, &values) {
                    Ok(0) => Reserved::Taken,
                    Ok(_) => Reserved::Written,
                    Err(error) => {
                        warn(error);
                        Reserved::Unwritten
                    }
                };
                let _ = reply.send(reserved);
            }
            Command::Live { window_id } => {
                if let Err(error) = conn.execute_fixed(
                    "UPDATE windows SET state = 'live' WHERE window_id = ?1",
                    params![window_id],
                ) {
                    warn(error);
                }
            }
            Command::Update { window_id, values } => {
                if let Err(error) = update_values(conn, &window_id, &values) {
                    warn(error);
                }
            }
            Command::Remove { window_id, reply } => {
                if let Err(error) = conn.execute_fixed(
                    "DELETE FROM windows WHERE window_id = ?1",
                    params![window_id],
                ) {
                    warn(error);
                }
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
            }
        }
    }
}

fn insert_opening(
    conn: &Connection,
    window_id: &str,
    values: &WindowValues,
) -> rusqlite::Result<usize> {
    let command_json = serde_json::to_string(&values.command)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    conn.execute_fixed(
        "INSERT INTO windows \
         (window_id, name, cwd, vendor, command_json, detection, profile, model, effort, \
         layout_json, session_id, opener, task_plan_id, task_id, credentials, state) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 'opening') \
         ON CONFLICT(window_id) DO NOTHING",
        params![
            window_id,
            values.name,
            values.cwd,
            values.vendor,
            command_json,
            values.detection,
            values.profile,
            values.model,
            values.effort,
            values.layout_json,
            values.session_id,
            values.opener,
            values.task_plan_id,
            values.task_id,
            values.credentials,
        ],
    )
}

fn update_values(
    conn: &Connection,
    window_id: &str,
    values: &WindowValues,
) -> rusqlite::Result<usize> {
    let command_json = serde_json::to_string(&values.command)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    conn.execute_fixed(
        "UPDATE windows SET name = ?2, cwd = ?3, vendor = ?4, command_json = ?5, \
         detection = ?6, profile = ?7, model = ?8, effort = ?9, layout_json = ?10, \
         session_id = ?11, opener = ?12, task_plan_id = ?13, task_id = ?14, credentials = ?15 \
         WHERE window_id = ?1",
        params![
            window_id,
            values.name,
            values.cwd,
            values.vendor,
            command_json,
            values.detection,
            values.profile,
            values.model,
            values.effort,
            values.layout_json,
            values.session_id,
            values.opener,
            values.task_plan_id,
            values.task_id,
            values.credentials,
        ],
    )
}

/// A window's row, from its `opening` write until it is removed. Dropped
/// without [`WindowRow::remove_then`], it is removed without waiting.
pub(crate) struct WindowRow {
    /// `None` when the registry has no table, or the row could not be written.
    table: Option<WindowTable>,
    window_id: String,
    /// Orders the value updates as the values were read.
    updates: Mutex<()>,
    removed: AtomicBool,
}

impl WindowRow {
    /// Writes the window's values as they are now; `values` is read under the
    /// row's own lock, so the last write carries every change made before it.
    pub(crate) fn update(&self, values: impl FnOnce() -> WindowValues) {
        let Some(table) = &self.table else {
            return;
        };
        let _ordered = lock_recover(&self.updates);
        if self.removed.load(Ordering::SeqCst) {
            return;
        }
        table.send(Command::Update {
            window_id: self.window_id.clone(),
            values: values(),
        });
    }

    /// Removes the row and, only once it is gone, runs `end`, which tells the
    /// window's holder to end: a daemon that dies in between leaves a holder
    /// without a row, which its record brings back, never a row whose window
    /// is gone.
    pub(crate) async fn remove_then<T>(&self, end: impl Future<Output = T>) -> T {
        if let Some(gone) = self.remove(true) {
            // A writer that is gone has nothing left to remove.
            let _ = gone.await;
        }
        end.await
    }

    /// Asks for the row's removal once; `wait` returns what tells it is done.
    fn remove(&self, wait: bool) -> Option<oneshot::Receiver<()>> {
        let table = self.table.as_ref()?;
        if self.removed.swap(true, Ordering::SeqCst) {
            return None;
        }
        let (reply, gone) = if wait {
            let (reply, gone) = oneshot::channel();
            (Some(reply), Some(gone))
        } else {
            (None, None)
        };
        table.send(Command::Remove {
            window_id: self.window_id.clone(),
            reply,
        });
        gone
    }
}

impl Drop for WindowRow {
    fn drop(&mut self) {
        let _ = self.remove(false);
    }
}

/// A window's row while it is `opening`: what [`WindowTable`] gave the id to.
pub(crate) struct OpeningRow(WindowRow);

/// What reserving a window id in the table came to.
pub(crate) enum Reservation {
    Reserved(OpeningRow),
    /// The table already has a row with the id.
    Taken,
}

impl OpeningRow {
    /// Writes `window_id`'s row as `opening` with `values`, and returns once
    /// it is written. With no `table`, or when the row cannot be written, the
    /// window opens without a row.
    pub(crate) async fn reserve(
        table: Option<&WindowTable>,
        window_id: &str,
        values: &WindowValues,
    ) -> Reservation {
        let row = |table: Option<WindowTable>| {
            Reservation::Reserved(Self(WindowRow {
                table,
                window_id: window_id.to_owned(),
                updates: Mutex::new(()),
                removed: AtomicBool::new(false),
            }))
        };
        let Some(table) = table else {
            return row(None);
        };
        let (reply, written) = oneshot::channel();
        let sent = table.send(Command::Reserve {
            window_id: window_id.to_owned(),
            values: values.clone(),
            reply,
        });
        if !sent {
            return row(None);
        }
        match written.await {
            Ok(Reserved::Written) => row(Some(table.clone())),
            Ok(Reserved::Taken) => Reservation::Taken,
            Ok(Reserved::Unwritten) | Err(_) => row(None),
        }
    }

    /// Runs `start`, which starts the window's holder and returns once the
    /// holder let the daemon in, only now that the row is in the table, and
    /// marks the row `live` once `start` succeeded. A failed start removes the
    /// row.
    pub(crate) async fn open<T, E>(
        self,
        start: impl Future<Output = Result<T, E>>,
    ) -> Result<(T, WindowRow), E> {
        let opened = start.await?;
        let row = self.0;
        if let Some(table) = &row.table {
            table.send(Command::Live {
                window_id: row.window_id.clone(),
            });
        }
        Ok((opened, row))
    }
}
