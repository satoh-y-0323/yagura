/**
 * Thin client for the Tauri core: invokes the window-scoped session commands
 * and subscribes to the session events plus the window-list snapshots
 * the core polls for.
 *
 * Every value received from the core passes through `session-boundary` before
 * it reaches a caller, so the handlers below only ever see validated data.
 *
 * The subscription is taken once for the whole application and routed by
 * `window_id` through an injected resolver: listening per window would open
 * one subscription per pane and make delivery depend on registration order.
 * The resolver is injected rather than imported so this layer never depends
 * on the window registry (the dependency runs registry -> client only).
 *
 * `session://status` is forwarded through `AppEventHandlers` rather than
 * `WindowEventHandlers`: it is delivered once to the application-level sink,
 * which owns the per-window status the view renders.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { FrameAckEntry } from './frame-pull';
import {
  asRecord,
  BoundaryError,
  narrowApprovalRegistrationRefusedPayload,
  narrowApprovalRequestedPayload,
  narrowApprovalResetPayload,
  narrowApprovalSyncOutcome,
  narrowApprovalWithdrawnPayload,
  narrowAttachResponse,
  narrowChatChanged,
  narrowChatMessages,
  narrowChatPosted,
  narrowChatRooms,
  narrowCloseOutcome,
  narrowDaemonRecoveryPayload,
  narrowDaemonSpawnWarningPayload,
  narrowDecideApprovalOutcome,
  narrowDisconnectedPayload,
  narrowExitPayload,
  narrowFrame,
  narrowRecoveryStatusReport,
  narrowDaemonHealth,
  narrowRestoreCandidateList,
  narrowSelectionText,
  narrowStatusPayload,
  narrowStringResult,
  narrowSupersededPayload,
  narrowWindowInfoList,
  narrowWindowListPayload,
  narrowWindowOpenFailure,
  ownEntries,
  readOptionalInteger,
  readOptionalString,
  readOwnField,
  requireArray,
  requireBoolean,
  requireRecord,
  requireNonNegativeInteger,
  requireString,
  SESSION_EVENT,
  WindowOpenFailureError,
  type ApprovalRegistrationRefusedPayload,
  type ApprovalRequestedPayload,
  type ApprovalResetPayload,
  type ApprovalSyncOutcome,
  type ApprovalWithdrawnPayload,
  type AttachResponse,
  type ChatChanged,
  type ChatMessages,
  type ChatPosted,
  type ChatRoomInfo,
  type CloseOutcome,
  type DaemonProblem,
  type DaemonRecoveryPayload,
  type DaemonSpawnWarningPayload,
  type DecideApprovalOutcome,
  type DisconnectedPayload,
  type ExitPayload,
  type Frame,
  type RecoveryStatusReport,
  type Rgb,
  type RestoreEntry,
  type SessionEventName,
  type StatusPayload,
  type SupersededPayload,
  type UntrustedRecord,
  type WindowInfo,
  type WindowListPayload,
  type WindowScoped,
} from './session-boundary';
import type { KeyInput, Modifiers } from '../terminal/key-input';

export type { ApprovalSyncOutcome, CloseOutcome, DecideApprovalOutcome, RestoreEntry, WindowInfo };

/** Callbacks invoked with validated event data of one window. */
export interface WindowEventHandlers {
  /**
   * One part of a frame of this window's view. Returns the frame number to
   * acknowledge, or `null` when the part is not one that is acknowledged.
   */
  onFrame: (frame: Frame) => number | null;
  /** The child process of this window exited. */
  onExit: (payload: ExitPayload) => void;
  /** This window's connection was lost and could not be recovered. */
  onDisconnected: (payload: DisconnectedPayload) => void;
  /** Another client took this window over; it must not reconnect. */
  onSuperseded: (payload: SupersededPayload) => void;
}

/** Callbacks for events that belong to the application, not to one window. */
export interface AppEventHandlers {
  /** The daemon had to be started without `CREATE_BREAKAWAY_FROM_JOB`. */
  onDaemonSpawnWarning: (payload: DaemonSpawnWarningPayload) => void;
  /** A value from the core failed boundary validation. */
  onBoundaryError: (error: unknown) => void;
  /**
   * A confirmed status transition. Optional so a caller that does not track
   * status can subscribe without supplying a sink for it.
   */
  onStatus?: (payload: StatusPayload) => void;
  /** A destructive operation is waiting for this approver's decision. */
  onApprovalRequested?: (payload: ApprovalRequestedPayload) => void;
  /** A pending request can no longer be decided. */
  onApprovalWithdrawn?: (payload: ApprovalWithdrawnPayload) => void;
  /** The whole pending queue, as it stands after approver registration. */
  onApprovalReset?: (payload: ApprovalResetPayload) => void;
  /**
   * This connection's `register_approver` attempt was refused:
   * the peer executable did not match the daemon's
   * `--approver-exe`, or the daemon has none configured.
   */
  onApprovalRegistrationRefused?: (payload: ApprovalRegistrationRefusedPayload) => void;
  /** The daemon-recovery supervisor moved to a new status. */
  onDaemonRecovery?: (payload: DaemonRecoveryPayload) => void;
}

/**
 * Event carrying the window list the core polls the daemon for. It is not one
 * of the session events: it belongs to the core's polling loop, which
 * owns the interval (`constants.rs`), so this side only reacts to what it
 * receives and never restates how often that is.
 */
const WINDOW_LIST_EVENT = 'session://window-list';

/** Resolves the handlers of a window, or `undefined` if it is not open. */
export type WindowHandlerResolver = (windowId: string) => WindowEventHandlers | undefined;

/**
 * Attaches to one window's session at the size the window will be drawn at
 * (`null` keeps the window's own size).
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeAttachWindow(
  windowId: string,
  size: { cols: number; rows: number } | null
): Promise<AttachResponse> {
  return narrowAttachResponse(
    await invoke<unknown>('attach_window', { window_id: windowId, size })
  );
}

/** A cell of a window's view, counted from 0 at the top-left. */
export interface WireCell {
  col: number;
  row: number;
}

