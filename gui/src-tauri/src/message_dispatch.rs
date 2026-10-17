//! Turns daemon messages into a completed request on the
//! [`crate::ipc_client::IpcClient`] awaiting it, a Tauri event delivered to
//! the WebView, or (frames) a part queued in the [`crate::frame_mailbox::FrameMailbox`].
//!
//! Separated from the connection machinery itself so the daemon protocol's
//! message shapes and the WebView's event vocabulary can be read (and
//! tested) without touching connection state.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, ReadHalf};
use tokio::sync::oneshot;
use yagura_protocol::{
    ApprovalSettingsInfo, DaemonMessage, Frame, PlaybookState, decode_daemon_message,
};

use crate::commands::{
    ApprovalSettingsReply, ChatMessagesReply, ChatPostedReply, ChatRoomListReply,
    DaemonHealthReply, DetectionListReply, McpRegistrationRequestedReply,
    McpRegistrationsListedReply, ModelAliasesSavedReply, ModelChoicesRequestedReply,
    OpenWindowReply, ProfileCommandSetReply, ProfileDetailReply, ProfileEnabledSetReply,
    ProfileListReply, RestoreCandidateListReply, RoleListReply, TaskWindowOpenedReply,
    ToolAutoApproveReply, UpdateWindowLayoutReply, mcp_registration_action_str,
};
use crate::constants::READ_CHUNK_LEN;
use crate::events::{
    ApprovalRequestedPayload, ApprovalWithdrawnPayload, AttachResponse, ChatChangedPayload,
    EVENT_APPROVAL_REQUESTED, EVENT_APPROVAL_WITHDRAWN, EVENT_CHAT_CHANGED, EVENT_EXIT,
    EVENT_MCP_REGISTRATION_RESULT, EVENT_MCP_REGISTRATION_STATUS, EVENT_MODEL_CHOICES,
    EVENT_PLAN_CHANGED, EVENT_STATUS, ExitPayload, McpRegistrationResultPayload,
    McpRegistrationStatusPayload, ModelChoicesPayload, PlanChangedPayload, StatusPayload,
};
use crate::host::GuiHost;
use crate::ipc_client::{
    Connection, ERROR_CODE_INTERNAL, ERROR_CODE_SUPERSEDED, IpcClient, IpcError, PendingKind, Reply,
};

/// Window-scoped event to be emitted to the WebView. Separated from dispatch
/// so payload assembly stays separate from delivery.
#[derive(Debug, Clone, PartialEq)]
enum WindowEvent {
    Frame { window_id: String, frame: Frame },
    Exit { window_id: String, exit_code: i32 },
}

/// Assembles a window-scoped event from a daemon message and the connection's
/// window id, taking the message (a frame is moved, not copied). Hands the
/// message back when the connection is attach-less or the message is not
/// window-scoped.
fn window_event_for(
    window_id: Option<&str>,
    message: DaemonMessage,
) -> Result<WindowEvent, Box<DaemonMessage>> {
    let Some(wid) = window_id else {
        return Err(Box::new(message));
    };
    match message {
        DaemonMessage::Frame(frame) => Ok(WindowEvent::Frame {
            window_id: wid.to_owned(),
            frame,
        }),
        DaemonMessage::Exit { exit_code } => Ok(WindowEvent::Exit {
            window_id: wid.to_owned(),
            exit_code,
        }),
        other => Err(Box::new(other)),
    }
}

