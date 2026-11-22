//! Turns daemon messages into a completed request on the
//! [`crate::ipc_client::Generation`] awaiting it, a Tauri event delivered to
//! the WebView, or (frames) a part queued in the [`crate::frame_mailbox::FrameMailbox`].
//!
//! Separated from the connection machinery itself so the daemon protocol's
//! message shapes and the WebView's event vocabulary can be read (and
//! tested) without touching connection state.

use std::sync::Arc;

use tokio::io::AsyncReadExt;
use yagura_protocol::{
    ApprovalSettingsInfo, DaemonMessage, Frame, FrameLabel, PlaybookState,
    daemon_message_wire_tags, decode_correlated_daemon_message, read_frame_label,
};

use crate::commands::{
    ApprovalSettingsReply, ChatMessagesReply, ChatPostedReply, ChatRoomListReply,
    DaemonHealthReply, DetectionListReply, McpRegistrationRequestedReply,
    McpRegistrationsListedReply, ModelAliasesSavedReply, ModelChoicesRequestedReply,
    OpenWindowReply, ProfileCommandSetReply, ProfileDetailReply, ProfileEnabledSetReply,
    ProfileListReply, RestoreCandidateListReply, RoleListReply, TaskWindowOpenedReply,
    TicketListReply, ToolAutoApproveReply, UpdateWindowLayoutReply, mcp_registration_action_str,
};
use crate::constants::READ_CHUNK_LEN;
use crate::events::{
    ApprovalRequestedPayload, ApprovalWithdrawnPayload, AttachResponse, ChatChangedPayload,
    ChatHistoryClearedPayload, ChatParticipantsChangedPayload, EVENT_APPROVAL_REQUESTED,
    EVENT_APPROVAL_WITHDRAWN, EVENT_CHAT_CHANGED, EVENT_CHAT_HISTORY_CLEARED,
    EVENT_CHAT_PARTICIPANTS_CHANGED, EVENT_MCP_REGISTRATION_RESULT, EVENT_MCP_REGISTRATION_STATUS,
    EVENT_MODEL_CHOICES, EVENT_PLAN_CHANGED, EVENT_STATUS, EVENT_TICKET_CHANGED,
    McpRegistrationResultPayload, McpRegistrationStatusPayload, ModelChoicesPayload,
    PlanChangedPayload, StatusPayload, TicketChangedPayload,
};
use crate::host::GuiHost;
// The host tests drive what the reader dispatches through a client.
#[cfg(test)]
use crate::ipc_client::IpcClient;
use crate::ipc_client::{
    BoxedRead, ERROR_CODE_INTERNAL, ERROR_CODE_SUPERSEDED, EndKind, Generation, Interrupt,
    IpcError, PendingKind, ReaderLink, Reply, Settled, answers_carry_correlation_ids,
};

/// Window-scoped event to be emitted to the WebView. Separated from dispatch
/// so payload assembly stays separate from delivery.
#[derive(Debug, Clone, PartialEq)]
enum WindowEvent {
    Frame { window_id: String, frame: Frame },
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
        other => Err(Box::new(other)),
    }
}

/// A frame body as a message and the correlation id it carries, or, when
/// this build cannot read it as one, what the body says about itself.
fn read_body(body: &[u8]) -> Result<(Option<u64>, DaemonMessage), FrameLabel> {
    decode_correlated_daemon_message(body).map_err(|_| read_frame_label(body))
}

/// Reads frames off `reader` until the connection ends or the owner ends the
/// generation, dispatching each message to the WebView or to the request it
/// answers, and tells the owner how the generation ended.
///
/// Told to stop by a write that failed in a way that does not say the daemon
/// closed, it first takes in the bytes already delivered to this end, so a
/// frame the daemon sent before it closed still decides how the connection
/// ended; with none deciding, it ends as [`EndKind::WriteFailed`].
pub(crate) async fn read_loop<H: GuiHost>(
    mut reader: BoxedRead,
    app: H,
    generation: Arc<Generation>,
    mut link: ReaderLink,
) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_LEN];

    let end = loop {
        // Biased so that, with several ready, one way is always taken; with
        // the delivered bytes read on a stop, the end is the same either way.
        let read = tokio::select! {
            biased;
            interrupt = link.interrupted() => match interrupt {
                // The owner ended it, so there is nothing to tell the owner.
                Interrupt::Cancelled => return,
                Interrupt::Stopped => {
                    let end =
                        take_in_delivered(&mut reader, &app, &generation, &mut buffer, &mut chunk);
                    break end.await.unwrap_or(EndKind::WriteFailed);
                }
            },
            // `read` is cancel-safe, so no byte is lost by losing this race.
            read = reader.read(&mut chunk) => match read {
                Ok(0) => break EndKind::Eof,
                Err(_) => break EndKind::ReadError,
                Ok(read) => read,
            },
        };
        if let Some(end) = take_in(&app, &generation, &mut buffer, &chunk[..read]).await {
            break end;
        }
    };
    link.report(end);
}

