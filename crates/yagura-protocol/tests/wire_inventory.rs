#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The wire ledger: a machine-held inventory of everything `ClientMessage` and
//! `DaemonMessage` can put on the wire, frozen per protocol revision.
//!
//! Four properties are measured, each against the *current* build's revision
//! (`yagura_protocol::PROTOCOL_REVISION`):
//!
//! - **WL-1 (corpus completeness)**: the tag set built from the corpus below
//!   matches the decoder's own tag table exactly, for both directions.
//! - **WL-2 (structure freeze)**: every wire value's structure -- reduced
//!   recursively, object keys sorted, array element structure taken from one
//!   element, scalars reduced to their JSON type name -- matches the frozen
//!   `shapes` section named for the current revision.
//! - **WL-3 (wire enum value-set freeze)**: the full wire-string variant set
//!   of every enum that appears on the wire matches the frozen `enums`
//!   section. Structurally, an enum always reduces to `"string"` in WL-2, so
//!   adding a value moves nothing there; this is why the value set is its own
//!   frozen section.
//! - **WL-4 (revision <-> frozen-file correspondence)**: a frozen file exists
//!   for every `N` in `1..=PROTOCOL_REVISION`, and for no `N` above it.
//!
//! # The corpus is built exclusively through `wire_sample!`
//!
//! Every `Sample` impl for a wire struct, and every `ClientMessage` /
//! `DaemonMessage` variant that enters the corpus, is built by handing
//! `wire_sample!` a type or variant name plus the bare names of its fields --
//! never a value. `Sample`'s blanket impls make `Option<T>::sample()` always
//! return `Some` and `Vec<T>::sample()` always return one element, and
//! coherence forbids a second, competing `impl Sample for Option<...>`. So
//! there is no place in this file to write a field that carries no value:
//! adding a field to a wire type and forgetting to add its name to the
//! corresponding `wire_sample!` call fails this file to compile with a
//! missing-field error, and the fix is always to add the name, never to write
//! `None`.
//!
//! # What this ledger does not watch
//!
//! - Removing a `skip_serializing_if` attribute (WL-2 sees no shape change).
//! - A change in the *meaning* of an existing key's value, the key and its
//!   JSON type staying the same.
//! - Whether an enum carries `#[serde(other)]` at all -- that is left to
//!   the decoder's own tests and to code review.
//! - A hand-written `impl Sample` that bypasses `wire_sample!`: nothing here
//!   stops one from being added next to the macro-built impls. Code review
//!   carries the responsibility for catching it.
//! - A new wire-enum variant that is added to neither the decoder's tag table
//!   (for `ClientMessage` / `DaemonMessage`, a plain `const` slice, not an
//!   exhaustive `match`) nor the corpus: nothing here forces the compiler to
//!   complain, and both sets can stay in agreement while the new variant
//!   reaches no daemon at all. The harm surfaces as a failing feature test,
//!   never here.
//!
//! # Frozen files are never rewritten to match today's measurement
//!
//! A `revision-N.json` file under `tests/wire-shape/` records the wire as it
//! stood when revision `N` was minted. Once a revision has shipped, its
//! frozen file is never edited or regenerated to match whatever the corpus
//! measures today -- doing so would turn WL-2/WL-3 back into a no-op, since
//! the comparison would always be against a moving target.
//! `write_current_revision_frozen_file` below is the only thing in this file
//! that writes a frozen file, and it refuses to overwrite one that already
//! exists.
//!
//! # The tag-table read-out
//!
//! `yagura_protocol::client_message_wire_tags()` and
//! `yagura_protocol::daemon_message_wire_tags()` are the names this file
//! requires the decoder's tag tables to be readable through.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use yagura_protocol::{
    ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalSettingsInfo,
    ApprovalTarget, AssignmentSource, AwaitingWindow, CellAttr, CellColor, CellFlag, CellPosition,
    ChatDeliveryInfo, ChatHalt, ChatMessage, ChatParticipant, ChatRoomInfo, ClientMessage,
    ColorOverride, ControlKey, CursorShape, CursorState, DaemonMessage, DaemonProblem,
    DaemonProblemKind, DeliveryStream, Frame, FrameLine, FrameRun, HandoffDelivery,
    InstructionNotice, InstructionNoticeState, Key, KeyInput, LaunchOptionAlias, LaunchOptionInfo,
    McpRegistrationAction, McpRegistrationCommand, McpRegistrationHookEntry, McpRegistrationRow,
    McpRegistrationState, McpRegistrationStatusInfo, McpRegistrationStep, McpRegistrationTomlSet,
    McpRegistrationTomlValue, ModelChoice, ModelChoiceSource, Modifiers, NamedKey, Palette,
    PaletteSlot, PlanInfo, PlanReview, PlanSummary, PlanTaskInfo, PlanTaskSpec, PlaybookState,
    PointerButton, PointerInput, PointerKind, ProfileCommandDetail, ProfileDetailEntry,
    ProfileGuidance, ProfileInfo, ProfileUnavailableKind, ReceivedReport, ReportOutcome,
    ReportTaskRef, ResolvedAssignment, RestoreCandidate, ReviewMark, RoleEntry, RoomRef,
    TaskAssignment, TaskReport, TerminalSize, ToolAutoApprove, UnknownReason, ViewState,
    WheelInput, WindowInfo, WindowStatus as WireWindowStatus, WithdrawReason,
};

// ---------------------------------------------------------------------
// Sample: construct one value of a type with every field populated.
// ---------------------------------------------------------------------

/// A type that can build one representative value of itself for the corpus.
/// `Option<T>` and `Vec<T>` below are the only impls that touch a container;
/// both always populate it, so a corpus built from `Sample` alone can never
/// contain `None` or an empty array.
trait Sample: Sized {
    fn sample() -> Self;
}

impl Sample for String {
    fn sample() -> Self {
        "sample".to_string()
    }
}

impl Sample for bool {
    fn sample() -> Self {
        true
    }
}

impl Sample for u8 {
    fn sample() -> Self {
        1
    }
}

impl Sample for u16 {
    fn sample() -> Self {
        1
    }
}

impl Sample for u32 {
    fn sample() -> Self {
        1
    }
}

impl Sample for u64 {
    fn sample() -> Self {
        1
    }
}

impl Sample for i32 {
    fn sample() -> Self {
        1
    }
}

impl Sample for i64 {
    fn sample() -> Self {
        1
    }
}

/// A string stands for any JSON value: the shape reduction keeps only a
/// scalar's type name, and a profile value's type is the profile's own.
impl Sample for Value {
    fn sample() -> Self {
        Value::String("sample".to_string())
    }
}

impl<T: Sample> Sample for Option<T> {
    fn sample() -> Self {
        Some(T::sample())
    }
}

impl Sample for [u8; 3] {
    fn sample() -> Self {
        [1, 1, 1]
    }
}

