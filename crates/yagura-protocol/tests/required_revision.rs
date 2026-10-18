#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `required_protocol_revision`: the pure function that turns one
//! `ClientMessage` into the wire revision a peer must speak before this
//! message may be sent to it.
//!
//! The caller never supplies this value itself -- it is derived from the
//! message alone, so a caller cannot forget to add it and a gate never
//! carries a second, competing copy of the same rule.
//!
//! Three properties are measured:
//!
//! - A message that can carry a launch option (`open_window_from_profile` /
//!   `send_handoff`) requires the launch-option threshold when `model` or
//!   `effort` is present.
//! - The same two message kinds, with neither `model` nor `effort` present,
//!   require no more than the baseline revision.
//! - Every other variant requires no more than the baseline revision,
//!   regardless of its fields.
//! - `approve_plan` / `mark_plan_seen` require `PLAN_REVIEW_MIN_REVISION`;
//!   `save_roles` requires it only when it carries a `default_role`.
//! - `set_profile_enabled` requires `PROFILE_ENABLED_MIN_REVISION` whatever
//!   value it carries.
//! - `get_approval_settings` / `set_auto_approve` / `set_plan_approval_required`
//!   require `APPROVAL_SETTINGS_MIN_REVISION` whatever value they carry.
//! - `list_mcp_registrations` / `run_mcp_registration` require
//!   `MCP_REGISTRATION_MIN_REVISION` whatever value they carry.
//! - `attach_view`, `frame_ack`, `set_palette`, `set_visible`, `copy_selection` and `scroll_view`
//!   require `FRAME_MIN_REVISION` whatever value they carry.
//! - The structured input messages require `STRUCTURED_INPUT_MIN_REVISION`
//!   (measured in `tests/structured_input_protocol.rs`).
//!
//! # Guarding against a silently uncovered future variant
//!
//! The last two properties are checked over a corpus holding exactly one
//! instance per `ClientMessage` variant. Rather than trust that list to stay
//! complete by inspection, [`corpus_covers_every_client_message_variant`]
//! compares the wire tags it builds against
//! [`yagura_protocol::client_message_wire_tags`] -- the decoder's own tag
//! table -- the same technique the wire ledger (`tests/wire_inventory.rs`)
//! uses for its own corpus completeness check. A variant added to
//! `ClientMessage` without a matching entry here fails that comparison, so
//! the "no more than baseline" properties below cannot go on passing over an
//! uncovered variant without that test failing first.

use std::collections::BTreeSet;

use serde_json::Value;

use yagura_protocol::{
    APPROVAL_SETTINGS_MIN_REVISION, CellPosition, ClientMessage, ControlKey,
    DAEMON_HEALTH_MIN_REVISION, FRAME_MIN_REVISION, Key, KeyInput, LAUNCH_OPTIONS_MIN_REVISION,
    MCP_REGISTRATION_MIN_REVISION, MODEL_ALIASES_MIN_REVISION, McpRegistrationAction, Modifiers,
    NamedKey, PLAN_REVIEW_MIN_REVISION, PROFILE_DETAIL_MIN_REVISION, PROFILE_ENABLED_MIN_REVISION,
    PROTOCOL_REVISION_BASELINE, Palette, PlanTaskSpec, PointerButton, PointerInput, PointerKind,
    ReportOutcome, TASK_INSTRUCTION_MIN_REVISION, TaskAssignment, WheelInput, WindowStatus,
    required_protocol_revision,
};

fn wire_tag_of(msg: &ClientMessage) -> String {
    let encoded = serde_json::to_value(msg).expect("a ClientMessage must serialize to JSON");
    encoded
        .get("type")
        .and_then(Value::as_str)
        .expect("an internally-tagged ClientMessage must carry a \"type\" field")
        .to_string()
}