/// Reads, a chunk at a time and no more than were delivered on entry, the
/// bytes already delivered to this end, taking each chunk in as a read does,
/// and returns how the generation must end when a frame says so.
async fn take_in_delivered<H: GuiHost>(
    reader: &mut BoxedRead,
    app: &H,
    generation: &Generation,
    buffer: &mut Vec<u8>,
    chunk: &mut [u8],
) -> Option<EndKind> {
    // An error says nothing of how the connection ended; what was read before
    // it has been taken in already.
    let mut left = reader.delivered_len().unwrap_or(0);
    while left > 0 {
        let want = left.min(chunk.len());
        let read = match reader.read_delivered(&mut chunk[..want]) {
            Ok(0) | Err(_) => return None,
            Ok(read) => read.min(want),
        };
        left -= read;
        if let Some(end) = take_in(app, generation, buffer, &chunk[..read]).await {
            return Some(end);
        }
    }
    None
}

/// Appends `bytes` to `buffer` and dispatches the whole frames it then holds.
async fn take_in<H: GuiHost>(
    app: &H,
    generation: &Generation,
    buffer: &mut Vec<u8>,
    bytes: &[u8],
) -> Option<EndKind> {
    buffer.extend_from_slice(bytes);
    dispatch_frames(app, generation, buffer).await
}

/// Dispatches every whole frame at the front of `buffer`, leaving a partial
/// one, and returns how the generation must end when a frame says so.
async fn dispatch_frames<H: GuiHost>(
    app: &H,
    generation: &Generation,
    buffer: &mut Vec<u8>,
) -> Option<EndKind> {
    loop {
        let (frame, consumed) = match yagura_protocol::frame_decode(buffer) {
            Ok(Some((body, consumed))) => (read_body(body), consumed),
            Ok(None) => return None,
            // A frame larger than the protocol allows can only come from a
            // peer that is not the daemon; drop the connection.
            Err(_) => return Some(EndKind::FrameTooLarge),
        };
        buffer.drain(..consumed);
        match frame {
            Ok((correlation_id, message)) => {
                let superseded = is_superseded(&message);
                if dispatch(
                    app,
                    generation,
                    correlation_id,
                    message,
                    generation.window_id(),
                )
                .await
                {
                    if superseded {
                        return Some(EndKind::Superseded);
                    }
                    eprintln!(
                        "warning: the daemon answered a request with a reply of another kind; reconnecting"
                    );
                    return Some(EndKind::UnreadableAnswer);
                }
            }
            Err(label) => {
                if settle_unreadable(generation, &label) {
                    eprintln!(
                        "warning: the daemon answered a request with a frame this build cannot read ({}); reconnecting",
                        known_tag_name(&label)
                    );
                    return Some(EndKind::UnreadableAnswer);
                }
            }
        }
    }
}

fn is_superseded(message: &DaemonMessage) -> bool {
    matches!(message, DaemonMessage::Error { code, .. } if code == ERROR_CODE_SUPERSEDED)
}

/// Weighs a frame this build cannot read as a message against the request
/// waiting. Returns whether the generation must end: the frame came under
/// that request's key.
///
/// From a daemon that returns no correlation id, a frame whose tag is the
/// kind of the answer awaited still ends the request: as success when the
/// answer carries nothing this client reads, as [`IpcError::UnexpectedReply`]
/// otherwise. Every other unreadable frame is dropped, as a push of a newer
/// daemon is.
fn settle_unreadable(generation: &Generation, label: &FrameLabel) -> bool {
    let Some(awaiting) = generation.awaiting() else {
        return false;
    };
    match label.correlation_id {
        Some(key) => key == awaiting.correlation_id,
        None if answers_carry_correlation_ids(generation.revision()) => false,
        None => {
            let Some((kind, carries)) = label.tag.as_deref().and_then(answer_kind) else {
                return false;
            };
            if kind != awaiting.kind {
                return false;
            }
            let result = if carries {
                eprintln!(
                    "warning: the daemon answered with a {} this build cannot read",
                    known_tag_name(label)
                );
                Err(IpcError::UnexpectedReply)
            } else {
                Ok(Reply::Done)
            };
            generation.settle(None, kind, result);
            false
        }
    }
}