/**
 * One input event for a window, as the wire carries it: the daemon turns it
 * into bytes with the window's terminal modes, or applies it to the view.
 */
export type WindowInput =
  | ({ type: 'key' } & KeyInput)
  | { type: 'text'; text: string }
  | { type: 'paste'; text: string }
  | {
      type: 'pointer';
      kind: 'down' | 'up' | 'move';
      button: 'left' | 'middle' | 'right' | 'none';
      cell: WireCell;
      mods: Modifiers;
      clicks: number;
    }
  | { type: 'wheel'; delta: number; cell: WireCell; mods: Modifiers }
  | { type: 'focus'; focused: boolean };

/** The colors a window is drawn with, as the daemon answers color queries with them. */
export interface WirePalette {
  foreground: Rgb;
  background: Rgb;
  cursor: Rgb;
  /** Colors 0-15. */
  ansi: Rgb[];
}

/**
 * Sends one input event to a window.
 *
 * @throws when the core rejects the request.
 */
export async function invokeSendWindowInput(windowId: string, input: WindowInput): Promise<void> {
  await invoke<unknown>('send_window_input', { window_id: windowId, input });
}

/**
 * Acknowledges the frames the WebView applied and waits for the next frame
 * parts of any window. A part that fails validation is reported to
 * `onInvalid` and left out; the rest are returned.
 *
 * @throws when the request itself fails.
 */
export async function invokeNextFrames(
  acks: FrameAckEntry[],
  onInvalid: (error: unknown) => void
): Promise<[string, Frame][]> {
  const response = requireArray(await invoke<unknown>('next_frames', { acks }), 'next_frames');
  const frames: [string, Frame][] = [];
  for (const entry of response) {
    try {
      const pair = requireArray(entry, 'next_frames.entry');
      const windowId: unknown = pair[0];
      if (typeof windowId !== 'string') {
        throw new BoundaryError('next_frames.entry: expected a window id');
      }
      frames.push([windowId, narrowFrame(pair[1])]);
    } catch (error) {
      onInvalid(error);
    }
  }
  return frames;
}

/**
 * Gives the daemon the colors a window is drawn with.
 *
 * @throws when the core rejects the request.
 */
export async function invokeSetWindowPalette(
  windowId: string,
  palette: WirePalette
): Promise<void> {
  await invoke<unknown>('set_window_palette', { window_id: windowId, palette });
}

/**
 * Tells the daemon whether a window is on screen; it builds no frames for one
 * that is not.
 *
 * @throws when the core rejects the request.
 */
export async function invokeSetWindowVisible(windowId: string, visible: boolean): Promise<void> {
  await invoke<unknown>('set_window_visible', { window_id: windowId, visible });
}

/**
 * Reads the text selected in a window; `null` when nothing is selected.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeCopyWindowSelection(windowId: string): Promise<string | null> {
  return narrowSelectionText(
    await invoke<unknown>('copy_window_selection', { window_id: windowId })
  );
}

/**
 * Clears the selection in a window.
 *
 * @throws when the core rejects the request.
 */
export async function invokeClearWindowSelection(windowId: string): Promise<void> {
  await invoke<unknown>('clear_window_selection', { window_id: windowId });
}

/**
 * Resizes one window's PTY.
 *
 * @throws when the core rejects the request.
 */
export async function invokeResize(windowId: string, cols: number, rows: number): Promise<void> {
  await invoke<unknown>('resize', { window_id: windowId, cols, rows });
}

/**
 * Lists the windows the daemon currently holds.
 *
 * @throws when the core rejects the request or an entry fails validation.
 */
export async function invokeListWindows(): Promise<WindowInfo[]> {
  return narrowWindowInfoList(await invoke<unknown>('list_windows'));
}

/** Result of `invokeListRestoreCandidates`. */
export interface RestoreCandidatesResult {
  /** The previous boot's windows the restore UI can offer, in position order. */
  candidates: RestoreEntry[];
  /** When that configuration was recorded, `null` when unknown. */
  takenAtEpochS: number | null;
}

/**
 * Lists the previous boot's window configuration for the restore UI.
 * Read-only: never rotates or writes the store.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 * @throws when the core rejects the request.
 */
export async function invokeListRestoreCandidates(): Promise<RestoreCandidatesResult> {
  const record = requireRecord(
    await invoke<unknown>('list_restore_candidates'),
    'list_restore_candidates'
  );
  return {
    candidates: narrowRestoreCandidateList(readOwnField(record, 'windows')),
    takenAtEpochS: readOptionalInteger(record, 'taken_at_epoch_s', 'list_restore_candidates'),
  };
}

/**
 * Reports a window's presentation layout to the daemon, verbatim -- the
 * daemon stores it unparsed and never interprets it. Argument keys are
 * `window_id` / `layout_json`, matching the `UpdateWindowLayout` protocol
 * variant one for one.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 * @throws when the core rejects the request (`window_not_found`, an
 * oversized `layoutJson`, or the daemon connection is gone).
 */
export async function invokeUpdateWindowLayout(
  windowId: string,
  layoutJson: string
): Promise<{ windowId: string }> {
  const context = 'update_window_layout';
  const record = requireRecord(
    await invoke<unknown>(context, {
      window_id: windowId,
      layout_json: layoutJson,
    }),
    context
  );
  return { windowId: requireString(record, 'window_id', context) };
}

/** The detection definitions the daemon can offer, and why it has none. */
export interface DetectionListResult {
  /** The available definition names, in the order the daemon listed them. */
  names: string[];
  /** Why the daemon could not enumerate them, `null` when it could. */
  warning: string | null;
}

/**
 * What `invokeOpenWindow` asks the core to open. The fields are named so two
 * string fields cannot be passed in each other's place.
 */