/// One instance of every `ClientMessage` variant. `open_window_from_profile`
/// and `send_handoff` are built with neither `model` nor `effort` present:
/// their launch-option-bearing form is covered separately by
/// [`launch_option_bearing_messages`].
fn one_message_per_variant_without_launch_option() -> Vec<ClientMessage> {
    vec![
        ClientMessage::AttachView {
            window_id: "w1".to_string(),
            size: None,
        },
        ClientMessage::FrameAck { seq: 1 },
        ClientMessage::SetPalette(Palette {
            foreground: [1, 2, 3],
            background: [4, 5, 6],
            cursor: [7, 8, 9],
            ansi: vec![[0, 0, 0]; 16],
        }),
        ClientMessage::SetVisible { visible: false },
        ClientMessage::CopySelection,
        ClientMessage::ClearSelection,
        ClientMessage::ScrollView { lines: -1 },
        ClientMessage::Key(KeyInput {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::default(),
        }),
        ClientMessage::Text {
            text: "a".to_string(),
        },
        ClientMessage::Paste {
            text: "a".to_string(),
        },
        ClientMessage::Pointer(PointerInput {
            kind: PointerKind::Down,
            button: PointerButton::Left,
            cell: CellPosition { col: 0, row: 0 },
            mods: Modifiers::default(),
            clicks: 1,
        }),
        ClientMessage::Wheel(WheelInput {
            delta: 1,
            cell: CellPosition { col: 0, row: 0 },
            mods: Modifiers::default(),
        }),
        ClientMessage::Focus { focused: true },
        ClientMessage::Resize { cols: 80, rows: 24 },
        ClientMessage::Ping,
        ClientMessage::Shutdown,
        ClientMessage::StatusEvent {
            source: "cli".to_string(),
            status: WindowStatus::Idle,
            window_id: None,
            token: "tok".to_string(),
        },
        ClientMessage::OpenWindow {
            command: vec!["cmd".to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
        ClientMessage::ListWindows,
        ClientMessage::GetWindowStatus {
            window_id: "w1".to_string(),
        },
        ClientMessage::SendText {
            window_id: "w1".to_string(),
            text: "hi".to_string(),
        },
        ClientMessage::SendKeys {
            window_id: "w1".to_string(),
            keys: vec![ControlKey::Enter],
        },
        ClientMessage::InterruptWindow {
            window_id: "w1".to_string(),
        },
        ClientMessage::CloseWindow {
            window_id: "w1".to_string(),
            force: None,
        },
        ClientMessage::WaitForStatus {
            window_id: "w1".to_string(),
            target_status: WindowStatus::Idle,
            timeout_sec: 5,
        },
        ClientMessage::RenameWindow {
            window_id: "w1".to_string(),
            name: "n".to_string(),
        },
        ClientMessage::ReadOutput {
            window_id: "w1".to_string(),
            lines: None,
        },
        ClientMessage::ListDetections,
        ClientMessage::ListRestoreCandidates,
        ClientMessage::UpdateWindowLayout {
            window_id: "w1".to_string(),
            layout_json: "{}".to_string(),
        },
        ClientMessage::ListProfiles,
        ClientMessage::OpenWindowFromProfile {
            profile: "p".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            on_name_taken: Default::default(),
        },
        ClientMessage::Identify {
            window_id: "w1".to_string(),
            token: "tok".to_string(),
        },
        ClientMessage::RegisterApprover,
        ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("p".to_string()),
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "r".to_string(),
            note: "n".to_string(),
        },
        ClientMessage::ApprovalDecision {
            request_id: "r1".to_string(),
            approved: true,
        },
        ClientMessage::SendChat {
            room_id: None,
            body: "hi".to_string(),
        },
        ClientMessage::BroadcastChat {
            room_id: None,
            body: "hi".to_string(),
        },
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        },
        ClientMessage::ListChatRooms,
        ClientMessage::GetProtocolRevision,
        ClientMessage::GetDaemonHealth,
        ClientMessage::CreatePlan {
            title: "t".to_string(),
            tasks: vec![],
        },
        ClientMessage::UpdatePlan {
            plan_id: "p1".to_string(),
            title: None,
            tasks: vec![],
        },
        ClientMessage::GetPlan {
            plan_id: "p1".to_string(),
        },
        ClientMessage::ListPlans,
        ClientMessage::DeletePlan {
            plan_id: "p1".to_string(),
        },
        ClientMessage::ListRoles,
        ClientMessage::SaveRoles {
            roles: vec![],
            default_role: None,
        },
        ClientMessage::SetTaskAssignment {
            plan_id: "p1".to_string(),
            task_id: "t1".to_string(),
            assignment: TaskAssignment {
                profile: "p".to_string(),
                model: None,
                effort: None,
            },
        },
        ClientMessage::ClearTaskAssignment {
            plan_id: "p1".to_string(),
            task_id: "t1".to_string(),
        },
        ClientMessage::SetPlanOwner {
            plan_id: "p1".to_string(),
            window_id: "w1".to_string(),
        },
        ClientMessage::OpenTaskWindow {
            plan_id: "p1".to_string(),
            task_id: "t1".to_string(),
            name: None,
        },
        ClientMessage::ApprovePlan {
            plan_id: "p1".to_string(),
            version: 1,
        },
        ClientMessage::MarkPlanSeen {
            plan_id: "p1".to_string(),
            version: 1,
        },
        ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
        ClientMessage::WaitForReport { timeout_sec: 0 },
        ClientMessage::ListModelChoices {
            profile: "p".to_string(),
            request_id: 1,
        },
        ClientMessage::SaveModelAliases {
            profile: "p".to_string(),
            aliases: vec![],
        },
        ClientMessage::SetProfileEnabled {
            profile: "p".to_string(),
            enabled: false,
        },
        ClientMessage::GetProfileDetail {
            profile: "p".to_string(),
        },
        ClientMessage::SetProfileCommand {
            profile: "p".to_string(),
            argv: None,
        },
        ClientMessage::GetApprovalSettings {},
        ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve: false,
        },
        ClientMessage::SetPlanApprovalRequired { required: false },
        ClientMessage::ListMcpRegistrations { request_id: 1 },
        ClientMessage::RunMcpRegistration {
            profile: "p".to_string(),
            action: McpRegistrationAction::Register,
            request_id: 1,
        },
        ClientMessage::DeliverContext {
            window_id: "w1".to_string(),
            token: "tok".to_string(),
        },
        ClientMessage::DeliverTake {
            window_id: "w1".to_string(),
            token: "tok".to_string(),
        },
        ClientMessage::DeliverWait {
            window_id: "w1".to_string(),
            token: "tok".to_string(),
        },
        ClientMessage::ReportSession {
            window_id: "w1".to_string(),
            token: "tok".to_string(),
            session: "s".to_string(),
        },
    ]
}

