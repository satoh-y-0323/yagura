//! The window table in `yagura.db`: one row for each window this daemon opened
//! and has not closed, so a next daemon can tell which windows should exist.
//!
//! A row is written `opening` before the window's holder starts, takes the
//! values the start settled before the holder starts too, turns `live` once
//! the holder let the daemon in, follows the window's values, and is
//! removed before the holder is told to end. [`OpeningRow::open`] and
//! [`WindowRow::remove_then`] are the only ways to start and end a window
//! around its row, so neither order can be written the other way round.
//!
//! The rows are written on a thread of their own, over a connection of their
//! own; a next daemon reads them back once, as it starts
//! ([`WindowTable::rows`]), to take back the windows they name.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, params};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use yagura_protocol::holder::HolderScopes;

use crate::profile::{CredentialsMode, HookTimeouts};
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
    /// The route the window's hooks take; `None` until the window's spawn
    /// settled it.
    pub(crate) hooks: Option<HooksRoute>,
    pub(crate) facts: WindowFacts,
}

/// What a window's open settled about the window for its whole life, kept
/// with its row and its holder's launch record and read back when the window
/// is taken back. Each fact is `None` in a row or record written before the
/// fact was kept, which its reader takes the safe way.
///
/// A new fact is a field here, its column in `read_rows`, `insert_opening` and
/// `update_values` (and the migration adding it), and its rule in
/// [`WindowFacts::or`]; the launch record carries the whole struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowFacts {
    /// Whether the window's CLI runs in another environment (a WSL guest):
    /// its open converts the daemon's paths for it (`[host_paths]`).
    pub runs_in_guest: Option<bool>,
    /// Whether the launch overlay of a window whose profile declares one was
    /// made; `None` for a window whose profile declared none when it opened.
    pub overlay: Option<OverlayFact>,
    /// Where the window's wake file is: the daemon can ask the window's CLI
    /// to wait again only through it. `None` for a window whose hooks were
    /// not injected, whose answers name no `{wake_paths}`, or whose open
    /// found no place for the file.
    pub wake: Option<crate::wake_file::WakeRecord>,
    /// The times the window's CLI gives its hooks: the `[hook_timeout]` table
    /// of the profile the window was opened from, as it was then. `None` for a
    /// window opened from no profile, or kept before the fact was.
    #[serde(deserialize_with = "kept_hook_timeouts")]
    pub hook_timeouts: Option<HookTimeouts>,
}

/// Reads back a kept `[hook_timeout]` table ([`HookTimeouts::kept`]): one
/// that breaks the table's rule is no table.
fn kept_hook_timeouts<'de, D>(deserializer: D) -> Result<Option<HookTimeouts>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let kept = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(kept.and_then(HookTimeouts::kept))
}

impl WindowFacts {
    /// `self`, with each fact it does not hold taken from `other`.
    #[must_use]
    pub(crate) fn or(self, other: Self) -> Self {
        Self {
            runs_in_guest: self.runs_in_guest.or(other.runs_in_guest),
            overlay: self.overlay.or(other.overlay),
            wake: self.wake.or(other.wake),
            hook_timeouts: self.hook_timeouts.or(other.hook_timeouts),
        }
    }

    /// `self` for a window whose hooks settled on `route` at spawn: its CLI
    /// learns of its wake file only through the injected hooks.
    #[must_use]
    pub(crate) fn settled(self, route: HooksRoute) -> Self {
        Self {
            wake: self.wake.filter(|_| route == HooksRoute::Injection),
            ..self
        }
    }
}

/// Whether a window's launch overlay was made: until it is, the window's
/// credentials do not speak for it.
///
/// Its spelling in the table and the launch record is its serde name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayFact {
    /// Not yet: the window's CLI runs in another environment, whose overlay
    /// door has not asked for it.
    Pending,
    /// The daemon made it for the window's launch.
    Made,
}