impl<T: Sample + Ord> Sample for BTreeSet<T> {
    fn sample() -> Self {
        BTreeSet::from([T::sample()])
    }
}

impl<T: Sample> Sample for Vec<T> {
    fn sample() -> Self {
        vec![T::sample()]
    }
}

/// Builds a `Sample` impl for a wire struct, or one populated
/// `Enum::Variant { .. }` / `Enum::Variant` value, from field names alone.
///
/// - `wire_sample!(struct Type { field, .. })` emits `impl Sample for Type`.
/// - `wire_sample!(enum Type => Variant)` emits `impl Sample for Type`
///   picking one field-less variant (used for the wire enums, whose JSON
///   shape is the same string regardless of which variant it is; WL-3, not
///   WL-2, is what tells their variants apart).
/// - `wire_sample!(Enum::Variant { field, .. })` / `wire_sample!(Enum::Variant)`
///   builds one corpus value directly, for `ClientMessage` / `DaemonMessage`.
macro_rules! wire_sample {
    (struct $ty:ident { $($field:ident),* $(,)? }) => {
        impl Sample for $ty {
            fn sample() -> Self {
                $ty { $( $field: Sample::sample() ),* }
            }
        }
    };
    (enum $ty:ident => $variant:ident) => {
        impl Sample for $ty {
            fn sample() -> Self {
                $ty::$variant
            }
        }
    };
    ($enum_ty:ident :: $variant:ident { $($field:ident),* $(,)? }) => {
        $enum_ty::$variant { $( $field: Sample::sample() ),* }
    };
    ($enum_ty:ident :: $variant:ident) => {
        $enum_ty::$variant
    };
}

// ---------------------------------------------------------------------
// Sample impls: wire enums (one arbitrary variant each -- WL-3 covers the
// full variant set separately) and wire structs reachable from
// ClientMessage / DaemonMessage.
// ---------------------------------------------------------------------

wire_sample!(enum WireWindowStatus => Running);
wire_sample!(enum UnknownReason => NoDefinition);
wire_sample!(enum ApprovalRequesterKind => External);
wire_sample!(enum WithdrawReason => Resolved);
wire_sample!(enum HandoffDelivery => Taken);
wire_sample!(enum AssignmentSource => User);
wire_sample!(enum ReportOutcome => Succeeded);
wire_sample!(enum InstructionNoticeState => Pending);
wire_sample!(enum PlanReview => Pending);
wire_sample!(enum ReviewMark => New);
wire_sample!(enum PlaybookState => Loaded);
wire_sample!(enum McpRegistrationState => Registered);
wire_sample!(enum McpRegistrationAction => Register);
wire_sample!(enum DaemonProblemKind => WindowsUnavailable);
wire_sample!(enum DeliveryStream => Stdout);
wire_sample!(enum ProfileUnavailableKind => NotFound);
wire_sample!(enum NamedKey => ArrowUp);
wire_sample!(enum PointerKind => Down);
wire_sample!(enum PointerButton => Left);
wire_sample!(enum CellFlag => Bold);
wire_sample!(enum CursorShape => Block);

/// Hand-written for the same reason as `Key`'s: `CellColor` carries data.
impl Sample for CellColor {
    fn sample() -> Self {
        CellColor::Rgb(Sample::sample())
    }
}

/// Hand-written for the same reason: `PaletteSlot` carries data in one variant.
impl Sample for PaletteSlot {
    fn sample() -> Self {
        PaletteSlot::Indexed(Sample::sample())
    }
}

/// Hand-written for the same reason as `ModelChoiceSource`'s: `Key` is a
/// data-carrying enum, which the macro's `enum ... => variant` arm cannot
/// build.
impl Sample for Key {
    fn sample() -> Self {
        Key::Named(NamedKey::sample())
    }
}

/// Hand-written rather than `wire_sample!`'s `enum ... => variant` arm: that
/// arm only builds a field-less variant, and the `Command` form is the one
/// that exercises WL-2's shape reduction over `ModelChoiceSource`'s `argv`
/// field.
impl Sample for ModelChoiceSource {
    fn sample() -> Self {
        ModelChoiceSource::Command {
            argv: Vec::<String>::sample(),
        }
    }
}