/// Reads frames off `reader` until the connection ends or `cancel` fires,
/// dispatching each decoded message to the WebView or to the pending request
/// it answers.
pub(crate) async fn read_loop<H: GuiHost>(
    mut reader: ReadHalf<Connection>,
    app: H,
    client: Arc<IpcClient>,
    generation: u64,
    mut cancel: oneshot::Receiver<()>,
) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_LEN];
    let mut superseded = false;
    let mut cancelled = false;

    'read: loop {
        let read = tokio::select! {
            // `read` is cancel-safe, and the loop exits on the other branch
            // anyway, so no byte is lost by losing this race.
            read = reader.read(&mut chunk) => match read {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            },
            _ = &mut cancel => {
                cancelled = true;
                break;
            }
        };
        buffer.extend_from_slice(&chunk[..read]);

        loop {
            let (message, consumed) = match yagura_protocol::frame_decode(&buffer) {
                Ok(Some((body, consumed))) => (decode_daemon_message(body), consumed),
                Ok(None) => break,
                // A frame larger than the protocol allows can only come from a
                // peer that is not the daemon; drop the connection.
                Err(_) => break 'read,
            };
            buffer.drain(..consumed);
            let Ok(message) = message else {
                continue;
            };
            let window_id = client.window_id().await;
            if dispatch(&app, &client, message, window_id).await {
                superseded = true;
                break 'read;
            }
        }
    }

    // A cancelled reader was retired by `disconnect`, which already reset the
    // client; reporting the disconnect it caused would be noise.
    if !cancelled {
        client.on_disconnect(&app, superseded, generation).await;
    }
}

