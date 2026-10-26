//! The ticket board's IPC surface: [`TicketRuntime`] (the store gate and the
//! bounded `tickets.toml` reads) and the `list_ticket_workspaces` /
//! `list_tickets` / `get_ticket` / `create_ticket` / `update_ticket` /
//! `add_ticket_result` / `set_milestone` handlers.
//!
//! Authorization is never derived here: every handler passes the connection's
//! [`RequestOrigin`] to [`ticket_permit`]'s `issue_*` functions and reaches the
//! store only through the permit they return. Text is checked here with
//! [`crate::ticket`]'s validators before the store sees it; no refusal echoes
//! a caller's value or the content of `tickets.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use yagura_protocol::{
    DaemonMessage, MilestoneInfo, TicketConfigInfo, TicketFieldInfo, TicketStateInfo,
};

use crate::approval::ApprovalBroker;
use crate::config::DaemonConfig;
use crate::control::RequestOrigin;
use crate::ticket::{self, TicketConfig, TicketConfigError, TicketTextError, read_ticket_config};
use crate::ticket_permit::{self, TicketPermitError};
use crate::ticket_store::{
    MAX_LIST_LIMIT, NewTicket, TicketChange, TicketLimitKind, TicketQuery, TicketStore,
    TicketStoreError,
};
use crate::window::WindowRegistry;

use super::store_gate::{StoreAccessError, StoreGate};
use super::{Outbound, error_msg};

/// [`super::plan::PlanRuntime`]'s counterpart for the ticket board: the store
/// gate, where `tickets.toml` lives, and the semaphore admitting its reads.
pub struct TicketRuntime {
    store: StoreGate<TicketStore>,
    tickets_path: Option<PathBuf>,
    /// Admits [`TicketRuntime::load_config`]'s blocking file reads, sized the
    /// same way the plan ledger's role and profile reads are.
    config_reads: Semaphore,
}

impl TicketRuntime {
    /// A disabled runtime: every ticket request answers `unsupported`.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            store: StoreGate::disabled(),
            tickets_path: None,
            config_reads: Semaphore::new(1),
        }
    }

    /// Opens the ticket store, or returns a disabled runtime when no store
    /// path is resolved or the open fails.
    ///
    /// A caller in an async context must run this on a blocking thread
    /// (`tokio::task::spawn_blocking`): opening a database is blocking I/O.
    #[must_use]
    pub fn from_config(config: &DaemonConfig) -> Self {
        let Some(db_path) = config.store_db_path_resolved.as_ref() else {
            tracing::warn!("ticket board disabled: no store database path is resolved");
            return Self::disabled();
        };
        match TicketStore::open(db_path) {
            Ok(store) => Self {
                store: StoreGate::opened(store),
                tickets_path: config.tickets_path.clone(),
                config_reads: Semaphore::new(config.max_windows.max(1)),
            },
            Err(err) => {
                tracing::warn!("ticket board disabled: failed to open the ticket store: {err}");
                Self::disabled()
            }
        }
    }

    fn is_enabled(&self) -> bool {
        self.store.is_enabled()
    }

    /// Reads `tickets.toml` afresh on a blocking thread admitted by
    /// `config_reads`, so an edit applies to the next request and the read
    /// never runs on a request's own async task.
    // The one caller of the blocking config read, admitted by `config_reads`.
    #[allow(clippy::disallowed_methods)]
    async fn load_config(&self) -> ConfigLoad {
        let Some(path) = self.tickets_path.clone() else {
            return ConfigLoad::Missing;
        };
        let Ok(_permit) = self.config_reads.acquire().await else {
            return ConfigLoad::Unavailable;
        };
        match tokio::task::spawn_blocking(move || read_ticket_config(&path)).await {
            Ok(Ok(Some(config))) => ConfigLoad::Ready(config),
            Ok(Ok(None)) => ConfigLoad::Missing,
            Ok(Err(err)) => ConfigLoad::Invalid(err),
            Err(join_err) => {
                tracing::warn!(error = %join_err, "tickets.toml read task failed");
                ConfigLoad::Unavailable
            }
        }
    }

    /// The settings a write needs, or the refusal to send instead.
    async fn config_for_write(&self) -> Result<TicketConfig, DaemonMessage> {
        match self.load_config().await {
            ConfigLoad::Ready(config) => Ok(config),
            ConfigLoad::Missing => Err(error_msg(
                "tickets_not_configured",
                TICKETS_NOT_CONFIGURED_REASON,
            )),
            ConfigLoad::Invalid(err) => Err(error_msg("tickets_config_invalid", &err.to_string())),
            ConfigLoad::Unavailable => Err(internal_msg()),
        }
    }

    /// Runs a store write and, on success, tells every approver which
    /// workspace changed before answering the caller.
    async fn write<F>(&self, broker: &ApprovalBroker, outbound: &Outbound, f: F)
    where
        F: FnOnce(&mut TicketStore) -> Result<(i64, DaemonMessage), TicketStoreError>
            + Send
            + 'static,
    {
        match self.store.with_store(f).await {
            Ok(Ok((workspace_id, reply))) => {
                broker.notify_approvers(&DaemonMessage::TicketChanged { workspace_id });
                outbound.reply(&reply);
            }
            Ok(Err(err)) => outbound.reply(&store_error_msg(err)),
            Err(access_err) => outbound.reply(&access_error_msg(access_err)),
        }
    }

    /// Runs a store read and answers with its reply.
    async fn read<F>(&self, outbound: &Outbound, f: F)
    where
        F: FnOnce(&mut TicketStore) -> Result<DaemonMessage, TicketStoreError> + Send + 'static,
    {
        match self.store.with_store(f).await {
            Ok(Ok(reply)) => outbound.reply(&reply),
            Ok(Err(err)) => outbound.reply(&store_error_msg(err)),
            Err(access_err) => outbound.reply(&access_error_msg(access_err)),
        }
    }
}