wire_sample!(struct WindowInfo {
    window_id,
    name,
    vendor,
    cwd,
    status,
    exit_code,
    layout_json,
    room,
    unknown_reason,
    detection_warning,
    status_rev,
    mcp_connected,
    status_reports_missing,
});
wire_sample!(struct RestoreCandidate {
    position,
    command,
    cwd,
    name,
    vendor,
    detection,
    layout_json,
    profile,
    model,
    effort,
});
wire_sample!(struct ProfileInfo {
    id,
    display_name,
    detection,
    detection_warning,
    cwd,
    command,
    available,
    unavailable_reason,
    model,
    effort,
    enabled,
    unavailable_kind,
    unavailable_guidance,
});
wire_sample!(struct ProfileGuidance { text, command, url });
wire_sample!(struct LaunchOptionInfo { aliases });
wire_sample!(struct LaunchOptionAlias { alias, value });
wire_sample!(struct ModelChoice { id, label });
wire_sample!(struct ApprovalRequest {
    request_id,
    tool,
    requester,
    target,
    text,
    room,
});
wire_sample!(struct RoomRef { room_id, name });
wire_sample!(struct ApprovalRequester { kind, window_id, name });
wire_sample!(struct ApprovalTarget { window_id, name, status });
wire_sample!(struct ChatMessage {
    seq,
    at_ms,
    from_kind,
    from_window_id,
    from_name,
    body,
    mentions,
    broadcast,
    deliveries,
});
wire_sample!(struct ChatDeliveryInfo {
    window_id,
    state,
    unknown_reason,
});
wire_sample!(struct ChatRoomInfo {
    room_id,
    name,
    participant_count,
    needs_user,
    halted,
    last_at_ms,
});
wire_sample!(struct DaemonProblem { kind, detail });
wire_sample!(struct ChatHalt { reason, since_ms });
wire_sample!(struct ChatParticipant { window_id, name });
wire_sample!(struct PlanTaskSpec {
    task_id,
    title,
    role,
    profile,
    model,
    effort,
    depends_on,
    writes,
    instruction,
});
wire_sample!(struct TaskAssignment {
    profile,
    model,
    effort,
});
wire_sample!(struct ResolvedAssignment { source, assignment });
wire_sample!(struct InstructionNotice { state, at });
wire_sample!(struct PlanTaskInfo {
    task_id,
    title,
    role,
    depends_on,
    writes,
    wave,
    parent,
    user,
    resolved,
    window_id,
    report,
    instruction,
    instruction_notice,
    review_mark,
});
wire_sample!(struct ReportTaskRef { plan_id, task_id });
wire_sample!(struct AwaitingWindow {
    window_id,
    name,
    status,
    mcp_connected,
});
wire_sample!(struct ReceivedReport {
    from_window_id,
    from_name,
    outcome,
    summary,
    path,
    task,
    at_ms,
});
wire_sample!(struct TaskReport {
    outcome,
    summary,
    path,
    reported_at,
});
wire_sample!(struct PlanInfo {
    plan_id,
    title,
    cwd,
    owner_window,
    waves,
    tasks,
    roles_warning,
    review,
    version,
});
wire_sample!(struct PlanSummary {
    plan_id,
    title,
    owner_window,
    task_count,
    review,
    unseen_changes,
});
wire_sample!(struct RoleEntry {
    name,
    profile,
    model,
    effort,
});
wire_sample!(struct ToolAutoApprove { tool, auto_approve });
wire_sample!(struct ApprovalSettingsInfo {
    playbook,
    tools,
    plans_require_approval,
});
wire_sample!(struct McpRegistrationCommand { argv, program_at });
wire_sample!(struct McpRegistrationTomlValue { key, value });
wire_sample!(struct ProfileDetailEntry { path, value, shipped, changed });
wire_sample!(struct ProfileCommandDetail { argv, shipped, changed });
wire_sample!(struct McpRegistrationTomlSet {
    file,
    table,
    values,
});
wire_sample!(struct McpRegistrationRow {
    profile,
    display_name,
    group,
    add,
    remove,
    toml_set,
    hook_entries,
});
wire_sample!(struct McpRegistrationHookEntry {
    file,
    name,
    content,
});
wire_sample!(struct McpRegistrationStatusInfo {
    profile,
    state,
    differs_in,
    reason,
    add,
    remove,
});
wire_sample!(struct TerminalSize { cols, rows });
wire_sample!(struct Modifiers { shift, alt, ctrl });
wire_sample!(struct KeyInput { key, mods });
wire_sample!(struct CellPosition { col, row });
wire_sample!(struct PointerInput {
    kind,
    button,
    cell,
    mods,
    clicks,
});
wire_sample!(struct WheelInput { delta, cell, mods });
wire_sample!(struct Palette {
    foreground,
    background,
    cursor,
    ansi,
});
wire_sample!(struct CellAttr { fg, bg, flags });
wire_sample!(struct FrameRun { attr, cells, wide });
wire_sample!(struct FrameLine { row, wrapped, runs });
wire_sample!(struct CursorState {
    row,
    col,
    visible,
    shape,
});
wire_sample!(struct ViewState {
    display_offset,
    history_rows,
    mouse_reporting,
    alt_screen,
});
wire_sample!(struct ColorOverride { slot, rgb });
wire_sample!(struct Frame {
    seq,
    cols,
    rows,
    reset,
    last,
    attrs,
    lines,
    cursor,
    view,
    overrides,
});
wire_sample!(struct McpRegistrationStep {
    argv,
    program_at,
    exit_code,
    output,
});

// ---------------------------------------------------------------------
// The corpus: one value per ClientMessage / DaemonMessage variant.
// ---------------------------------------------------------------------

/// One `ClientMessage` per variant the decoder accepts (WL-1 measures this
/// against `client_message_wire_tags()`).
fn client_message_corpus() -> Vec<ClientMessage> {
    vec![
        wire_sample!(ClientMessage::AttachView { window_id, size }),
        wire_sample!(ClientMessage::FrameAck { seq }),
        ClientMessage::SetPalette(Palette::sample()),
        wire_sample!(ClientMessage::SetVisible { visible }),
        wire_sample!(ClientMessage::CopySelection),
        wire_sample!(ClientMessage::ClearSelection),
        wire_sample!(ClientMessage::ScrollView { lines }),
        ClientMessage::Key(KeyInput::sample()),
        wire_sample!(ClientMessage::Text { text }),
        wire_sample!(ClientMessage::Paste { text }),
        ClientMessage::Pointer(PointerInput::sample()),
        ClientMessage::Wheel(WheelInput::sample()),
        wire_sample!(ClientMessage::Focus { focused }),
        wire_sample!(ClientMessage::Resize { cols, rows }),
        wire_sample!(ClientMessage::Ping),
        wire_sample!(ClientMessage::Shutdown),
        wire_sample!(ClientMessage::StatusEvent {
            source,
            status,
            window_id,
            token,
        }),
        wire_sample!(ClientMessage::OpenWindow {
            command,
            cwd,
            name,
            vendor,
            detection,
        }),
        wire_sample!(ClientMessage::ListWindows),
        wire_sample!(ClientMessage::GetWindowStatus { window_id }),
        wire_sample!(ClientMessage::SendText { window_id, text }),
        ClientMessage::SendKeys {
            window_id: "sample".to_string(),
            keys: vec![ControlKey::Escape, ControlKey::Enter],
        },
        wire_sample!(ClientMessage::InterruptWindow { window_id }),
        wire_sample!(ClientMessage::CloseWindow { window_id, force }),
        wire_sample!(ClientMessage::WaitForStatus {
            window_id,
            target_status,
            timeout_sec,
        }),
        wire_sample!(ClientMessage::RenameWindow { window_id, name }),
        wire_sample!(ClientMessage::ReadOutput { window_id, lines }),
        wire_sample!(ClientMessage::ListDetections),
        wire_sample!(ClientMessage::ListRestoreCandidates),
        wire_sample!(ClientMessage::UpdateWindowLayout {
            window_id,
            layout_json,
        }),
        wire_sample!(ClientMessage::ListProfiles),
        wire_sample!(ClientMessage::OpenWindowFromProfile {
            profile,
            cwd,
            name,
            model,
            effort,
        }),
        wire_sample!(ClientMessage::Identify { window_id, token }),
        wire_sample!(ClientMessage::RegisterApprover),
        wire_sample!(ClientMessage::SendHandoff {
            window_id,
            profile,
            cwd,
            name,
            model,
            effort,
            reference,
            note,
        }),
        wire_sample!(ClientMessage::ApprovalDecision {
            request_id,
            approved,
        }),
        wire_sample!(ClientMessage::SendChat { room_id, body }),
        wire_sample!(ClientMessage::BroadcastChat { room_id, body }),
        wire_sample!(ClientMessage::ReadChat {
            room_id,
            after_seq,
            limit,
        }),
        wire_sample!(ClientMessage::ListChatRooms),
        wire_sample!(ClientMessage::GetProtocolRevision),
        wire_sample!(ClientMessage::GetDaemonHealth),
        wire_sample!(ClientMessage::DeliverContext { window_id, token }),
        wire_sample!(ClientMessage::DeliverTake { window_id, token }),
        wire_sample!(ClientMessage::DeliverWait { window_id, token }),
        wire_sample!(ClientMessage::ReportSession {
            window_id,
            token,
            session,
        }),
        wire_sample!(ClientMessage::CreatePlan { title, tasks }),
        wire_sample!(ClientMessage::UpdatePlan {
            plan_id,
            title,
            tasks,
        }),
        wire_sample!(ClientMessage::GetPlan { plan_id }),
        wire_sample!(ClientMessage::ListPlans),
        wire_sample!(ClientMessage::DeletePlan { plan_id }),
        wire_sample!(ClientMessage::ListRoles),
        wire_sample!(ClientMessage::SaveRoles {
            roles,
            default_role,
        }),
        wire_sample!(ClientMessage::SetTaskAssignment {
            plan_id,
            task_id,
            assignment,
        }),
        wire_sample!(ClientMessage::ClearTaskAssignment { plan_id, task_id }),
        wire_sample!(ClientMessage::SetPlanOwner { plan_id, window_id }),
        wire_sample!(ClientMessage::OpenTaskWindow {
            plan_id,
            task_id,
            name,
        }),
        wire_sample!(ClientMessage::ApprovePlan { plan_id, version }),
        wire_sample!(ClientMessage::MarkPlanSeen { plan_id, version }),
        wire_sample!(ClientMessage::SendReport {
            outcome,
            summary,
            path,
        }),
        wire_sample!(ClientMessage::WaitForReport { timeout_sec }),
        wire_sample!(ClientMessage::ListModelChoices {
            profile,
            request_id,
        }),
        wire_sample!(ClientMessage::SaveModelAliases { profile, aliases }),
        wire_sample!(ClientMessage::SetProfileEnabled { profile, enabled }),
        wire_sample!(ClientMessage::GetProfileDetail { profile }),
        wire_sample!(ClientMessage::SetProfileCommand { profile, argv }),
        wire_sample!(ClientMessage::GetApprovalSettings {}),
        wire_sample!(ClientMessage::SetAutoApprove { tool, auto_approve }),
        wire_sample!(ClientMessage::SetPlanApprovalRequired { required }),
        wire_sample!(ClientMessage::ListMcpRegistrations { request_id }),
        wire_sample!(ClientMessage::RunMcpRegistration {
            profile,
            action,
            request_id,
        }),
    ]
}