fn is_launch_option_bearing_variant(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::OpenWindowFromProfile { .. } | ClientMessage::SendHandoff { .. }
    )
}

fn is_model_alias_variant(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::ListModelChoices { .. } | ClientMessage::SaveModelAliases { .. }
    )
}

/// `open_window_from_profile` / `send_handoff`, each once with `model` set
/// and once with `effort` set.
fn launch_option_bearing_messages() -> Vec<ClientMessage> {
    vec![
        ClientMessage::OpenWindowFromProfile {
            profile: "p".to_string(),
            cwd: None,
            name: None,
            model: Some("m".to_string()),
            effort: None,
            on_name_taken: Default::default(),
        },
        ClientMessage::OpenWindowFromProfile {
            profile: "p".to_string(),
            cwd: None,
            name: None,
            model: None,
            effort: Some("e".to_string()),
            on_name_taken: Default::default(),
        },
        ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("p".to_string()),
            cwd: None,
            name: None,
            model: Some("m".to_string()),
            effort: None,
            reference: "r".to_string(),
            note: "n".to_string(),
        },
        ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("p".to_string()),
            cwd: None,
            name: None,
            model: None,
            effort: Some("e".to_string()),
            reference: "r".to_string(),
            note: "n".to_string(),
        },
    ]
}

#[test]
fn corpus_covers_every_client_message_variant() {
    let built_tags: BTreeSet<String> = one_message_per_variant_without_launch_option()
        .iter()
        .map(wire_tag_of)
        .collect();
    let decoder_tags: BTreeSet<String> = yagura_protocol::client_message_wire_tags()
        .iter()
        .map(|tag| (*tag).to_string())
        .collect();
    assert_eq!(
        built_tags, decoder_tags,
        "this file's per-variant corpus must build exactly one message per tag the decoder \
         accepts, or a variant added later could keep defaulting to \"no constraint\" here \
         without ever being checked"
    );
}