/// The request a daemon message answers, if it answers one at all.
///
/// Matched exhaustively on purpose: a new protocol message must force a
/// decision about whether it completes a request.
fn reply_for(message: DaemonMessage) -> Option<(PendingKind, Reply)> {
    match message {
        DaemonMessage::Attached {
            child_exited,
            exit_code,
        } => Some((
            PendingKind::Attach,
            Reply::Attached(AttachResponse {
                child_exited,
                exit_code,
            }),
        )),
        DaemonMessage::WindowList { windows } => {
            Some((PendingKind::ListWindows, Reply::WindowList(windows)))
        }
        DaemonMessage::SelectionText { text } => {
            Some((PendingKind::CopySelection, Reply::SelectionText(text)))
        }
        DaemonMessage::WindowOpened {
            window_id,
            detection_warning,
        } => Some((
            PendingKind::OpenWindow,
            Reply::WindowOpened(OpenWindowReply {
                window_id,
                detection_warning,
            }),
        )),
        DaemonMessage::DetectionList { names, warning, .. } => Some((
            PendingKind::ListDetections,
            Reply::DetectionList(DetectionListReply { names, warning }),
        )),
        DaemonMessage::ProfileList { profiles, warnings } => Some((
            PendingKind::ListProfiles,
            Reply::ProfileList(ProfileListReply { profiles, warnings }),
        )),
        DaemonMessage::WindowRenamed { name, .. } => {
            Some((PendingKind::RenameWindow, Reply::WindowRenamed(name)))
        }
        DaemonMessage::WindowClosed { .. } => Some((PendingKind::CloseWindow, Reply::Done)),
        DaemonMessage::WindowInterrupted { .. } => {
            Some((PendingKind::InterruptWindow, Reply::Done))
        }
        DaemonMessage::RestoreCandidateList {
            windows,
            taken_at_epoch_s,
        } => Some((
            PendingKind::ListRestoreCandidates,
            Reply::RestoreCandidateList(RestoreCandidateListReply {
                windows,
                taken_at_epoch_s,
            }),
        )),
        DaemonMessage::WindowLayoutUpdated { window_id } => Some((
            PendingKind::UpdateWindowLayout,
            Reply::WindowLayoutUpdated(UpdateWindowLayoutReply { window_id }),
        )),
        DaemonMessage::ApproverRegistered { pending } => Some((
            PendingKind::RegisterApprover,
            Reply::ApproverRegistered(pending),
        )),
        DaemonMessage::ApprovalResolved { .. } => Some((PendingKind::DecideApproval, Reply::Done)),
        DaemonMessage::ChatPosted {
            room_id,
            seq,
            targets,
            user,
            unresolved,
            halted,
        } => Some((
            PendingKind::SendChat,
            Reply::ChatPosted(ChatPostedReply {
                room_id,
                seq,
                targets,
                user,
                unresolved,
                halted,
            }),
        )),
        DaemonMessage::ChatMessages {
            room_id,
            messages,
            next_after_seq,
            halted,
            needs_user,
            roster,
        } => Some((
            PendingKind::ReadChat,
            Reply::ChatMessages(ChatMessagesReply {
                room_id,
                messages,
                next_after_seq,
                halted,
                needs_user,
                roster,
            }),
        )),
        DaemonMessage::ChatRooms { rooms } => Some((
            PendingKind::ListChatRooms,
            Reply::ChatRoomList(ChatRoomListReply { rooms }),
        )),
        DaemonMessage::ProtocolRevision { revision } => Some((
            PendingKind::GetProtocolRevision,
            Reply::ProtocolRevision(revision),
        )),
        DaemonMessage::DaemonHealth { problems } => Some((
            PendingKind::GetDaemonHealth,
            Reply::DaemonHealth(DaemonHealthReply { problems }),
        )),
        DaemonMessage::PlanDetail { plan } => Some((PendingKind::PlanDetail, Reply::PlanDetail(plan))),
        DaemonMessage::PlanList { plans } => Some((PendingKind::PlanList, Reply::PlanList(plans))),
        DaemonMessage::PlanDeleted { plan_id } => {
            Some((PendingKind::PlanDeleted, Reply::PlanDeleted(plan_id)))
        }
        DaemonMessage::RoleList {
            roles,
            default_role,
        } => Some((
            PendingKind::RoleList,
            Reply::RoleList(RoleListReply {
                roles,
                default_role,
            }),
        )),
        DaemonMessage::TaskWindowOpened {
            plan_id,
            task_id,
            window_id,
            assignment,
        } => Some((
            PendingKind::TaskWindowOpened,
            Reply::TaskWindowOpened(TaskWindowOpenedReply {
                plan_id,
                task_id,
                window_id,
                assignment,
            }),
        )),
        DaemonMessage::ModelChoicesRequested {
            request_id,
            profile,
        } => Some((
            PendingKind::ListModelChoices,
            Reply::ModelChoicesRequested(ModelChoicesRequestedReply {
                request_id,
                profile,
            }),
        )),
        DaemonMessage::ModelAliasesSaved { profile, aliases } => Some((
            PendingKind::SaveModelAliases,
            Reply::ModelAliasesSaved(ModelAliasesSavedReply { profile, aliases }),
        )),
        DaemonMessage::ProfileEnabledSet { profile, enabled } => Some((
            PendingKind::SetProfileEnabled,
            Reply::ProfileEnabledSet(ProfileEnabledSetReply { profile, enabled }),
        )),
        DaemonMessage::ProfileDetail {
            profile,
            shipped_base,
            command,
            entries,
        } => Some((
            PendingKind::GetProfileDetail,
            Reply::ProfileDetail(ProfileDetailReply {
                profile,
                shipped_base,
                command,
                entries,
            }),
        )),
        DaemonMessage::ProfileCommandSet { profile, command } => Some((
            PendingKind::SetProfileCommand,
            Reply::ProfileCommandSet(ProfileCommandSetReply { profile, command }),
        )),
        DaemonMessage::ApprovalSettings { settings } => Some((
            PendingKind::GetApprovalSettings,
            Reply::ApprovalSettings(approval_settings_reply(settings)),
        )),
        DaemonMessage::AutoApproveSet { settings } => Some((
            PendingKind::SetAutoApprove,
            Reply::AutoApproveSet(approval_settings_reply(settings)),
        )),
        DaemonMessage::PlanApprovalRequiredSet { settings } => Some((
            PendingKind::SetPlanApprovalRequired,
            Reply::PlanApprovalRequiredSet(approval_settings_reply(settings)),
        )),
        DaemonMessage::McpRegistrationsListed {
            request_id,
            bridge,
            rows,
        } => Some((
            PendingKind::ListMcpRegistrations,
            Reply::McpRegistrationsListed(McpRegistrationsListedReply {
                request_id,
                bridge,
                rows,
            }),
        )),
        DaemonMessage::McpRegistrationRequested {
            request_id,
            profile,
            action,
        } => Some((
            PendingKind::RunMcpRegistration,
            Reply::McpRegistrationRequested(McpRegistrationRequestedReply {
                request_id,
                profile,
                action: mcp_registration_action_str(action).to_owned(),
            }),
        )),
        // Identified and HandoffSent are never received: this client sends
        // neither `identify` nor `send_handoff`.
        // ApprovalRequested / ApprovalWithdrawn are unsolicited event-class
        // messages `dispatch` handles directly rather than as a reply. The
        // rest stream to the WebView as events elsewhere, or answer nothing.
        DaemonMessage::Frame(_)
        | DaemonMessage::Exit { .. }
        | DaemonMessage::StatusChanged { .. }
        | DaemonMessage::Error { .. }
        | DaemonMessage::Pong
        | DaemonMessage::WindowStatus { .. }
        | DaemonMessage::TextSent { .. }
        | DaemonMessage::KeysSent { .. }
        | DaemonMessage::WaitResult { .. }
        | DaemonMessage::WindowOutput { .. }
        | DaemonMessage::Identified { .. }
        | DaemonMessage::ApprovalRequested { .. }
        | DaemonMessage::ApprovalWithdrawn { .. }
        | DaemonMessage::HandoffSent { .. }
        // ChatChanged, PlanChanged, ModelChoices and the two MCP registration
        // pushes are turned into events by `dispatch`; the ones answering a
        // request follow its acknowledgement rather than answering it.
        | DaemonMessage::ChatChanged { .. }
        | DaemonMessage::PlanChanged { .. }
        | DaemonMessage::ModelChoices { .. }
        | DaemonMessage::McpRegistrationStatus { .. }
        | DaemonMessage::McpRegistrationResult { .. }
        // ReportSent and Reports are never received: this client sends
        // neither `send_report` nor `wait_for_report`.
        | DaemonMessage::ReportSent { .. }
        | DaemonMessage::Reports { .. }
        // Delivery and SessionRecorded answer the doors a CLI's hook reads a
        // window's inbox through; this client never sends those.
        | DaemonMessage::Delivery { .. }
        | DaemonMessage::SessionRecorded { .. } => None,
    }
}

