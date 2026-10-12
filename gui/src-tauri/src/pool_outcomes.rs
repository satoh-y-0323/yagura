//! The result types the GUI reports for daemon replies that are outcomes
//! rather than errors, and the pure functions that map a reply onto them.
//!
//! Held apart from the connection pool so the mapping rules can be read,
//! and tested, without a live connection.

use serde::Serialize;
use yagura_protocol::{ApprovalRequest, PROTOCOL_REVISION_BASELINE};

use crate::ipc_client::IpcError;

/// Whether a frame that needs `required_revision` may be written to a
/// connection whose peer settled on `settled_revision` as of
/// `settled_generation`, weighed against the connection's `generation` now.
///
/// A message that needs nothing beyond [`PROTOCOL_REVISION_BASELINE`] always
/// carries: every peer that speaks this wire at all implements the baseline,
/// including one that has not answered the revision probe yet -- which is
/// what lets that probe itself go out over an unsettled connection.
///
/// A settled revision recorded for a generation other than the one being
/// written to is weighed the same as no revision at all: a value learned
/// from a connection that has since been replaced is not a fact about the
/// connection this call is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolRevisionGate {
    /// The connection's peer is known to carry the requirement, or the
    /// message needs nothing beyond the baseline.
    Carried,
    /// The connection's peer has not settled a revision for its current
    /// generation, or settled on one below the requirement.
    Refused,
}

/// The pure judgement behind [`ProtocolRevisionGate`]. Held apart from
/// [`crate::ipc_client::IpcClient::send`] (which needs the live connection
/// state to supply these four values) so the decision itself is
/// unit-testable without one.
pub fn protocol_revision_gate(
    settled_revision: Option<u32>,
    settled_generation: u64,
    generation: u64,
    required_revision: u32,
) -> ProtocolRevisionGate {
    if required_revision <= PROTOCOL_REVISION_BASELINE {
        return ProtocolRevisionGate::Carried;
    }
    if settled_generation != generation {
        return ProtocolRevisionGate::Refused;
    }
    match settled_revision {
        Some(revision) if revision >= required_revision => ProtocolRevisionGate::Carried,
        _ => ProtocolRevisionGate::Refused,
    }
}

/// Outcome of settling one pending approval request
/// ([`crate::connection_pool::ConnectionPool::decide_approval`]). A distinct
/// success value, not an error: the request being no longer pending (already
/// decided elsewhere, or expired) is not a failure of the decision the
/// approver just made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecideApprovalOutcome {
    /// The daemon accepted and recorded this decision.
    Decided,
    /// The request was no longer pending when the decision arrived.
    NotPending,
}

/// Maps the result of one `decide_approval` round trip onto
/// [`DecideApprovalOutcome`]. A pure function, kept separate from
/// [`crate::connection_pool::ConnectionPool::decide_approval`] (which needs a
/// live connection) so the mapping itself is unit-testable.
pub(crate) fn map_decide_approval_result(
    result: Result<(), IpcError>,
) -> Result<DecideApprovalOutcome, IpcError> {
    match result {
        Ok(()) => Ok(DecideApprovalOutcome::Decided),
        Err(IpcError::Rejected { code, .. }) if code == "approval_not_pending" => {
            Ok(DecideApprovalOutcome::NotPending)
        }
        Err(error) => Err(error),
    }
}

/// Outcome of one pull of the pending approval list
/// ([`crate::connection_pool::ConnectionPool::sync_approvals`]). A refused
/// approver registration is a success value rather than an error: the WebView
/// has to show that state, and the pull is how it learns of a refusal whose
/// event was emitted before the subscription opened.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalSyncOutcome {
    /// This connection is a registered approver; `pending` is the list the
    /// daemon holds for it right now, oldest first.
    Registered {
        /// Every request pending for this approver.
        pending: Vec<ApprovalRequest>,
    },
    /// The daemon refused this connection as an approver.
    Refused,
}

/// Maps the result of one `sync_approvals` round trip onto
/// [`ApprovalSyncOutcome`]. A pure function, kept separate from
/// [`crate::connection_pool::ConnectionPool::sync_approvals`] (which needs a
/// live connection) so the mapping itself is unit-testable.
pub(crate) fn map_sync_approvals_result(
    result: Result<Vec<ApprovalRequest>, IpcError>,
) -> Result<ApprovalSyncOutcome, IpcError> {
    match result {
        Ok(pending) => Ok(ApprovalSyncOutcome::Registered { pending }),
        Err(IpcError::Rejected { code, .. }) if code == "auth_failed" => {
            Ok(ApprovalSyncOutcome::Refused)
        }
        Err(error) => Err(error),
    }
}

/// Outcome of closing one window
/// ([`crate::connection_pool::ConnectionPool::close_window`]). The daemon
/// holding no such window, and the daemon being out of reach, are success
/// values rather than errors: in both cases the window this pane stands for
/// is gone from the caller's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseOutcome {
    /// The daemon closed the window.
    Closed,
    /// The daemon answered that it holds no such window.
    NotFound,
    /// The daemon could not be reached at all.
    Unreachable,
}

