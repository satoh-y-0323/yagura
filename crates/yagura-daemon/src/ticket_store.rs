//! Persistence port for the ticket board: `ticket_workspace`, `ticket`,
//! `ticket_field`, `ticket_result` and `ticket_milestone` (schema v16:
//! `migrations/016_tickets.sql`).
//!
//! Every write takes a [`crate::ticket_permit`] permit rather than a workspace
//! as an argument: the workspace is the one the permit was issued for. Every
//! write is one `IMMEDIATE` transaction in which the numbering, the resource
//! limits and the required-field gate ([`TicketConfig::missing_required`])
//! are decided, so a concurrent writer cannot slip between a check and the
//! write it guards. A refused write leaves nothing behind, not even the
//! workspace's row.
//!
//! Validating the text of a title, body, field value or result is the IPC
//! layer's job ([`crate::ticket`]); this module stores what it is given. An
//! empty field value or milestone name means "remove it".

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;
use yagura_protocol::{
    MilestoneInfo, TicketInfo, TicketResultInfo, TicketSummary, TicketWorkspaceInfo,
};

use crate::sql::FixedSql;
use crate::store::{self, StoreError, current_epoch_millis};
use crate::ticket::TicketConfig;
use crate::ticket_permit::{
    ReadScope, TicketReadPermit, TicketWritePermit, WorkspaceListPermit, WorkspaceRef,
};

/// Most tickets one workspace may hold.
pub const MAX_TICKETS_PER_WORKSPACE: i64 = 10_000;

/// Most results one ticket may hold.
pub const MAX_RESULTS_PER_TICKET: i64 = 256;

/// Most milestones one workspace may hold.
pub const MAX_MILESTONES_PER_WORKSPACE: i64 = 64;

/// Most workspaces the board may hold. A window can open in any directory, so
/// the per-workspace limits alone do not bound the total.
pub const MAX_TICKET_WORKSPACES: i64 = 256;

/// Rows a listing returns when the query names no limit.
pub const DEFAULT_LIST_LIMIT: u32 = 200;

/// Most rows a listing returns, whatever limit the query names.
pub const MAX_LIST_LIMIT: u32 = 1_000;

/// Which limit [`TicketStoreError::ResourceExhausted`] was refused by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketLimitKind {
    /// [`MAX_TICKETS_PER_WORKSPACE`].
    Tickets,
    /// [`MAX_RESULTS_PER_TICKET`].
    Results,
    /// [`MAX_MILESTONES_PER_WORKSPACE`].
    Milestones,
}

/// Why a store call refused or failed. No variant carries caller-supplied
/// text; the field names in [`Self::MissingRequiredFields`] come from the
/// user's `tickets.toml`.
#[derive(Debug, Error)]
pub enum TicketStoreError {
    /// A storage failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// No such ticket in the workspace.
    #[error("no such ticket")]
    TicketNotFound,
    /// A limit is already reached; nothing was written.
    #[error("the ticket board's resource limit is already reached")]
    ResourceExhausted(TicketLimitKind),
    /// A new workspace would exceed [`MAX_TICKET_WORKSPACES`].
    #[error("too many workspaces already hold tickets")]
    TooManyWorkspaces,
    /// The state is not one the config declares.
    #[error("the state is not declared in tickets.toml")]
    UnknownState,
    /// A field is not one the config declares.
    #[error("a field is not declared in tickets.toml")]
    UnknownField,
    /// The state requires fields that would be empty after the change.
    #[error("the state requires fields that are empty")]
    MissingRequiredFields(Vec<String>),
    /// The milestone does not exist in the workspace.
    #[error("no such milestone in this workspace")]
    UnknownMilestone,
}

/// A ticket to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTicket {
    /// One-line title.
    pub title: String,
    /// Multi-line body.
    pub body: String,
    /// Milestone name; `None` or `""` for none.
    pub milestone: Option<String>,
    /// State; `None` takes the config's first state.
    pub state: Option<String>,
    /// Field values; `""` is the same as absent.
    pub fields: BTreeMap<String, String>,
}

/// A change to a ticket: only what is `Some` (or listed in `fields`) changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketChange {
    /// New title.
    pub title: Option<String>,
    /// New body.
    pub body: Option<String>,
    /// New milestone; `Some("")` takes the ticket out of its milestone.
    pub milestone: Option<String>,
    /// New state.
    pub state: Option<String>,
    /// Fields to set; a value of `""` removes the field.
    pub fields: BTreeMap<String, String>,
}