/// What reading `tickets.toml` found.
enum ConfigLoad {
    Ready(TicketConfig),
    /// No file, or no location to look for one.
    Missing,
    Invalid(TicketConfigError),
    /// The read itself could not run (the runtime is shutting down).
    Unavailable,
}

const TICKETS_UNAVAILABLE_MESSAGE: &str = "tickets are not available on this daemon";

const TICKETS_NOT_CONFIGURED_REASON: &str =
    "tickets.toml does not exist; create it to define the board's states and fields";

// --- handlers ----------------------------------------------------------------

/// The request a ticket handler serves, as the dispatcher received it.
pub(super) enum TicketRequest {
    ListWorkspaces,
    List {
        workspace_id: Option<i64>,
        query: TicketQuery,
    },
    Get {
        workspace_id: Option<i64>,
        number: u32,
    },
    Create(NewTicket),
    Update {
        number: u32,
        change: TicketChange,
    },
    AddResult {
        number: u32,
        text: String,
    },
    SetMilestone(MilestoneInfo),
}

/// Serves one ticket request: refuses on a disabled runtime, then decides the
/// permit, then checks the text, then reaches the store.
pub(super) async fn handle_ticket_request(
    tickets: &TicketRuntime,
    registry: &WindowRegistry,
    broker: &ApprovalBroker,
    outbound: &Arc<Outbound>,
    origin: &RequestOrigin,
    request: TicketRequest,
) {
    if !tickets.is_enabled() {
        outbound.reply(&error_msg("unsupported", TICKETS_UNAVAILABLE_MESSAGE));
        return;
    }
    match request {
        TicketRequest::ListWorkspaces => {
            let permit = match ticket_permit::issue_list_workspaces(origin) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            tickets
                .read(outbound, move |store| {
                    let workspaces = store.list_workspaces(&permit)?;
                    Ok(DaemonMessage::TicketWorkspaceList { workspaces })
                })
                .await;
        }
        TicketRequest::List {
            workspace_id,
            query,
        } => {
            let permit = match ticket_permit::issue_read(origin, registry, workspace_id) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            if query.limit.is_some_and(|limit| limit > MAX_LIST_LIMIT) {
                return outbound.reply(&invalid_argument_msg(
                    "limit",
                    &format!("limit must be at most {MAX_LIST_LIMIT}"),
                ));
            }
            let (config, config_error) = match tickets.load_config().await {
                ConfigLoad::Ready(config) => (Some(config_info(config)), None),
                ConfigLoad::Missing => (None, Some(TICKETS_NOT_CONFIGURED_REASON.to_string())),
                ConfigLoad::Invalid(err) => (None, Some(err.to_string())),
                ConfigLoad::Unavailable => return outbound.reply(&internal_msg()),
            };
            tickets
                .read(outbound, move |store| {
                    let listing = store.list_tickets(&permit, &query)?;
                    Ok(DaemonMessage::TicketList {
                        workspace_id,
                        config,
                        config_error,
                        milestones: listing.milestones,
                        tickets: listing.tickets,
                        truncated: listing.truncated,
                    })
                })
                .await;
        }
        TicketRequest::Get {
            workspace_id,
            number,
        } => {
            let permit = match ticket_permit::issue_read(origin, registry, workspace_id) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            tickets
                .read(outbound, move |store| {
                    let ticket = store.get_ticket(&permit, number)?;
                    Ok(DaemonMessage::TicketDetail { ticket })
                })
                .await;
        }
        TicketRequest::Create(new) => {
            let permit = match ticket_permit::issue_write(origin, registry) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            if let Err(refusal) = validate_new_ticket(&new) {
                return outbound.reply(&refusal.to_msg());
            }
            let config = match tickets.config_for_write().await {
                Ok(config) => config,
                Err(refusal) => return outbound.reply(&refusal),
            };
            tickets
                .write(broker, outbound, move |store| {
                    let written = store.create_ticket(&permit, &config, &new)?;
                    Ok(detail_reply(written))
                })
                .await;
        }
        TicketRequest::Update { number, change } => {
            let permit = match ticket_permit::issue_write(origin, registry) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            if let Err(refusal) = validate_change(&change) {
                return outbound.reply(&refusal.to_msg());
            }
            let config = match tickets.config_for_write().await {
                Ok(config) => config,
                Err(refusal) => return outbound.reply(&refusal),
            };
            tickets
                .write(broker, outbound, move |store| {
                    let written = store.update_ticket(&permit, &config, number, &change)?;
                    Ok(detail_reply(written))
                })
                .await;
        }
        TicketRequest::AddResult { number, text } => {
            let permit = match ticket_permit::issue_write(origin, registry) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            if let Err(refusal) =
                ticket::validate_ticket_result(&text).map_err(TextRefusal::of("text"))
            {
                return outbound.reply(&refusal.to_msg());
            }
            tickets
                .write(broker, outbound, move |store| {
                    let written = store.add_result(&permit, number, &text)?;
                    Ok(detail_reply(written))
                })
                .await;
        }
        TicketRequest::SetMilestone(milestone) => {
            let permit = match ticket_permit::issue_write(origin, registry) {
                Ok(permit) => permit,
                Err(err) => return outbound.reply(&permit_error_msg(err)),
            };
            if let Err(refusal) = validate_milestone(&milestone) {
                return outbound.reply(&refusal.to_msg());
            }
            tickets
                .write(broker, outbound, move |store| {
                    let written = store.set_milestone(&permit, &milestone)?;
                    Ok((
                        written.workspace_id,
                        DaemonMessage::MilestoneSet {
                            milestone: written.milestone,
                        },
                    ))
                })
                .await;
        }
    }
}

