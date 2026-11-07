//! The parcels kept in `yagura.db`: what each window's inbox holds and what
//! each window's report inbox holds, so a next daemon of the instance hands
//! out what this one accepted.
//!
//! The inboxes in memory are the copy: every change to one is sent here
//! under that inbox's own lock, so the rows follow the same order, and a
//! caller that answers for an accepted parcel waits for [`ParcelStore::kept`]
//! first. A parcel is kept only while its window has a row in the window
//! table, and leaves with that row (a trigger of the schema), so a closed
//! window's parcels are gone and a `lost` window's stay.
//!
//! The rows are written on a thread of their own, over a connection of their
//! own, like the window table's.

use std::sync::mpsc;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, params};
use tokio::sync::oneshot;
use yagura_protocol::{REPORT_PATH_MAX_BYTES, REPORT_SUMMARY_MAX_BYTES, ReceivedReport};

use crate::sql::FixedSql;

/// The most parcels one window's inbox reads back: the file is writable by
/// any process of the user, so a read never grows without bound.
const MAX_KEPT_PARCELS: i64 = 1024;

/// The longest parcel text read back, in bytes. A parcel longer than its
/// window's answers carry is dropped by the inbox it is read into.
const MAX_KEPT_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// The most reports one window's report inbox reads back.
const MAX_KEPT_REPORTS: i64 = crate::ipc::report::REPORT_INBOX_MAX as i64;

/// The longest report row read back: a report's text fields within their
/// limits, with room for its names and JSON.
const MAX_REPORT_JSON_BYTES: usize = 4 * (REPORT_SUMMARY_MAX_BYTES + REPORT_PATH_MAX_BYTES) + 4096;

/// One parcel of a window's inbox as it is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptParcel {
    /// Its place in the inbox: lower is older.
    pub(crate) seq: i64,
    pub(crate) key: String,
    pub(crate) notice: bool,
    pub(crate) text: String,
}

/// One report of a window's report inbox as it is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptReport {
    /// Its place in the report inbox: lower is older.
    pub(crate) seq: i64,
    pub(crate) report: ReceivedReport,
}

enum Command {
    Keep {
        window_id: String,
        parcel: KeptParcel,
    },
    Forget {
        window_id: String,
        seqs: Vec<i64>,
    },
    Load {
        window_id: String,
        reply: oneshot::Sender<Vec<KeptParcel>>,
    },
    KeepReports {
        window_id: String,
        reports: Vec<(i64, String)>,
    },
    ForgetReports {
        window_id: String,
    },
    LoadReports {
        window_id: String,
        reply: oneshot::Sender<Vec<KeptReport>>,
    },
    Kept {
        reply: oneshot::Sender<()>,
    },
}

/// The writer of one database's kept parcels.
#[derive(Clone)]
pub(crate) struct ParcelStore {
    commands: mpsc::Sender<Command>,
}

impl std::fmt::Debug for ParcelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParcelStore").finish_non_exhaustive()
    }
}

impl ParcelStore {
    /// Starts the thread that writes the parcels over `conn`, which must
    /// already be migrated. The thread ends once every handle is dropped.
    pub(crate) fn spawn(conn: Connection) -> Self {
        let (commands, received) = mpsc::channel::<Command>();
        std::thread::spawn(move || write_parcels(&conn, &received));
        Self { commands }
    }

    fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Returns once every change sent before it is written (or failed).
    pub(crate) async fn kept(&self) {
        let (reply, written) = oneshot::channel();
        if self.send(Command::Kept { reply }) {
            // A writer that is gone writes nothing more.
            let _ = written.await;
        }
    }

    /// The parcels kept for `window_id`'s inbox, oldest first.
    pub(crate) async fn load(&self, window_id: &str) -> Vec<KeptParcel> {
        let (reply, read) = oneshot::channel();
        if !self.send(Command::Load {
            window_id: window_id.to_owned(),
            reply,
        }) {
            return Vec::new();
        }
        read.await.unwrap_or_default()
    }