/// One `DaemonMessage` per variant the decoder accepts (WL-1 measures this
/// against `daemon_message_wire_tags()`).
fn daemon_message_corpus() -> Vec<DaemonMessage> {
    vec![
        wire_sample!(DaemonMessage::Attached {
            child_exited,
            exit_code,
        }),
        DaemonMessage::Frame(Frame::sample()),
        wire_sample!(DaemonMessage::SelectionText { text }),
        wire_sample!(DaemonMessage::Exit { exit_code }),
        wire_sample!(DaemonMessage::Pong),
        wire_sample!(DaemonMessage::Error {
            code,
            message,
            rejected_argument,
        }),
        wire_sample!(DaemonMessage::StatusChanged {
            window_id,
            status,
            unknown_reason,
            status_rev,
        }),
        wire_sample!(DaemonMessage::WindowOpened {
            window_id,
            detection_warning,
        }),
        wire_sample!(DaemonMessage::WindowList { windows }),
        wire_sample!(DaemonMessage::WindowStatus {
            window_id,
            status,
            since_ms,
            last_output_summary,
            exit_code,
            unknown_reason,
            detection_warning,
            mcp_connected,
        }),
        wire_sample!(DaemonMessage::TextSent { window_id }),
        wire_sample!(DaemonMessage::KeysSent { window_id }),
        wire_sample!(DaemonMessage::WindowInterrupted { window_id }),
        wire_sample!(DaemonMessage::WindowClosed { window_id }),
        wire_sample!(DaemonMessage::WaitResult {
            window_id,
            outcome,
            status,
        }),
        wire_sample!(DaemonMessage::WindowRenamed { window_id, name }),
        wire_sample!(DaemonMessage::WindowOutput {
            window_id,
            text,
            truncated,
        }),
        wire_sample!(DaemonMessage::DetectionList {
            names,
            bundled_names,
            directory_names,
            warning,
        }),
        wire_sample!(DaemonMessage::RestoreCandidateList {
            windows,
            taken_at_epoch_s,
        }),
        wire_sample!(DaemonMessage::WindowLayoutUpdated { window_id }),
        wire_sample!(DaemonMessage::ProfileList { profiles, warnings }),
        wire_sample!(DaemonMessage::Identified { window_id }),
        wire_sample!(DaemonMessage::ApproverRegistered { pending }),
        wire_sample!(DaemonMessage::ApprovalRequested { request }),
        wire_sample!(DaemonMessage::ApprovalWithdrawn { request_id, reason }),
        wire_sample!(DaemonMessage::ApprovalResolved { request_id }),
        wire_sample!(DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
        }),
        wire_sample!(DaemonMessage::ChatPosted {
            room_id,
            seq,
            targets,
            user,
            unresolved,
            halted,
        }),
        wire_sample!(DaemonMessage::ChatMessages {
            room_id,
            messages,
            next_after_seq,
            halted,
            needs_user,
            roster,
        }),
        wire_sample!(DaemonMessage::ChatChanged {
            room_id,
            seq,
            needs_user,
        }),
        wire_sample!(DaemonMessage::ChatParticipantsChanged),
        wire_sample!(DaemonMessage::ChatRooms { rooms }),
        wire_sample!(DaemonMessage::ProtocolRevision { revision }),
        wire_sample!(DaemonMessage::DaemonHealth { problems }),
        wire_sample!(DaemonMessage::Delivery {
            output,
            stream,
            exit_code,
        }),
        wire_sample!(DaemonMessage::SessionRecorded { held }),
        wire_sample!(DaemonMessage::PlanDetail { plan }),
        wire_sample!(DaemonMessage::PlanList { plans }),
        wire_sample!(DaemonMessage::PlanDeleted { plan_id }),
        wire_sample!(DaemonMessage::RoleList {
            roles,
            default_role,
        }),
        wire_sample!(DaemonMessage::TaskWindowOpened {
            plan_id,
            task_id,
            window_id,
            assignment,
        }),
        wire_sample!(DaemonMessage::PlanChanged { plan_id }),
        wire_sample!(DaemonMessage::ReportSent { delivered_to, task }),
        wire_sample!(DaemonMessage::Reports { reports, awaiting }),
        wire_sample!(DaemonMessage::ModelChoicesRequested {
            request_id,
            profile,
        }),
        wire_sample!(DaemonMessage::ModelChoices {
            request_id,
            profile,
            source,
            choices,
            error,
        }),
        wire_sample!(DaemonMessage::ModelAliasesSaved { profile, aliases }),
        wire_sample!(DaemonMessage::ProfileEnabledSet { profile, enabled }),
        wire_sample!(DaemonMessage::ProfileDetail {
            profile,
            shipped_base,
            command,
            entries
        }),
        wire_sample!(DaemonMessage::ProfileCommandSet { profile, command }),
        wire_sample!(DaemonMessage::ApprovalSettings { settings }),
        wire_sample!(DaemonMessage::AutoApproveSet { settings }),
        wire_sample!(DaemonMessage::PlanApprovalRequiredSet { settings }),
        wire_sample!(DaemonMessage::McpRegistrationsListed {
            request_id,
            bridge,
            rows,
        }),
        wire_sample!(DaemonMessage::McpRegistrationStatus { request_id, status }),
        wire_sample!(DaemonMessage::McpRegistrationRequested {
            request_id,
            profile,
            action,
        }),
        wire_sample!(DaemonMessage::McpRegistrationResult {
            request_id,
            profile,
            action,
            steps,
            error,
            status,
        }),
    ]
}