fn detail_reply(written: crate::ticket_store::TicketWritten) -> (i64, DaemonMessage) {
    (
        written.workspace_id,
        DaemonMessage::TicketDetail {
            ticket: written.ticket,
        },
    )
}

fn config_info(config: TicketConfig) -> TicketConfigInfo {
    TicketConfigInfo {
        guide: config.guide,
        states: config
            .states
            .into_iter()
            .map(|state| TicketStateInfo {
                name: state.name,
                description: state.description,
                requires: state.requires,
            })
            .collect(),
        fields: config
            .fields
            .into_iter()
            .map(|field| TicketFieldInfo {
                name: field.name,
                description: field.description,
            })
            .collect(),
    }
}

/// `""` means "remove" for a milestone and a field value, so it is never
/// checked as text.
fn is_removal(value: &str) -> bool {
    value.is_empty()
}

/// A text check that failed: which argument, and why.
struct TextRefusal {
    argument: &'static str,
    err: TicketTextError,
}

impl TextRefusal {
    fn of(argument: &'static str) -> impl FnOnce(TicketTextError) -> Self {
        move |err| Self { argument, err }
    }

    /// The validators' errors report a length or a code point, never the
    /// value.
    fn to_msg(&self) -> DaemonMessage {
        invalid_argument_msg(
            self.argument,
            &format!("{} is invalid: {}", self.argument, self.err),
        )
    }
}

fn validate_milestone_ref(milestone: Option<&str>) -> Result<(), TextRefusal> {
    match milestone {
        Some(name) if !is_removal(name) => {
            ticket::validate_milestone_name(name).map_err(TextRefusal::of("milestone"))
        }
        _ => Ok(()),
    }
}

fn validate_field_values(fields: &BTreeMap<String, String>) -> Result<(), TextRefusal> {
    fields
        .values()
        .filter(|value| !is_removal(value))
        .try_for_each(|value| {
            ticket::validate_ticket_field_value(value).map_err(TextRefusal::of("fields"))
        })
}

fn validate_new_ticket(new: &NewTicket) -> Result<(), TextRefusal> {
    ticket::validate_ticket_title(&new.title).map_err(TextRefusal::of("title"))?;
    ticket::validate_ticket_body(&new.body).map_err(TextRefusal::of("body"))?;
    validate_milestone_ref(new.milestone.as_deref())?;
    validate_field_values(&new.fields)
}

