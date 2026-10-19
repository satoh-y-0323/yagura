//! Persistence port for the daemon's chat rooms: `chat_room`, `chat_message`
//! and `chat_delivery` (schema v13, `migrations/013_chat_delivery_state_check.sql`
//! on top of v6's `migrations/006_chat_delivery_reason_check.sql`, v5's
//! `migrations/005_chat_delivery_reason.sql`, v4's `migrations/004_chat_rooms.sql`
//! and v3's `migrations/003_chat.sql`). A
//! window's own room and the `main` room are both rows in `chat_room`;
//! nothing in this module treats `main` specially beyond the seed row
//! `003_chat.sql` inserts.
//!
//! Every write method commits its own transaction; [`ChatStore::append_message`]
//! is the exception that needs one, since it must make the halt decision and its
//! consequences (the room's halted state, `needs_user`, and the system notice)
//! atomic with the post that triggered them.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use yagura_protocol::UnknownReason;

use crate::room::{self, DeliveryOutcome};
use crate::sql::FixedSql;
use crate::store::{self, StoreError};
use crate::workspace::WorkspaceKind;

/// One message about to be appended. `mentions` carries resolved window ids the
/// message targets (never the literal "user"); `mentions_user` is the separate
/// `@user` flag. Both are persisted together in `chat_message.mentions_json` as
/// `{"targets": [...], "user": bool}` -- this module's own storage encoding, not
/// the wire protocol's shape.
pub struct NewChatMessage {
    pub room_id: String,
    pub at_ms: i64,
    /// `"window"` | `"user"` | `"external"` | `"system"`.
    pub from_kind: String,
    pub from_window_id: Option<String>,
    pub from_name: String,
    pub body: String,
    pub mentions: Vec<String>,
    pub mentions_user: bool,
    pub broadcast: bool,
}

/// The outcome of [`ChatStore::append_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    /// The seq assigned to the posted message (never the system notice's seq).
    pub seq: i64,
    /// Set when this post halted the room.
    pub halt: Option<room::HaltReason>,
}

/// One persisted chat message, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessageRow {
    pub seq: i64,
    pub at_ms: i64,
    pub from_kind: String,
    pub from_window_id: Option<String>,
    pub from_name: String,
    pub body: String,
    pub mentions: Vec<String>,
    pub mentions_user: bool,
    pub broadcast: bool,
    pub silence_noted: bool,
}

/// A page of messages returned by [`ChatStore::read_after`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAfter {
    pub messages: Vec<ChatMessageRow>,
    /// The cursor to pass as `after_seq` on the next call: the last returned
    /// message's seq, or the `after_seq` that was passed in when nothing new
    /// was found.
    pub next_after_seq: i64,
}

/// `chat_message.mentions_json`'s shape: this module's own storage encoding.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MentionsJson {
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    user: bool,
}

/// The room's persistence port: a single connection, migrated to the schema
/// this build ships ([`store::MIGRATIONS`]).
#[derive(Debug)]
pub struct ChatStore {
    conn: Connection,
}

/// The wire vocabulary [`room::HaltReason`] is stored as, in `chat_room.halted_reason`.
fn halt_reason_str(reason: room::HaltReason) -> &'static str {
    match reason {
        room::HaltReason::Repetition => "repetition",
        room::HaltReason::Monologue => "monologue",
        room::HaltReason::TurnBudget => "turn_budget",
    }
}

/// The inverse of [`halt_reason_str`]. `None` for anything unrecognized --
/// unreachable in practice, since that function is this column's only
/// writer, but a mismatch here must read back as "not halted" rather than
/// panic.
fn parse_halt_reason(reason: &str) -> Option<room::HaltReason> {
    match reason {
        "repetition" => Some(room::HaltReason::Repetition),
        "monologue" => Some(room::HaltReason::Monologue),
        "turn_budget" => Some(room::HaltReason::TurnBudget),
        _ => None,
    }
}

/// `chat_room`'s persisted halt and `needs_user` flag for one room: the
/// single source of truth [`ChatStore::room_status`] reads back, exactly as
/// any writer in this module last left it -- including one from a previous
/// process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomStatus {
    pub halted: Option<(room::HaltReason, i64)>,
    pub needs_user: bool,
}