/// Builds the Tauri-facing settings reply from the wire shape, translating
/// the playbook state to the plain string the WebView narrows.
fn approval_settings_reply(settings: ApprovalSettingsInfo) -> ApprovalSettingsReply {
    ApprovalSettingsReply {
        playbook: playbook_state_str(settings.playbook).to_owned(),
        tools: settings
            .tools
            .into_iter()
            .map(|tool| ToolAutoApproveReply {
                tool: tool.tool,
                auto_approve: tool.auto_approve,
            })
            .collect(),
        plans_require_approval: settings.plans_require_approval,
    }
}

/// The wire's [`PlaybookState`] as the plain string [`ApprovalSettingsReply`]
/// carries.
fn playbook_state_str(state: PlaybookState) -> &'static str {
    match state {
        PlaybookState::Loaded => "loaded",
        PlaybookState::NotConfigured => "not_configured",
        PlaybookState::Failed => "failed",
        PlaybookState::Unknown => "unknown",
    }
}

/// The failure handed to the caller awaiting a request when the daemon
/// answers it with an error frame; any other frame is handed back.
///
/// Takes the whole frame, so the rejection carries every value the daemon
/// sent and no caller can substitute its own. Kept out of `dispatch` (which
/// needs a host and a live client) so what a rejection tells the
/// caller can be read and tested on its own.
///
/// # Errors
///
/// Returns `frame` itself when it is not an error frame.
pub fn rejection_for(frame: DaemonMessage) -> Result<IpcError, Box<DaemonMessage>> {
    match frame {
        DaemonMessage::Error {
            code,
            message,
            rejected_argument,
        } => Ok(IpcError::Rejected {
            code,
            message,
            rejected_argument,
        }),
        other => Err(Box::new(other)),
    }
}