#[test]
fn messages_carrying_a_launch_option_require_the_launch_option_threshold() {
    for msg in launch_option_bearing_messages() {
        assert_eq!(
            required_protocol_revision(&msg),
            LAUNCH_OPTIONS_MIN_REVISION,
            "{msg:?} carries a launch option and must require the launch-option threshold"
        );
    }
}

#[test]
fn open_window_from_profile_and_send_handoff_without_a_launch_option_require_no_more_than_baseline()
{
    for msg in one_message_per_variant_without_launch_option()
        .into_iter()
        .filter(is_launch_option_bearing_variant)
    {
        assert_eq!(
            required_protocol_revision(&msg),
            PROTOCOL_REVISION_BASELINE,
            "{msg:?} carries no launch option and must require no more than the baseline revision"
        );
    }
}

#[test]
fn every_other_variant_requires_no_more_than_baseline() {
    for msg in one_message_per_variant_without_launch_option()
        .into_iter()
        .filter(|msg| {
            !is_launch_option_bearing_variant(msg)
                && !is_model_alias_variant(msg)
                && !matches!(
                    msg,
                    ClientMessage::AttachView { .. }
                        | ClientMessage::FrameAck { .. }
                        | ClientMessage::SetPalette(_)
                        | ClientMessage::SetVisible { .. }
                        | ClientMessage::CopySelection
                        | ClientMessage::ClearSelection
                        | ClientMessage::ScrollView { .. }
                        | ClientMessage::Key(_)
                        | ClientMessage::Text { .. }
                        | ClientMessage::Paste { .. }
                        | ClientMessage::Pointer(_)
                        | ClientMessage::Wheel(_)
                        | ClientMessage::Focus { .. }
                        | ClientMessage::SendKeys { .. }
                        | ClientMessage::ApprovePlan { .. }
                        | ClientMessage::MarkPlanSeen { .. }
                        | ClientMessage::SetProfileEnabled { .. }
                        | ClientMessage::GetProfileDetail { .. }
                        | ClientMessage::SetProfileCommand { .. }
                        | ClientMessage::GetApprovalSettings { .. }
                        | ClientMessage::SetAutoApprove { .. }
                        | ClientMessage::SetPlanApprovalRequired { .. }
                        | ClientMessage::ListMcpRegistrations { .. }
                        | ClientMessage::RunMcpRegistration { .. }
                        | ClientMessage::GetDaemonHealth
                        | ClientMessage::DeliverContext { .. }
                        | ClientMessage::DeliverTake { .. }
                        | ClientMessage::DeliverWait { .. }
                        | ClientMessage::ReportSession { .. }
                )
        })
    {
        assert_eq!(
            required_protocol_revision(&msg),
            PROTOCOL_REVISION_BASELINE,
            "{msg:?} cannot carry a launch option and must require no more than the baseline \
             revision"
        );
    }
}

#[test]
fn send_keys_requires_revision_six() {
    let message = ClientMessage::SendKeys {
        window_id: "w1".to_string(),
        keys: vec![ControlKey::Enter],
    };
    assert_eq!(required_protocol_revision(&message), 6);
}

// =============================================================================
// create_plan / update_plan requires
// TASK_INSTRUCTION_MIN_REVISION when any task carries an `instruction`, and
// no more than baseline when none does.
// =============================================================================

fn plan_task_spec_with_instruction(instruction: Option<&str>) -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: vec![],
        writes: vec![],
        instruction: instruction.map(str::to_string),
    }
}

#[test]
fn task_instruction_min_revision_is_five() {
    assert_eq!(TASK_INSTRUCTION_MIN_REVISION, 5);
}

#[test]
fn create_plan_or_update_plan_with_any_task_instruction_requires_the_instruction_threshold() {
    let messages = [
        ClientMessage::CreatePlan {
            title: "t".to_string(),
            tasks: vec![
                plan_task_spec_with_instruction(None),
                plan_task_spec_with_instruction(Some("read this before you start")),
            ],
        },
        ClientMessage::UpdatePlan {
            plan_id: "p1".to_string(),
            title: None,
            tasks: vec![plan_task_spec_with_instruction(Some(
                "read this before you start",
            ))],
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            TASK_INSTRUCTION_MIN_REVISION,
            "{msg:?} carries a task instruction and must require the instruction threshold"
        );
    }
}