    /// The reports kept for `window_id`'s report inbox, oldest first.
    pub(crate) async fn load_reports(&self, window_id: &str) -> Vec<KeptReport> {
        let (reply, read) = oneshot::channel();
        if !self.send(Command::LoadReports {
            window_id: window_id.to_owned(),
            reply,
        }) {
            return Vec::new();
        }
        read.await.unwrap_or_default()
    }

    /// Keeps `reports` at the places `seqs` gives them in `window_id`'s
    /// report inbox.
    pub(crate) fn keep_reports(&self, window_id: &str, reports: &[(i64, &ReceivedReport)]) {
        let encoded = reports
            .iter()
            .filter_map(|(seq, report)| Some((*seq, serde_json::to_string(report).ok()?)))
            .collect();
        self.send(Command::KeepReports {
            window_id: window_id.to_owned(),
            reports: encoded,
        });
    }

    /// Forgets every report kept for `window_id`.
    pub(crate) fn forget_reports(&self, window_id: &str) {
        self.send(Command::ForgetReports {
            window_id: window_id.to_owned(),
        });
    }
}

/// What one window's inbox keeps its parcels through.
#[derive(Debug, Clone)]
pub(crate) struct InboxKeeper {
    store: ParcelStore,
    window_id: String,
}

impl InboxKeeper {
    pub(crate) fn new(store: ParcelStore, window_id: &str) -> Self {
        Self {
            store,
            window_id: window_id.to_owned(),
        }
    }

    /// Keeps `parcel`, replacing what was kept at its place.
    pub(crate) fn keep(&self, parcel: KeptParcel) {
        self.store.send(Command::Keep {
            window_id: self.window_id.clone(),
            parcel,
        });
    }

    /// Forgets the parcels at `seqs`.
    pub(crate) fn forget(&self, seqs: Vec<i64>) {
        if seqs.is_empty() {
            return;
        }
        self.store.send(Command::Forget {
            window_id: self.window_id.clone(),
            seqs,
        });
    }
}

fn write_parcels(conn: &Connection, received: &mpsc::Receiver<Command>) {
    // One warning for the whole run: a broken database must not flood the log.
    let mut warned = false;
    let mut warn = |error: rusqlite::Error| {
        if !std::mem::replace(&mut warned, true) {
            tracing::warn!(%error, "failed to write the kept parcels; parcels are kept in memory only");
        }
    };
    while let Ok(command) = received.recv() {
        match command {
            Command::Keep { window_id, parcel } => {
                // Kept only while the window has a row, which takes it along
                // when it goes.
                if let Err(error) = conn.execute_fixed(
                    "INSERT INTO inbox_parcels (window_id, seq, key, notice, body) \
                     SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS \
                     (SELECT 1 FROM windows WHERE window_id = ?1) \
                     ON CONFLICT(window_id, seq) DO UPDATE SET \
                     key = excluded.key, notice = excluded.notice, body = excluded.body",
                    params![
                        window_id,
                        parcel.seq,
                        parcel.key,
                        i64::from(parcel.notice),
                        parcel.text
                    ],
                ) {
                    warn(error);
                }
            }
            Command::Forget { window_id, seqs } => {
                for seq in seqs {
                    if let Err(error) = conn.execute_fixed(
                        "DELETE FROM inbox_parcels WHERE window_id = ?1 AND seq = ?2",
                        params![window_id, seq],
                    ) {
                        warn(error);
                    }
                }
            }
            Command::Load { window_id, reply } => {
                let parcels = read_parcels(conn, &window_id).unwrap_or_else(|error| {
                    tracing::warn!(window_id, %error, "failed to read a window's kept parcels");
                    Vec::new()
                });
                let _ = reply.send(parcels);
            }
            Command::KeepReports { window_id, reports } => {
                for (seq, report_json) in reports {
                    if let Err(error) = conn.execute_fixed(
                        "INSERT INTO report_parcels (window_id, seq, report_json) \
                         SELECT ?1, ?2, ?3 WHERE EXISTS \
                         (SELECT 1 FROM windows WHERE window_id = ?1) \
                         ON CONFLICT(window_id, seq) DO UPDATE SET report_json = excluded.report_json",
                        params![window_id, seq, report_json],
                    ) {
                        warn(error);
                    }
                }
            }
            Command::ForgetReports { window_id } => {
                if let Err(error) = conn.execute_fixed(
                    "DELETE FROM report_parcels WHERE window_id = ?1",
                    params![window_id],
                ) {
                    warn(error);
                }
            }
            Command::LoadReports { window_id, reply } => {
                let reports = read_reports(conn, &window_id).unwrap_or_else(|error| {
                    tracing::warn!(window_id, %error, "failed to read a window's kept reports");
                    Vec::new()
                });
                let _ = reply.send(reports);
            }
            Command::Kept { reply } => {
                let _ = reply.send(());
            }
        }
    }
}