export interface OpenWindowRequest {
  /** The argument vector to run; empty when `profile` supplies it. */
  command: string[];
  /** The working directory. */
  cwd?: string | undefined;
  /** The window's display name. */
  name?: string | undefined;
  /** The detection definition's name. */
  detection?: string | undefined;
  /** The launch profile's id, which supplies the command, detection and vendor itself. */
  profile?: string | undefined;
  /** The vendor badge. */
  vendor?: string | undefined;
  /** The model alias; requires `profile`. */
  model?: string | undefined;
  /** The effort level; requires `profile`. */
  effort?: string | undefined;
}

/** Result of `invokeOpenWindow`: the new window, and how its detection went. */
export interface OpenWindowResult {
  /** The id the daemon assigned to the new window. */
  windowId: string;
  /**
   * Why the window's detection definition failed to load, `null` when it
   * loaded or when detection is disabled. A warning does not mean the window
   * failed to open: it is open, with its status pinned to `Unknown`.
   */
  detectionWarning: string | null;
}

/**
 * Lists the detection definition names the daemon can offer for
 * `invokeOpenWindow`'s `detection` argument.
 *
 * A directory that cannot be read is reported through `warning` with an empty
 * `names`, so the caller can tell "no definitions" from "could not look".
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeListDetections(): Promise<DetectionListResult> {
  return narrowDetectionListResult(await invoke<unknown>('list_detections'));
}

/**
 * One `model` / `effort` launch-option spec a profile maps (protocol
 * `LaunchOptionInfo`): the alias table `AddWindowDialog` offers as datalist
 * candidates, in the order the profile file declared them.
 */
export interface LaunchOptionInfo {
  aliases: { alias: string; value: string }[];
}

/**
 * Why a profile cannot be launched right now (protocol `ProfileUnavailableKind`).
 * `'unknown'` stands for a kind this build does not know.
 */
export type ProfileUnavailableKind =
  | 'not_found'
  | 'metacharacter'
  | 'no_command'
  | 'no_default_shell'
  | 'needs_native_program'
  | 'unknown';

const PROFILE_UNAVAILABLE_KINDS: ReadonlySet<string> = new Set<ProfileUnavailableKind>([
  'not_found',
  'metacharacter',
  'no_command',
  'no_default_shell',
  'needs_native_program',
]);

/**
 * What a profile tells its user to do about a reason it cannot be used
 * (protocol `ProfileGuidance`): an explanation, a command to copy and a page.
 */
export interface ProfileGuidance {
  /** The explanation, one line. */
  text: string;
  /** A command the user can copy and run, `null` when the profile names none. */
  command: string | null;
  /** An `https://` page that says more, `null` when there is none to show. */
  url: string | null;
}

/** One launch profile the daemon offers, in the WebView's vocabulary. */
export interface ProfileEntry {
  /** The profile identifier, which `invokeOpenWindow`'s `profile` takes. */
  id: string;
  /** The profile's declared display name. */
  displayName: string;
  /** The detection definition this profile names, `null` for the default. */
  detection: string | null;
  /** Why that definition could not be found, `null` when it was. */
  detectionWarning: string | null;
  /** The profile's own default working directory, `null` when it has none. */
  cwd: string | null;
  /**
   * The unresolved argv launching this profile would run, empty when the
   * daemon has none to offer on its operating system.
   */
  command: string[];
  /** Whether the profile currently resolves to something launchable. */
  available: boolean;
  /** The daemon's English reason text when `available` is false. */
  reason: string | null;
  /** The kind of that reason, `null` when available or from a daemon that does not say. */
  unavailableKind: ProfileUnavailableKind | null;
  /** The profile's own guidance for a `needs_native_program` reason, `null` otherwise. */
  guidance: ProfileGuidance | null;
  /** This profile's `model` launch option, `null` when it maps no such axis. */
  model: LaunchOptionInfo | null;
  /** This profile's `effort` launch option, `null` when it maps no such axis. */
  effort: LaunchOptionInfo | null;
  /** Whether the user left this profile switched on. */
  enabled: boolean;
}

/** Result of `invokeListProfiles`. */
export interface ProfileListResult {
  /** The profiles the daemon's profiles directory currently holds. */
  profiles: ProfileEntry[];
  /** One line per file the daemon excluded, or per reason it enumerated none. */
  warnings: string[];
}

/**
 * Lists the launch profiles the daemon can offer for `invokeOpenWindow`'s
 * `profile` argument.
 *
 * A profiles directory that could not be enumerated is reported through
 * `warnings` with an empty `profiles`, so the caller can tell "no profiles"
 * from "could not look".
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeListProfiles(): Promise<ProfileListResult> {
  return narrowProfileListResult(await invoke<unknown>('list_profiles'));
}

/** Result of `invokeSetProfileEnabled`: the state the daemon stored. */
export interface ProfileEnabledResult {
  /** The profile identifier the daemon updated. */
  profile: string;
  /** Whether that profile is now switched on. */
  enabled: boolean;
}

/**
 * Switches one launch profile on or off.
 *
 * @returns the daemon's reply, not an echo of the request.
 * @throws when the core rejects the request.
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeSetProfileEnabled(
  profile: string,
  enabled: boolean
): Promise<ProfileEnabledResult> {
  return narrowProfileEnabledResult(
    await invoke<unknown>('set_profile_enabled', { profile, enabled })
  );
}

/**
 * The state the daemon's playbook is in (mirrors the Rust `PlaybookState`).
 * An unrecognized wire value narrows to `'unknown'`, the same fail-safe
 * discipline `ApprovalRequesterKind` follows: a state a newer daemon adds
 * must not be mistaken for `'loaded'`.
 */
export type PlaybookState = 'loaded' | 'not_configured' | 'failed' | 'unknown';

const PLAYBOOK_STATE_VALUES: ReadonlySet<string> = new Set(['loaded', 'not_configured', 'failed']);

/** One gated tool's auto-approve switch, in the daemon's order. */
export interface ToolAutoApprove {
  /**
   * The tool's wire name (`DestructiveTool::as_tool_name`), carried through
   * verbatim even when this build does not recognize it: an unknown name is
   * still a row the user can switch on or off.
   */
  tool: string;
  /** Whether this tool currently runs without waiting for approval. */
  autoApprove: boolean;
}