#[test]
fn create_plan_or_update_plan_with_no_task_instruction_requires_no_more_than_baseline() {
    let messages = [
        ClientMessage::CreatePlan {
            title: "t".to_string(),
            tasks: vec![plan_task_spec_with_instruction(None)],
        },
        ClientMessage::UpdatePlan {
            plan_id: "p1".to_string(),
            title: None,
            tasks: vec![plan_task_spec_with_instruction(None)],
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            PROTOCOL_REVISION_BASELINE,
            "{msg:?} carries no task instruction and must require no more than the baseline \
             revision"
        );
    }
}

// =============================================================================
// approve_plan / mark_plan_seen require PLAN_REVIEW_MIN_REVISION;
// save_roles requires it only when it carries a default_role.
// =============================================================================

#[test]
fn plan_review_min_revision_is_nine() {
    assert_eq!(PLAN_REVIEW_MIN_REVISION, 9);
}

#[test]
fn approve_plan_and_mark_plan_seen_require_the_plan_review_threshold() {
    let messages = [
        ClientMessage::ApprovePlan {
            plan_id: "p1".to_string(),
            version: 1,
        },
        ClientMessage::MarkPlanSeen {
            plan_id: "p1".to_string(),
            version: 1,
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            PLAN_REVIEW_MIN_REVISION,
            "{msg:?} is approver-only and must require the plan-review threshold"
        );
    }
}

#[test]
fn save_roles_with_a_default_role_requires_the_plan_review_threshold() {
    let msg = ClientMessage::SaveRoles {
        roles: vec![],
        default_role: Some("tester".to_string()),
    };
    assert_eq!(
        required_protocol_revision(&msg),
        PLAN_REVIEW_MIN_REVISION,
        "a save_roles carrying a default_role must require the plan-review threshold"
    );
}

#[test]
fn save_roles_without_a_default_role_requires_no_more_than_baseline() {
    let msg = ClientMessage::SaveRoles {
        roles: vec![],
        default_role: None,
    };
    assert_eq!(
        required_protocol_revision(&msg),
        PROTOCOL_REVISION_BASELINE,
        "a save_roles carrying no default_role must require no more than the baseline revision"
    );
}

// =============================================================================
// list_model_choices / save_model_aliases always require
// MODEL_ALIASES_MIN_REVISION, regardless of their fields.
// =============================================================================

#[test]
fn model_alias_min_revision_is_ten() {
    assert_eq!(MODEL_ALIASES_MIN_REVISION, 10);
}

#[test]
fn model_alias_messages_require_the_model_alias_threshold() {
    let messages = [
        ClientMessage::ListModelChoices {
            profile: "p".to_string(),
            request_id: 1,
        },
        ClientMessage::SaveModelAliases {
            profile: "p".to_string(),
            aliases: vec![],
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            MODEL_ALIASES_MIN_REVISION,
            "{msg:?} is a model-alias message and must require the model-alias threshold"
        );
    }
}

// =============================================================================
// set_profile_enabled always requires PROFILE_ENABLED_MIN_REVISION,
// regardless of the value it carries.
// =============================================================================

#[test]
fn profile_enabled_min_revision_is_eleven() {
    assert_eq!(PROFILE_ENABLED_MIN_REVISION, 11);
}

#[test]
fn set_profile_enabled_requires_revision_eleven_for_both_values() {
    for enabled in [true, false] {
        let msg = ClientMessage::SetProfileEnabled {
            profile: "p".to_string(),
            enabled,
        };
        assert_eq!(
            required_protocol_revision(&msg),
            11,
            "{msg:?} is understood only from revision 11 on"
        );
        assert_eq!(
            required_protocol_revision(&msg),
            PROFILE_ENABLED_MIN_REVISION,
            "{msg:?} must require the profile-enabled threshold"
        );
    }
}

// =============================================================================
// get_profile_detail / set_profile_command always require
// PROFILE_DETAIL_MIN_REVISION, whatever they carry.
// =============================================================================