// ---------------------------------------------------------------------
// Wire enums: the full wire-string variant set of each (WL-3). A plain
// `match` with no wildcard arm is the enforcement point: a
// variant added to the type without a matching arm here fails this file to
// compile. Each arm's string is checked against what serde writes, so the
// frozen set is the wire's, not this table's.
// ---------------------------------------------------------------------

/// The string serde writes for one wire enum value: the bare string of a
/// unit variant, the `kind` tag of an internally tagged one, or the single
/// key of an externally tagged one. Panics unless it equals `hand`.
fn serde_spelling<T: Serialize>(value: &T, hand: &str) -> String {
    let encoded = serde_json::to_value(value).expect("a wire enum must serialize");
    let spelled = match &encoded {
        Value::String(name) => name.clone(),
        Value::Object(map) => match map.get("kind").and_then(Value::as_str) {
            Some(kind) => kind.to_string(),
            None => {
                assert_eq!(
                    map.len(),
                    1,
                    "an externally tagged variant has one key: {encoded}"
                );
                map.keys().next().cloned().unwrap_or_default()
            }
        },
        other => panic!("a wire enum must serialize as a string or an object, got {other}"),
    };
    assert_eq!(
        spelled, hand,
        "the variant table must spell each variant exactly as serde writes it"
    );
    spelled
}

fn window_status_wire_variants() -> BTreeSet<String> {
    [
        WireWindowStatus::Running,
        WireWindowStatus::WaitingInput,
        WireWindowStatus::WaitingApproval,
        WireWindowStatus::Interrupted,
        WireWindowStatus::Idle,
        WireWindowStatus::Exited,
        WireWindowStatus::Unknown,
    ]
    .into_iter()
    .map(|status| {
        let hand = match &status {
            WireWindowStatus::Running => "running",
            WireWindowStatus::WaitingInput => "waiting_input",
            WireWindowStatus::WaitingApproval => "waiting_approval",
            WireWindowStatus::Interrupted => "interrupted",
            WireWindowStatus::Idle => "idle",
            WireWindowStatus::Exited => "exited",
            WireWindowStatus::Unknown => "unknown",
        };
        serde_spelling(&status, hand)
    })
    .collect()
}

fn unknown_reason_wire_variants() -> BTreeSet<String> {
    [
        UnknownReason::NoDefinition,
        UnknownReason::NoRuleMatched,
        UnknownReason::ScreenLost,
        UnknownReason::DetectorStopped,
        UnknownReason::NotYetJudged,
        UnknownReason::DeclaredUnknown,
    ]
    .into_iter()
    .map(|reason| {
        let hand = match &reason {
            UnknownReason::NoDefinition => "no_definition",
            UnknownReason::NoRuleMatched => "no_rule_matched",
            UnknownReason::ScreenLost => "screen_lost",
            UnknownReason::DetectorStopped => "detector_stopped",
            UnknownReason::NotYetJudged => "not_yet_judged",
            UnknownReason::DeclaredUnknown => "declared_unknown",
        };
        serde_spelling(&reason, hand)
    })
    .collect()
}

fn approval_requester_kind_wire_variants() -> BTreeSet<String> {
    [
        ApprovalRequesterKind::External,
        ApprovalRequesterKind::Window,
        ApprovalRequesterKind::Unknown,
    ]
    .into_iter()
    .map(|kind| {
        let hand = match &kind {
            ApprovalRequesterKind::External => "external",
            ApprovalRequesterKind::Window => "window",
            ApprovalRequesterKind::Unknown => "unknown",
        };
        serde_spelling(&kind, hand)
    })
    .collect()
}

fn withdraw_reason_wire_variants() -> BTreeSet<String> {
    [
        WithdrawReason::Resolved,
        WithdrawReason::Timeout,
        WithdrawReason::RequesterDisconnected,
        WithdrawReason::Unknown,
    ]
    .into_iter()
    .map(|reason| {
        let hand = match &reason {
            WithdrawReason::Resolved => "resolved",
            WithdrawReason::Timeout => "timeout",
            WithdrawReason::RequesterDisconnected => "requester_disconnected",
            WithdrawReason::Unknown => "unknown",
        };
        serde_spelling(&reason, hand)
    })
    .collect()
}

fn handoff_delivery_wire_variants() -> BTreeSet<String> {
    [
        HandoffDelivery::Taken,
        HandoffDelivery::UndeliveredTimeout,
        HandoffDelivery::UndeliveredNoDelivery,
    ]
    .into_iter()
    .map(|delivery| {
        let hand = match &delivery {
            HandoffDelivery::Taken => "taken",
            HandoffDelivery::UndeliveredTimeout => "undelivered_timeout",
            HandoffDelivery::UndeliveredNoDelivery => "undelivered_no_delivery",
        };
        serde_spelling(&delivery, hand)
    })
    .collect()
}

fn assignment_source_wire_variants() -> BTreeSet<String> {
    [
        AssignmentSource::User,
        AssignmentSource::Parent,
        AssignmentSource::Role,
        AssignmentSource::Default,
        AssignmentSource::Unassigned,
    ]
    .into_iter()
    .map(|source| {
        let hand = match &source {
            AssignmentSource::User => "user",
            AssignmentSource::Parent => "parent",
            AssignmentSource::Role => "role",
            AssignmentSource::Default => "default",
            AssignmentSource::Unassigned => "unassigned",
        };
        serde_spelling(&source, hand)
    })
    .collect()
}

fn plan_review_wire_variants() -> BTreeSet<String> {
    [PlanReview::Pending, PlanReview::Approved]
        .into_iter()
        .map(|review| {
            let hand = match &review {
                PlanReview::Pending => "pending",
                PlanReview::Approved => "approved",
            };
            serde_spelling(&review, hand)
        })
        .collect()
}

fn review_mark_wire_variants() -> BTreeSet<String> {
    [ReviewMark::New, ReviewMark::Changed]
        .into_iter()
        .map(|mark| {
            let hand = match &mark {
                ReviewMark::New => "new",
                ReviewMark::Changed => "changed",
            };
            serde_spelling(&mark, hand)
        })
        .collect()
}