impl OverlayFact {
    /// The fact `kind` spells; `None` for a spelling that is no fact's, which
    /// its reader takes as not kept.
    fn named(kind: &str) -> Option<Self> {
        let kind: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
            kind.into_deserializer();
        Self::deserialize(kind).ok()
    }
}

impl ToSql for OverlayFact {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(match self {
            Self::Pending => "pending",
            Self::Made => "made",
        }))
    }
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

/// The spelling `kind` names in the table and the launch record, back as
/// what a window is opened with; anything else is the default, `env`.
pub(crate) fn credentials_mode(kind: &str) -> CredentialsMode {
    if kind == credentials_kind(CredentialsMode::Launch) {
        CredentialsMode::Launch
    } else {
        CredentialsMode::Env
    }
}

/// How a window's CLI comes to run the daemon's hooks: the one fact both
/// whether the window expects status reports and whether it takes part in
/// delivery are read from.
///
/// Its spelling in the table and the launch record is its serde name, both
/// ways ([`route_named`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HooksRoute {
    /// The daemon arranged no hooks: no reports are expected, and a declared
    /// `[delivery]` relies on the CLI's own configuration.
    #[default]
    Unarranged,
    /// The launch's `[hooks]` injection. Settled at spawn: it stays only
    /// when the spawned argv still carries the injection.
    Injection,
    /// Hooks the CLI reads from its own settings, whose registered entries
    /// were read and held at launch.
    RegisteredEntries,
    /// The daemon arranged hooks, but they do not reach the CLI: their
    /// registration did not hold, their settings could not be written, the
    /// command interpreter would have reinterpreted them, or there was
    /// nowhere to write them.
    LeftOut,
}

impl HooksRoute {
    /// The route as it really is once the argv is spawned:
    /// `hooks_in_argv` is whether a `[hooks]` injection is in it.
    #[must_use]
    pub fn settle(self, hooks_in_argv: bool) -> Self {
        match self {
            _ if hooks_in_argv => Self::Injection,
            Self::Injection => Self::LeftOut,
            other => other,
        }
    }

    /// Whether the CLI runs hooks the daemon arranged, so it is expected to
    /// report its turns.
    #[must_use]
    pub fn reaches(self) -> bool {
        matches!(self, Self::Injection | Self::RegisteredEntries)
    }
}

/// The route `kind` names in the table and the launch record, back as what a
/// window is built with. Any spelling that is no route's is `LeftOut`: a
/// route not known is taken for hooks that do not reach the CLI.
pub(crate) fn route_named(kind: &str) -> HooksRoute {
    let kind: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
        kind.into_deserializer();
    HooksRoute::deserialize(kind).unwrap_or(HooksRoute::LeftOut)
}

/// The route a window taken back is built with: `LeftOut` without one (a row
/// written before the window's spawn settled it, a record of an older holder).
pub(crate) fn route_taken_back(hooks: Option<HooksRoute>) -> HooksRoute {
    hooks.unwrap_or(HooksRoute::LeftOut)
}

impl ToSql for HooksRoute {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(kind)) => Ok(ToSqlOutput::from(kind)),
            Ok(other) => Err(rusqlite::Error::ToSqlConversionFailure(
                format!("a hooks route spelled as {other}").into(),
            )),
            Err(error) => Err(rusqlite::Error::ToSqlConversionFailure(Box::new(error))),
        }
    }
}

/// Where a row's window stands, as the table records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowState {
    /// Written before the window's holder started.
    Opening,
    /// The holder let the daemon in.
    Live,
    /// The holder ended without the window being closed.
    Lost,
}

/// One row of the table as a next daemon reads it.
#[derive(Debug, Clone)]
pub(crate) struct StoredWindow {
    pub(crate) window_id: String,
    /// `None` for a `lost` row, and for a row whose values do not read back.
    pub(crate) values: Option<WindowValues>,
    pub(crate) state: RowState,
    /// The systemd user scope the window's child was asked to be moved into
    /// (Linux), as written before the holder asked for it; any process of
    /// the user can write the file, so a reader checks its form.
    // Read only where windows have scopes (Linux).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) child_scope: Option<String>,
}

