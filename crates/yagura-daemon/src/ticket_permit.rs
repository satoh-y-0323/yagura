//! Proof-of-authorization values for the ticket board.
//!
//! Like [`crate::plan_permit`], a permit is never built from a caller-supplied
//! fact: every `issue_*` function reads the window's registration at the
//! moment it decides, and a permit's fields are private. The workspace a
//! window reads or writes comes from its registration (the `WorkspaceKey` its
//! cwd resolved to), never from an argument, so a window cannot reach another
//! repository's tickets by naming it.

use crate::control::RequestOrigin;
use crate::window::{WindowLiveness, WindowRegistry};
use crate::workspace::WorkspaceKey;

/// Why an `issue_*` call refused. No variant carries data, so a refusal never
/// echoes a window id, a workspace id or a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TicketPermitError {
    /// The connection is not allowed this operation at all.
    #[error("the caller is not authorized for this operation")]
    AuthFailed,
    /// The window cannot take a request: its child has exited, or it was never
    /// registered.
    #[error("the window is not in a state that allows this operation")]
    InvalidState,
    /// A window named a workspace; its own comes from its registration.
    #[error("a window cannot name a workspace")]
    WorkspaceNotAllowed,
    /// An approver read without naming a workspace.
    #[error("a workspace must be named")]
    WorkspaceRequired,
}

/// A workspace as the `ticket_workspace` table keys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRef {
    kind: &'static str,
    path: String,
}

impl WorkspaceRef {
    pub(crate) fn kind(&self) -> &'static str {
        self.kind
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

impl From<&WorkspaceKey> for WorkspaceRef {
    fn from(key: &WorkspaceKey) -> Self {
        Self {
            kind: key.kind.as_str(),
            path: key.path.clone(),
        }
    }
}

/// Proof that the calling window is alive; licenses writing its own workspace.
#[derive(Debug)]
pub struct TicketWritePermit {
    workspace: WorkspaceRef,
}

impl TicketWritePermit {
    /// The workspace this permit licenses writing, decided at issue time.
    pub(crate) fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }
}

/// Which workspace a [`TicketReadPermit`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadScope {
    /// A window's own workspace, by registration. It may not have a row yet.
    Own(WorkspaceRef),
    /// A workspace an approver named by id.
    Named(i64),
}

/// Proof that reading one workspace's tickets is authorized.
#[derive(Debug)]
pub struct TicketReadPermit {
    scope: ReadScope,
}

impl TicketReadPermit {
    /// The workspace this permit licenses reading.
    pub(crate) fn scope(&self) -> &ReadScope {
        &self.scope
    }
}

/// Proof that listing every workspace is authorized (approver only).
#[derive(Debug)]
pub struct WorkspaceListPermit {
    _private: (),
}

/// Issues a [`TicketWritePermit`]. Only a window that can currently take a
/// request may write, and only into the workspace it is registered in; an
/// approver reads but never writes.
///
/// # Errors
///
/// [`TicketPermitError::AuthFailed`] for an external connection or an
/// approver; [`TicketPermitError::InvalidState`] for a window that has exited
/// or was never registered.
pub fn issue_write(
    origin: &RequestOrigin,
    registry: &WindowRegistry,
) -> Result<TicketWritePermit, TicketPermitError> {
    let RequestOrigin::Window(window_id) = origin else {
        return Err(TicketPermitError::AuthFailed);
    };
    match registry.liveness(window_id) {
        WindowLiveness::Alive(entry) => Ok(TicketWritePermit {
            workspace: WorkspaceRef::from(entry.workspace()),
        }),
        WindowLiveness::Dead | WindowLiveness::NotFound => Err(TicketPermitError::InvalidState),
    }
}

/// Issues a [`TicketReadPermit`]. A window reads its own workspace
/// (`workspace_id` must be `None`); an approver must name one. Reading does
/// not check liveness: a window that has since exited still reads what it
/// wrote.
///
/// # Errors
///
/// [`TicketPermitError::AuthFailed`] for an external connection;
/// [`TicketPermitError::WorkspaceNotAllowed`] for a window that names a
/// workspace; [`TicketPermitError::WorkspaceRequired`] for an approver that
/// does not; [`TicketPermitError::InvalidState`] for a window this registry
/// never opened.
pub fn issue_read(
    origin: &RequestOrigin,
    registry: &WindowRegistry,
    workspace_id: Option<i64>,
) -> Result<TicketReadPermit, TicketPermitError> {
    match (origin, workspace_id) {
        (RequestOrigin::External, _) => Err(TicketPermitError::AuthFailed),
        (RequestOrigin::Approver, Some(id)) => Ok(TicketReadPermit {
            scope: ReadScope::Named(id),
        }),
        (RequestOrigin::Approver, None) => Err(TicketPermitError::WorkspaceRequired),
        (RequestOrigin::Window(_), Some(_)) => Err(TicketPermitError::WorkspaceNotAllowed),
        (RequestOrigin::Window(window_id), None) => {
            let entry = registry
                .get(window_id)
                .ok_or(TicketPermitError::InvalidState)?;
            Ok(TicketReadPermit {
                scope: ReadScope::Own(WorkspaceRef::from(entry.workspace())),
            })
        }
    }
}

/// Issues a [`WorkspaceListPermit`]; listing every workspace is the
/// approver's alone.
///
/// # Errors
///
/// [`TicketPermitError::AuthFailed`] for any other origin.
pub fn issue_list_workspaces(
    origin: &RequestOrigin,
) -> Result<WorkspaceListPermit, TicketPermitError> {
    if matches!(origin, RequestOrigin::Approver) {
        Ok(WorkspaceListPermit { _private: () })
    } else {
        Err(TicketPermitError::AuthFailed)
    }
}