/// The name this build gives the `type` tag of a frame it could not read:
/// the protocol's own spelling when the tag is one it decodes, so a peer's
/// arbitrary string never reaches a log line.
fn known_tag_name(label: &FrameLabel) -> &'static str {
    label
        .tag
        .as_deref()
        .and_then(|tag| {
            daemon_message_wire_tags()
                .iter()
                .copied()
                .find(|known| *known == tag)
        })
        .unwrap_or("a type this build does not know")
}

/// Writes the one table of the messages that answer a request: from it come
/// both [`reply_for`], which reads a message, and [`answer_kind`], which
/// reads only a tag. `bare` answers carry nothing this client reads.
macro_rules! answer_table {
    (
        bare { $( $bare:ident => $bare_kind:ident, )* }
        valued { $( $valued:ident $fields:tt => $valued_kind:ident : $reply:expr, )* }
        none { $( $none:pat ),* $(,)? }
    ) => {
        /// The `type` tag of each answer, named after its `DaemonMessage`
        /// variant and spelled by serde under the same `rename_all` as
        /// `DaemonMessage`, so no copy of the naming rule is kept here.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
        #[cfg_attr(test, derive(serde::Serialize))]
        #[serde(rename_all = "snake_case")]
        enum AnswerTag {
            $( $bare, )*
            $( $valued, )*
        }

        impl AnswerTag {
            #[cfg(test)]
            const ALL: &[Self] = &[ $( Self::$bare, )* $( Self::$valued, )* ];

            fn answer(self) -> (PendingKind, bool) {
                match self {
                    $( Self::$bare => (PendingKind::$bare_kind, false), )*
                    $( Self::$valued => (PendingKind::$valued_kind, true), )*
                }
            }
        }

        /// The request a daemon message answers, if it answers one at all.
        ///
        /// Matched exhaustively on purpose: a new protocol message must force a
        /// decision about whether it completes a request.
        fn reply_for(message: DaemonMessage) -> Option<(PendingKind, Reply)> {
            match message {
                $( DaemonMessage::$bare { .. } => Some((PendingKind::$bare_kind, Reply::Done)), )*
                $( DaemonMessage::$valued $fields => Some((PendingKind::$valued_kind, $reply)), )*
                $( $none )|* => None,
            }
        }

        /// The request an answer tagged `tag` belongs to, and whether this
        /// client reads anything it carries.
        fn answer_kind(tag: &str) -> Option<(PendingKind, bool)> {
            use serde::de::{Deserialize, IntoDeserializer, value::Error};
            AnswerTag::deserialize(IntoDeserializer::<Error>::into_deserializer(tag))
                .ok()
                .map(AnswerTag::answer)
        }
    };
}