/// The scope names of a window's holder start, as its `opening` row was
/// written with them: only an [`OpeningRow`] whose row is in the table gives
/// them out, so a holder is never handed a name a next daemon cannot read back.
#[derive(Debug, Clone)]
pub(crate) struct RowScopes(HolderScopes);

impl RowScopes {
    /// The names as a holder's launch carries them.
    pub(crate) fn launch(&self) -> HolderScopes {
        self.0.clone()
    }
}

/// What a holder's launch record gives back, read from the record its
/// `Welcome` returns.
pub(crate) struct RecordedLaunch {
    pub(crate) token: Option<String>,
    pub(crate) values: WindowValues,
}

/// Reads back the launch record `record` of `window_id`; `None` when it is
/// not one, or names another window.
pub(crate) fn recorded_launch(record: &str, window_id: &str) -> Option<RecordedLaunch> {
    #[derive(serde::Deserialize)]
    struct Values {
        name: String,
        cwd: String,
        vendor: Option<String>,
        command: Vec<String>,
        detection: Option<String>,
        profile: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        layout_json: Option<String>,
        session_id: Option<String>,
        opener: Option<String>,
        task_plan_id: Option<String>,
        task_id: Option<String>,
        credentials: String,
        // Absent from the record of a holder older than the field.
        #[serde(default)]
        hooks: Option<String>,
        #[serde(default)]
        facts: WindowFacts,
    }
    #[derive(serde::Deserialize)]
    struct Recorded {
        window_id: String,
        token: Option<String>,
        window: Values,
    }
    let recorded: Recorded = serde_json::from_str(record).ok()?;
    if recorded.window_id != window_id {
        return None;
    }
    let values = recorded.window;
    Some(RecordedLaunch {
        token: recorded.token,
        values: WindowValues {
            name: values.name,
            cwd: values.cwd,
            vendor: values.vendor,
            command: values.command,
            detection: values.detection,
            profile: values.profile,
            model: values.model,
            effort: values.effort,
            layout_json: values.layout_json,
            session_id: values.session_id,
            opener: values.opener,
            task_plan_id: values.task_plan_id,
            task_id: values.task_id,
            credentials: credentials_kind(credentials_mode(&values.credentials)),
            hooks: values.hooks.as_deref().map(route_named),
            facts: values.facts,
        },
    })
}

enum Command {
    Rows {
        reply: oneshot::Sender<Vec<StoredWindow>>,
    },
    Lost {
        window_id: String,
    },
    Settle {
        lost: Vec<String>,
        removed: Vec<String>,
        reply: oneshot::Sender<()>,
    },
    Reserve {
        window_id: String,
        values: WindowValues,
        child_scope: Option<String>,
        reply: oneshot::Sender<Reserved>,
    },
    Live {
        window_id: String,
    },
    Update {
        window_id: String,
        values: WindowValues,
        /// Told whether the row took `values`.
        reply: oneshot::Sender<bool>,
    },
    Remove {
        window_id: String,
        reply: Option<oneshot::Sender<()>>,
    },
    /// The fault the writer acts out from now on.
    #[cfg(feature = "fault-injection")]
    ArmWriter(crate::fault_point::FaultPoints),
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

    /// Has the writer act out `faults` from its next command on.
    #[cfg(feature = "fault-injection")]
    pub(crate) fn arm_writer(&self, faults: crate::fault_point::FaultPoints) {
        self.send(Command::ArmWriter(faults));
    }

    /// Every row, in the order the windows were opened; none when the rows
    /// cannot be read.
    pub(crate) async fn rows(&self) -> Vec<StoredWindow> {
        let (reply, read) = oneshot::channel();
        if !self.send(Command::Rows { reply }) {
            return Vec::new();
        }
        read.await.unwrap_or_default()
    }