/// Maps the result of the whole close path -- the connection, the approver
/// registration and the round trip alike -- onto [`CloseOutcome`]. A pure
/// function, kept separate from
/// [`crate::connection_pool::ConnectionPool::close_window`] (which needs a
/// live connection) so the mapping itself is unit-testable.
pub(crate) fn map_close_result(result: Result<(), IpcError>) -> Result<CloseOutcome, IpcError> {
    match result {
        Ok(()) => Ok(CloseOutcome::Closed),
        Err(IpcError::Rejected { code, .. }) if code == "window_not_found" => {
            Ok(CloseOutcome::NotFound)
        }
        Err(
            IpcError::EndpointName(_)
            | IpcError::NotConnected
            | IpcError::ConnectTimeout
            | IpcError::AttachTimeout
            | IpcError::RequestTimeout
            | IpcError::Write
            | IpcError::DaemonNotFound
            | IpcError::DaemonSpawn,
        ) => Ok(CloseOutcome::Unreachable),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_decide_approval_result_maps_ok_to_decided() {
        assert!(matches!(
            map_decide_approval_result(Ok(())),
            Ok(DecideApprovalOutcome::Decided)
        ));
    }

    #[test]
    fn map_decide_approval_result_maps_an_approval_not_pending_rejection_to_not_pending() {
        let result = map_decide_approval_result(Err(IpcError::Rejected {
            code: "approval_not_pending".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(result, Ok(DecideApprovalOutcome::NotPending)));
    }

    #[test]
    fn map_decide_approval_result_passes_through_every_other_rejection_as_an_error() {
        let result = map_decide_approval_result(Err(IpcError::Rejected {
            code: "invalid_state".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(
            result,
            Err(IpcError::Rejected { code, .. }) if code == "invalid_state"
        ));
    }

    #[test]
    fn map_sync_approvals_result_maps_ok_to_registered_carrying_the_pending_list() {
        let result = map_sync_approvals_result(Ok(Vec::new()));
        assert!(matches!(
            result,
            Ok(ApprovalSyncOutcome::Registered { pending }) if pending.is_empty()
        ));
    }

    #[test]
    fn map_sync_approvals_result_maps_an_auth_failed_rejection_to_refused() {
        let result = map_sync_approvals_result(Err(IpcError::Rejected {
            code: "auth_failed".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(result, Ok(ApprovalSyncOutcome::Refused)));
    }

    #[test]
    fn map_sync_approvals_result_passes_through_every_other_rejection_as_an_error() {
        let result = map_sync_approvals_result(Err(IpcError::Rejected {
            code: "invalid_state".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(
            result,
            Err(IpcError::Rejected { code, .. }) if code == "invalid_state"
        ));
    }

    // -----------------------------------------------------------------
    // CloseOutcome / map_close_result: the full
    // 15-row IpcError -> CloseOutcome table, split so a mutation to one arm
    // cannot pass by falling through to a neighbouring assertion.
    // -----------------------------------------------------------------

    #[test]
    fn map_close_result_maps_ok_to_closed() {
        assert!(matches!(map_close_result(Ok(())), Ok(CloseOutcome::Closed)));
    }

    #[test]
    fn map_close_result_maps_a_window_not_found_rejection_to_not_found() {
        let result = map_close_result(Err(IpcError::Rejected {
            code: "window_not_found".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(result, Ok(CloseOutcome::NotFound)));
    }

    #[test]
    fn map_close_result_passes_through_every_other_rejection_as_an_error_preserving_the_code() {
        let result = map_close_result(Err(IpcError::Rejected {
            code: "approval_denied".to_owned(),
            message: String::new(),
            rejected_argument: None,
        }));
        assert!(matches!(
            result,
            Err(IpcError::Rejected { code, .. }) if code == "approval_denied"
        ));
    }

    #[test]
    fn map_close_result_maps_every_unreachable_variant_to_unreachable() {
        let unreachable_cases = [
            IpcError::EndpointName(yagura_os::OsError::Security("boom".to_owned())),
            IpcError::NotConnected,
            IpcError::ConnectTimeout,
            IpcError::AttachTimeout,
            IpcError::RequestTimeout,
            IpcError::Write,
            IpcError::DaemonNotFound,
            IpcError::DaemonSpawn,
        ];
        for error in unreachable_cases {
            let debug = format!("{error:?}");
            assert!(
                matches!(map_close_result(Err(error)), Ok(CloseOutcome::Unreachable)),
                "expected Unreachable for {debug}"
            );
        }
    }

    #[test]
    fn map_close_result_passes_through_every_contract_violation_variant_unchanged() {
        assert!(matches!(
            map_close_result(Err(IpcError::UnexpectedReply)),
            Err(IpcError::UnexpectedReply)
        ));
        assert!(matches!(
            map_close_result(Err(IpcError::InvalidArgument("window_id"))),
            Err(IpcError::InvalidArgument("window_id"))
        ));
        assert!(matches!(
            map_close_result(Err(IpcError::WindowNotAttached {
                window_id: "w1".to_owned(),
            })),
            Err(IpcError::WindowNotAttached { window_id }) if window_id == "w1"
        ));
        assert!(matches!(
            map_close_result(Err(IpcError::ApproverNotRegistered)),
            Err(IpcError::ApproverNotRegistered)
        ));
    }

    // -----------------------------------------------------------------
    // CloseOutcome wire shape: a bare
    // snake_case string, matching DecideApprovalOutcome's convention.
    // -----------------------------------------------------------------

    #[test]
    fn close_outcome_closed_serializes_to_a_bare_string() {
        assert_eq!(
            serde_json::to_value(CloseOutcome::Closed).expect("CloseOutcome must serialize"),
            serde_json::json!("closed")
        );
    }

    #[test]
    fn close_outcome_not_found_serializes_to_a_bare_string() {
        assert_eq!(
            serde_json::to_value(CloseOutcome::NotFound).expect("CloseOutcome must serialize"),
            serde_json::json!("not_found")
        );
    }

    #[test]
    fn close_outcome_unreachable_serializes_to_a_bare_string() {
        assert_eq!(
            serde_json::to_value(CloseOutcome::Unreachable).expect("CloseOutcome must serialize"),
            serde_json::json!("unreachable")
        );
    }
}