#[test]
fn profile_detail_messages_require_the_profile_detail_threshold() {
    let messages = [
        ClientMessage::GetProfileDetail {
            profile: "p".to_string(),
        },
        ClientMessage::SetProfileCommand {
            profile: "p".to_string(),
            argv: None,
        },
        ClientMessage::SetProfileCommand {
            profile: "p".to_string(),
            argv: Some(vec!["c".to_string()]),
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            PROFILE_DETAIL_MIN_REVISION,
            "{msg:?} is understood only from the profile-detail revision on"
        );
    }
}

// =============================================================================
// get_approval_settings / set_auto_approve / set_plan_approval_required
// always require APPROVAL_SETTINGS_MIN_REVISION, regardless of their fields.
// =============================================================================

#[test]
fn approval_settings_min_revision_is_twelve() {
    assert_eq!(APPROVAL_SETTINGS_MIN_REVISION, 12);
}

#[test]
fn approval_settings_messages_require_the_approval_settings_threshold() {
    let messages = [
        ClientMessage::GetApprovalSettings {},
        ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve: true,
        },
        ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve: false,
        },
        ClientMessage::SetPlanApprovalRequired { required: true },
        ClientMessage::SetPlanApprovalRequired { required: false },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            12,
            "{msg:?} is understood only from revision 12 on"
        );
        assert_eq!(
            required_protocol_revision(&msg),
            APPROVAL_SETTINGS_MIN_REVISION,
            "{msg:?} must require the approval-settings threshold"
        );
    }
}

// =============================================================================
// list_mcp_registrations / run_mcp_registration always require
// MCP_REGISTRATION_MIN_REVISION, regardless of their fields.
// =============================================================================

#[test]
fn mcp_registration_min_revision_is_thirteen() {
    assert_eq!(MCP_REGISTRATION_MIN_REVISION, 13);
}

#[test]
fn mcp_registration_messages_require_the_mcp_registration_threshold() {
    let messages = [
        ClientMessage::ListMcpRegistrations { request_id: 1 },
        ClientMessage::RunMcpRegistration {
            profile: "p".to_string(),
            action: McpRegistrationAction::Register,
            request_id: 1,
        },
        ClientMessage::RunMcpRegistration {
            profile: "p".to_string(),
            action: McpRegistrationAction::Update,
            request_id: 2,
        },
        ClientMessage::RunMcpRegistration {
            profile: "p".to_string(),
            action: McpRegistrationAction::Remove,
            request_id: 3,
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            13,
            "{msg:?} is understood only from revision 13 on"
        );
        assert_eq!(
            required_protocol_revision(&msg),
            MCP_REGISTRATION_MIN_REVISION,
            "{msg:?} must require the MCP registration threshold"
        );
    }
}

#[test]
fn get_daemon_health_requires_revision_twenty() {
    assert_eq!(
        required_protocol_revision(&ClientMessage::GetDaemonHealth),
        DAEMON_HEALTH_MIN_REVISION
    );
    assert_eq!(DAEMON_HEALTH_MIN_REVISION, 20);
}

#[test]
fn the_view_messages_require_the_frame_threshold_whatever_they_carry() {
    let messages = [
        ClientMessage::AttachView {
            window_id: "w1".to_string(),
            size: None,
        },
        ClientMessage::AttachView {
            window_id: "w1".to_string(),
            size: Some(yagura_protocol::TerminalSize { cols: 80, rows: 24 }),
        },
        ClientMessage::FrameAck { seq: 0 },
        ClientMessage::SetPalette(Palette {
            foreground: [0, 0, 0],
            background: [255, 255, 255],
            cursor: [0, 0, 0],
            ansi: Vec::new(),
        }),
        ClientMessage::SetVisible { visible: true },
        ClientMessage::CopySelection,
        ClientMessage::ClearSelection,
        ClientMessage::ScrollView { lines: 0 },
    ];
    for message in messages {
        assert_eq!(
            required_protocol_revision(&message),
            FRAME_MIN_REVISION,
            "{message:?}"
        );
    }
    assert_eq!(FRAME_MIN_REVISION, 19);
}