    /// Writes `window_id`'s row again from `values`, `live`, for a window whose
    /// holder runs without one, or whose row's values do not read back; a row
    /// already there keeps its state and takes `values`.
    pub(crate) async fn restore_row(&self, window_id: &str, values: &WindowValues) -> WindowRow {
        match OpeningRow::reserve(Some(self), window_id, values, None).await {
            Reservation::Reserved(row) => {
                let opened: Result<((), WindowRow), std::convert::Infallible> =
                    row.open(async { Ok(()) }).await;
                match opened {
                    Ok(((), row)) => row,
                    Err(never) => match never {},
                }
            }
            Reservation::Taken => {
                let row = WindowRow::taken_back(Some(self.clone()), window_id);
                row.update(|| values.clone());
                row
            }
        }
    }

    /// Writes what the first step of a start judged, and returns once it is
    /// written: the rows of `lost` turn `lost`, and the `opening` rows of
    /// `removed` are removed.
    pub(crate) async fn settle_start(&self, lost: Vec<String>, removed: Vec<String>) {
        let (reply, written) = oneshot::channel();
        if self.send(Command::Settle {
            lost,
            removed,
            reply,
        }) {
            // A writer that is gone has written nothing more.
            let _ = written.await;
        }
    }

    /// Marks `window_id`'s row `lost` if it is still there: only a row that
    /// is `opening` or `live` changes, so a window removed by a close never
    /// comes back as lost.
    pub(crate) fn mark_lost(&self, window_id: &str) {
        self.send(Command::Lost {
            window_id: window_id.to_owned(),
        });
    }
}

/// A cell's text, `None` for anything but valid UTF-8 text.
fn text_of(cell: ValueRef<'_>) -> Option<String> {
    match cell {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
        _ => None,
    }
}

/// Every row, in the order the windows were opened. A `lost` row's values are
/// not read here: only the reader of the restore candidates uses them, and
/// judges them there. The file is writable by any process of the user, so a
/// row whose id does not read back is skipped, and a row of another state
/// whose values do not read back comes without them, its window taking them
/// from its holder's launch record; either warns once for the read.
fn read_rows(conn: &Connection) -> rusqlite::Result<Vec<StoredWindow>> {
    let mut statement = conn.prepare_fixed(
        "SELECT window_id, state, name, cwd, vendor, command_json, detection, profile, model, \
         effort, layout_json, session_id, opener, task_plan_id, task_id, credentials, \
         child_scope, hooks, guest, overlay, wake, hook_timeouts FROM windows \
         ORDER BY opened_seq",
    )?;
    let rows = statement.query_map([], |row| {
        let Some(window_id) = text_of(row.get_ref(0)?) else {
            return Ok(None);
        };
        let state = match text_of(row.get_ref(1)?).as_deref() {
            Some("opening") => RowState::Opening,
            Some("live") => RowState::Live,
            _ => RowState::Lost,
        };
        let values = if state == RowState::Lost {
            None
        } else {
            row_values(row).ok()
        };
        let child_scope = text_of(row.get_ref(16)?);
        Ok(Some(StoredWindow {
            window_id,
            values,
            state,
            child_scope,
        }))
    })?;
    let mut read = Vec::new();
    let mut unreadable = false;
    for row in rows {
        match row? {
            Some(row) => {
                unreadable |= row.state != RowState::Lost && row.values.is_none();
                read.push(row);
            }
            None => unreadable = true,
        }
    }
    if unreadable {
        tracing::warn!(
            "one or more window table rows do not read back; their windows take their values \
             from their holders"
        );
    }
    Ok(read)
}