/// The outcome of [`ChatStore::ensure_room`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureRoom {
    /// A row for this `room_id` already existed, with a matching
    /// `(workspace_kind, workspace_key)`; it was left untouched.
    Existing,
    /// No row for this `room_id` existed yet; one was inserted.
    Created,
}

/// One `chat_room` row, as read back by [`ChatStore::find_room`] /
/// [`ChatStore::list_rooms`]. `kind` and `key` are `None` for `main`, which
/// carries no workspace binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub room_id: String,
    pub kind: Option<WorkspaceKind>,
    pub key: Option<String>,
    pub created_at_ms: i64,
    /// `MAX(chat_message.at_ms)` for this room, or `None` if nothing has
    /// been posted yet.
    pub last_at_ms: Option<i64>,
    pub needs_user: bool,
    pub halted: Option<(room::HaltReason, i64)>,
}

/// The wire-adjacent vocabulary [`WorkspaceKind`] is stored as, in
/// `chat_room.workspace_kind`.
fn workspace_kind_str(kind: WorkspaceKind) -> &'static str {
    match kind {
        WorkspaceKind::Git => "git",
        WorkspaceKind::Directory => "directory",
    }
}

/// The inverse of [`workspace_kind_str`]. `None` for anything unrecognized --
/// unreachable in practice, since that function is this column's only
/// writer, but a mismatch here must read back as "no workspace kind" rather
/// than panic.
fn parse_workspace_kind(kind: &str) -> Option<WorkspaceKind> {
    match kind {
        "git" => Some(WorkspaceKind::Git),
        "directory" => Some(WorkspaceKind::Directory),
        _ => None,
    }
}

/// The columns and joins every [`RoomRow`] read shares: [`ChatStore::find_room`]
/// appends its own `WHERE`, [`ChatStore::list_rooms`] its own `ORDER BY`. A
/// macro rather than a constant, so both statements are joined by `concat!`
/// and stay text fixed at build time.
macro_rules! room_row_select {
    () => {
        "SELECT room_id, workspace_kind, workspace_key, \
         created_at_ms, needs_user, halted_reason, halted_at_ms, \
         (SELECT MAX(at_ms) FROM chat_message WHERE chat_message.room_id = chat_room.room_id) \
         FROM chat_room"
    };
}

/// Maps one `chat_room` row, in [`room_row_select!`]'s fixed column order,
/// into a [`RoomRow`].
fn row_to_room_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoomRow> {
    let room_id: String = row.get(0)?;
    let kind: Option<String> = row.get(1)?;
    let key: Option<String> = row.get(2)?;
    let created_at_ms: i64 = row.get(3)?;
    let needs_user: i64 = row.get(4)?;
    let halted_reason: Option<String> = row.get(5)?;
    let halted_at_ms: Option<i64> = row.get(6)?;
    let last_at_ms: Option<i64> = row.get(7)?;

    let halted = match (halted_reason.as_deref(), halted_at_ms) {
        (Some(reason_str), Some(at_ms)) => parse_halt_reason(reason_str).map(|r| (r, at_ms)),
        _ => None,
    };
    Ok(RoomRow {
        room_id,
        kind: kind.as_deref().and_then(parse_workspace_kind),
        key,
        created_at_ms,
        last_at_ms,
        needs_user: needs_user != 0,
        halted,
    })
}