fn report_outcome_wire_variants() -> BTreeSet<String> {
    [ReportOutcome::Succeeded, ReportOutcome::Failed]
        .into_iter()
        .map(|outcome| {
            let hand = match &outcome {
                ReportOutcome::Succeeded => "succeeded",
                ReportOutcome::Failed => "failed",
            };
            serde_spelling(&outcome, hand)
        })
        .collect()
}

fn instruction_notice_state_wire_variants() -> BTreeSet<String> {
    [
        InstructionNoticeState::Pending,
        InstructionNoticeState::Delivered,
        InstructionNoticeState::WindowClosed,
    ]
    .into_iter()
    .map(|state| {
        let hand = match &state {
            InstructionNoticeState::Pending => "pending",
            InstructionNoticeState::Delivered => "delivered",
            InstructionNoticeState::WindowClosed => "window_closed",
        };
        serde_spelling(&state, hand)
    })
    .collect()
}

fn playbook_state_wire_variants() -> BTreeSet<String> {
    [
        PlaybookState::Loaded,
        PlaybookState::NotConfigured,
        PlaybookState::Failed,
        PlaybookState::Unknown,
    ]
    .into_iter()
    .map(|state| {
        let hand = match &state {
            PlaybookState::Loaded => "loaded",
            PlaybookState::NotConfigured => "not_configured",
            PlaybookState::Failed => "failed",
            PlaybookState::Unknown => "unknown",
        };
        serde_spelling(&state, hand)
    })
    .collect()
}

fn profile_unavailable_kind_wire_variants() -> BTreeSet<String> {
    [
        ProfileUnavailableKind::NotFound,
        ProfileUnavailableKind::Metacharacter,
        ProfileUnavailableKind::NoCommand,
        ProfileUnavailableKind::NoDefaultShell,
        ProfileUnavailableKind::NeedsNativeProgram,
        ProfileUnavailableKind::Unknown,
    ]
    .into_iter()
    .map(|kind| {
        let hand = match &kind {
            ProfileUnavailableKind::NotFound => "not_found",
            ProfileUnavailableKind::Metacharacter => "metacharacter",
            ProfileUnavailableKind::NoCommand => "no_command",
            ProfileUnavailableKind::NoDefaultShell => "no_default_shell",
            ProfileUnavailableKind::NeedsNativeProgram => "needs_native_program",
            ProfileUnavailableKind::Unknown => "unknown",
        };
        serde_spelling(&kind, hand)
    })
    .collect()
}

fn mcp_registration_state_wire_variants() -> BTreeSet<String> {
    [
        McpRegistrationState::Registered,
        McpRegistrationState::Differs,
        McpRegistrationState::NotRegistered,
        McpRegistrationState::NotInstalled,
        McpRegistrationState::Unknown,
    ]
    .into_iter()
    .map(|state| {
        let hand = match &state {
            McpRegistrationState::Registered => "registered",
            McpRegistrationState::Differs => "differs",
            McpRegistrationState::NotRegistered => "not_registered",
            McpRegistrationState::NotInstalled => "not_installed",
            McpRegistrationState::Unknown => "unknown",
        };
        serde_spelling(&state, hand)
    })
    .collect()
}

fn mcp_registration_action_wire_variants() -> BTreeSet<String> {
    [
        McpRegistrationAction::Register,
        McpRegistrationAction::Update,
        McpRegistrationAction::Remove,
    ]
    .into_iter()
    .map(|action| {
        let hand = match &action {
            McpRegistrationAction::Register => "register",
            McpRegistrationAction::Update => "update",
            McpRegistrationAction::Remove => "remove",
        };
        serde_spelling(&action, hand)
    })
    .collect()
}

fn named_key_wire_variants() -> BTreeSet<String> {
    [
        NamedKey::Enter,
        NamedKey::Tab,
        NamedKey::Backspace,
        NamedKey::Escape,
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::Home,
        NamedKey::End,
        NamedKey::PageUp,
        NamedKey::PageDown,
        NamedKey::Insert,
        NamedKey::Delete,
        NamedKey::F1,
        NamedKey::F2,
        NamedKey::F3,
        NamedKey::F4,
        NamedKey::F5,
        NamedKey::F6,
        NamedKey::F7,
        NamedKey::F8,
        NamedKey::F9,
        NamedKey::F10,
        NamedKey::F11,
        NamedKey::F12,
    ]
    .into_iter()
    .map(|key| {
        let hand = match &key {
            NamedKey::Enter => "enter",
            NamedKey::Tab => "tab",
            NamedKey::Backspace => "backspace",
            NamedKey::Escape => "escape",
            NamedKey::ArrowUp => "arrow_up",
            NamedKey::ArrowDown => "arrow_down",
            NamedKey::ArrowLeft => "arrow_left",
            NamedKey::ArrowRight => "arrow_right",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "page_up",
            NamedKey::PageDown => "page_down",
            NamedKey::Insert => "insert",
            NamedKey::Delete => "delete",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
        };
        serde_spelling(&key, hand)
    })
    .collect()
}

/// `Key`'s variant set, keyed by its external tag.
fn key_wire_variants() -> BTreeSet<String> {
    [Key::Named(NamedKey::Enter), Key::Char('a')]
        .into_iter()
        .map(|key| {
            let hand = match &key {
                Key::Named(_) => "named",
                Key::Char(_) => "char",
            };
            serde_spelling(&key, hand)
        })
        .collect()
}

fn pointer_kind_wire_variants() -> BTreeSet<String> {
    [PointerKind::Down, PointerKind::Up, PointerKind::Move]
        .into_iter()
        .map(|kind| {
            let hand = match &kind {
                PointerKind::Down => "down",
                PointerKind::Up => "up",
                PointerKind::Move => "move",
            };
            serde_spelling(&kind, hand)
        })
        .collect()
}

fn pointer_button_wire_variants() -> BTreeSet<String> {
    [
        PointerButton::Left,
        PointerButton::Middle,
        PointerButton::Right,
        PointerButton::None,
    ]
    .into_iter()
    .map(|button| {
        let hand = match &button {
            PointerButton::Left => "left",
            PointerButton::Middle => "middle",
            PointerButton::Right => "right",
            PointerButton::None => "none",
        };
        serde_spelling(&button, hand)
    })
    .collect()
}

fn cell_flag_wire_variants() -> BTreeSet<String> {
    [
        CellFlag::Bold,
        CellFlag::Dim,
        CellFlag::Italic,
        CellFlag::Underline,
        CellFlag::Strikeout,
        CellFlag::Inverse,
        CellFlag::Hidden,
        CellFlag::Selected,
    ]
    .into_iter()
    .map(|flag| {
        let hand = match &flag {
            CellFlag::Bold => "bold",
            CellFlag::Dim => "dim",
            CellFlag::Italic => "italic",
            CellFlag::Underline => "underline",
            CellFlag::Strikeout => "strikeout",
            CellFlag::Inverse => "inverse",
            CellFlag::Hidden => "hidden",
            CellFlag::Selected => "selected",
        };
        serde_spelling(&flag, hand)
    })
    .collect()
}