/// The values of a row read by [`read_rows`], from its third column on.
fn row_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<WindowValues> {
    let command_json: String = row.get(5)?;
    let credentials: String = row.get(15)?;
    Ok(WindowValues {
        name: row.get(2)?,
        cwd: row.get(3)?,
        vendor: row.get(4)?,
        // A row whose command does not read back is still a window.
        command: serde_json::from_str(&command_json).unwrap_or_default(),
        detection: row.get(6)?,
        profile: row.get(7)?,
        model: row.get(8)?,
        effort: row.get(9)?,
        layout_json: row.get(10)?,
        session_id: row.get(11)?,
        opener: row.get(12)?,
        task_plan_id: row.get(13)?,
        task_id: row.get(14)?,
        credentials: credentials_kind(credentials_mode(&credentials)),
        hooks: text_of(row.get_ref(17)?).as_deref().map(route_named),
        facts: WindowFacts {
            // Any process of the user can write the file: a cell that is not
            // a flag reads as not kept.
            runs_in_guest: row.get::<_, Option<bool>>(18).ok().flatten(),
            overlay: text_of(row.get_ref(19)?)
                .as_deref()
                .and_then(OverlayFact::named),
            wake: text_of(row.get_ref(20)?)
                .as_deref()
                .and_then(crate::wake_file::WakeRecord::named),
            hook_timeouts: text_of(row.get_ref(21)?)
                .and_then(|kept| serde_json::from_str(&kept).ok())
                .and_then(HookTimeouts::kept),
        },
    })
}

const MARK_LOST_SQL: &str = "UPDATE windows SET state = 'lost' \
     WHERE window_id = ?1 AND state IN ('opening', 'live')";

fn write_rows(conn: &Connection, received: &mpsc::Receiver<Command>) {
    // One warning for the whole run: a broken database must not flood the log.
    let mut warned = false;
    let mut warn = |error: rusqlite::Error| {
        if !std::mem::replace(&mut warned, true) {
            tracing::warn!(%error, "failed to write the window table; windows open without rows");
        }
    };
    #[cfg(feature = "fault-injection")]
    let mut faults = crate::fault_point::FaultPoints::default();
    while let Ok(command) = received.recv() {
        match command {
            Command::Rows { reply } => {
                let rows = read_rows(conn).unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to read the window table; no window is taken back");
                    Vec::new()
                });
                let _ = reply.send(rows);
            }
            Command::Lost { window_id } => {
                if let Err(error) = conn.execute_fixed(MARK_LOST_SQL, params![window_id]) {
                    warn(error);
                }
            }
            Command::Settle {
                lost,
                removed,
                reply,
            } => {
                for window_id in lost {
                    if let Err(error) = conn.execute_fixed(MARK_LOST_SQL, params![window_id]) {
                        warn(error);
                    }
                }
                for window_id in removed {
                    if let Err(error) = conn.execute_fixed(
                        "DELETE FROM windows WHERE window_id = ?1 AND state = 'opening'",
                        params![window_id],
                    ) {
                        warn(error);
                    }
                }
                let _ = reply.send(());
            }
            Command::Reserve {
                window_id,
                values,
                child_scope,
                reply,
            } => {
                let reserved = match insert_opening(conn, &window_id, &values, child_scope) {
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
            Command::Update {
                window_id,
                values,
                reply,
            } => {
                #[cfg(feature = "fault-injection")]
                let updated = {
                    faults.pause_row_update();
                    if faults.row_update_fails() {
                        Err(crate::fault_point::simulated_write_failure())
                    } else {
                        update_values(conn, &window_id, &values)
                    }
                };
                #[cfg(not(feature = "fault-injection"))]
                let updated = update_values(conn, &window_id, &values);
                let written = match updated {
                    Ok(changed) => changed > 0,
                    Err(error) => {
                        warn(error);
                        false
                    }
                };
                let _ = reply.send(written);
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
            #[cfg(feature = "fault-injection")]
            Command::ArmWriter(armed) => faults = armed,
        }
    }
}

fn insert_opening(
    conn: &Connection,
    window_id: &str,
    values: &WindowValues,
    child_scope: Option<String>,
) -> rusqlite::Result<usize> {
    let command_json = serde_json::to_string(&values.command)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let wake = wake_text(values)?;
    let hook_timeouts = hook_timeouts_text(values)?;
    conn.execute_fixed(
        "INSERT INTO windows \
         (window_id, name, cwd, vendor, command_json, detection, profile, model, effort, \
         layout_json, session_id, opener, task_plan_id, task_id, credentials, child_scope, \
         hooks, guest, overlay, wake, hook_timeouts, state) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
         ?18, ?19, ?20, ?21, 'opening') \
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
            child_scope,
            values.hooks,
            values.facts.runs_in_guest,
            values.facts.overlay,
            wake,
            hook_timeouts,
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
    let wake = wake_text(values)?;
    let hook_timeouts = hook_timeouts_text(values)?;
    conn.execute_fixed(
        "UPDATE windows SET name = ?2, cwd = ?3, vendor = ?4, command_json = ?5, \
         detection = ?6, profile = ?7, model = ?8, effort = ?9, layout_json = ?10, \
         session_id = ?11, opener = ?12, task_plan_id = ?13, task_id = ?14, credentials = ?15, \
         hooks = ?16, guest = ?17, overlay = ?18, wake = ?19, hook_timeouts = ?20 \
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
            values.hooks,
            values.facts.runs_in_guest,
            values.facts.overlay,
            wake,
            hook_timeouts,
        ],
    )
}