/// Fails the request in flight with `rejection` when the error frame can be
/// attributed to it. Returns whether the session was superseded.
async fn settle_rejection(client: &IpcClient, rejection: IpcError) -> bool {
    let has_code =
        |wanted: &str| matches!(&rejection, IpcError::Rejected { code, .. } if code == wanted);
    let superseded = has_code(ERROR_CODE_SUPERSEDED);
    let attributable = match client.pending_kind().await {
        // Input, frame acknowledgements and `resize` are fire-and-forget on a
        // window connection, so an error frame there may answer one of them
        // rather than the request in flight; only the codes that request
        // itself can produce are attributed to it.
        Some(PendingKind::Attach | PendingKind::CopySelection) => {
            superseded || has_code(ERROR_CODE_INTERNAL)
        }
        // Nothing but the outstanding request is in flight on a control
        // connection, so the error can only answer it.
        Some(_) => true,
        None => false,
    };
    if attributable {
        client.fail_pending(rejection).await;
    }
    superseded
}

/// A WebView event carrying an MCP registration push. Serializes as its
/// payload alone, so the event name is the only thing telling the two apart.
#[derive(Clone, serde::Serialize)]
#[serde(untagged)]
enum McpRegistrationEvent {
    Status(McpRegistrationStatusPayload),
    Result(McpRegistrationResultPayload),
}

impl McpRegistrationEvent {
    fn name(&self) -> &'static str {
        match self {
            Self::Status(_) => EVENT_MCP_REGISTRATION_STATUS,
            Self::Result(_) => EVENT_MCP_REGISTRATION_RESULT,
        }
    }
}

/// The event an MCP registration push becomes; `None` for every other
/// message, the acknowledgements included.
fn mcp_registration_event_for(message: DaemonMessage) -> Option<McpRegistrationEvent> {
    match message {
        DaemonMessage::McpRegistrationStatus { request_id, status } => {
            Some(McpRegistrationEvent::Status(McpRegistrationStatusPayload {
                request_id,
                status,
            }))
        }
        DaemonMessage::McpRegistrationResult {
            request_id,
            profile,
            action,
            steps,
            error,
            status,
        } => Some(McpRegistrationEvent::Result(McpRegistrationResultPayload {
            request_id,
            profile,
            action,
            steps,
            error,
            status,
        })),
        _ => None,
    }
}

/// Delivers a window-scoped event: a frame goes to the mailbox the WebView
/// pulls from, an exit is emitted as an event.
fn emit_window_event(app: &impl GuiHost, event: WindowEvent) {
    match event {
        WindowEvent::Frame { window_id, frame } => {
            if let Some(mailbox) = app.frame_mailbox() {
                mailbox.push(&window_id, frame);
            }
        }
        WindowEvent::Exit {
            window_id,
            exit_code,
        } => app.emit_event(
            EVENT_EXIT,
            ExitPayload {
                window_id,
                exit_code,
            },
        ),
    }
}

