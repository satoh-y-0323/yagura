//! The parcels kept in `yagura.db`: what each window's inbox holds, but for
//! its unsaved parcels (`Parcel::unsaved` in `inbox.rs`), and what
//! each window's report inbox holds, so a next daemon of the instance hands
//! out what this one accepted; and each window's delivery marks, the body of
//! every delivery that may have left the daemon's text in the window's
//! composer, so a next daemon can take that text back.
//!
//! The inboxes in memory are the copy: every change to one is sent here
//! under that inbox's own lock, so the rows follow the same order. A kept
//! parcel or report is shown only once its write tells how it went
//! ([`Written`]), and a caller that answers for an accepted one answers by
//! that. A parcel is kept only while its window has a row in the window
//! table, and leaves with that row (a trigger of the schema), so a closed
//! window's parcels are gone and a `lost` window's stay.
//!
//! The rows are written on a thread of their own, over a connection of their
//! own, like the window table's. A write's transaction holds no other wait:
//! what it tells is acted on once it has committed.

use std::sync::mpsc;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Params, params};
use tokio::sync::oneshot;
use yagura_protocol::{REPORT_PATH_MAX_BYTES, REPORT_SUMMARY_MAX_BYTES, ReceivedReport};

use crate::chat_store::{ParcelEndRecord, reopen_delivery_in, settle_parcel_end_in};
use crate::room::DeliveryOutcome;
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

/// The most delivery marks one window reads back. One delivery at a time
/// writes into a window, so more than a few means a damaged file.
const MAX_KEPT_MARKS: i64 = 64;

/// How one write of kept rows ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Written {
    /// Committed, changing this many rows: none when the window has no row
    /// to keep them with.
    Rows(usize),
    /// Failed: nothing changed.
    Failed,
}

/// Told, on the writer's thread, how a parcel's write ended; returns whether
/// that showed parcels its inbox held back.
pub(crate) type OnWritten = Box<dyn FnOnce(Written) -> bool + Send>;

/// Called, on the writer's thread, with the window whose inbox a write
/// showed parcels in.
pub(crate) type OnShown = Box<dyn Fn(&str) + Send>;

/// Called, on the writer's thread, once a push's settling has committed.
pub(crate) type OnSettled = Box<dyn FnOnce() + Send>;

/// The chat delivery a kept parcel carries: recorded with its row's removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatDelivery {
    pub(crate) room_id: String,
    pub(crate) seq: i64,
}

/// One kept row a push's settling forgets, and its undoing writes back.
#[derive(Debug, Clone)]
pub(crate) struct RowEnd {
    pub(crate) parcel: KeptParcel,
    /// The chat delivery the row's removal records as taken, and its undoing
    /// as pending again.
    pub(crate) delivery: Option<ChatDelivery>,
}

/// What a typed push commits right before its first submit key, or writes
/// back when that key did not go out: the rows of what it carries, and the
/// delivery mark of its body.
struct PushRows {
    window_id: String,
    push_id: u64,
    rows: Vec<RowEnd>,
    mark: Option<String>,
}

impl PushRows {
    fn is_push(&self, window_id: &str, push_id: u64) -> bool {
        self.window_id == window_id && self.push_id == push_id
    }

    fn shares_a_row(&self, other: &Self) -> bool {
        self.window_id == other.window_id
            && self
                .rows
                .iter()
                .any(|row| other.rows.iter().any(|o| o.parcel.seq == row.parcel.seq))
    }
}