answer_table! {
    bare {
        WindowClosed => CloseWindow,
        WindowInterrupted => InterruptWindow,
        ApprovalResolved => DecideApproval,
    }
    valued {
        Attached { child_exited, exit_code } => Attach: Reply::Attached(AttachResponse {
            child_exited,
            exit_code,
        }),
        WindowList { windows } => ListWindows: Reply::WindowList(windows),
        SelectionText { text } => CopySelection: Reply::SelectionText(text),
        WindowOpened { window_id, detection_warning, name } => OpenWindow:
            Reply::WindowOpened(OpenWindowReply {
                window_id,
                name,
                detection_warning,
            }),
        DetectionList { names, warning, .. } => ListDetections:
            Reply::DetectionList(DetectionListReply { names, warning }),
        ProfileList { profiles, warnings } => ListProfiles:
            Reply::ProfileList(ProfileListReply { profiles, warnings }),
        WindowRenamed { name, .. } => RenameWindow: Reply::WindowRenamed(name),
        RestoreCandidateList { windows, taken_at_epoch_s } => ListRestoreCandidates:
            Reply::RestoreCandidateList(RestoreCandidateListReply {
                windows,
                taken_at_epoch_s,
            }),
        WindowLayoutUpdated { window_id } => UpdateWindowLayout:
            Reply::WindowLayoutUpdated(UpdateWindowLayoutReply { window_id }),
        ApproverRegistered { pending } => RegisterApprover: Reply::ApproverRegistered(pending),
        ChatPosted { room_id, seq, targets, user, unresolved, halted } => SendChat:
            Reply::ChatPosted(ChatPostedReply {
                room_id,
                seq,
                targets,
                user,
                unresolved,
                halted,
            }),
        ChatMessages { room_id, messages, next_after_seq, halted, needs_user, roster } => ReadChat:
            Reply::ChatMessages(ChatMessagesReply {
                room_id,
                messages,
                next_after_seq,
                halted,
                needs_user,
                roster,
            }),
        ChatRooms { rooms } => ListChatRooms: Reply::ChatRoomList(ChatRoomListReply { rooms }),
        ChatRoomCleared { room_id } => ClearChatRoom: Reply::ChatRoomCleared(room_id),
        ChatRoomDeleted { room_id } => DeleteChatRoom: Reply::ChatRoomDeleted(room_id),
        ProtocolRevision { revision, boot } => GetProtocolRevision:
            Reply::ProtocolRevision { revision, boot },
        DaemonHealth { problems } => GetDaemonHealth:
            Reply::DaemonHealth(DaemonHealthReply { problems }),
        PlanDetail { plan } => PlanDetail: Reply::PlanDetail(plan),
        PlanList { plans } => PlanList: Reply::PlanList(plans),
        PlanDeleted { plan_id } => PlanDeleted: Reply::PlanDeleted(plan_id),
        RoleList { roles, default_role } => RoleList: Reply::RoleList(RoleListReply {
            roles,
            default_role,
        }),
        TaskWindowOpened { plan_id, task_id, window_id, assignment, .. } => TaskWindowOpened:
            Reply::TaskWindowOpened(TaskWindowOpenedReply {
                plan_id,
                task_id,
                window_id,
                assignment,
            }),
        ModelChoicesRequested { request_id, profile } => ListModelChoices:
            Reply::ModelChoicesRequested(ModelChoicesRequestedReply {
                request_id,
                profile,
            }),
        ModelAliasesSaved { profile, aliases } => SaveModelAliases:
            Reply::ModelAliasesSaved(ModelAliasesSavedReply { profile, aliases }),
        ProfileEnabledSet { profile, enabled } => SetProfileEnabled:
            Reply::ProfileEnabledSet(ProfileEnabledSetReply { profile, enabled }),
        ProfileDetail { profile, shipped_base, command, entries } => GetProfileDetail:
            Reply::ProfileDetail(ProfileDetailReply {
                profile,
                shipped_base,
                command,
                entries,
            }),
        ProfileCommandSet { profile, command } => SetProfileCommand:
            Reply::ProfileCommandSet(ProfileCommandSetReply { profile, command }),
        ApprovalSettings { settings } => GetApprovalSettings:
            Reply::ApprovalSettings(approval_settings_reply(settings)),
        AutoApproveSet { settings } => SetAutoApprove:
            Reply::AutoApproveSet(approval_settings_reply(settings)),
        PlanApprovalRequiredSet { settings } => SetPlanApprovalRequired:
            Reply::PlanApprovalRequiredSet(approval_settings_reply(settings)),
        McpRegistrationsListed { request_id, bridge, rows } => ListMcpRegistrations:
            Reply::McpRegistrationsListed(McpRegistrationsListedReply {
                request_id,
                bridge,
                rows,
            }),
        McpRegistrationRequested { request_id, profile, action } => RunMcpRegistration:
            Reply::McpRegistrationRequested(McpRegistrationRequestedReply {
                request_id,
                profile,
                action: mcp_registration_action_str(action).to_owned(),
            }),
        TicketWorkspaceList { workspaces } => TicketWorkspaceList:
            Reply::TicketWorkspaceList(workspaces),
        TicketList { workspace_id, config, config_error, milestones, tickets, truncated } => TicketList:
            Reply::TicketList(TicketListReply {
                workspace_id,
                config,
                config_error,
                milestones,
                tickets,
                truncated,
            }),
        TicketDetail { ticket } => TicketDetail: Reply::TicketDetail(ticket),
    }
    none {
        // Identified, HandoffSent and MessageWithdrawn are never received:
        // this client sends none of `identify`, `send_handoff` and
        // `withdraw_message`.
        // ApprovalRequested / ApprovalWithdrawn are unsolicited event-class
        // messages `dispatch` handles directly rather than as a reply. The
        // rest stream to the WebView as events elsewhere, or answer nothing.
        DaemonMessage::Frame(_),
        DaemonMessage::StatusChanged { .. },
        DaemonMessage::Error { .. },
        DaemonMessage::Pong,
        DaemonMessage::WindowStatus { .. },
        DaemonMessage::TextSent { .. },
        DaemonMessage::WaitResult { .. },
        DaemonMessage::WindowOutput { .. },
        DaemonMessage::Identified { .. },
        DaemonMessage::ApprovalRequested { .. },
        DaemonMessage::ApprovalWithdrawn { .. },
        DaemonMessage::HandoffSent { .. },
        DaemonMessage::MessageWithdrawn { .. },
        // ChatChanged, PlanChanged, TicketChanged, ModelChoices and the two
        // MCP registration pushes are turned into events by `dispatch`; the
        // ones answering a request follow its acknowledgement rather than
        // answering it.
        DaemonMessage::ChatChanged { .. },
        DaemonMessage::ChatParticipantsChanged,
        DaemonMessage::ChatHistoryCleared { .. },
        DaemonMessage::PlanChanged { .. },
        DaemonMessage::TicketChanged { .. },
        DaemonMessage::ModelChoices { .. },
        DaemonMessage::McpRegistrationStatus { .. },
        DaemonMessage::McpRegistrationResult { .. },
        // ReportSent and Reports are never received: this client sends
        // neither `send_report` nor `wait_for_report`.
        DaemonMessage::ReportSent { .. },
        DaemonMessage::Reports { .. },
        // Delivery, DeliverConfirmed and SessionRecorded answer the doors a
        // CLI's hook reads a window's inbox through; this client never sends
        // those.
        DaemonMessage::Delivery { .. },
        DaemonMessage::DeliverConfirmed { .. },
        DaemonMessage::SessionRecorded { .. },
        // MilestoneSet is never received: the GUI only reads tickets.
        DaemonMessage::MilestoneSet { .. },
        // HookInputWanted answers a hook's request, which the GUI never sends.
        DaemonMessage::HookInputWanted { .. },
        // LaunchOverlay answers a window's overlay door, which the GUI is not.
        DaemonMessage::LaunchOverlay { .. },
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

/// Fails the request in flight with `rejection` when the error frame is its
/// answer. Returns whether the session was superseded.
///
/// A `superseded` frame ends the connection and answers no request, under
/// any key and from any revision; the request waiting ends with the
/// generation.
async fn settle_rejection(
    generation: &Generation,
    correlation_id: Option<u64>,
    rejection: IpcError,
) -> bool {
    let has_code =
        |wanted: &str| matches!(&rejection, IpcError::Rejected { code, .. } if code == wanted);
    if has_code(ERROR_CODE_SUPERSEDED) {
        return true;
    }
    let Some(awaiting) = generation.awaiting() else {
        return false;
    };
    let attributable = match (correlation_id, generation.revision()) {
        // The key names the request; `settle` compares it.
        (Some(_), _) => true,
        // Such a daemon names every answer, so an error under no key answers
        // nothing waiting: a refused send.
        (None, revision) if answers_carry_correlation_ids(revision) => false,
        // Still probing: the probe is the only frame written.
        (None, None) => true,
        // Input, frame acknowledgements and `resize` are fire-and-forget on a
        // window connection, so an error frame there may answer one of them
        // rather than the request in flight; only the codes that request
        // itself can produce are attributed to it.
        (None, Some(_)) => match awaiting.kind {
            PendingKind::Attach | PendingKind::CopySelection => has_code(ERROR_CODE_INTERNAL),
            // Nothing but the outstanding request is in flight on a control
            // connection, so the error can only answer it.
            _ => true,
        },
    };
    if attributable {
        generation.settle(correlation_id, awaiting.kind, Err(rejection));
    }
    false
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
/// pulls from.
fn emit_window_event(app: &impl GuiHost, event: WindowEvent) {
    match event {
        WindowEvent::Frame { window_id, frame } => {
            if let Some(mailbox) = app.frame_mailbox() {
                mailbox.push(&window_id, frame);
            }
        }
    }
}

/// Handles one daemon message that arrived under `correlation_id`. Returns
/// whether the generation must end: the session was superseded, or the
/// daemon answered the request waiting, under its key, with an answer of
/// another kind. `window_id` comes from the attached connection; events from
/// a window-specific connection include the window ID; control connections
/// (attach-less) use None.
async fn dispatch(
    app: &impl GuiHost,
    generation: &Generation,
    correlation_id: Option<u64>,
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
        Ok(rejection) => return settle_rejection(generation, correlation_id, rejection).await,
        Err(message) => *message,
    };

    match message {
        DaemonMessage::StatusChanged {
            window_id,
            status,
            unknown_reason,
            status_rev,
            status_boot,
            task_report,
            exit_code,
        } => {
            app.emit_event(
                EVENT_STATUS,
                StatusPayload {
                    window_id,
                    status,
                    unknown_reason,
                    status_rev,
                    status_boot,
                    task_report,
                    exit_code,
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
        DaemonMessage::ChatParticipantsChanged => {
            app.emit_event(
                EVENT_CHAT_PARTICIPANTS_CHANGED,
                ChatParticipantsChangedPayload {},
            );
            false
        }
        DaemonMessage::ChatHistoryCleared { room_id } => {
            app.emit_event(
                EVENT_CHAT_HISTORY_CLEARED,
                ChatHistoryClearedPayload { room_id },
            );
            false
        }
        DaemonMessage::PlanChanged { plan_id } => {
            app.emit_event(EVENT_PLAN_CHANGED, PlanChangedPayload { plan_id });
            false
        }
        DaemonMessage::TicketChanged { workspace_id } => {
            app.emit_event(EVENT_TICKET_CHANGED, TicketChangedPayload { workspace_id });
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
        other => reply_for(other).is_some_and(|(kind, reply)| {
            generation.settle(correlation_id, kind, Ok(reply)) == Settled::Broken
        }),
    }
}

#[cfg(test)]
mod plan_tests;

#[cfg(test)]
mod ticket_tests;

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
        ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, DeliveryState,
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
                id: "envelope-1".to_owned(),
                state: DeliveryState::Delivered,
                note: None,
                name: None,
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
            has_open_windows: false,
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

    fn rejection_with_code(code: &str) -> IpcError {
        IpcError::Rejected {
            code: code.to_owned(),
            message: String::new(),
            rejected_argument: None,
        }
    }

    /// A generation over an in-memory stream that nobody answers on, with the
    /// far end and the reader's ticket kept alive beside it.
    fn idle_generation() -> (
        Arc<crate::ipc_client::Generation>,
        tokio::io::DuplexStream,
        impl Sized,
    ) {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let (generation, reader) = crate::ipc_client::Generation::wired(near);
        (generation, far, reader)
    }

    #[tokio::test]
    async fn a_superseded_error_frame_ends_the_session() {
        let (generation, _peer, _reader) = idle_generation();
        assert!(
            settle_rejection(
                &generation,
                None,
                rejection_with_code(ERROR_CODE_SUPERSEDED)
            )
            .await
        );
    }

    #[tokio::test]
    async fn any_other_error_frame_leaves_the_session_open() {
        let (generation, _peer, _reader) = idle_generation();
        assert!(
            !settle_rejection(&generation, None, rejection_with_code(ERROR_CODE_INTERNAL)).await
        );
    }

    #[test]
    fn every_answer_tag_is_a_distinct_decoder_tag_and_no_other_tag_answers() {
        let wire = daemon_message_wire_tags();
        let mut answers = std::collections::BTreeSet::new();
        for &item in AnswerTag::ALL {
            let tag = match serde_json::to_value(item) {
                Ok(serde_json::Value::String(tag)) => tag,
                other => panic!("{item:?} did not serialize as a tag: {other:?}"),
            };
            assert!(
                wire.contains(&tag.as_str()),
                "{item:?} reads `{tag}`, which the decoder does not accept"
            );
            assert_eq!(answer_kind(&tag), Some(item.answer()), "{item:?}");
            assert!(answers.insert(tag), "{item:?} shares its tag");
        }
        for &tag in wire.iter().filter(|tag| !answers.contains(**tag)) {
            assert_eq!(answer_kind(tag), None, "`{tag}` answers no request");
        }
    }

    #[test]
    fn window_event_ignores_non_window_scoped_messages() {
        let msg = DaemonMessage::Pong;
        assert_eq!(window_event_for(Some("window-a"), msg).ok(), None);
    }
}