/// Handles one daemon message. Returns whether the session was superseded.
/// `window_id` comes from the attached connection; events from a window-specific
/// connection include the window ID; control connections (attach-less) use None.
async fn dispatch(
    app: &impl GuiHost,
    client: &Arc<IpcClient>,
    message: DaemonMessage,
    window_id: Option<String>,
) -> bool {
    let message = match window_event_for(window_id.as_deref(), message) {
        Ok(event) => {
            emit_window_event(app, event);
            return false;
        }
        Err(message) => *message,
    };

    let message = match rejection_for(message) {
        Ok(rejection) => return settle_rejection(client, rejection).await,
        Err(message) => *message,
    };

    match message {
        DaemonMessage::StatusChanged {
            window_id,
            status,
            unknown_reason,
            status_rev,
        } => {
            app.emit_event(
                EVENT_STATUS,
                StatusPayload {
                    window_id,
                    status,
                    unknown_reason,
                    status_rev,
                },
            );
            false
        }
        DaemonMessage::ApprovalRequested { request } => {
            app.emit_event(
                EVENT_APPROVAL_REQUESTED,
                ApprovalRequestedPayload { request },
            );
            false
        }
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            app.emit_event(
                EVENT_APPROVAL_WITHDRAWN,
                ApprovalWithdrawnPayload { request_id, reason },
            );
            false
        }
        DaemonMessage::ChatChanged {
            room_id,
            seq,
            needs_user,
        } => {
            app.emit_event(
                EVENT_CHAT_CHANGED,
                ChatChangedPayload {
                    room_id,
                    seq,
                    needs_user,
                },
            );
            false
        }
        DaemonMessage::PlanChanged { plan_id } => {
            app.emit_event(EVENT_PLAN_CHANGED, PlanChangedPayload { plan_id });
            false
        }
        DaemonMessage::ModelChoices {
            request_id,
            profile,
            source,
            choices,
            error,
        } => {
            app.emit_event(
                EVENT_MODEL_CHOICES,
                ModelChoicesPayload {
                    request_id,
                    profile,
                    source,
                    choices,
                    error,
                },
            );
            false
        }
        message @ (DaemonMessage::McpRegistrationStatus { .. }
        | DaemonMessage::McpRegistrationResult { .. }) => {
            if let Some(event) = mcp_registration_event_for(message) {
                app.emit_event(event.name(), event);
            }
            false
        }
        other => {
            if let Some((kind, reply)) = reply_for(other) {
                client.complete_pending(kind, reply).await;
            }
            false
        }
    }
}

#[cfg(test)]
mod plan_tests;

#[cfg(test)]
mod model_tests;

#[cfg(test)]
mod profile_tests;

#[cfg(test)]
mod approval_tests;

#[cfg(test)]
mod mcp_registration_tests;

#[cfg(test)]
mod host_tests;

#[cfg(test)]
mod tests {
    use yagura_protocol::{
        ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, HandoffDelivery,
        WindowStatus, WithdrawReason,
    };

    use super::*;

    fn sample_approval_request(request_id: &str) -> ApprovalRequest {
        ApprovalRequest {
            request_id: request_id.to_owned(),
            tool: "close_window".to_owned(),
            requester: ApprovalRequester {
                kind: ApprovalRequesterKind::External,
                window_id: None,
                name: None,
            },
            target: Some(ApprovalTarget {
                window_id: "window-a".to_owned(),
                name: "window a".to_owned(),
                status: WindowStatus::Running,
            }),
            text: None,
            room: None,
        }
    }

    #[test]
    fn reply_for_classifies_approver_registered_as_a_register_approver_reply() {
        let pending = vec![sample_approval_request("req1")];
        let result = reply_for(DaemonMessage::ApproverRegistered {
            pending: pending.clone(),
        });
        match result {
            Some((PendingKind::RegisterApprover, Reply::ApproverRegistered(got))) => {
                assert_eq!(got, pending);
            }
            other => panic!("expected a RegisterApprover reply, got {other:?}"),
        }
    }

    #[test]
    fn reply_for_classifies_approval_resolved_as_a_decide_approval_reply() {
        let result = reply_for(DaemonMessage::ApprovalResolved {
            request_id: "req1".to_owned(),
        });
        assert!(matches!(
            result,
            Some((PendingKind::DecideApproval, Reply::Done))
        ));
    }

    #[test]
    fn reply_for_classifies_handoff_sent_as_answering_no_request_of_this_client() {
        // The GUI never sends `send_handoff`, so a HandoffSent frame completes
        // nothing it is waiting for.
        assert!(
            reply_for(DaemonMessage::HandoffSent {
                window_id: "window-a".to_owned(),
                opened: true,
                delivery: HandoffDelivery::Taken,
            })
            .is_none()
        );
    }