/// The next seq for `room_id`: one past the highest seq stored so far, or `1`
/// for an empty room.
fn next_seq(tx: &rusqlite::Transaction<'_>, room_id: &str) -> Result<i64, StoreError> {
    let max: Option<i64> = tx.query_row_fixed(
        "SELECT MAX(seq) FROM chat_message WHERE room_id = ?1",
        params![room_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

/// Inserts one `chat_message` row.
#[allow(clippy::too_many_arguments)]
fn insert_message(
    tx: &rusqlite::Transaction<'_>,
    room_id: &str,
    seq: i64,
    at_ms: i64,
    from_kind: &str,
    from_window_id: Option<&str>,
    from_name: &str,
    body: &str,
    mentions: &[String],
    mentions_user: bool,
    broadcast: bool,
) -> Result<(), StoreError> {
    let mentions_json = serde_json::to_string(&MentionsJson {
        targets: mentions.to_vec(),
        user: mentions_user,
    })?;
    tx.execute_fixed(
        "INSERT INTO chat_message \
         (room_id, seq, at_ms, from_kind, from_window_id, from_name, body, mentions_json, \
          broadcast, silence_noted) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
        params![
            room_id,
            seq,
            at_ms,
            from_kind,
            from_window_id,
            from_name,
            body,
            mentions_json,
            broadcast
        ],
    )?;
    Ok(())
}

/// Writes a `pending` `chat_delivery` row for every distinct id in `mentions`,
/// so that the very first read after a post already shows it as being
/// delivered. A destination named more than once still gets
/// exactly one row (`ON CONFLICT ... DO NOTHING`).
fn insert_pending_deliveries(
    tx: &rusqlite::Transaction<'_>,
    room_id: &str,
    seq: i64,
    mentions: &[String],
    at_ms: i64,
) -> Result<(), StoreError> {
    for window_id in mentions {
        tx.execute_fixed(
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(room_id, seq, window_id) DO NOTHING",
            params![
                room_id,
                seq,
                window_id,
                room::ChatDeliveryState::Pending.as_str(),
                at_ms
            ],
        )?;
    }
    Ok(())
}

/// Deletes every `chat_message` (and matching `chat_delivery`) row for
/// `room_id` past the most recent `history_max` entries.
fn evict_past_history_max(
    tx: &rusqlite::Transaction<'_>,
    room_id: &str,
    history_max: u64,
) -> Result<(), StoreError> {
    let keep = i64::try_from(history_max).unwrap_or(i64::MAX);
    tx.execute_fixed(
        "DELETE FROM chat_message WHERE room_id = ?1 AND seq NOT IN \
         (SELECT seq FROM chat_message WHERE room_id = ?1 ORDER BY seq DESC LIMIT ?2)",
        params![room_id, keep],
    )?;
    tx.execute_fixed(
        "DELETE FROM chat_delivery WHERE room_id = ?1 AND seq NOT IN \
         (SELECT seq FROM chat_message WHERE room_id = ?1)",
        params![room_id],
    )?;
    Ok(())
}

/// Decodes one `chat_message` row into a [`room::HistoryEntry`] for
/// [`room::evaluate_limits`]. An unrecognized `from_kind` degrades to
/// [`room::Speaker::System`] -- excluded from every halt rule rather than
/// risking a wrong count.
fn history_entry(
    from_kind: &str,
    from_window_id: Option<String>,
    body: String,
    mentions_json: &str,
) -> room::HistoryEntry {
    let speaker = match from_kind {
        "window" => room::Speaker::Window(from_window_id.unwrap_or_default()),
        "user" => room::Speaker::User,
        "external" => room::Speaker::External,
        _ => room::Speaker::System,
    };
    let decoded: MentionsJson = serde_json::from_str(mentions_json).unwrap_or_default();
    room::HistoryEntry {
        speaker,
        mentions_user: decoded.user,
        targets: decoded.targets,
        body,
    }
}

/// Loads every remaining `chat_message` row for `room_id`, oldest first, for
/// [`room::evaluate_limits`]. Called right after [`evict_past_history_max`],
/// so the result is already bounded by `history_max`.
fn load_history(
    tx: &rusqlite::Transaction<'_>,
    room_id: &str,
) -> Result<Vec<room::HistoryEntry>, StoreError> {
    let mut stmt = tx.prepare_fixed(
        "SELECT from_kind, from_window_id, body, mentions_json FROM chat_message \
         WHERE room_id = ?1 ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(params![room_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut history = Vec::new();
    for row in rows {
        let (from_kind, from_window_id, body, mentions_json) = row?;
        history.push(history_entry(
            &from_kind,
            from_window_id,
            body,
            &mentions_json,
        ));
    }
    Ok(history)
}

/// Writes `room_id`'s halted state and raises `needs_user`, in one statement:
/// the single place both [`ChatStore::set_halt`] and the atomic halt inside
/// [`ChatStore::append_message`] write from.
fn apply_halt(
    tx: &rusqlite::Transaction<'_>,
    room_id: &str,
    reason: room::HaltReason,
    at_ms: i64,
) -> Result<(), StoreError> {
    tx.execute_fixed(
        "UPDATE chat_room SET halted_reason = ?2, halted_at_ms = ?3, needs_user = 1 \
         WHERE room_id = ?1",
        params![room_id, halt_reason_str(reason), at_ms],
    )?;
    Ok(())
}

/// One persisted delivery as both delivery reads return it: the destination
/// window id, together with the recorded outcome folded back into one value.
pub type DeliveryRow = (String, DeliveryOutcome);

/// Maps one `chat_delivery` row -- `(window_id, state, unknown_reason)`, the
/// leading column order both delivery reads share -- into the pair they
/// return, or `Ok(None)` when `state` is outside this build's vocabulary: the
/// state is the row's own record, not a value that can be dropped off a
/// surviving row, so the whole row is skipped by the caller rather than
/// guessed at. A reason value outside this build's wire vocabulary is
/// dropped while the row itself survives, and
/// [`UnknownReason::from_wire_str`] is the only place that decoding happens.
/// [`DeliveryOutcome::from_parts`] is the only place a
/// persisted pair is folded back into one outcome.
fn row_to_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<DeliveryRow>> {
    let window_id: String = row.get(0)?;
    let state: String = row.get(1)?;
    let reason: Option<String> = row.get(2)?;
    let Some(state) = room::ChatDeliveryState::parse(&state) else {
        return Ok(None);
    };
    let reason = reason.as_deref().and_then(UnknownReason::from_wire_str);
    Ok(Some((
        window_id,
        DeliveryOutcome::from_parts(state, reason),
    )))
}

/// Which delivery read is reporting a skipped row. The two reads differ only in what they
/// can name -- a whole room, or one `(room_id, seq)` pair -- and `tracing` resolves a
/// warning's fields at the macro call site, so the difference lives here as a type.
enum SkippedDeliveryContext<'a> {
    Room { room_id: &'a str },
    Seq { room_id: &'a str, seq: i64 },
}

/// Wording shared by both delivery reads when they skip a `chat_delivery` row whose
/// `state` is outside this build's vocabulary. The only place this text is written, so
/// rewording it never needs to be done in two places at once.
const DELIVERY_SKIP_WARNING: &str =
    "skipped one or more chat_delivery rows with an unrecognized state";

/// The one place both delivery reads decide whether to warn about a skipped row, and what
/// to say. [`ChatStore::deliveries_by_room`] and [`ChatStore::deliveries_for_seq`] each
/// still count their own skips (that count is inherent to the `Some`/`None` split each
/// builds its own result from), and pass the outcome here together with the fields the
/// route can name.
fn warn_if_delivery_row_skipped(any_skipped: bool, context: SkippedDeliveryContext<'_>) {
    if !any_skipped {
        return;
    }
    match context {
        SkippedDeliveryContext::Room { room_id } => {
            tracing::warn!(room_id, "{}", DELIVERY_SKIP_WARNING);
        }
        SkippedDeliveryContext::Seq { room_id, seq } => {
            tracing::warn!(room_id, seq, "{}", DELIVERY_SKIP_WARNING);
        }
    }
}

/// Maps one `chat_message` row (in the fixed column order used by every
/// `SELECT` in this module) into a [`ChatMessageRow`].
fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessageRow> {
    let mentions_json: String = row.get(6)?;
    let decoded: MentionsJson = serde_json::from_str(&mentions_json).unwrap_or_default();
    let broadcast: i64 = row.get(7)?;
    let silence_noted: i64 = row.get(8)?;
    Ok(ChatMessageRow {
        seq: row.get(0)?,
        at_ms: row.get(1)?,
        from_kind: row.get(2)?,
        from_window_id: row.get(3)?,
        from_name: row.get(4)?,
        body: row.get(5)?,
        mentions: decoded.targets,
        mentions_user: decoded.user,
        broadcast: broadcast != 0,
        silence_noted: silence_noted != 0,
    })
}

impl ChatStore {
    /// Opens `db_path`, migrating it if needed: the room's
    /// persistence port works without a prior [`store::open_store`] call.
    ///
    /// # Errors
    ///
    /// [`StoreError::FutureSchemaVersion`] for a database newer than this
    /// build supports; other [`StoreError`] variants for the same reasons as
    /// [`store::open_migrated`].
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        let conn = store::open_migrated(db_path)?;
        Ok(Self { conn })
    }

    /// Reads `room_id`'s `(workspace_kind, workspace_key)`, then either
    /// leaves an existing matching row untouched (`Existing`) or inserts a
    /// new one stamped `created_at_ms = now_ms` (`Created`).
    ///
    /// # Errors
    ///
    /// [`StoreError::RoomKeyMismatch`] if a row for `room_id` already exists
    /// with a different `(kind, key)` pair. Any other [`StoreError`] from
    /// the underlying SQLite operations, including the partial
    /// `chat_room_workspace` UNIQUE index rejecting an insert whose `(kind,
    /// key)` is already bound to a *different* `room_id` (surfaced as
    /// [`StoreError::Sqlite`], not [`StoreError::RoomKeyMismatch`]).
    pub fn ensure_room(
        &mut self,
        room_id: &str,
        kind: WorkspaceKind,
        key: &str,
        now_ms: i64,
    ) -> Result<EnsureRoom, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let existing: Option<(Option<String>, Option<String>)> = tx
            .query_row_fixed(
                "SELECT workspace_kind, workspace_key FROM chat_room WHERE room_id = ?1",
                params![room_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let outcome = match existing {
            None => {
                tx.execute_fixed(
                    "INSERT INTO chat_room \
                     (room_id, needs_user, workspace_kind, workspace_key, created_at_ms) \
                     VALUES (?1, 0, ?2, ?3, ?4)",
                    params![room_id, workspace_kind_str(kind), key, now_ms],
                )?;
                EnsureRoom::Created
            }
            Some((stored_kind, stored_key)) => {
                if stored_kind.as_deref() == Some(workspace_kind_str(kind))
                    && stored_key.as_deref() == Some(key)
                {
                    EnsureRoom::Existing
                } else {
                    return Err(StoreError::RoomKeyMismatch {
                        room_id: room_id.to_string(),
                    });
                }
            }
        };

        tx.commit()?;
        Ok(outcome)
    }

    /// Reads back one `chat_room` row by `room_id`, or `None` if it does not
    /// exist.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn find_room(&self, room_id: &str) -> Result<Option<RoomRow>, StoreError> {
        self.conn
            .query_row_fixed(
                concat!(room_row_select!(), " WHERE room_id = ?1"),
                params![room_id],
                row_to_room_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Every `chat_room` row: `main` sorted first, the rest by `last_at_ms`
    /// descending, falling back to `created_at_ms` for a room with no
    /// messages yet. No method in this module removes a `chat_room` row.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn list_rooms(&self) -> Result<Vec<RoomRow>, StoreError> {
        let mut stmt = self.conn.prepare_fixed(concat!(
            room_row_select!(),
            " ORDER BY CASE WHEN room_id = 'main' THEN 0 ELSE 1 END, \
             COALESCE((SELECT MAX(at_ms) FROM chat_message \
             WHERE chat_message.room_id = chat_room.room_id), created_at_ms) DESC"
        ))?;
        let rows = stmt
            .query_map([], row_to_room_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Appends `msg`, then evicts rows past `limits.history_max`, evaluates
    /// the repetition / monologue / turn-budget rules over what remains, and
    /// -- if they halt the room -- writes the halted state, raises
    /// `needs_user` and inserts a system notice mentioning `@user`, all in
    /// one transaction.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operations.
    pub fn append_message(
        &mut self,
        msg: NewChatMessage,
        limits: &room::ChatLimits,
    ) -> Result<Appended, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let seq = next_seq(&tx, &msg.room_id)?;
        insert_message(
            &tx,
            &msg.room_id,
            seq,
            msg.at_ms,
            &msg.from_kind,
            msg.from_window_id.as_deref(),
            &msg.from_name,
            &msg.body,
            &msg.mentions,
            msg.mentions_user,
            msg.broadcast,
        )?;
        insert_pending_deliveries(&tx, &msg.room_id, seq, &msg.mentions, msg.at_ms)?;
        evict_past_history_max(&tx, &msg.room_id, limits.history_max)?;

        let history = load_history(&tx, &msg.room_id)?;
        let halt = room::evaluate_limits(&history, limits);
        if let Some(reason) = halt {
            apply_halt(&tx, &msg.room_id, reason, msg.at_ms)?;
            let notice_seq = next_seq(&tx, &msg.room_id)?;
            insert_message(
                &tx,
                &msg.room_id,
                notice_seq,
                msg.at_ms,
                "system",
                None,
                "system",
                &format!("the room halted ({})", halt_reason_str(reason)),
                &[],
                true,
                false,
            )?;
        }

        tx.commit()?;
        Ok(Appended { seq, halt })
    }

    /// Sets (or updates) one window's delivery outcome for `(room_id, seq)`.
    /// Only [`DeliveryOutcome::UndeliveredUnknown`] carries a
    /// reason; every other outcome writes `unknown_reason` back to `NULL`, so
    /// re-settling a delivery never leaves a reason next to a state that
    /// cannot carry one.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn set_delivery(
        &mut self,
        room_id: &str,
        seq: i64,
        window_id: &str,
        outcome: DeliveryOutcome,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let state = outcome.state().as_str();
        let reason = outcome.unknown_reason().map(UnknownReason::as_wire);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "INSERT INTO chat_delivery (room_id, seq, window_id, state, at_ms, unknown_reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(room_id, seq, window_id) DO UPDATE SET \
             state = excluded.state, at_ms = excluded.at_ms, \
             unknown_reason = excluded.unknown_reason",
            params![room_id, seq, window_id, state, at_ms, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Halts `room_id` and raises `needs_user`, independent of any post.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn set_halt(
        &mut self,
        room_id: &str,
        reason: room::HaltReason,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_halt(&tx, room_id, reason, at_ms)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets `room_id`'s `needs_user` flag, independent of the halted state.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn set_needs_user(&mut self, room_id: &str, needs_user: bool) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE chat_room SET needs_user = ?2 WHERE room_id = ?1",
            params![room_id, needs_user],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks `(room_id, seq)` as already noted for silence.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn mark_silence_noted(&mut self, room_id: &str, seq: i64) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE chat_message SET silence_noted = 1 WHERE room_id = ?1 AND seq = ?2",
            params![room_id, seq],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Settles every delivery still `pending` as never delivered because the
    /// daemon stopped: the inbox that held it ended with that daemon. Called
    /// once, when a daemon opens its chat store, before any post of its own.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn settle_pending_after_restart(&mut self) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE chat_delivery SET state = ?1, unknown_reason = NULL WHERE state = ?2",
            params![
                room::ChatDeliveryState::UndeliveredRestart.as_str(),
                room::ChatDeliveryState::Pending.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Clears `room_id`'s halted state and `needs_user` together.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn clear_halt_and_needs_user(&mut self, room_id: &str) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_fixed(
            "UPDATE chat_room SET halted_reason = NULL, halted_at_ms = NULL, needs_user = 0 \
             WHERE room_id = ?1",
            params![room_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reads every message after `after_seq`, ascending, up to `limit` rows.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn read_after(
        &self,
        room_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<ReadAfter, StoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT seq, at_ms, from_kind, from_window_id, from_name, body, mentions_json, \
             broadcast, silence_noted FROM chat_message \
             WHERE room_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let messages: Vec<ChatMessageRow> = stmt
            .query_map(params![room_id, after_seq, limit], row_to_message)?
            .collect::<rusqlite::Result<_>>()?;
        let next_after_seq = messages.last().map_or(after_seq, |m| m.seq);
        Ok(ReadAfter {
            messages,
            next_after_seq,
        })
    }

    /// Reads the most recent `limit` messages, ascending (oldest first).
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn read_tail(&self, room_id: &str, limit: i64) -> Result<Vec<ChatMessageRow>, StoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT seq, at_ms, from_kind, from_window_id, from_name, body, mentions_json, \
             broadcast, silence_noted FROM chat_message \
             WHERE room_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let mut messages: Vec<ChatMessageRow> = stmt
            .query_map(params![room_id, limit], row_to_message)?
            .collect::<rusqlite::Result<_>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// The seq of `window_id`'s own most recent post in `room_id`, or `None`
    /// if it has never posted there.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn last_window_seq(
        &self,
        room_id: &str,
        window_id: &str,
    ) -> Result<Option<i64>, StoreError> {
        let seq = self.conn.query_row_fixed(
            "SELECT MAX(seq) FROM chat_message WHERE room_id = ?1 AND from_window_id = ?2",
            params![room_id, window_id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(seq)
    }

    /// `room_id`'s currently persisted halt and `needs_user` flag: the
    /// single source of truth for both facts, read back exactly as
    /// [`Self::append_message`], [`Self::set_halt`], [`Self::set_needs_user`]
    /// or [`Self::clear_halt_and_needs_user`] last left them.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn room_status(&self, room_id: &str) -> Result<RoomStatus, StoreError> {
        let (reason, at_ms, needs_user): (Option<String>, Option<i64>, i64) =
            self.conn.query_row_fixed(
                "SELECT halted_reason, halted_at_ms, needs_user FROM chat_room WHERE room_id = ?1",
                params![room_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let halted = match (reason.as_deref(), at_ms) {
            (Some(reason_str), Some(at_ms)) => parse_halt_reason(reason_str).map(|r| (r, at_ms)),
            _ => None,
        };
        Ok(RoomStatus {
            halted,
            needs_user: needs_user != 0,
        })
    }

    /// Every persisted delivery for `room_id`, grouped by the message seq it
    /// belongs to: the source [`Self::room_status`]'s caller builds each
    /// message's delivery list from.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn deliveries_by_room(
        &self,
        room_id: &str,
    ) -> Result<HashMap<i64, Vec<DeliveryRow>>, StoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT window_id, state, unknown_reason, seq FROM chat_delivery WHERE room_id = ?1",
        )?;
        let rows = stmt.query_map(params![room_id], |row| {
            Ok((row.get::<_, i64>(3)?, row_to_delivery(row)?))
        })?;
        let mut by_seq: HashMap<i64, Vec<DeliveryRow>> = HashMap::new();
        let mut skipped_a_delivery_row = false;
        for row in rows {
            let (seq, delivery) = row?;
            match delivery {
                Some(delivery) => by_seq.entry(seq).or_default().push(delivery),
                None => skipped_a_delivery_row = true,
            }
        }
        warn_if_delivery_row_skipped(
            skipped_a_delivery_row,
            SkippedDeliveryContext::Room { room_id },
        );
        Ok(by_seq)
    }

    /// Every persisted delivery for one `(room_id, seq)` pair, rebuilt into
    /// the outcome [`Self::set_delivery`] recorded.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying SQLite operation.
    pub fn deliveries_for_seq(
        &self,
        room_id: &str,
        seq: i64,
    ) -> Result<Vec<DeliveryRow>, StoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT window_id, state, unknown_reason FROM chat_delivery \
             WHERE room_id = ?1 AND seq = ?2",
        )?;
        let rows: Vec<Option<DeliveryRow>> = stmt
            .query_map(params![room_id, seq], row_to_delivery)?
            .collect::<rusqlite::Result<_>>()?;
        let mut skipped_a_delivery_row = false;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            match row {
                Some(delivery) => entries.push(delivery),
                None => skipped_a_delivery_row = true,
            }
        }
        warn_if_delivery_row_skipped(
            skipped_a_delivery_row,
            SkippedDeliveryContext::Seq { room_id, seq },
        );
        Ok(entries)
    }
}