fn cursor_shape_wire_variants() -> BTreeSet<String> {
    [
        CursorShape::Block,
        CursorShape::Underline,
        CursorShape::Bar,
        CursorShape::HollowBlock,
    ]
    .into_iter()
    .map(|shape| {
        let hand = match &shape {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Bar => "bar",
            CursorShape::HollowBlock => "hollow_block",
        };
        serde_spelling(&shape, hand)
    })
    .collect()
}

/// `CellColor`'s variant set, keyed by its external tag.
fn cell_color_wire_variants() -> BTreeSet<String> {
    [
        CellColor::Default,
        CellColor::Indexed(0),
        CellColor::Rgb([0, 0, 0]),
    ]
    .into_iter()
    .map(|color| {
        let hand = match &color {
            CellColor::Default => "default",
            CellColor::Indexed(_) => "indexed",
            CellColor::Rgb(_) => "rgb",
        };
        serde_spelling(&color, hand)
    })
    .collect()
}

/// `PaletteSlot`'s variant set, keyed by its external tag.
fn palette_slot_wire_variants() -> BTreeSet<String> {
    [
        PaletteSlot::Foreground,
        PaletteSlot::Background,
        PaletteSlot::Cursor,
        PaletteSlot::Indexed(0),
    ]
    .into_iter()
    .map(|slot| {
        let hand = match &slot {
            PaletteSlot::Foreground => "foreground",
            PaletteSlot::Background => "background",
            PaletteSlot::Cursor => "cursor",
            PaletteSlot::Indexed(_) => "indexed",
        };
        serde_spelling(&slot, hand)
    })
    .collect()
}

/// `ModelChoiceSource`'s wire-string variant set, keyed by its `"kind"` tag
/// rather than the bare wire string `WindowStatus` and its siblings use: a
/// plain `match` with no wildcard arm is what forces this function to be
/// revisited when a variant is added to the type.
fn model_choice_source_wire_variants() -> BTreeSet<String> {
    [
        ModelChoiceSource::Values,
        ModelChoiceSource::Command { argv: vec![] },
    ]
    .into_iter()
    .map(|source| {
        let hand = match &source {
            ModelChoiceSource::Values => "values",
            ModelChoiceSource::Command { .. } => "command",
        };
        serde_spelling(&source, hand)
    })
    .collect()
}

/// Every wire enum's full wire-string variant set, keyed by the enum's own
/// name.
fn daemon_problem_kind_wire_variants() -> BTreeSet<String> {
    [DaemonProblemKind::WindowsUnavailable]
        .into_iter()
        .map(|kind| {
            let hand = match &kind {
                DaemonProblemKind::WindowsUnavailable => "windows_unavailable",
            };
            serde_spelling(&kind, hand)
        })
        .collect()
}

fn delivery_stream_wire_variants() -> BTreeSet<String> {
    [DeliveryStream::Stdout, DeliveryStream::Stderr]
        .into_iter()
        .map(|stream| {
            let hand = match &stream {
                DeliveryStream::Stdout => "stdout",
                DeliveryStream::Stderr => "stderr",
            };
            serde_spelling(&stream, hand)
        })
        .collect()
}

fn current_enum_wire_variants() -> BTreeMap<String, BTreeSet<String>> {
    BTreeMap::from([
        ("WindowStatus".to_string(), window_status_wire_variants()),
        ("UnknownReason".to_string(), unknown_reason_wire_variants()),
        (
            "ApprovalRequesterKind".to_string(),
            approval_requester_kind_wire_variants(),
        ),
        (
            "WithdrawReason".to_string(),
            withdraw_reason_wire_variants(),
        ),
        (
            "HandoffDelivery".to_string(),
            handoff_delivery_wire_variants(),
        ),
        (
            "AssignmentSource".to_string(),
            assignment_source_wire_variants(),
        ),
        ("ReportOutcome".to_string(), report_outcome_wire_variants()),
        (
            "InstructionNoticeState".to_string(),
            instruction_notice_state_wire_variants(),
        ),
        ("PlanReview".to_string(), plan_review_wire_variants()),
        ("ReviewMark".to_string(), review_mark_wire_variants()),
        (
            "ModelChoiceSource".to_string(),
            model_choice_source_wire_variants(),
        ),
        ("PlaybookState".to_string(), playbook_state_wire_variants()),
        (
            "McpRegistrationState".to_string(),
            mcp_registration_state_wire_variants(),
        ),
        (
            "McpRegistrationAction".to_string(),
            mcp_registration_action_wire_variants(),
        ),
        (
            "DaemonProblemKind".to_string(),
            daemon_problem_kind_wire_variants(),
        ),
        (
            "DeliveryStream".to_string(),
            delivery_stream_wire_variants(),
        ),
        (
            "ProfileUnavailableKind".to_string(),
            profile_unavailable_kind_wire_variants(),
        ),
        ("NamedKey".to_string(), named_key_wire_variants()),
        ("Key".to_string(), key_wire_variants()),
        ("PointerKind".to_string(), pointer_kind_wire_variants()),
        ("PointerButton".to_string(), pointer_button_wire_variants()),
        ("CellFlag".to_string(), cell_flag_wire_variants()),
        ("CellColor".to_string(), cell_color_wire_variants()),
        ("CursorShape".to_string(), cursor_shape_wire_variants()),
        ("PaletteSlot".to_string(), palette_slot_wire_variants()),
    ])
}

// ---------------------------------------------------------------------
// Shape reduction (WL-2): object keys sorted, array element structure taken
// from one element, scalars reduced to their JSON type name. The corpus
// never contains an empty array or a null (every `Option` is `Some`), so
// those two cases are not expected on real corpus output.
// ---------------------------------------------------------------------

fn reduce_shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(key, val)| (key.clone(), reduce_shape(val)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => {
            let first = items
                .first()
                .expect("a wire array built by Sample must never be empty");
            Value::Array(vec![reduce_shape(first)])
        }
        Value::String(_) => Value::String("string".to_string()),
        Value::Number(_) => Value::String("number".to_string()),
        Value::Bool(_) => Value::String("boolean".to_string()),
        Value::Null => Value::String("null".to_string()),
    }
}

/// The wire tag (the `"type"` value) of one internally-tagged message.
fn wire_tag<T: Serialize>(value: &T) -> String {
    let encoded = serde_json::to_value(value).expect("a wire message must serialize to JSON");
    encoded
        .get("type")
        .and_then(Value::as_str)
        .expect("an internally-tagged wire message must carry a \"type\" field")
        .to_string()
}

/// Tag -> reduced structure, for one direction's corpus.
fn shapes_for<T: Serialize>(corpus: &[T]) -> BTreeMap<String, Value> {
    corpus
        .iter()
        .map(|item| {
            let encoded = serde_json::to_value(item).expect("a wire message must serialize");
            (wire_tag(item), reduce_shape(&encoded))
        })
        .collect()
}