    #[test]
    fn reply_for_classifies_chat_rooms_as_a_list_chat_rooms_reply() {
        let rooms = vec![yagura_protocol::ChatRoomInfo {
            room_id: "main".to_owned(),
            name: "main".to_owned(),
            participant_count: 2,
            needs_user: false,
            halted: None,
            last_at_ms: None,
        }];
        let result = reply_for(DaemonMessage::ChatRooms {
            rooms: rooms.clone(),
        });
        match result {
            Some((PendingKind::ListChatRooms, Reply::ChatRoomList(got))) => {
                assert_eq!(got.rooms, rooms);
            }
            other => panic!("expected a ListChatRooms reply, got {other:?}"),
        }
    }

    #[test]
    fn reply_for_classifies_unsolicited_approval_flow_messages_as_events_not_replies() {
        assert!(
            reply_for(DaemonMessage::Identified {
                window_id: "window-a".to_owned(),
            })
            .is_none()
        );
        assert!(
            reply_for(DaemonMessage::ApprovalRequested {
                request: sample_approval_request("req2"),
            })
            .is_none()
        );
        assert!(
            reply_for(DaemonMessage::ApprovalWithdrawn {
                request_id: "req2".to_owned(),
                reason: WithdrawReason::Resolved,
            })
            .is_none()
        );
    }

    fn sample_frame(seq: u64) -> Frame {
        Frame {
            seq,
            cols: 2,
            rows: 1,
            reset: true,
            last: true,
            attrs: Vec::new(),
            lines: Vec::new(),
            cursor: yagura_protocol::CursorState {
                row: 0,
                col: 0,
                visible: true,
                shape: yagura_protocol::CursorShape::Block,
            },
            view: yagura_protocol::ViewState {
                display_offset: 0,
                history_rows: 0,
                mouse_reporting: false,
                alt_screen: false,
            },
            overrides: Vec::new(),
        }
    }

    #[test]
    fn window_event_from_frame_includes_window_id() {
        let msg = DaemonMessage::Frame(sample_frame(1));
        let event = window_event_for(Some("window-a"), msg).ok();
        assert_eq!(
            event,
            Some(WindowEvent::Frame {
                window_id: "window-a".to_owned(),
                frame: sample_frame(1),
            })
        );
    }

    #[test]
    fn window_event_from_frame_uses_correct_window_id() {
        let msg = DaemonMessage::Frame(sample_frame(2));
        let event = window_event_for(Some("window-b"), msg).ok();
        assert_eq!(
            event,
            Some(WindowEvent::Frame {
                window_id: "window-b".to_owned(),
                frame: sample_frame(2),
            })
        );
    }

    #[test]
    fn window_event_returns_none_for_control_connection() {
        let msg = DaemonMessage::Frame(sample_frame(1));
        assert_eq!(window_event_for(None, msg).ok(), None);
    }

    #[test]
    fn window_event_from_exit_includes_window_id() {
        let msg = DaemonMessage::Exit { exit_code: 42 };
        let event = window_event_for(Some("window-c"), msg).ok();
        assert_eq!(
            event,
            Some(WindowEvent::Exit {
                window_id: "window-c".to_owned(),
                exit_code: 42,
            })
        );
    }

    fn rejection_with_code(code: &str) -> IpcError {
        IpcError::Rejected {
            code: code.to_owned(),
            message: String::new(),
            rejected_argument: None,
        }
    }

    #[tokio::test]
    async fn a_superseded_error_frame_ends_the_session() {
        let client = IpcClient::new();
        assert!(settle_rejection(&client, rejection_with_code(ERROR_CODE_SUPERSEDED)).await);
    }

    #[tokio::test]
    async fn any_other_error_frame_leaves_the_session_open() {
        let client = IpcClient::new();
        assert!(!settle_rejection(&client, rejection_with_code(ERROR_CODE_INTERNAL)).await);
    }

    #[test]
    fn window_event_ignores_non_window_scoped_messages() {
        let msg = DaemonMessage::Pong;
        assert_eq!(window_event_for(Some("window-a"), msg).ok(), None);
    }
}