fn validate_change(change: &TicketChange) -> Result<(), TextRefusal> {
    if let Some(title) = &change.title {
        ticket::validate_ticket_title(title).map_err(TextRefusal::of("title"))?;
    }
    if let Some(body) = &change.body {
        ticket::validate_ticket_body(body).map_err(TextRefusal::of("body"))?;
    }
    validate_milestone_ref(change.milestone.as_deref())?;
    validate_field_values(&change.fields)
}

fn validate_milestone(milestone: &MilestoneInfo) -> Result<(), TextRefusal> {
    ticket::validate_milestone_name(&milestone.name).map_err(TextRefusal::of("name"))?;
    ticket::validate_milestone_title(&milestone.title).map_err(TextRefusal::of("title"))?;
    match milestone.description.as_deref() {
        Some(description) => ticket::validate_milestone_description(description)
            .map_err(TextRefusal::of("description")),
        None => Ok(()),
    }
}

/// Builds the store's view of a `create_ticket` request.
pub(super) fn new_ticket(
    title: String,
    body: String,
    milestone: Option<String>,
    state: Option<String>,
    fields: Option<BTreeMap<String, String>>,
) -> NewTicket {
    NewTicket {
        title,
        body,
        milestone,
        state,
        fields: fields.unwrap_or_default(),
    }
}

/// Builds the store's view of a `set_milestone` request; an empty
/// description is the same as none.
pub(super) fn milestone(
    name: String,
    title: String,
    description: Option<String>,
    position: i64,
) -> MilestoneInfo {
    MilestoneInfo {
        name,
        position,
        title,
        description: description.filter(|text| !is_removal(text)),
    }
}

// --- refusals ------------------------------------------------------------------

fn invalid_argument_msg(argument: &str, message: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: message.to_string(),
        rejected_argument: Some(argument.to_string()),
    }
}

fn permit_error_msg(err: TicketPermitError) -> DaemonMessage {
    match err {
        TicketPermitError::AuthFailed => error_msg(
            "auth_failed",
            "this connection is not authorized for this ticket operation",
        ),
        TicketPermitError::InvalidState => super::plan::invalid_state_msg(),
        TicketPermitError::WorkspaceNotAllowed => invalid_argument_msg(
            "workspace_id",
            "a window reads its own workspace and cannot name one",
        ),
        TicketPermitError::WorkspaceRequired => {
            invalid_argument_msg("workspace_id", "a workspace_id is required")
        }
    }
}

fn store_error_msg(err: TicketStoreError) -> DaemonMessage {
    match err {
        TicketStoreError::TicketNotFound => ticket_not_found_msg(),
        TicketStoreError::ResourceExhausted(kind) => {
            let what = match kind {
                TicketLimitKind::Tickets => "the workspace already holds the most tickets allowed",
                TicketLimitKind::Results => "the ticket already holds the most results allowed",
                TicketLimitKind::Milestones => {
                    "the workspace already holds the most milestones allowed"
                }
            };
            error_msg("resource_exhausted", what)
        }
        TicketStoreError::TooManyWorkspaces => error_msg(
            "invalid_argument",
            "too many workspaces already hold tickets; this one cannot start a board",
        ),
        TicketStoreError::UnknownState => {
            invalid_argument_msg("state", "the state is not declared in tickets.toml")
        }
        TicketStoreError::UnknownField => {
            invalid_argument_msg("fields", "a field is not declared in tickets.toml")
        }
        TicketStoreError::MissingRequiredFields(missing) => invalid_argument_msg(
            "fields",
            &format!(
                "the state requires these fields to have a value: {}",
                missing.join(", ")
            ),
        ),
        TicketStoreError::UnknownMilestone => {
            invalid_argument_msg("milestone", "no such milestone in this workspace")
        }
        TicketStoreError::Store(err) => {
            tracing::warn!(error = %err, "ticket store access failed");
            internal_msg()
        }
        TicketStoreError::Sqlite(err) => {
            tracing::warn!(error = %err, "ticket store access failed");
            internal_msg()
        }
    }
}

/// The one answer for a ticket number the caller's workspace does not hold,
/// whether it is absent or belongs to another workspace.
pub(super) fn ticket_not_found_msg() -> DaemonMessage {
    error_msg(
        "ticket_not_found",
        "no ticket with this number in the caller's workspace",
    )
}

fn access_error_msg(err: StoreAccessError) -> DaemonMessage {
    match err {
        StoreAccessError::Disabled => error_msg("unsupported", TICKETS_UNAVAILABLE_MESSAGE),
        StoreAccessError::Failed(join_err) => {
            tracing::warn!(error = %join_err, "ticket store access task failed");
            internal_msg()
        }
    }
}

fn internal_msg() -> DaemonMessage {
    error_msg(
        "internal",
        "an internal error occurred while accessing the ticket board",
    )
}