// ---------------------------------------------------------------------
// The ledger: what a frozen file under tests/wire-shape/ holds.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WireShapes {
    client: BTreeMap<String, Value>,
    daemon: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WireLedger {
    revision: u32,
    shapes: WireShapes,
    enums: BTreeMap<String, BTreeSet<String>>,
}

fn current_ledger() -> WireLedger {
    WireLedger {
        revision: yagura_protocol::PROTOCOL_REVISION,
        shapes: WireShapes {
            client: shapes_for(&client_message_corpus()),
            daemon: shapes_for(&daemon_message_corpus()),
        },
        enums: current_enum_wire_variants(),
    }
}

fn wire_shape_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wire-shape")
}

fn frozen_file_path(revision: u32) -> PathBuf {
    wire_shape_dir().join(format!("revision-{revision}.json"))
}

fn load_frozen_ledger(revision: u32) -> WireLedger {
    let path = frozen_file_path(revision);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("frozen file {path:?} must be readable: {err}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("frozen file {path:?} must parse as a wire ledger: {err}"))
}

// ---------------------------------------------------------------------
// WL-1: corpus completeness.
// ---------------------------------------------------------------------

#[test]
fn corpus_tag_set_matches_decoder_tag_table_for_client_messages() {
    let corpus_tags: BTreeSet<String> = client_message_corpus().iter().map(wire_tag).collect();
    let decoder_tags: BTreeSet<String> = yagura_protocol::client_message_wire_tags()
        .iter()
        .map(|tag| (*tag).to_string())
        .collect();
    assert_eq!(
        corpus_tags, decoder_tags,
        "the ClientMessage corpus must build exactly one value per tag the decoder accepts"
    );
}

#[test]
fn corpus_tag_set_matches_decoder_tag_table_for_daemon_messages() {
    let corpus_tags: BTreeSet<String> = daemon_message_corpus().iter().map(wire_tag).collect();
    let decoder_tags: BTreeSet<String> = yagura_protocol::daemon_message_wire_tags()
        .iter()
        .map(|tag| (*tag).to_string())
        .collect();
    assert_eq!(
        corpus_tags, decoder_tags,
        "the DaemonMessage corpus must build exactly one value per tag the decoder accepts"
    );
}

// ---------------------------------------------------------------------
// WL-2: structure freeze, against the current revision's frozen file.
// ---------------------------------------------------------------------

#[test]
fn current_revision_shapes_match_the_frozen_file() {
    let revision = yagura_protocol::PROTOCOL_REVISION;
    let current = current_ledger().shapes;
    let frozen = load_frozen_ledger(revision).shapes;

    for (tag, shape) in &current.client {
        assert_eq!(
            frozen.client.get(tag),
            Some(shape),
            "client message \"{tag}\"'s structure no longer matches revision {revision}'s frozen \
             shape; bump PROTOCOL_REVISION and mint a new frozen file for an intentional wire change"
        );
    }
    for tag in frozen.client.keys() {
        assert!(
            current.client.contains_key(tag),
            "revision {revision}'s frozen file names a client message \"{tag}\" the current \
             corpus no longer builds"
        );
    }
    for (tag, shape) in &current.daemon {
        assert_eq!(
            frozen.daemon.get(tag),
            Some(shape),
            "daemon message \"{tag}\"'s structure no longer matches revision {revision}'s frozen \
             shape; bump PROTOCOL_REVISION and mint a new frozen file for an intentional wire change"
        );
    }
    for tag in frozen.daemon.keys() {
        assert!(
            current.daemon.contains_key(tag),
            "revision {revision}'s frozen file names a daemon message \"{tag}\" the current \
             corpus no longer builds"
        );
    }
}

// ---------------------------------------------------------------------
// WL-3: wire enum value-set freeze, against the current revision's frozen
// file.
// ---------------------------------------------------------------------

#[test]
fn current_revision_enum_wire_variants_match_the_frozen_file() {
    let revision = yagura_protocol::PROTOCOL_REVISION;
    let current = current_enum_wire_variants();
    let frozen = load_frozen_ledger(revision).enums;

    for (name, variants) in &current {
        assert_eq!(
            frozen.get(name),
            Some(variants),
            "enum \"{name}\"'s wire-string variant set no longer matches revision {revision}'s \
             frozen set; bump PROTOCOL_REVISION and mint a new frozen file for an intentional \
             wire enum change"
        );
    }
    for name in frozen.keys() {
        assert!(
            current.contains_key(name),
            "revision {revision}'s frozen file names an enum \"{name}\" this file no longer tracks"
        );
    }
}

// ---------------------------------------------------------------------
// WL-4: revision <-> frozen-file correspondence.
// ---------------------------------------------------------------------

#[test]
fn frozen_files_exist_for_exactly_one_through_the_current_revision() {
    let dir = wire_shape_dir();
    let mut found: Vec<u32> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("{dir:?} must be readable: {err}"))
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("revision-"))
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|digits| digits.parse::<u32>().ok())
        })
        .collect();
    found.sort_unstable();

    let expected: Vec<u32> = (1..=yagura_protocol::PROTOCOL_REVISION).collect();
    assert_eq!(
        found,
        expected,
        "tests/wire-shape/ must hold exactly revision-1.json..=revision-{}.json, no gaps and \
         nothing above",
        yagura_protocol::PROTOCOL_REVISION
    );
}

// ---------------------------------------------------------------------
// The only thing in this file allowed to write a frozen file. Not part of
// the normal suite: it is `#[ignore]`d, and further refuses to run unless an
// environment variable is set explicitly, so neither `cargo test` nor
// `cargo test -- --ignored` writes a byte by accident.
// ---------------------------------------------------------------------

#[test]
#[ignore = "minting tool, not a contract: run with WIRE_LEDGER_WRITE=1 to write a new revision's frozen file"]
fn write_current_revision_frozen_file() {
    assert_eq!(
        std::env::var("WIRE_LEDGER_WRITE").as_deref(),
        Ok("1"),
        "set WIRE_LEDGER_WRITE=1 to run this; it is not part of the normal test suite"
    );

    let revision = yagura_protocol::PROTOCOL_REVISION;
    let path = frozen_file_path(revision);
    assert!(
        !path.exists(),
        "refusing to overwrite {path:?}: a revision that has already been minted is never \
         rewritten to match today's measurement"
    );

    let ledger = current_ledger();
    let json = serde_json::to_string_pretty(&ledger).expect("the ledger must serialize");
    std::fs::create_dir_all(wire_shape_dir())
        .unwrap_or_else(|err| panic!("{:?} must be creatable: {err}", wire_shape_dir()));
    std::fs::write(&path, json).unwrap_or_else(|err| panic!("{path:?} must be writable: {err}"));
}