/// A cell's text, `None` for anything but valid UTF-8 text within
/// [`MAX_KEPT_TEXT_BYTES`].
fn text_of(cell: ValueRef<'_>) -> Option<String> {
    match cell {
        ValueRef::Text(bytes) if bytes.len() <= MAX_KEPT_TEXT_BYTES => {
            std::str::from_utf8(bytes).ok().map(str::to_owned)
        }
        _ => None,
    }
}

/// `window_id`'s kept parcels, oldest first, bounded by [`MAX_KEPT_PARCELS`].
/// A row that does not read back is skipped (warned once for the read).
fn read_parcels(conn: &Connection, window_id: &str) -> rusqlite::Result<Vec<KeptParcel>> {
    let mut statement = conn.prepare_fixed(
        "SELECT seq, key, notice, body FROM inbox_parcels WHERE window_id = ?1 \
         ORDER BY seq LIMIT ?2",
    )?;
    let rows = statement.query_map(params![window_id, MAX_KEPT_PARCELS], |row| {
        let seq = row.get::<_, i64>(0).ok();
        let key = text_of(row.get_ref(1)?);
        let notice = row.get::<_, i64>(2).ok();
        let text = text_of(row.get_ref(3)?);
        Ok(seq
            .zip(key)
            .zip(notice)
            .zip(text)
            .map(|(((seq, key), notice), text)| KeptParcel {
                seq,
                key,
                notice: notice != 0,
                text,
            }))
    })?;
    let mut parcels = Vec::new();
    let mut unreadable = false;
    for row in rows {
        match row? {
            Some(parcel) => parcels.push(parcel),
            None => unreadable = true,
        }
    }
    if unreadable {
        tracing::warn!(
            window_id,
            "skipped one or more kept parcels that do not read back"
        );
    }
    Ok(parcels)
}

/// `window_id`'s kept reports, oldest first, bounded by [`MAX_KEPT_REPORTS`];
/// a row that does not read back as a report within its limits is skipped
/// (warned once for the read).
fn read_reports(conn: &Connection, window_id: &str) -> rusqlite::Result<Vec<KeptReport>> {
    let mut statement = conn.prepare_fixed(
        "SELECT seq, report_json FROM report_parcels WHERE window_id = ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = statement.query_map(params![window_id, MAX_KEPT_REPORTS], |row| {
        let seq = row.get::<_, i64>(0).ok();
        let report = match row.get_ref(1)? {
            ValueRef::Text(bytes) if bytes.len() <= MAX_REPORT_JSON_BYTES => {
                serde_json::from_slice::<ReceivedReport>(bytes)
                    .ok()
                    .filter(within_limits)
            }
            _ => None,
        };
        Ok(seq
            .zip(report)
            .map(|(seq, report)| KeptReport { seq, report }))
    })?;
    let mut reports = Vec::new();
    let mut unreadable = false;
    for row in rows {
        match row? {
            Some(report) => reports.push(report),
            None => unreadable = true,
        }
    }
    if unreadable {
        tracing::warn!(
            window_id,
            "skipped one or more kept reports that do not read back"
        );
    }
    Ok(reports)
}

/// Whether a report read back keeps to what `send_report` accepts.
fn within_limits(report: &ReceivedReport) -> bool {
    yagura_protocol::validate_handoff_text(&report.summary, REPORT_SUMMARY_MAX_BYTES).is_ok()
        && report.path.as_deref().is_none_or(|path| {
            yagura_protocol::validate_handoff_text(path, REPORT_PATH_MAX_BYTES).is_ok()
        })
}