/// What a listing keeps, and how much.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketQuery {
    /// Only tickets in this state.
    pub state: Option<String>,
    /// Only tickets in this milestone.
    pub milestone: Option<String>,
    /// At most this many rows; [`DEFAULT_LIST_LIMIT`] when `None`, never more
    /// than [`MAX_LIST_LIMIT`].
    pub limit: Option<u32>,
}

/// A listing: the workspace's milestones and the matching tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketListing {
    /// Milestones by position, then name.
    pub milestones: Vec<MilestoneInfo>,
    /// Matching tickets, summaries only, in board order.
    pub tickets: Vec<TicketSummary>,
    /// Whether more tickets matched than the limit allowed.
    pub truncated: bool,
}

/// A ticket after a write, with the workspace it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketWritten {
    /// The workspace's id.
    pub workspace_id: i64,
    /// The ticket as stored.
    pub ticket: TicketInfo,
}

/// A milestone after a write, with the workspace it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MilestoneWritten {
    /// The workspace's id.
    pub workspace_id: i64,
    /// The milestone as stored.
    pub milestone: MilestoneInfo,
}

/// A connection to the ticket tables.
#[derive(Debug)]
pub struct TicketStore {
    conn: Connection,
}

impl TicketStore {
    /// Opens (creating and migrating if needed) `db_path`.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] the shared migration path can return.
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            conn: store::open_migrated(db_path)?,
        })
    }

    /// Runs `f` in one `IMMEDIATE` transaction, committing only on `Ok`.
    fn write<T>(
        &mut self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, TicketStoreError>,
    ) -> Result<T, TicketStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Creates a ticket in the permit's workspace and numbers it.
    ///
    /// # Errors
    ///
    /// [`TicketStoreError::UnknownState`], [`TicketStoreError::UnknownField`],
    /// [`TicketStoreError::MissingRequiredFields`],
    /// [`TicketStoreError::UnknownMilestone`],
    /// [`TicketStoreError::TooManyWorkspaces`],
    /// [`TicketStoreError::ResourceExhausted`], or a storage failure. Nothing
    /// is written on any of them.
    pub fn create_ticket(
        &mut self,
        permit: &TicketWritePermit,
        config: &TicketConfig,
        new: &NewTicket,
    ) -> Result<TicketWritten, TicketStoreError> {
        self.write(|tx| {
            let state = new
                .state
                .as_deref()
                .unwrap_or_else(|| config.default_state());
            config.state(state).ok_or(TicketStoreError::UnknownState)?;
            let fields = effective_fields(config, BTreeMap::new(), &new.fields)?;
            gate(config, state, &fields)?;

            let workspace_id = ensure_workspace(tx, permit.workspace())?;
            let milestone = non_empty(new.milestone.as_deref());
            if let Some(name) = milestone {
                require_milestone(tx, workspace_id, name)?;
            }
            let held: i64 = tx.query_row_fixed(
                "SELECT COUNT(*) FROM ticket WHERE workspace_id = ?1",
                params![workspace_id],
                |row| row.get(0),
            )?;
            if held >= MAX_TICKETS_PER_WORKSPACE {
                return Err(TicketStoreError::ResourceExhausted(
                    TicketLimitKind::Tickets,
                ));
            }
            let number: i64 = tx.query_row_fixed(
                "SELECT next_number FROM ticket_workspace WHERE workspace_id = ?1",
                params![workspace_id],
                |row| row.get(0),
            )?;
            tx.execute_fixed(
                "UPDATE ticket_workspace SET next_number = ?2 WHERE workspace_id = ?1",
                params![workspace_id, number + 1],
            )?;
            let now = current_epoch_millis();
            tx.execute_fixed(
                "INSERT INTO ticket (workspace_id, number, title, body, state, milestone, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    workspace_id,
                    number,
                    new.title,
                    new.body,
                    state,
                    milestone,
                    now
                ],
            )?;
            write_fields(tx, workspace_id, number, &fields)?;
            let ticket = read_ticket(tx, workspace_id, number)?;
            Ok(TicketWritten {
                workspace_id,
                ticket,
            })
        })
    }

    /// Changes a ticket in the permit's workspace.
    ///
    /// The required-field gate runs when the state or any field changes, on
    /// the state and fields as they will be after the change; a title, body or
    /// milestone change alone never consults it.
    ///
    /// # Errors
    ///
    /// [`TicketStoreError::TicketNotFound`], and the refusals of
    /// [`Self::create_ticket`] that apply. Nothing is written on any of them.
    pub fn update_ticket(
        &mut self,
        permit: &TicketWritePermit,
        config: &TicketConfig,
        number: u32,
        change: &TicketChange,
    ) -> Result<TicketWritten, TicketStoreError> {
        self.write(|tx| {
            let workspace_id =
                find_workspace(tx, permit.workspace())?.ok_or(TicketStoreError::TicketNotFound)?;
            let current = read_ticket(tx, workspace_id, i64::from(number))?;

            let state = match change.state.as_deref() {
                Some(name) => {
                    config.state(name).ok_or(TicketStoreError::UnknownState)?;
                    name
                }
                None => current.state.as_str(),
            };
            let fields = effective_fields(config, current.fields.clone(), &change.fields)?;
            if change.state.is_some() || !change.fields.is_empty() {
                gate(config, state, &fields)?;
            }
            let milestone = match change.milestone.as_deref() {
                None => current.milestone.as_deref(),
                Some(name) => non_empty(Some(name)),
            };
            if change.milestone.is_some()
                && let Some(name) = milestone
            {
                require_milestone(tx, workspace_id, name)?;
            }

            let number = i64::from(number);
            tx.execute_fixed(
                "UPDATE ticket SET title = ?3, body = ?4, state = ?5, milestone = ?6, \
                 updated_at = ?7 WHERE workspace_id = ?1 AND number = ?2",
                params![
                    workspace_id,
                    number,
                    change.title.as_deref().unwrap_or(&current.title),
                    change.body.as_deref().unwrap_or(&current.body),
                    state,
                    milestone,
                    current_epoch_millis()
                ],
            )?;
            tx.execute_fixed(
                "DELETE FROM ticket_field WHERE workspace_id = ?1 AND number = ?2",
                params![workspace_id, number],
            )?;
            write_fields(tx, workspace_id, number, &fields)?;
            let ticket = read_ticket(tx, workspace_id, number)?;
            Ok(TicketWritten {
                workspace_id,
                ticket,
            })
        })
    }

    /// Appends a result to a ticket in the permit's workspace; the store
    /// stamps the time and the next `seq`.
    ///
    /// # Errors
    ///
    /// [`TicketStoreError::TicketNotFound`] or
    /// [`TicketStoreError::ResourceExhausted`]; nothing is written.
    pub fn add_result(
        &mut self,
        permit: &TicketWritePermit,
        number: u32,
        text: &str,
    ) -> Result<TicketWritten, TicketStoreError> {
        self.write(|tx| {
            let workspace_id =
                find_workspace(tx, permit.workspace())?.ok_or(TicketStoreError::TicketNotFound)?;
            let number = i64::from(number);
            read_ticket(tx, workspace_id, number)?;
            let (held, last): (i64, i64) = tx.query_row_fixed(
                "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM ticket_result \
                 WHERE workspace_id = ?1 AND number = ?2",
                params![workspace_id, number],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if held >= MAX_RESULTS_PER_TICKET {
                return Err(TicketStoreError::ResourceExhausted(
                    TicketLimitKind::Results,
                ));
            }
            let now = current_epoch_millis();
            tx.execute_fixed(
                "INSERT INTO ticket_result (workspace_id, number, seq, at, text) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![workspace_id, number, last + 1, now, text],
            )?;
            tx.execute_fixed(
                "UPDATE ticket SET updated_at = ?3 WHERE workspace_id = ?1 AND number = ?2",
                params![workspace_id, number, now],
            )?;
            let ticket = read_ticket(tx, workspace_id, number)?;
            Ok(TicketWritten {
                workspace_id,
                ticket,
            })
        })
    }

    /// Creates or replaces a milestone of the permit's workspace.
    ///
    /// # Errors
    ///
    /// [`TicketStoreError::TooManyWorkspaces`] or
    /// [`TicketStoreError::ResourceExhausted`]; changing an existing milestone
    /// adds nothing, so it is never refused for the milestone limit.
    pub fn set_milestone(
        &mut self,
        permit: &TicketWritePermit,
        milestone: &MilestoneInfo,
    ) -> Result<MilestoneWritten, TicketStoreError> {
        self.write(|tx| {
            let workspace_id = ensure_workspace(tx, permit.workspace())?;
            let exists = milestone_exists(tx, workspace_id, &milestone.name)?;
            if !exists {
                let held: i64 = tx.query_row_fixed(
                    "SELECT COUNT(*) FROM ticket_milestone WHERE workspace_id = ?1",
                    params![workspace_id],
                    |row| row.get(0),
                )?;
                if held >= MAX_MILESTONES_PER_WORKSPACE {
                    return Err(TicketStoreError::ResourceExhausted(
                        TicketLimitKind::Milestones,
                    ));
                }
            }
            tx.execute_fixed(
                "INSERT INTO ticket_milestone (workspace_id, name, position, title, description) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (workspace_id, name) DO UPDATE SET \
                 position = excluded.position, title = excluded.title, \
                 description = excluded.description",
                params![
                    workspace_id,
                    milestone.name,
                    milestone.position,
                    milestone.title,
                    milestone.description
                ],
            )?;
            Ok(MilestoneWritten {
                workspace_id,
                milestone: milestone.clone(),
            })
        })
    }

    /// Lists a workspace's milestones and tickets (summaries, no bodies) in
    /// board order: milestone position then name, tickets without a
    /// milestone last, then number. A workspace with no row reads as empty.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub fn list_tickets(
        &self,
        permit: &TicketReadPermit,
        query: &TicketQuery,
    ) -> Result<TicketListing, TicketStoreError> {
        let Some(workspace_id) = resolve_scope(&self.conn, permit.scope())? else {
            return Ok(TicketListing {
                milestones: Vec::new(),
                tickets: Vec::new(),
                truncated: false,
            });
        };
        let limit = query
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .min(MAX_LIST_LIMIT);
        let mut stmt = self.conn.prepare_fixed(
            "SELECT t.number, t.title, t.state, t.milestone, t.updated_at \
             FROM ticket t LEFT JOIN ticket_milestone m \
             ON m.workspace_id = t.workspace_id AND m.name = t.milestone \
             WHERE t.workspace_id = ?1 AND (?2 IS NULL OR t.state = ?2) \
             AND (?3 IS NULL OR t.milestone = ?3) \
             ORDER BY m.position IS NULL, m.position, m.name, t.number LIMIT ?4",
        )?;
        let mut tickets = stmt
            .query_map(
                params![
                    workspace_id,
                    query.state,
                    query.milestone,
                    i64::from(limit) + 1
                ],
                |row| {
                    Ok(TicketSummary {
                        number: number_from_column(row.get(0)?),
                        title: row.get(1)?,
                        state: row.get(2)?,
                        milestone: row.get(3)?,
                        updated_at: millis_from_column(row.get(4)?),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let truncated = tickets.len() > limit as usize;
        tickets.truncate(limit as usize);

        let mut stmt = self.conn.prepare_fixed(
            "SELECT name, position, title, description FROM ticket_milestone \
             WHERE workspace_id = ?1 ORDER BY position, name",
        )?;
        let milestones = stmt
            .query_map(params![workspace_id], |row| {
                Ok(MilestoneInfo {
                    name: row.get(0)?,
                    position: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(TicketListing {
            milestones,
            tickets,
            truncated,
        })
    }

    /// Reads one ticket in full.
    ///
    /// # Errors
    ///
    /// [`TicketStoreError::TicketNotFound`] when the workspace has no such
    /// ticket (including when the workspace has no row).
    pub fn get_ticket(
        &self,
        permit: &TicketReadPermit,
        number: u32,
    ) -> Result<TicketInfo, TicketStoreError> {
        let workspace_id =
            resolve_scope(&self.conn, permit.scope())?.ok_or(TicketStoreError::TicketNotFound)?;
        read_ticket(&self.conn, workspace_id, i64::from(number))
    }

    /// Lists every workspace that holds tickets.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub fn list_workspaces(
        &self,
        _permit: &WorkspaceListPermit,
    ) -> Result<Vec<TicketWorkspaceInfo>, TicketStoreError> {
        let mut stmt = self.conn.prepare_fixed(
            "SELECT workspace_id, kind, key FROM ticket_workspace ORDER BY workspace_id",
        )?;
        Ok(stmt
            .query_map([], |row| {
                Ok(TicketWorkspaceInfo {
                    workspace_id: row.get(0)?,
                    kind: row.get(1)?,
                    path: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// `Some(value)` unless it is absent or `""` (which means "none").
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// `base` with `changes` applied: a declared field set to its value, `""`
/// removing it. A field the config does not declare is refused.
fn effective_fields(
    config: &TicketConfig,
    mut base: BTreeMap<String, String>,
    changes: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, TicketStoreError> {
    for (name, value) in changes {
        config.field(name).ok_or(TicketStoreError::UnknownField)?;
        if value.is_empty() {
            base.remove(name);
        } else {
            base.insert(name.clone(), value.clone());
        }
    }
    Ok(base)
}

/// The required-field gate: `state` may hold `fields` only if every field it
/// requires has a non-blank value.
fn gate(
    config: &TicketConfig,
    state: &str,
    fields: &BTreeMap<String, String>,
) -> Result<(), TicketStoreError> {
    let missing = config.missing_required(state, fields);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(TicketStoreError::MissingRequiredFields(missing))
    }
}

fn number_from_column(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn millis_from_column(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn find_workspace(
    conn: &Connection,
    workspace: &WorkspaceRef,
) -> Result<Option<i64>, rusqlite::Error> {
    conn.query_row_fixed(
        "SELECT workspace_id FROM ticket_workspace WHERE kind = ?1 AND key = ?2",
        params![workspace.kind(), workspace.path()],
        |row| row.get(0),
    )
    .optional()
}

fn resolve_scope(conn: &Connection, scope: &ReadScope) -> Result<Option<i64>, rusqlite::Error> {
    match scope {
        ReadScope::Own(workspace) => find_workspace(conn, workspace),
        ReadScope::Named(id) => conn
            .query_row_fixed(
                "SELECT workspace_id FROM ticket_workspace WHERE workspace_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional(),
    }
}

/// The workspace's id, creating its row on a first write. The row count is
/// bounded here, inside the writer's transaction.
fn ensure_workspace(
    tx: &Transaction<'_>,
    workspace: &WorkspaceRef,
) -> Result<i64, TicketStoreError> {
    if let Some(id) = find_workspace(tx, workspace)? {
        return Ok(id);
    }
    let held: i64 = tx.query_row_fixed("SELECT COUNT(*) FROM ticket_workspace", [], |row| {
        row.get(0)
    })?;
    if held >= MAX_TICKET_WORKSPACES {
        return Err(TicketStoreError::TooManyWorkspaces);
    }
    tx.execute_fixed(
        "INSERT INTO ticket_workspace (kind, key, next_number) VALUES (?1, ?2, 1)",
        params![workspace.kind(), workspace.path()],
    )?;
    Ok(tx.last_insert_rowid())
}

fn milestone_exists(
    conn: &Connection,
    workspace_id: i64,
    name: &str,
) -> Result<bool, rusqlite::Error> {
    Ok(conn
        .query_row_fixed(
            "SELECT 1 FROM ticket_milestone WHERE workspace_id = ?1 AND name = ?2",
            params![workspace_id, name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn require_milestone(
    conn: &Connection,
    workspace_id: i64,
    name: &str,
) -> Result<(), TicketStoreError> {
    if milestone_exists(conn, workspace_id, name)? {
        Ok(())
    } else {
        Err(TicketStoreError::UnknownMilestone)
    }
}

fn write_fields(
    tx: &Transaction<'_>,
    workspace_id: i64,
    number: i64,
    fields: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    for (name, value) in fields {
        tx.execute_fixed(
            "INSERT INTO ticket_field (workspace_id, number, name, value) \
             VALUES (?1, ?2, ?3, ?4)",
            params![workspace_id, number, name, value],
        )?;
    }
    Ok(())
}

/// One ticket in full, with its fields, results and the plans made from it.
fn read_ticket(
    conn: &Connection,
    workspace_id: i64,
    number: i64,
) -> Result<TicketInfo, TicketStoreError> {
    let (title, body, state, milestone, created_at, updated_at): (
        String,
        String,
        String,
        Option<String>,
        i64,
        i64,
    ) = conn
        .query_row_fixed(
            "SELECT title, body, state, milestone, created_at, updated_at FROM ticket \
             WHERE workspace_id = ?1 AND number = ?2",
            params![workspace_id, number],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(TicketStoreError::TicketNotFound)?;

    let mut stmt = conn.prepare_fixed(
        "SELECT name, value FROM ticket_field WHERE workspace_id = ?1 AND number = ?2",
    )?;
    let fields = stmt
        .query_map(params![workspace_id, number], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;

    let mut stmt = conn.prepare_fixed(
        "SELECT seq, at, text FROM ticket_result \
         WHERE workspace_id = ?1 AND number = ?2 ORDER BY seq",
    )?;
    let results = stmt
        .query_map(params![workspace_id, number], |row| {
            Ok(TicketResultInfo {
                seq: millis_from_column(row.get(0)?),
                at: millis_from_column(row.get(1)?),
                text: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare_fixed(
        "SELECT plan_id FROM plan WHERE ticket_workspace_id = ?1 AND ticket_number = ?2 \
         ORDER BY created_at, plan_id",
    )?;
    let plans = stmt
        .query_map(params![workspace_id, number], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(TicketInfo {
        number: number_from_column(number),
        title,
        body,
        state,
        milestone,
        fields,
        results,
        plans,
        created_at: millis_from_column(created_at),
        updated_at: millis_from_column(updated_at),
    })
}