/** Result of `get_approval_settings` / `set_auto_approve` / `set_plan_approval_required`. */
export interface ApprovalSettingsInfo {
  /** Whether the daemon's playbook loaded, and if not, why. */
  playbook: PlaybookState;
  /** Every gated tool's switch, in the order the daemon sent them. */
  tools: ToolAutoApprove[];
  /** Whether a newly created plan currently waits for review before it runs. */
  plansRequireApproval: boolean;
}

/**
 * Validates the `playbook` field of an approval-settings reply.
 *
 * @throws {BoundaryError} when the record owns no such field, or owns one
 * that is not a string.
 */
function narrowPlaybookState(record: UntrustedRecord, context: string): PlaybookState {
  const value = requireString(record, 'playbook', context);
  return PLAYBOOK_STATE_VALUES.has(value) ? (value as PlaybookState) : 'unknown';
}

/**
 * Validates one entry of the `tools` array of an approval-settings reply.
 *
 * @throws {BoundaryError} when the entry does not match the contract.
 */
function narrowToolAutoApprove(value: unknown, context: string): ToolAutoApprove {
  const record = requireRecord(value, context);
  return {
    tool: requireString(record, 'tool', context),
    autoApprove: requireBoolean(record, 'auto_approve', context),
  };
}

/**
 * Validates the `tools` field of an approval-settings reply.
 *
 * @throws {BoundaryError} when the value is not an array, or an entry does
 * not match the contract.
 */