/// A write the writer owes: it failed, and is written again behind the next
/// write that succeeds.
enum Owed {
    /// A push's settling; `settled` is called once it commits.
    Settle { rows: PushRows, settled: OnSettled },
    /// The undoing of a settling that committed.
    Undo(PushRows),
}

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
        written: OnWritten,
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
        written: oneshot::Sender<Written>,
    },
    ForgetReports {
        window_id: String,
        seqs: Vec<i64>,
    },
    LoadReports {
        window_id: String,
        reply: oneshot::Sender<Vec<KeptReport>>,
    },
    Mark {
        window_id: String,
        body: String,
        reply: oneshot::Sender<()>,
    },
    Unmark {
        window_id: String,
        body: String,
    },
    LoadMarks {
        window_id: String,
        reply: oneshot::Sender<Vec<String>>,
    },
    /// Answered once the settling has committed, or has failed and is owed.
    SettlePush {
        rows: PushRows,
        settled: OnSettled,
        reply: oneshot::Sender<()>,
    },
    /// Answered once the undoing has committed, or has failed and is owed.
    UnsettlePush {
        rows: PushRows,
        reply: oneshot::Sender<()>,
    },
    /// Answered once every command sent before it is done, to a caller that
    /// blocks its thread.
    #[cfg(feature = "fault-injection")]
    KeptBlocking {
        reply: mpsc::Sender<()>,
    },
    /// The fault the writer acts out from now on.
    #[cfg(feature = "fault-injection")]
    ArmWriter(crate::fault_point::FaultPoints),
}

impl Command {
    /// Whether the command changes the database.
    #[cfg(feature = "fault-injection")]
    fn writes(&self) -> bool {
        matches!(
            self,
            Self::Keep { .. }
                | Self::Forget { .. }
                | Self::KeepReports { .. }
                | Self::ForgetReports { .. }
                | Self::Mark { .. }
                | Self::Unmark { .. }
                | Self::SettlePush { .. }
                | Self::UnsettlePush { .. }
        )
    }
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
    /// already be migrated, calling `on_shown` for a window whose inbox a
    /// write showed parcels in. The thread ends once every handle is dropped.
    pub(crate) fn spawn(conn: Connection, on_shown: OnShown) -> Self {
        let (commands, received) = mpsc::channel::<Command>();
        std::thread::spawn(move || write_parcels(&conn, &received, &on_shown));
        Self { commands }
    }

    fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Returns once every change sent before it is written (or failed),
    /// blocking the calling thread.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn kept_blocking(&self) {
        let (reply, written) = mpsc::channel();
        if self.send(Command::KeptBlocking { reply }) {
            let _ = written.recv();
        }
    }

    /// Has the writer act out `faults` from its next command on.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn arm_writer(&self, faults: crate::fault_point::FaultPoints) {
        self.send(Command::ArmWriter(faults));
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

    /// Keeps `reports` at the places their seqs give them in `window_id`'s
    /// report inbox, in one transaction; yields how that was written, and is
    /// dropped unanswered once the writer is gone.
    pub(crate) fn keep_reports(
        &self,
        window_id: &str,
        reports: &[(i64, &ReceivedReport)],
    ) -> oneshot::Receiver<Written> {
        let encoded = reports
            .iter()
            .filter_map(|(seq, report)| Some((*seq, serde_json::to_string(report).ok()?)))
            .collect();
        let (written, outcome) = oneshot::channel();
        self.send(Command::KeepReports {
            window_id: window_id.to_owned(),
            reports: encoded,
            written,
        });
        outcome
    }

    /// Forgets the reports kept at `seqs` for `window_id`.
    pub(crate) fn forget_reports(&self, window_id: &str, seqs: Vec<i64>) {
        if seqs.is_empty() {
            return;
        }
        self.send(Command::ForgetReports {
            window_id: window_id.to_owned(),
            seqs,
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

    /// Keeps `parcel`, replacing what was kept at its place, and tells
    /// `written` how that went. Called under the inbox's lock, which
    /// `written` takes, so a writer that is gone tells it on a thread of its
    /// own.
    pub(crate) fn keep(&self, parcel: KeptParcel, written: OnWritten) {
        let command = Command::Keep {
            window_id: self.window_id.clone(),
            parcel,
            written,
        };
        if let Err(mpsc::SendError(Command::Keep { written, .. })) =
            self.store.commands.send(command)
        {
            std::thread::spawn(move || written(Written::Failed));
        }
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

    /// Forgets `rows` and the delivery mark of `mark` in one transaction for
    /// push `push_id`, calling `settled` once that commits. Yields once it
    /// has committed, or has failed and is owed behind the writer's next
    /// write that succeeds; at once when the writer is gone.
    pub(crate) fn settle_push(
        &self,
        push_id: u64,
        rows: Vec<RowEnd>,
        mark: Option<String>,
        settled: OnSettled,
    ) -> oneshot::Receiver<()> {
        let (reply, done) = oneshot::channel();
        self.store.send(Command::SettlePush {
            rows: self.push_rows(push_id, rows, mark),
            settled,
            reply,
        });
        done
    }

    /// Undoes push `push_id`'s settling, whose submit key did not go out:
    /// `rows` are kept again with their chat deliveries pending, and the mark
    /// of `mark` with them, in one transaction. A settling still owed is
    /// dropped instead, as it never committed. Yields as
    /// [`Self::settle_push`] does.
    pub(crate) fn unsettle_push(
        &self,
        push_id: u64,
        rows: Vec<RowEnd>,
        mark: Option<String>,
    ) -> oneshot::Receiver<()> {
        let (reply, done) = oneshot::channel();
        self.store.send(Command::UnsettlePush {
            rows: self.push_rows(push_id, rows, mark),
            reply,
        });
        done
    }

    fn push_rows(&self, push_id: u64, rows: Vec<RowEnd>, mark: Option<String>) -> PushRows {
        PushRows {
            window_id: self.window_id.clone(),
            push_id,
            rows,
            mark,
        }
    }
}

/// What one window's deliveries keep their marks through. A body is the
/// user's text: it is kept in the store alone and never logged.
#[derive(Clone)]
pub(crate) struct MarkKeeper {
    store: ParcelStore,
    window_id: String,
}

impl std::fmt::Debug for MarkKeeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkKeeper")
            .field("window_id", &self.window_id)
            .finish_non_exhaustive()
    }
}

impl MarkKeeper {
    pub(crate) fn new(store: ParcelStore, window_id: &str) -> Self {
        Self {
            store,
            window_id: window_id.to_owned(),
        }
    }

    /// Keeps `body` as a mark of the window, returning once it is written (or
    /// failed): a delivery marks its body before its first byte.
    pub(crate) async fn mark(&self, body: &str) {
        let (reply, written) = oneshot::channel();
        if self.store.send(Command::Mark {
            window_id: self.window_id.clone(),
            body: body.to_owned(),
            reply,
        }) {
            let _ = written.await;
        }
    }

    /// Forgets the mark of `body`.
    pub(crate) fn unmark(&self, body: &str) {
        self.store.send(Command::Unmark {
            window_id: self.window_id.clone(),
            body: body.to_owned(),
        });
    }

    /// The bodies the window's marks keep.
    pub(crate) async fn marks(&self) -> Vec<String> {
        let (reply, read) = oneshot::channel();
        if !self.store.send(Command::LoadMarks {
            window_id: self.window_id.clone(),
            reply,
        }) {
            return Vec::new();
        }
        read.await.unwrap_or_default()
    }
}

/// How the writer's writes have gone this run.
#[derive(Default)]
struct WriteLog {
    /// One warning for the whole run: a broken database must not flood the
    /// log.
    warned: bool,
    /// Whether a write succeeded since this was last cleared.
    wrote: bool,
}

impl WriteLog {
    fn written_as(&mut self, result: rusqlite::Result<usize>) -> Written {
        match result {
            Ok(rows) => {
                self.wrote = true;
                Written::Rows(rows)
            }
            Err(error) => {
                if !std::mem::replace(&mut self.warned, true) {
                    tracing::warn!(%error, "failed to write the kept parcels; parcels are kept in memory only");
                }
                Written::Failed
            }
        }
    }
}

fn write_parcels(conn: &Connection, received: &mpsc::Receiver<Command>, on_shown: &OnShown) {
    let mut log = WriteLog::default();
    // A push's settling or undoing that failed: its submit key went out (or
    // did not) regardless, so it is written again behind the next write that
    // succeeds.
    let mut owed: Vec<Owed> = Vec::new();
    #[cfg(feature = "fault-injection")]
    let mut faults = crate::fault_point::FaultPoints::default();
    while let Ok(command) = received.recv() {
        #[cfg(feature = "fault-injection")]
        let fails = command.writes() && {
            faults.pause_writer();
            faults.write_fails()
        };
        #[cfg(not(feature = "fault-injection"))]
        let fails = false;
        match command {
            Command::Keep {
                window_id,
                parcel,
                written,
            } => {
                // Kept only while the window has a row, which takes it along
                // when it goes.
                let outcome = log.written_as(run(
                    conn,
                    fails,
                    KEEP_PARCEL_SQL,
                    params![
                        window_id,
                        parcel.seq,
                        parcel.key,
                        i64::from(parcel.notice),
                        parcel.text
                    ],
                ));
                if written(outcome) {
                    on_shown(&window_id);
                }
            }
            Command::Forget { window_id, seqs } => {
                for seq in seqs {
                    log.written_as(run(
                        conn,
                        fails,
                        FORGET_PARCEL_AT_SQL,
                        params![window_id, seq],
                    ));
                }
            }
            Command::Load { window_id, reply } => {
                let parcels = read_parcels(conn, &window_id).unwrap_or_else(|error| {
                    tracing::warn!(window_id, %error, "failed to read a window's kept parcels");
                    Vec::new()
                });
                let _ = reply.send(parcels);
            }
            Command::KeepReports {
                window_id,
                reports,
                written,
            } => {
                let outcome = log.written_as(keep_report_rows(conn, fails, &window_id, &reports));
                let _ = written.send(outcome);
            }
            Command::ForgetReports { window_id, seqs } => {
                for seq in seqs {
                    log.written_as(run(
                        conn,
                        fails,
                        "DELETE FROM report_parcels WHERE window_id = ?1 AND seq = ?2",
                        params![window_id, seq],
                    ));
                }
            }
            Command::LoadReports { window_id, reply } => {
                let reports = read_reports(conn, &window_id).unwrap_or_else(|error| {
                    tracing::warn!(window_id, %error, "failed to read a window's kept reports");
                    Vec::new()
                });
                let _ = reply.send(reports);
            }
            Command::Mark {
                window_id,
                body,
                reply,
            } => {
                // Kept only while the window has a row, which takes it along
                // when it goes.
                log.written_as(run(conn, fails, KEEP_MARK_SQL, params![window_id, body]));
                let _ = reply.send(());
            }
            Command::Unmark { window_id, body } => {
                log.written_as(run(conn, fails, FORGET_MARK_SQL, params![window_id, body]));
            }
            Command::LoadMarks { window_id, reply } => {
                let marks = read_marks(conn, &window_id).unwrap_or_else(|error| {
                    tracing::warn!(window_id, %error, "failed to read a window's delivery marks");
                    Vec::new()
                });
                let _ = reply.send(marks);
            }
            Command::SettlePush {
                rows,
                settled,
                reply,
            } => {
                // A settling of the same rows takes the place of an undoing
                // still owed: the two cancel out.
                owed.retain(|owed| !matches!(owed, Owed::Undo(undo) if undo.shares_a_row(&rows)));
                write_owed(
                    conn,
                    fails,
                    &mut log,
                    &mut owed,
                    Owed::Settle { rows, settled },
                );
                let _ = reply.send(());
            }
            Command::UnsettlePush { rows, reply } => {
                let before = owed.len();
                owed.retain(|owed| {
                    !matches!(owed, Owed::Settle { rows: settle, .. }
                        if settle.is_push(&rows.window_id, rows.push_id))
                });
                // A settling that never committed has nothing to undo.
                if owed.len() == before {
                    write_owed(conn, fails, &mut log, &mut owed, Owed::Undo(rows));
                }
                let _ = reply.send(());
            }
            #[cfg(feature = "fault-injection")]
            Command::KeptBlocking { reply } => {
                let _ = reply.send(());
            }
            #[cfg(feature = "fault-injection")]
            Command::ArmWriter(armed) => faults = armed,
        }
        if std::mem::take(&mut log.wrote) && !owed.is_empty() {
            for write in std::mem::take(&mut owed) {
                write_owed(conn, fails, &mut log, &mut owed, write);
            }
            log.wrote = false;
        }
    }
}

/// Writes `write` in one transaction, calling a settling's `settled` once it
/// commits, or keeps it in `owed` when that fails.
fn write_owed(
    conn: &Connection,
    fails: bool,
    log: &mut WriteLog,
    owed: &mut Vec<Owed>,
    write: Owed,
) {
    let result = if fails {
        Err(simulated_write_failure())
    } else {
        match &write {
            Owed::Settle { rows, .. } => settle_push_rows(conn, rows),
            Owed::Undo(rows) => undo_push_rows(conn, rows),
        }
    };
    match (log.written_as(result), write) {
        (Written::Rows(_), Owed::Settle { settled, .. }) => settled(),
        (Written::Rows(_), Owed::Undo(_)) => {}
        (Written::Failed, write) => owed.push(write),
    }
}

/// Forgets what `push` names in one transaction, returning how many rows it
/// changed.
fn settle_push_rows(conn: &Connection, push: &PushRows) -> rusqlite::Result<usize> {
    let at_ms = i64::try_from(crate::ipc::now_ms()).unwrap_or(i64::MAX);
    let window_id = push.window_id.as_str();
    let tx = conn.unchecked_transaction()?;
    let mut rows = 0;
    for row in &push.rows {
        match &row.delivery {
            None => {
                rows +=
                    tx.execute_fixed(FORGET_PARCEL_AT_SQL, params![window_id, row.parcel.seq])?;
            }
            Some(delivery) => {
                let end = ParcelEndRecord {
                    room_id: &delivery.room_id,
                    seq: delivery.seq,
                    window_id,
                    parcel_key: &row.parcel.key,
                    outcome: DeliveryOutcome::Taken,
                    at_ms,
                };
                rows += usize::from(settle_parcel_end_in(&tx, &end, || {})?);
            }
        }
    }
    if let Some(body) = &push.mark {
        rows += tx.execute_fixed(FORGET_MARK_SQL, params![window_id, body])?;
    }
    tx.commit()?;
    Ok(rows)
}

/// Writes back what [`settle_push_rows`] forgot for `push` in one
/// transaction, returning how many rows it changed.
fn undo_push_rows(conn: &Connection, push: &PushRows) -> rusqlite::Result<usize> {
    let at_ms = i64::try_from(crate::ipc::now_ms()).unwrap_or(i64::MAX);
    let window_id = push.window_id.as_str();
    let tx = conn.unchecked_transaction()?;
    let mut rows = 0;
    for row in &push.rows {
        rows += keep_parcel_row(&tx, window_id, &row.parcel)?;
        if let Some(delivery) = &row.delivery {
            rows += usize::from(reopen_delivery_in(
                &tx,
                &delivery.room_id,
                delivery.seq,
                window_id,
                at_ms,
            )?);
        }
    }
    if let Some(body) = &push.mark {
        rows += tx.execute_fixed(KEEP_MARK_SQL, params![window_id, body])?;
    }
    tx.commit()?;
    Ok(rows)
}

/// Forgets a parcel kept at its place in its window's inbox.
const FORGET_PARCEL_AT_SQL: &str = "DELETE FROM inbox_parcels WHERE window_id = ?1 AND seq = ?2";

/// Keeps the delivery mark of one body in its window, while the window has a
/// row, which takes it along when it goes.
const KEEP_MARK_SQL: &str = "INSERT OR IGNORE INTO delivery_marks (window_id, body) \
     SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM windows WHERE window_id = ?1)";

/// Forgets the delivery mark of one body in its window.
const FORGET_MARK_SQL: &str = "DELETE FROM delivery_marks WHERE window_id = ?1 AND body = ?2";

/// Runs one write, or fails it as a disk that cannot be written would when
/// `fails`.
fn run<P: Params>(
    conn: &Connection,
    fails: bool,
    sql: &'static str,
    params: P,
) -> rusqlite::Result<usize> {
    if fails {
        return Err(simulated_write_failure());
    }
    conn.execute_fixed(sql, params)
}

/// Writes `reports` for `window_id` in one transaction, returning how many
/// rows it wrote.
fn keep_report_rows(
    conn: &Connection,
    fails: bool,
    window_id: &str,
    reports: &[(i64, String)],
) -> rusqlite::Result<usize> {
    if fails {
        return Err(simulated_write_failure());
    }
    let tx = conn.unchecked_transaction()?;
    let mut rows = 0;
    for (seq, report_json) in reports {
        rows += keep_report_row(&tx, window_id, *seq, report_json)?;
    }
    tx.commit()?;
    Ok(rows)
}

/// Keeps a parcel at its place in its window's inbox, while the window has a
/// row to keep it with.
const KEEP_PARCEL_SQL: &str = "INSERT INTO inbox_parcels (window_id, seq, key, notice, body) \
     SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS \
     (SELECT 1 FROM windows WHERE window_id = ?1) \
     ON CONFLICT(window_id, seq) DO UPDATE SET \
     key = excluded.key, notice = excluded.notice, body = excluded.body";

/// Writes `parcel` for `window_id` over `conn`, which may be inside a
/// transaction of its own, returning how many rows it wrote: none when the
/// window has no row.
pub(crate) fn keep_parcel_row(
    conn: &Connection,
    window_id: &str,
    parcel: &KeptParcel,
) -> rusqlite::Result<usize> {
    conn.execute_fixed(
        KEEP_PARCEL_SQL,
        params![
            window_id,
            parcel.seq,
            parcel.key,
            i64::from(parcel.notice),
            parcel.text
        ],
    )
}

/// Forgets the parcel named `key` in `window_id`'s inbox over `conn`, which
/// may be inside a transaction of its own.
pub(crate) fn forget_parcel_row(
    conn: &Connection,
    window_id: &str,
    key: &str,
) -> rusqlite::Result<usize> {
    conn.execute_fixed(
        "DELETE FROM inbox_parcels WHERE window_id = ?1 AND key = ?2",
        params![window_id, key],
    )
}

/// Writes the report `report_json` at `seq` in `window_id`'s report inbox over
/// `conn`, which may be inside a transaction of its own, returning how many
/// rows it wrote: none when the window has no row.
pub(crate) fn keep_report_row(
    conn: &Connection,
    window_id: &str,
    seq: i64,
    report_json: &str,
) -> rusqlite::Result<usize> {
    conn.execute_fixed(
        "INSERT INTO report_parcels (window_id, seq, report_json) \
         SELECT ?1, ?2, ?3 WHERE EXISTS \
         (SELECT 1 FROM windows WHERE window_id = ?1) \
         ON CONFLICT(window_id, seq) DO UPDATE SET report_json = excluded.report_json",
        params![window_id, seq, report_json],
    )
}

/// The error a write fails with under the `write_fails` fault point: the one
/// a disk that cannot be written returns.
pub(crate) fn simulated_write_failure() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
        Some("simulated write failure".to_owned()),
    )
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

/// `window_id`'s delivery marks, bounded by [`MAX_KEPT_MARKS`]; a body that
/// does not read back as text within [`MAX_KEPT_TEXT_BYTES`] is skipped.
fn read_marks(conn: &Connection, window_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare_fixed(
        "SELECT body FROM delivery_marks WHERE window_id = ?1 ORDER BY body LIMIT ?2",
    )?;
    let rows = statement.query_map(params![window_id, MAX_KEPT_MARKS], |row| {
        Ok(text_of(row.get_ref(0)?))
    })?;
    let mut marks = Vec::new();
    for row in rows {
        if let Some(body) = row? {
            marks.push(body);
        }
    }
    Ok(marks)
}

/// Whether a report read back keeps to what `send_report` accepts.
fn within_limits(report: &ReceivedReport) -> bool {
    yagura_protocol::validate_handoff_text(&report.summary, REPORT_SUMMARY_MAX_BYTES).is_ok()
        && report.path.as_deref().is_none_or(|path| {
            yagura_protocol::validate_handoff_text(path, REPORT_PATH_MAX_BYTES).is_ok()
        })
}