/// The spelling of `values`' `[hook_timeout]` table in the table, `None`
/// without one.
fn hook_timeouts_text(values: &WindowValues) -> rusqlite::Result<Option<String>> {
    values
        .facts
        .hook_timeouts
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

/// The spelling of `values`' wake record in the table, `None` without one.
fn wake_text(values: &WindowValues) -> rusqlite::Result<Option<String>> {
    values
        .facts
        .wake
        .as_ref()
        .map(crate::wake_file::WakeRecord::spelled)
        .transpose()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
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
    /// The row a previous daemon wrote for `window_id`, which a window taken
    /// back goes on with.
    pub(crate) fn taken_back(table: Option<WindowTable>, window_id: &str) -> Self {
        Self {
            table,
            window_id: window_id.to_owned(),
            updates: Mutex::new(()),
            removed: AtomicBool::new(false),
        }
    }

    /// What marks this row `lost` once the window's holder is found gone;
    /// nothing for a window without a row.
    pub(crate) fn lost_notice(&self) -> crate::pty::LostNotice {
        let table = self.table.clone();
        let window_id = self.window_id.clone();
        Box::new(move || {
            if let Some(table) = table {
                table.mark_lost(&window_id);
            }
        })
    }

    /// Writes the window's values as they are now; `values` is read under the
    /// row's own lock, so the last write carries every change made before it.
    pub(crate) fn update(&self, values: impl FnOnce() -> WindowValues) {
        // Unawaited, the write still takes its place in the writer's order.
        let _unawaited = self.update_waited(values);
    }

    /// [`Self::update`], returning what tells once the row took the values.
    /// The row's lock is held only while the values are read and sent, never
    /// while the write is awaited.
    pub(crate) fn update_waited(&self, values: impl FnOnce() -> WindowValues) -> PendingWrite {
        let Some(table) = &self.table else {
            return PendingWrite::NoRow;
        };
        let _ordered = lock_recover(&self.updates);
        if self.removed.load(Ordering::SeqCst) {
            return PendingWrite::NoRow;
        }
        PendingWrite::sent(table, &self.window_id, values())
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

/// The row could not take the values: the write failed, the row is not in
/// the table, or the writer is gone.
#[derive(Debug)]
pub(crate) struct RowUnwritten;

/// A value write sent to a window's row, to be awaited outside every lock.
pub(crate) enum PendingWrite {
    /// The window has no row, or its row is being removed: nothing is written.
    NoRow,
    /// The writer is gone, so nothing was sent.
    Unsent,
    Sent(oneshot::Receiver<bool>),
}

impl PendingWrite {
    fn sent(table: &WindowTable, window_id: &str, values: WindowValues) -> Self {
        let (reply, written) = oneshot::channel();
        if table.send(Command::Update {
            window_id: window_id.to_owned(),
            values,
            reply,
        }) {
            Self::Sent(written)
        } else {
            Self::Unsent
        }
    }

    /// Returns once the write is done; `Ok` also for a window without a row.
    pub(crate) async fn written(self) -> Result<(), RowUnwritten> {
        match self {
            Self::NoRow => Ok(()),
            Self::Unsent => Err(RowUnwritten),
            // A reply dropped unsent is a writer that ended before writing.
            Self::Sent(written) => match written.await {
                Ok(true) => Ok(()),
                Ok(false) | Err(_) => Err(RowUnwritten),
            },
        }
    }
}

/// What writes an `opening` row's values before its window's holder starts.
pub(crate) struct OpeningWrite {
    table: Option<WindowTable>,
    window_id: String,
}

impl OpeningWrite {
    /// Writes `values` to the row; see [`PendingWrite::written`].
    pub(crate) async fn write(self, values: WindowValues) -> Result<(), RowUnwritten> {
        match &self.table {
            Some(table) => PendingWrite::sent(table, &self.window_id, values),
            None => PendingWrite::NoRow,
        }
        .written()
        .await
    }
}

/// A window's row while it is `opening`: what [`WindowTable`] gave the id to.
pub(crate) struct OpeningRow {
    row: WindowRow,
    /// The scope names the row was written with; `None` for a window without
    /// a row.
    scopes: Option<HolderScopes>,
}

/// What reserving a window id in the table came to.
pub(crate) enum Reservation {
    Reserved(OpeningRow),
    /// The table already has a row with the id.
    Taken,
}

impl OpeningRow {
    /// Writes `window_id`'s row as `opening` with `values` and the child's
    /// scope name of `scopes`, and returns once it is written. With no
    /// `table`, or when the row cannot be written, the window opens without a
    /// row, and without scopes.
    pub(crate) async fn reserve(
        table: Option<&WindowTable>,
        window_id: &str,
        values: &WindowValues,
        scopes: Option<HolderScopes>,
    ) -> Reservation {
        let row = |table: Option<WindowTable>, scopes: Option<HolderScopes>| {
            Reservation::Reserved(Self {
                row: WindowRow {
                    table,
                    window_id: window_id.to_owned(),
                    updates: Mutex::new(()),
                    removed: AtomicBool::new(false),
                },
                scopes,
            })
        };
        let Some(table) = table else {
            return row(None, None);
        };
        let (reply, written) = oneshot::channel();
        let sent = table.send(Command::Reserve {
            window_id: window_id.to_owned(),
            values: values.clone(),
            child_scope: scopes.as_ref().map(|scopes| scopes.child.clone()),
            reply,
        });
        if !sent {
            return row(None, None);
        }
        match written.await {
            Ok(Reserved::Written) => row(Some(table.clone()), scopes),
            Ok(Reserved::Taken) => Reservation::Taken,
            Ok(Reserved::Unwritten) | Err(_) => row(None, None),
        }
    }

    /// See [`WindowRow::lost_notice`].
    pub(crate) fn lost_notice(&self) -> crate::pty::LostNotice {
        self.row.lost_notice()
    }

    /// What writes the values the start settles, for `start` of
    /// [`Self::open`] to await before it starts the holder.
    pub(crate) fn writer(&self) -> OpeningWrite {
        OpeningWrite {
            table: self.row.table.clone(),
            window_id: self.row.window_id.clone(),
        }
    }

    /// The scope names the row was written with, for the window's holder to
    /// ask for; `None` for a window without a row.
    pub(crate) fn scopes(&self) -> Option<RowScopes> {
        self.scopes.clone().map(RowScopes)
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
        let row = self.row;
        if let Some(table) = &row.table {
            table.send(Command::Live {
                window_id: row.window_id.clone(),
            });
        }
        Ok((opened, row))
    }
}