function narrowToolAutoApproveList(value: unknown, context: string): ToolAutoApprove[] {
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${context}: expected an array`);
  }
  return (value as unknown[]).map((entry) => narrowToolAutoApprove(entry, context));
}

/**
 * Validates a reply to `get_approval_settings` / `set_auto_approve` /
 * `set_plan_approval_required`, all three of which answer with the same
 * shape (the daemon's current values, not an echo of the request).
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowApprovalSettingsInfo(value: unknown): ApprovalSettingsInfo {
  const context = 'approval_settings';
  const record = requireRecord(value, context);
  return {
    playbook: narrowPlaybookState(record, context),
    tools: narrowToolAutoApproveList(readOwnField(record, 'tools'), `${context}.tools`),
    plansRequireApproval: requireBoolean(record, 'plans_require_approval', context),
  };
}

/**
 * Reads the approval settings: the playbook state, every gated tool's
 * auto-approve switch, and whether a new plan waits for review.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeGetApprovalSettings(): Promise<ApprovalSettingsInfo> {
  return narrowApprovalSettingsInfo(await invoke<unknown>('get_approval_settings'));
}

/**
 * Switches one gated tool's auto-approve on or off.
 *
 * @returns the daemon's current settings, not an echo of the request.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeSetAutoApprove(
  tool: string,
  autoApprove: boolean
): Promise<ApprovalSettingsInfo> {
  return narrowApprovalSettingsInfo(
    await invoke<unknown>('set_auto_approve', { tool, auto_approve: autoApprove })
  );
}

/**
 * Switches whether a newly created plan waits for review before it runs.
 *
 * @returns the daemon's current settings, not an echo of the request.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeSetPlanApprovalRequired(
  required: boolean
): Promise<ApprovalSettingsInfo> {
  return narrowApprovalSettingsInfo(
    await invoke<unknown>('set_plan_approval_required', { required })
  );
}

/**
 * Opens a window, either running `command` (optionally in `cwd`, with an
 * optional display `name`, detection definition, and `vendor` badge) or
 * expanding the launch `profile` of that id, which supplies all of those
 * itself.
 *
 * The command is an argument vector, never a shell string, so nothing in it
 * can be interpreted by a shell (CWE-78).
 *
 * `name`, `detection`, `profile`, `vendor`, `model` and `effort` are omitted
 * from the call entirely when absent or empty, rather than sent as `null`:
 * the core distinguishes "not provided" from an empty string, and value
 * validation (including that `profile` excludes the other three, and that
 * `model` / `effort` require `profile`) is left to the core and daemon.
 *
 * @returns the new window's id together with its detection warning, if any.
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeOpenWindow(request: OpenWindowRequest): Promise<OpenWindowResult> {
  const { command, cwd, name, detection, profile, vendor, model, effort } = request;
  const args: Record<string, unknown> = { command, cwd };
  for (const [key, value] of [
    ['name', name],
    ['detection', detection],
    ['profile', profile],
    ['vendor', vendor],
    ['model', model],
    ['effort', effort],
  ] as const) {
    if (value !== undefined && value !== '') {
      args[key] = value;
    }
  }
  let reply: unknown;
  try {
    reply = await invoke<unknown>('open_window', args);
  } catch (rejection) {
    throw new WindowOpenFailureError(narrowWindowOpenFailure(rejection));
  }
  return narrowOpenWindowResult(reply);
}

/**
 * Validates a `list_profiles` reply, narrowing each entry from the daemon's
 * own field names (`display_name` / `detection_warning` /
 * `unavailable_reason`) into this layer's camelCase.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowProfileListResult(value: unknown): ProfileListResult {
  const context = 'list_profiles';
  const record = requireRecord(value, context);
  const rawProfiles = readOwnField(record, 'profiles');
  if (!Array.isArray(rawProfiles)) {
    throw new BoundaryError(`${context}.profiles: expected an array`);
  }
  const profiles = (rawProfiles as unknown[]).map((entry) => narrowProfileEntry(entry, context));
  return { profiles, warnings: narrowProfileWarnings(readOwnField(record, 'warnings'), context) };
}

/**
 * Validates one `list_profiles` entry.
 *
 * @throws {BoundaryError} when the entry does not match the contract.
 */
function narrowProfileEntry(value: unknown, context: string): ProfileEntry {
  const record = requireRecord(value, `${context}.profiles`);
  const available = readOwnField(record, 'available');
  if (typeof available !== 'boolean') {
    throw new BoundaryError(`${context}.profiles.available: expected a boolean`);
  }
  return {
    id: requireString(record, 'id', context),
    displayName: requireString(record, 'display_name', context),
    detection: readOptionalString(record, 'detection', context),
    detectionWarning: readOptionalString(record, 'detection_warning', context),
    cwd: readOptionalString(record, 'cwd', context),
    // The daemon always sends `command` (empty when it has no argv to run),
    // so a missing key is a contract violation rather than "no value".
    command: narrowStringArray(readOwnField(record, 'command'), `${context}.profiles.command`),
    available,
    reason: readOptionalString(record, 'unavailable_reason', context),
    unavailableKind: narrowUnavailableKind(readOptionalString(record, 'unavailable_kind', context)),
    guidance: narrowProfileGuidance(readOwnField(record, 'unavailable_guidance'), context),
    model: narrowLaunchOptionInfo(readOwnField(record, 'model'), `${context}.profiles.model`),
    effort: narrowLaunchOptionInfo(readOwnField(record, 'effort'), `${context}.profiles.effort`),
    enabled: narrowProfileEnabled(readOwnField(record, 'enabled'), `${context}.profiles.enabled`),
  };
}

/**
 * Validates a profile's install guidance. A page that is not `https://` is
 * dropped rather than shown, whatever the daemon let through.
 *
 * @throws {BoundaryError} when it is present and not of the guidance shape.
 */
function narrowProfileGuidance(value: unknown, context: string): ProfileGuidance | null {
  if (value === null || value === undefined) {
    return null;
  }
  const guidanceContext = `${context}.unavailable_guidance`;
  const record = requireRecord(value, guidanceContext);
  const url = readOptionalString(record, 'url', guidanceContext);
  return {
    text: requireString(record, 'text', guidanceContext),
    command: readOptionalString(record, 'command', guidanceContext),
    url: url !== null && url.startsWith('https://') ? url : null,
  };
}

/** Reads a kind this build does not know as `'unknown'`, as the native side does. */
function narrowUnavailableKind(value: string | null): ProfileUnavailableKind | null {
  if (value === null) {
    return null;
  }
  return PROFILE_UNAVAILABLE_KINDS.has(value) ? (value as ProfileUnavailableKind) : 'unknown';
}

/**
 * Validates a profile's `enabled` flag. A daemon that predates the flag
 * cannot disable a profile, so a missing key reads as enabled.
 *
 * @throws {BoundaryError} when it is present and not a boolean.
 */
function narrowProfileEnabled(value: unknown, path: string): boolean {
  if (value === undefined) {
    return true;
  }
  if (typeof value !== 'boolean') {
    throw new BoundaryError(`${path}: expected a boolean`);
  }
  return value;
}

/**
 * Validates a `set_profile_enabled` reply. Unlike `list_profiles`, the flag
 * is required here: the reply exists to report the state the daemon stored.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowProfileEnabledResult(value: unknown): ProfileEnabledResult {
  const context = 'set_profile_enabled';
  const record = requireRecord(value, context);
  const enabled = readOwnField(record, 'enabled');
  if (typeof enabled !== 'boolean') {
    throw new BoundaryError(`${context}.enabled: expected a boolean`);
  }
  return { profile: requireString(record, 'profile', context), enabled };
}

/**
 * Validates one `model` / `effort` launch-option spec of a `list_profiles`
 * entry. A missing key and an explicit `null` both mean the profile does not
 * map that axis; a well-formed object is carried through with `aliases` kept
 * in the order the daemon sent it.
 *
 * @throws {BoundaryError} when it is present with any other shape.
 */
function narrowLaunchOptionInfo(value: unknown, path: string): LaunchOptionInfo | null {
  if (value === undefined || value === null) {
    return null;
  }
  const spec = asRecord(value);
  if (spec === null) {
    throw new BoundaryError(`${path}: expected an object or null`);
  }
  const rawAliases = readOwnField(spec, 'aliases');
  if (!Array.isArray(rawAliases)) {
    throw new BoundaryError(`${path}.aliases: expected an array`);
  }
  const aliases: { alias: string; value: string }[] = [];
  for (const entry of rawAliases as unknown[]) {
    const record = asRecord(entry);
    if (record === null) {
      throw new BoundaryError(`${path}.aliases: expected an object entry`);
    }
    const alias = readOwnField(record, 'alias');
    const aliasValue = readOwnField(record, 'value');
    if (typeof alias !== 'string' || typeof aliasValue !== 'string') {
      throw new BoundaryError(`${path}.aliases: expected string alias/value fields`);
    }
    aliases.push({ alias, value: aliasValue });
  }
  return { aliases };
}

/**
 * Validates the `warnings` of a `list_profiles` reply. A missing key is an
 * empty list: the daemon omits it when it has nothing to report.
 *
 * @throws {BoundaryError} when it is present with any other shape.
 */
function narrowProfileWarnings(value: unknown, context: string): string[] {
  if (value === undefined || value === null) {
    return [];
  }
  return narrowStringArray(value, `${context}.warnings`);
}

/**
 * Validates one array-of-strings field of a reply, named by `path` in the
 * error text.
 *
 * @throws {BoundaryError} when it is not an array, or holds a non-string.
 */
function narrowStringArray(value: unknown, path: string): string[] {
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${path}: expected an array`);
  }
  const entries: string[] = [];
  for (const entry of value as unknown[]) {
    if (typeof entry !== 'string') {
      throw new BoundaryError(`${path}: expected a string entry`);
    }
    entries.push(entry);
  }
  return entries;
}

/**
 * Validates a `list_detections` reply.
 *
 * The names are collected into an array, never into the keys of an object: a
 * definition name is external text and must not be able to reach a prototype
 * slot.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowDetectionListResult(value: unknown): DetectionListResult {
  const context = 'list_detections';
  const record = requireRecord(value, context);
  const names = narrowStringArray(readOwnField(record, 'names'), `${context}.names`);
  return { names, warning: readOptionalString(record, 'warning', context) };
}

/**
 * Validates an `open_window` reply.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowOpenWindowResult(value: unknown): OpenWindowResult {
  const context = 'open_window';
  const record = requireRecord(value, context);
  return {
    windowId: requireString(record, 'window_id', context),
    detectionWarning: readOptionalString(record, 'detection_warning', context),
  };
}

/**
 * Closes one window, terminating its process tree. A window the daemon does
 * not hold (`'not_found'`) and a daemon out of reach (`'unreachable'`) also
 * resolve: the caller takes any of the three as the signal to drop the pane,
 * which is presentation cleanup -- a window that is in fact alive gets its
 * pane back from the next window-list reconciliation.
 *
 * @throws when the core rejects the request.
 */
export async function invokeCloseWindow(windowId: string, force?: boolean): Promise<CloseOutcome> {
  return narrowCloseOutcome(await invoke<unknown>('close_window', { window_id: windowId, force }));
}

/**
 * Interrupts the child process of one window.
 *
 * @throws when the core rejects the request.
 */
export async function invokeInterruptWindow(windowId: string): Promise<void> {
  await invoke<unknown>('interrupt_window', { window_id: windowId });
}

/**
 * Answers one pending approval request. The request is identified by the
 * opaque id the daemon issued; validating it is the daemon's job. A request
 * that is no longer pending (already decided elsewhere, or expired) is
 * reported as `'not_pending'`, not thrown -- see [`DecideApprovalOutcome`].
 *
 * @throws when the core rejects the request for any other reason.
 */
export async function invokeDecideApproval(
  requestId: string,
  approved: boolean
): Promise<DecideApprovalOutcome> {
  const result = await invoke<unknown>('decide_approval', { request_id: requestId, approved });
  return narrowDecideApprovalOutcome(result);
}

/**
 * Pulls the approval requests currently pending for this GUI, and whether it
 * is a registered approver at all. Started by this side once its event
 * subscription is open, so a pending request the daemon announced before then
 * is not lost.
 *
 * @throws when the core rejects the request or the outcome fails validation.
 */
export async function invokeSyncApprovals(): Promise<ApprovalSyncOutcome> {
  return narrowApprovalSyncOutcome(await invoke<unknown>('sync_approvals'));
}

/**
 * Renames one window.
 *
 * @returns the name the daemon stored, which may differ from the request.
 * @throws when the core rejects the request.
 */
export async function invokeRenameWindow(windowId: string, name: string): Promise<string> {
  return narrowStringResult(
    await invoke<unknown>('rename_window', { window_id: windowId, name }),
    'rename_window'
  );
}

/**
 * Posts one chat message, addressed to the windows its `@` mentions resolve
 * to. `roomId` selects the room; omitting it leaves the choice to the daemon.
 *
 * @throws when the core rejects the post or the reply fails validation.
 */
export async function invokeSendChat(body: string, roomId?: string): Promise<ChatPosted> {
  return narrowChatPosted(await invoke<unknown>('send_chat', { body, room_id: roomId }));
}

/**
 * Posts one chat message to every participant of the room but the poster.
 *
 * @throws when the core rejects the post or the reply fails validation.
 */
export async function invokeBroadcastChat(body: string, roomId?: string): Promise<ChatPosted> {
  return narrowChatPosted(await invoke<unknown>('broadcast_chat', { body, room_id: roomId }));
}

/**
 * Reads one page of a room's history. `afterSeq` is the cursor the previous
 * page returned as `next_after_seq`; omitting it asks the daemon for the most
 * recent page.
 *
 * @throws when the core rejects the read or the reply fails validation.
 */
export async function invokeReadChat(
  roomId?: string,
  afterSeq?: number,
  limit?: number
): Promise<ChatMessages> {
  return narrowChatMessages(
    await invoke<unknown>('read_chat', { room_id: roomId, after_seq: afterSeq, limit })
  );
}

/**
 * Lists every chat room the daemon knows of.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListChatRooms(): Promise<ChatRoomInfo[]> {
  return narrowChatRooms(await invoke<unknown>('list_chat_rooms'));
}

/**
 * Reads the current daemon-recovery status, so a subscriber that has just
 * started listening does not have to wait for the next change to know where
 * recovery stands.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeGetRecoveryStatus(): Promise<RecoveryStatusReport> {
  return narrowRecoveryStatusReport(await invoke<unknown>('get_recovery_status'));
}

/**
 * Asks the daemon what keeps it from opening windows. A daemon that predates
 * the request answers with no problems.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 * @throws when the core rejects the request (the daemon is unreachable).
 */
export async function invokeGetDaemonHealth(): Promise<DaemonProblem[]> {
  return narrowDaemonHealth(await invoke<unknown>('get_daemon_health'));
}

/**
 * Asks the supervisor to start the daemon again after automatic recovery has
 * given up. Never rejects on the daemon still being unreachable:
 * whether it came back is reported through the recovery status, not by this
 * call.
 *
 * @throws when the core rejects the request for a reason other than the
 * daemon being unreachable.
 */
export async function invokeReconnectDaemon(): Promise<void> {
  await invoke<unknown>('reconnect_daemon');
}

/** One reason `load_keymap` could not produce keys from the keymap file. */
export type KeymapLoadFailure = 'not_found' | 'read_failed' | 'invalid_toml' | 'too_large';

/** Result of `invokeLoadKeymap`: this call never throws, it always resolves. */
export interface LoadKeymapResult {
  /** Action-to-chord overrides of the file. Empty when `failure` is set. */
  keys: Record<string, string>;
  /** Entries of the old chord-to-action table, which no longer take effect. */
  legacy_entries: number;
  /** Set only when the file could not be read into keys. */
  failure?: KeymapLoadFailure;
}

/** One reason `save_keymap` refused to write the keymap file. */
export type KeymapSaveFailure = 'no_config_root' | 'too_large' | 'write_failed';

/** Outcome of `invokeSaveKeymap`: a refusal is a value, not a rejection. */
export type SaveKeymapResult = { ok: true } | { ok: false; reason: KeymapSaveFailure };

const KEYMAP_LOAD_FAILURES: ReadonlySet<string> = new Set([
  'not_found',
  'read_failed',
  'invalid_toml',
  'too_large',
]);

const KEYMAP_SAVE_FAILURES: ReadonlySet<string> = new Set([
  'no_config_root',
  'too_large',
  'write_failed',
]);

/**
 * Loads the user's keymap through the `load_keymap` command.
 *
 * Never throws for the file itself: a missing, unreadable, malformed, or
 * oversized keymap file is reported via `failure` with empty `keys`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeLoadKeymap(): Promise<LoadKeymapResult> {
  return narrowLoadKeymapResult(await invoke<unknown>('load_keymap'));
}

/**
 * Asks the `keymap_file_path` command where this instance reads and writes
 * `keymap.toml`; `null` when it has no config location.
 *
 * @throws {BoundaryError} when the reply is neither a string nor `null`.
 */
export async function invokeKeymapFilePath(): Promise<string | null> {
  const reply = await invoke<unknown>('keymap_file_path');
  if (reply === null || typeof reply === 'string') return reply;
  throw new BoundaryError(`keymap_file_path: expected a string or null, got ${typeof reply}`);
}

/**
 * Saves the keymap overrides through the `save_keymap` command.
 *
 * A refusal the core names is returned as `{ ok: false, reason }`, so callers
 * above this boundary see one result type and never a rejection.
 *
 * @throws {BoundaryError} when the core rejects with a value that is not a
 * known refusal.
 */
export async function invokeSaveKeymap(keys: Record<string, string>): Promise<SaveKeymapResult> {
  try {
    await invoke<unknown>('save_keymap', { keys });
  } catch (rejection) {
    if (typeof rejection === 'string' && KEYMAP_SAVE_FAILURES.has(rejection)) {
      return { ok: false, reason: rejection as KeymapSaveFailure };
    }
    throw new BoundaryError(`save_keymap: unrecognized rejection ${JSON.stringify(rejection)}`);
  }
  return { ok: true };
}

/**
 * Validates a `load_keymap` reply.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowLoadKeymapResult(value: unknown): LoadKeymapResult {
  const record = requireRecord(value, 'load_keymap');
  const rawKeys = requireRecord(readOwnField(record, 'keys'), 'load_keymap.keys');
  const keys: Record<string, string> = {};
  for (const [action, chord] of ownEntries(rawKeys)) {
    if (typeof chord !== 'string') {
      throw new BoundaryError(`load_keymap.keys.${action}: expected a string`);
    }
    keys[action] = chord;
  }
  const legacyEntries = requireNonNegativeInteger(record, 'legacy_entries', 'load_keymap');
  const failure = readOwnField(record, 'failure');
  if (failure === undefined) {
    return { keys, legacy_entries: legacyEntries };
  }
  if (typeof failure !== 'string' || !KEYMAP_LOAD_FAILURES.has(failure)) {
    throw new BoundaryError(`load_keymap.failure: unrecognized failure ${JSON.stringify(failure)}`);
  }
  return { keys, legacy_entries: legacyEntries, failure: failure as KeymapLoadFailure };
}

/** One reason `load_themes` could not produce data from the themes file. */
export type ThemesLoadFailure = 'not_found' | 'read_failed' | 'invalid_toml' | 'too_large';

/** Result of `invokeLoadThemes`: this call never throws, it always resolves. */
export interface LoadThemesResult {
  /** Theme name to (CSS-variable-name-without-`--` to colour value) map. Empty when `failure` is set. */
  themes: Record<string, Record<string, string>>;
  /** Vendor name to theme name. Empty when `failure` is set. */
  vendorAssignments: Record<string, string>;
  /** The file's `default` key, or `null` when absent or loading failed. */
  defaultTheme: string | null;
  /** Set only when the core could not produce data from the themes file. */
  failure?: ThemesLoadFailure;
}

const THEMES_LOAD_FAILURES: ReadonlySet<string> = new Set([
  'not_found',
  'read_failed',
  'invalid_toml',
  'too_large',
]);

/**
 * Loads the user's theme palettes and vendor/default assignments through the
 * frozen `load_themes` command.
 *
 * Never throws for a missing, unreadable, malformed, or oversized themes
 * file: that is reported via `failure` with empty `themes` /
 * `vendorAssignments`, never as a rejection.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export async function invokeLoadThemes(): Promise<LoadThemesResult> {
  return narrowLoadThemesResult(await invoke<unknown>('load_themes'));
}

/**
 * Validates a `load_themes` reply.
 *
 * Value types only, not key names: a name such as `__proto__` is handled by
 * the null-prototype construction below and by the reserved-name checks in
 * `theme-resolve.ts`, which stay the single source of truth for what a valid
 * theme name is.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
function narrowLoadThemesResult(value: unknown): LoadThemesResult {
  const record = requireRecord(value, 'load_themes');

  const rawThemes = requireRecord(readOwnField(record, 'themes'), 'load_themes.themes');
  // Null-prototype, so a theme name of `__proto__` becomes a plain own key
  // instead of rewriting this object's prototype.
  const themes: Record<string, Record<string, string>> = Object.create(null) as Record<
    string,
    Record<string, string>
  >;
  for (const [themeName, rawTokens] of ownEntries(rawThemes)) {
    const tokenRecord = requireRecord(rawTokens, `load_themes.themes.${themeName}`);
    // Same reasoning as `themes`: a token key of `__proto__` must not rewrite
    // this object's prototype.
    const tokens: Record<string, string> = Object.create(null) as Record<string, string>;
    for (const [key, tokenValue] of ownEntries(tokenRecord)) {
      if (typeof tokenValue !== 'string') {
        throw new BoundaryError(`load_themes.themes.${themeName}.${key}: expected a string`);
      }
      tokens[key] = tokenValue;
    }
    themes[themeName] = tokens;
  }

  const rawAssignments = requireRecord(
    readOwnField(record, 'vendor_assignments'),
    'load_themes.vendor_assignments'
  );
  // Not null-prototype, unlike `themes`/`tokens` above: every value assigned
  // here is a `string`, and `__proto__`'s legacy setter is a no-op for a value
  // that is neither an object nor null.
  const vendorAssignments: Record<string, string> = {};
  for (const [vendor, themeName] of ownEntries(rawAssignments)) {
    if (typeof themeName !== 'string') {
      throw new BoundaryError(`load_themes.vendor_assignments.${vendor}: expected a string`);
    }
    vendorAssignments[vendor] = themeName;
  }

  const rawDefaultTheme = readOwnField(record, 'default_theme');
  let defaultTheme: string | null;
  if (rawDefaultTheme === undefined) {
    defaultTheme = null;
  } else if (typeof rawDefaultTheme === 'string') {
    defaultTheme = rawDefaultTheme;
  } else {
    throw new BoundaryError('load_themes.default_theme: expected a string');
  }

  const rawFailure = readOwnField(record, 'failure');
  if (rawFailure === undefined) {
    return { themes, vendorAssignments, defaultTheme };
  }
  if (typeof rawFailure !== 'string' || !THEMES_LOAD_FAILURES.has(rawFailure)) {
    throw new BoundaryError(
      `load_themes.failure: unrecognized failure ${JSON.stringify(rawFailure)}`
    );
  }
  return { themes, vendorAssignments, defaultTheme, failure: rawFailure as ThemesLoadFailure };
}

/**
 * Subscribes to every session event, once for the whole application.
 *
 * Window-scoped events are routed through `resolve`; an event whose window
 * the resolver does not know is dropped, because a window evicted between the
 * core emitting an event and this dispatch is a normal race, not an error.
 *
 * @returns a function that cancels every subscription.
 */
export async function subscribeSession(
  resolve: WindowHandlerResolver,
  app: AppEventHandlers
): Promise<UnlistenFn> {
  const unlisteners = await Promise.all([
    listenWindowEvent(SESSION_EVENT.exit, narrowExitPayload, resolve, app, (handlers, payload) => {
      handlers.onExit(payload);
    }),
    listenWindowEvent(
      SESSION_EVENT.disconnected,
      narrowDisconnectedPayload,
      resolve,
      app,
      (handlers, payload) => {
        handlers.onDisconnected(payload);
      }
    ),
    listenWindowEvent(
      SESSION_EVENT.superseded,
      narrowSupersededPayload,
      resolve,
      app,
      (handlers, payload) => {
        handlers.onSuperseded(payload);
      }
    ),
    listenAppEvent(
      SESSION_EVENT.daemonSpawnWarning,
      narrowDaemonSpawnWarningPayload,
      app.onDaemonSpawnWarning,
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.status,
      narrowStatusPayload,
      (payload) => {
        app.onStatus?.(payload);
      },
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.approvalRequested,
      narrowApprovalRequestedPayload,
      (payload) => {
        app.onApprovalRequested?.(payload);
      },
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.approvalWithdrawn,
      narrowApprovalWithdrawnPayload,
      (payload) => {
        app.onApprovalWithdrawn?.(payload);
      },
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.approvalReset,
      narrowApprovalResetPayload,
      (payload) => {
        app.onApprovalReset?.(payload);
      },
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.approvalRegistrationRefused,
      narrowApprovalRegistrationRefusedPayload,
      (payload) => {
        app.onApprovalRegistrationRefused?.(payload);
      },
      app.onBoundaryError
    ),
    listenAppEvent(
      SESSION_EVENT.daemonRecovery,
      narrowDaemonRecoveryPayload,
      (payload) => {
        app.onDaemonRecovery?.(payload);
      },
      app.onBoundaryError
    ),
  ]);
  return () => {
    for (const unlisten of unlisteners) {
      unlisten();
    }
  };
}

/**
 * Subscribes to the window-list snapshots the core pushes.
 *
 * A snapshot that fails validation is reported and dropped, so an unusable
 * list never tears panes down.
 *
 * @returns a function that cancels the subscription.
 */
export async function subscribeWindowList(
  onWindowList: (payload: WindowListPayload) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listenAppEvent(WINDOW_LIST_EVENT, narrowWindowListPayload, onWindowList, onBoundaryError);
}

/**
 * Subscribes to the chat-changed pushes the core forwards from the daemon.
 *
 * A payload that fails validation is reported and dropped, the same way the
 * window-list snapshots are: an unusable push never disturbs what is shown.
 *
 * @returns a function that cancels the subscription.
 */
export async function subscribeChatChanged(
  onChatChanged: (payload: ChatChanged) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listenAppEvent(
    SESSION_EVENT.chatChanged,
    narrowChatChanged,
    onChatChanged,
    onBoundaryError
  );
}

/**
 * Listens to one window-scoped event, validating its payload and routing it to
 * the window it names. A payload that fails validation is reported and
 * dropped: the terminal keeps whatever it already shows instead of acting on
 * data of an unknown shape.
 */
async function listenWindowEvent<T extends WindowScoped>(
  name: SessionEventName,
  narrow: (value: unknown) => T,
  resolve: WindowHandlerResolver,
  app: AppEventHandlers,
  deliver: (handlers: WindowEventHandlers, payload: T) => void
): Promise<UnlistenFn> {
  return listen<unknown>(name, (event) => {
    let payload: T;
    try {
      payload = narrow(event.payload);
    } catch (error) {
      app.onBoundaryError(error);
      return;
    }
    const handlers = resolve(payload.window_id);
    if (handlers === undefined) {
      return;
    }
    deliver(handlers, payload);
  });
}

/** Listens to one application-scoped event, validating its payload. */
async function listenAppEvent<T>(
  name: SessionEventName | typeof WINDOW_LIST_EVENT,
  narrow: (value: unknown) => T,
  handle: (payload: T) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listen<unknown>(name, (event) => {
    let payload: T;
    try {
      payload = narrow(event.payload);
    } catch (error) {
      onBoundaryError(error);
      return;
    }
    handle(payload);
  });
}
