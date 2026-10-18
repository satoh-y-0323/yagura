/**
 * The single WebView-side boundary for everything that crosses the Tauri IPC
 * edge: the canonical session events and the `attach` command result.
 *
 * `gui/src-tauri/src/events.rs` is the source of truth for these shapes, so
 * the field names are copied verbatim (snake_case). Everything the core sends
 * is accepted as `unknown` and validated here; no other module has to trust
 * the boundary.
 */
import { parseLayoutJson, type LayoutV1 } from '../session/restore-model';
import type { ApprovalRequesterKind, WithdrawReason } from '../session/approval-types';
import type {
  ClientConfigFailure,
  DaemonRecoveryStatus,
  StartupEnvFailure,
} from '../session/recovery-types';

/** The canonical session events emitted by the Tauri core. */
export const SESSION_EVENT = {
  exit: 'session://exit',
  disconnected: 'session://disconnected',
  superseded: 'session://superseded',
  daemonSpawnWarning: 'session://daemon-spawn-warning',
  status: 'session://status',
  approvalRequested: 'session://approval-requested',
  approvalWithdrawn: 'session://approval-withdrawn',
  approvalReset: 'session://approval-reset',
  approvalRegistrationRefused: 'session://approval-registration-refused',
  chatChanged: 'session://chat-changed',
  chatParticipantsChanged: 'session://chat-participants-changed',
  planChanged: 'session://plan-changed',
  modelChoices: 'session://model-choices',
  daemonRecovery: 'session://daemon-recovery',
  mcpRegistrationStatus: 'session://mcp-registration-status',
  mcpRegistrationResult: 'session://mcp-registration-result',
} as const;

/** Name of one of the canonical session events. */
export type SessionEventName = (typeof SESSION_EVENT)[keyof typeof SESSION_EVENT];

/** Fewest PTY columns the core accepts (the same value as Rust's `yagura_protocol::MIN_WINDOW_COLS`). */
export const MIN_PTY_COLS = 2;

/** Fewest PTY rows the core accepts (the same value as Rust's `yagura_protocol::MIN_WINDOW_ROWS`). */
export const MIN_PTY_ROWS = 1;

/** Largest PTY dimension the core accepts: the same value as Rust's `yagura_protocol::MAX_WINDOW_DIMENSION`. */
export const MAX_PTY_DIMENSION = 1000;

/**
 * The wire revision a daemon must have settled on before `attach_view` reaches
 * it (mirrors `FRAME_MIN_REVISION`, `yagura-protocol`): below it no window
 * can be shown. Informational only: it decides what the
 * `daemon-protocol-stale` banner shows, never what a request may send --
 * the core's own gate is the authority for that.
 */
export const ATTACH_VIEW_MIN_REVISION = 19;

/**
 * The first wire revision whose daemon reports and stores a profile's
 * enabled state (mirrors the daemon's protocol revision). A daemon below it
 * cannot switch a profile on or off.
 */
export const PROFILE_ENABLED_MIN_REVISION = 11;

/**
 * The first wire revision whose daemon reports and switches the approval
 * settings (`GetApprovalSettings` / `SetAutoApprove` /
 * `SetPlanApprovalRequired`, mirrors the daemon's protocol revision). A
 * daemon below it cannot answer any of the three.
 */
export const APPROVAL_SETTINGS_MIN_REVISION = 12;

/**
 * The first wire revision whose daemon lists and drives MCP registration
 * (`list_mcp_registrations` / `run_mcp_registration`, mirrors
 * `MCP_REGISTRATION_MIN_REVISION`, `yagura-protocol`). A daemon below it
 * cannot register the bridge with a CLI.
 */
export const MCP_REGISTRATION_MIN_REVISION = 13;

/** The `error.code` a daemon answers with when asked to launch a disabled profile. */
export const PROFILE_DISABLED_CODE = 'profile_disabled';

/**
 * Fields every window-scoped payload carries.
 *
 * `window_id` is required rather than optional: a missing id would otherwise
 * fall back to some default window and route another window's bytes into it
 * silently, so the absence has to be a detectable failure.
 */
export interface WindowScoped {
  /** The window the event belongs to. */
  window_id: string;
}

/** Payload of `session://exit`. */
export interface ExitPayload extends WindowScoped {
  /** Exit code of the child process (`-1` when it could not be determined). */
  exit_code: number;
}

/** A 24-bit color as `[red, green, blue]`. */
export type Rgb = [number, number, number];

/** A color of a cell; the theme resolves `default` and palette entries. */
export type CellColor = 'default' | { indexed: number } | { rgb: Rgb };

/** A flag of a cell; `selected` marks a cell inside the window's selection. */
export type CellFlag =
  'bold' | 'dim' | 'italic' | 'underline' | 'strikeout' | 'inverse' | 'hidden' | 'selected';

/** What a run of cells looks like. */
export interface CellAttr {
  fg: CellColor;
  bg: CellColor;
  flags: CellFlag[];
}

/** Consecutive cells sharing one attribute. */
export interface FrameRun {
  /** Index into {@link Frame.attrs}. */
  attr: number;
  /** One string per cell; a wide cell appears once. */
  cells: string[];
  /** Indices into `cells`, ascending, of the cells two columns wide. */
  wide: number[];
}

/** One screen row of a frame. */
export interface FrameLine {
  row: number;
  /** The line goes on in the next row. */
  wrapped: boolean;
  /** Cells from column 0 on; the columns after the last run are blank. */
  runs: FrameRun[];
}

/** The shape of the cursor. */
export type CursorShape = 'block' | 'underline' | 'bar' | 'hollow_block';

/** The cursor in the view. */
export interface CursorState {
  row: number;
  /** The column; `cols` while a character waits to wrap. */
  col: number;
  visible: boolean;
  shape: CursorShape;
}

/** What the view shows besides cells. */
export interface ViewState {
  /** Rows the view is scrolled back from the live screen (0 = live). */
  display_offset: number;
  history_rows: number;
  /** The program in the window asked for mouse reports. */
  mouse_reporting: boolean;
  alt_screen: boolean;
}

/** A color slot a program can change with OSC 4/10/11/12. */
export type PaletteSlot = 'foreground' | 'background' | 'cursor' | { indexed: number };

/** A color a program set over the theme. */
export interface ColorOverride {
  slot: PaletteSlot;
  rgb: Rgb;
}

/** One part of a frame of a window's view (`DaemonMessage::Frame`). */
export interface Frame {
  /** The frame number; every part of one frame carries the same value. */
  seq: number;
  cols: number;
  rows: number;
  /** Blank the grid before applying `lines`; only a first part sets it. */
  reset: boolean;
  /** The last part of frame `seq`, the one acknowledged. */
  last: boolean;
  attrs: CellAttr[];
  lines: FrameLine[];
  cursor: CursorState;
  view: ViewState;
  /** Every color the program set over the theme. */
  overrides: ColorOverride[];
}

/** Payload of `session://disconnected`. */
export interface DisconnectedPayload extends WindowScoped {
  /** Machine-readable reason. */
  reason: string;
}

/** Payload of `session://superseded`. */
export interface SupersededPayload extends WindowScoped {
  /** Machine-readable reason. */
  reason: string;
}

/** Payload of `session://daemon-spawn-warning`. */
export interface DaemonSpawnWarningPayload {
  /** English, human-readable description for `console.warn`. */
  message: string;
}

/**
 * The confirmed window states (mirrors the Rust `WindowStatus` enum,
 * serialized as `snake_case`).
 */
export type WindowStatus =
  'running' | 'waiting_input' | 'waiting_approval' | 'interrupted' | 'idle' | 'exited' | 'unknown';

/**
 * Why `status` is `'unknown'` (mirrors the Rust `UnknownReason` enum,
 * serialized as `snake_case`).
 */
export type UnknownReason =
  | 'no_definition'
  | 'no_rule_matched'
  | 'screen_lost'
  | 'detector_stopped'
  | 'not_yet_judged'
  | 'declared_unknown';

const UNKNOWN_REASON_VALUES: ReadonlySet<string> = new Set<UnknownReason>([
  'no_definition',
  'no_rule_matched',
  'screen_lost',
  'detector_stopped',
  'not_yet_judged',
  'declared_unknown',
]);

/**
 * A window's status together with the reason it is `'unknown'`, as one value.
 *
 * The reason lives on the `'unknown'` arm alone, so no reader downstream can
 * hold a status that is not `'unknown'` next to a reason for it. That arm's
 * `reason` is `null`, rather than absent, when the core reported none.
 */
export type DetectedStatusView =
  | { status: Exclude<WindowStatus, 'unknown'> }
  | { status: 'unknown'; reason: UnknownReason | null };

/** Payload of `session://status`. */
export interface StatusPayload {
  /** The window whose status changed (protocol v2). */
  window_id: string;
  /** The status the window moved to, with the reason it is `'unknown'`. */
  detected: DetectedStatusView;
  /**
   * The generation this observation was taken at, so a reader can tell an
   * observation older than the one it holds. Omitted from this object -- and
   * from the wire -- when the core has none to report
   * (`exactOptionalPropertyTypes`).
   */
  status_rev?: number;
}

const WINDOW_STATUS_VALUES: ReadonlySet<string> = new Set<WindowStatus>([
  'running',
  'waiting_input',
  'waiting_approval',
  'interrupted',
  'idle',
  'exited',
  'unknown',
]);

/**
 * Who a pending approval request came from (mirrors the Rust
 * `ApprovalRequester`). `window_id` and `name` are both present exactly when
 * `kind` is `"window"`, and `null` otherwise.
 */
export interface ApprovalRequester {
  /** The requester kind the daemon reported, normalized fail-safe. */
  kind: ApprovalRequesterKind;
  /** The requesting window's id, `null` when it did not come from one. */
  window_id: string | null;
  /** The requesting window's display name, `null` when it has none. */
  name: string | null;
}

/** The window a pending approval request would act on. */
export interface ApprovalTarget {
  /** The target window's id. */
  window_id: string;
  /** The target window's display name. */
  name: string;
  /** The target's status when the request was submitted, not a live value. */
  status: WindowStatus;
}

/**
 * One approval request awaiting a decision (mirrors the Rust
 * `ApprovalRequest`). It carries no expiry: the daemon owns the deadline and
 * withdraws the request when it passes.
 */
export interface ApprovalRequest {
  /** Identifies the request in a decision; unique within this daemon boot. */
  request_id: string;
  /** The gated operation's external name. */
  tool: string;
  /** Who asked. */
  requester: ApprovalRequester;
  /**
   * The window the operation would act on. Omitted from the wire -- and from
   * this object as an own property -- for a chat post, which is addressed to
   * the room rather than to one window, and for `open_window` and a
   * `send_handoff` that opens a window from a profile, whose window does not
   * exist yet (`exactOptionalPropertyTypes`).
   */
  target?: ApprovalTarget;
  /** The message body, `null` for the operations that carry none. */
  text: string | null;
  /**
   * The chat room a post is addressed to. `Some` only for `send_chat` /
   * `broadcast_chat`; omitted from the wire -- and from this object as an own
   * property -- for every other tool (`exactOptionalPropertyTypes`), the same
   * discipline `target` follows.
   */
  room?: RoomRef;
}

/**
 * A room's identifier and the display name it resolved to when the value was
 * produced (mirrors the Rust `RoomRef`). Carried both by a pending approval
 * request and by a listed window, which is why it is one type.
 */
export interface RoomRef {
  /** The room's identifier. */
  room_id: string;
  /** The room's display name as of the moment the value was produced. */
  name: string;
}

/** Payload of `session://approval-requested`. */
export interface ApprovalRequestedPayload {
  /** The request awaiting a decision. */
  request: ApprovalRequest;
}

/** Payload of `session://approval-withdrawn`. */
export interface ApprovalWithdrawnPayload {
  /** The request that is no longer actionable. */
  request_id: string;
  /** Why the request ended, normalized fail-safe. */
  reason: WithdrawReason;
}

/** Payload of `session://approval-reset`: the pending queue, wholesale. */
export interface ApprovalResetPayload {
  /** Every request pending for this approver, oldest first. */
  requests: ApprovalRequest[];
}

/**
 * Outcome of the `decide_approval` command, a request this side made itself:
 * an unrecognized value is a contract violation, not a forward-compatible
 * fallback (unlike the fail-safe normalization above).
 */
export type DecideApprovalOutcome = 'decided' | 'not_pending';

/**
 * Outcome of the `close_window` command. A window the daemon does not hold,
 * and a daemon out of reach, are outcomes rather than errors: the pane is
 * stale either way and the view has to be able to retire it.
 */
export type CloseOutcome = 'closed' | 'not_found' | 'unreachable';

/**
 * Outcome of the `sync_approvals` command: the pending list this side pulled,
 * or the daemon refusing it as an approver. Carrying the refusal as a value
 * is what lets a pull report a state whose event was emitted before the
 * subscription opened.
 */
export type ApprovalSyncOutcome =
  | {
      /** The pull registered (or confirmed) this connection as an approver. */
      status: 'registered';
      /** Every request pending for this approver, oldest first. */
      pending: ApprovalRequest[];
    }
  | {
      /** The daemon refused this connection as an approver. */
      status: 'refused';
    };

/**
 * Payload of `session://approval-registration-refused`. Fieldless: the
 * daemon's `auth_failed` refusal serializes to `{}` on the wire (a Rust unit
 * struct or `()` would serialize to `null` and fail {@link requireRecord}).
 */
export type ApprovalRegistrationRefusedPayload = Record<string, never>;

/** One participant of a chat room. Never the human user. */
export interface ChatParticipant {
  /** The participant window's id. */
  window_id: string;
  /** The participant window's display name. */
  name: string;
}

/** What kind of party posted a chat message. */
export type ChatMessageFromKind = 'window' | 'user' | 'external' | 'system';

const CHAT_FROM_KIND_VALUES: ReadonlySet<string> = new Set([
  'window',
  'user',
  'external',
  'system',
]);

/** How one chat message landed in one addressed window. */
export type ChatDeliveryState =
  | 'pending'
  | 'taken'
  | 'undelivered_unknown'
  | 'undelivered_halted'
  | 'undelivered_exhausted'
  | 'undelivered_restart';

const CHAT_DELIVERY_STATE_VALUES: ReadonlySet<string> = new Set([
  'pending',
  'taken',
  'undelivered_unknown',
  'undelivered_halted',
  'undelivered_exhausted',
  'undelivered_restart',
]);

/**
 * How one delivery ended, together with the reason the addressed window's
 * status was `'unknown'` when that is why it did not happen.
 *
 * `state` stays the top-level discriminant rather than moving into a nested
 * object, so every reader of `delivery.state` keeps reading it there.
 */
export type ChatDeliveryOutcome =
  | { state: Exclude<ChatDeliveryState, 'undelivered_unknown'> }
  | { state: 'undelivered_unknown'; reason: UnknownReason | null };

/** One delivery record of a chat message. */
export type ChatDeliveryInfo = {
  /** The addressed window. */
  window_id: string;
} & ChatDeliveryOutcome;

/** One message of a chat room. */
export interface ChatMessage {
  /** This message's sequence number within its room. */
  seq: number;
  /** When this message was posted, in Unix epoch milliseconds. */
  at_ms: number;
  /** What kind of party posted it. */
  from_kind: ChatMessageFromKind;
  /** The posting window's id, `null` for every non-window kind. */
  from_window_id: string | null;
  /** The poster's display name. */
  from_name: string;
  /** The message body, verbatim. */
  body: string;
  /** Resolved window ids this message mentioned, plus `"user"` for `@user`. */
  mentions: string[];
  /** Whether this message was posted to every participant. */
  broadcast: boolean;
  /** One delivery record per addressed window. */
  deliveries: ChatDeliveryInfo[];
}

/** A room's current halt. */
export interface ChatHalt {
  /** Why the room halted. */
  reason: string;
  /** When the halt began, in Unix epoch milliseconds. */
  since_ms: number;
}

/** Result of posting one chat message. */
export interface ChatPosted {
  /** The room the message was posted to. */
  room_id: string;
  /** The message's sequence number within the room. */
  seq: number;
  /** The resolved window ids the message was addressed to. */
  targets: string[];
  /** Whether the post counts as addressed to (or from) the human user. */
  user: boolean;
  /** Raw mention text that did not resolve to exactly one participant. */
  unresolved: string[];
  /** Whether the room is halted as of this post. */
  halted: boolean;
}

/** One page of a room's message history. */
export interface ChatMessages {
  /** The room read from. */
  room_id: string;
  /** The messages found, ascending by `seq`. */
  messages: ChatMessage[];
  /** The cursor to pass as `after_seq` on the next read. */
  next_after_seq: number;
  /** The room's current halt, `null` when it is not halted. */
  halted: ChatHalt | null;
  /** Whether the room currently needs a human's attention. */
  needs_user: boolean;
  /** The room's current participants. */
  roster: ChatParticipant[];
}

/**
 * One row of the `list_chat_rooms` result (mirrors the Rust `ChatRoomInfo`).
 *
 * `halted` is optional only so `applyChatChangedToRooms` (`chat-store.ts`)
 * can build a minimal placeholder row without it; `narrowChatRooms` always
 * sets the key, folding the wire's absent-or-`null` into `null`
 * (`exactOptionalPropertyTypes`).
 */
export interface ChatRoomInfo {
  /** The room's identifier. */
  room_id: string;
  /** The room's display name. */
  name: string;
  /** How many of the daemon's current participants belong to this room. */
  participant_count: number;
  /** Whether the room currently needs a human's attention. */
  needs_user: boolean;
  /** The room's current halt, `null` when it is not halted. */
  halted?: ChatHalt | null;
  /** When this room was last posted to, `null` when nothing has been posted yet. */
  last_at_ms: number | null;
}

/** Payload of `session://chat-changed`. */
export interface ChatChanged {
  /** The room whose state changed. */
  room_id: string;
  /** The sequence number of the message that triggered the change. */
  seq: number;
  /** The room's `needs_user` state at the moment of the push. */
  needs_user: boolean;
}

/**
 * One entry of the `list_windows` result (mirrors the Rust `WindowInfo`).
 *
 * `vendor` is a data value, never a type variant: the WebView must stay
 * vendor-neutral.
 */
export interface WindowInfo {
  /** Opaque window id assigned by the daemon; valid for its process lifetime. */
  window_id: string;
  /** Display name of the window. */
  name: string;
  /** Vendor identifier, `null` when the window is not attributed to one. */
  vendor: string | null;
  /** Absolute working directory of the child process. */
  cwd: string;
  /**
   * Status of the child process at the time the list was taken, with the
   * reason it is `'unknown'`.
   */
  detected: DetectedStatusView;
  /** Exit code once the child has exited, `null` while it is still running. */
  exit_code: number | null;
  /**
   * The window's presentation layout, as the opaque JSON string the GUI last
   * reported. Omitted from the wire -- and from this object as an own property
   * -- when the core has none to report, rather than sent as `null`
   * (`exactOptionalPropertyTypes`).
   */
  layout_json?: string;
  /**
   * The room the window belongs to. Omitted from the wire -- and from this
   * object as an own property -- when the core has none to report
   * (`exactOptionalPropertyTypes`), the same discipline `layout_json` follows.
   */
  room?: RoomRef;
  /**
   * English reason text carried alongside the pair's reason when the window's
   * detection definition failed to load. It describes the definition rather
   * than the status, so it stays a field of its own instead of joining the
   * pair. Omitted from this object -- and from the wire -- when the core has
   * nothing to report (`exactOptionalPropertyTypes`).
   */
  detection_warning?: string;
  /**
   * The generation the reported pair was taken at, under the same discipline
   * {@link StatusPayload.status_rev} follows.
   */
  status_rev?: number;
  /**
   * Whether the window's child is currently connected to the daemon over
   * MCP. Omitted from the wire -- and from this object as an own property --
   * when the core has nothing to report (`exactOptionalPropertyTypes`), the
   * same discipline `layout_json` follows. Carried only by the window list,
   * never by {@link StatusPayload}: the status push has no route for it.
   */
  mcp_connected?: boolean;
  /**
   * Whether the window's CLI is expected to report its turns but none has
   * arrived while the screen showed one running. Same discipline as
   * `mcp_connected`.
   */
  status_reports_missing?: boolean;
}

const DAEMON_RECOVERY_STATUS_VALUES: ReadonlySet<string> = new Set<DaemonRecoveryStatus>([
  'idle',
  'retrying',
  'exhausted',
]);

const CLIENT_CONFIG_FAILURE_VALUES: ReadonlySet<string> = new Set<ClientConfigFailure>([
  'not_found',
  'read_failed',
  'invalid_toml',
  'too_large',
]);

const STARTUP_ENV_FAILURE_VALUES: ReadonlySet<string> = new Set<StartupEnvFailure>([
  'invalid_instance_id',
  'store_override_with_named_instance',
]);

/** Payload of `session://daemon-recovery`. */
export interface DaemonRecoveryPayload {
  /** The status recovery has moved to. */
  status: DaemonRecoveryStatus;
  /** The generation of this change, which orders it against every other. */
  rev: number;
}

/**
 * Return value of the `get_recovery_status` command: what the WebView reads
 * when it initializes, so a change published before it subscribed is not
 * lost. The settings failure travels by this route only.
 */
export interface RecoveryStatusReport {
  /** The status as of the last tick. */
  status: DaemonRecoveryStatus;
  /** The generation of that status. */
  rev: number;
  /** Why the client settings could not be loaded, `null` when they were. */
  config_failure: ClientConfigFailure | null;
  /**
   * Why start-up could not resolve which instance this process belongs to,
   * `null` when it could.
   */
  startup_env_failure: StartupEnvFailure | null;
}

/** What keeps the daemon from opening windows, as `get_daemon_health` names it. */
export type DaemonProblemKind = 'windows_unavailable';

/** One problem the daemon reports about itself. */
export interface DaemonProblem {
  kind: DaemonProblemKind;
  /** The daemon's own description of the cause, cleaned for display. */
  detail: string;
}

/** Return value of the `attach` command; the screen follows as frames. */
export interface AttachResponse {
  /** Whether the child process has already exited. */
  child_exited: boolean;
  /** Exit code of the child process, `null` while it is still running. */
  exit_code: number | null;
}

/**
 * Why an `open_window` attempt failed, in a shape a caller can branch on
 * without reading the English reason text.
 *
 * The daemon's own rejection, a refusal the core's Rust layer raised on its
 * own, and the wire-revision gate's refusal are
 * tagged objects. Everything else carries text alone: a refusal Tauri raised
 * before the command ran arrives as a bare string, and a failure of the
 * invoke bridge itself arrives as an `Error`.
 */
export type WindowOpenFailure =
  | {
      /** The daemon answered the request with an error frame. */
      kind: 'rejected';
      /** The `error.code` the daemon reported. */
      code: string;
      /** The daemon's human-readable English reason. */
      message: string;
      /** The argument the rejection points at, `null` when it names none. */
      rejectedArgument: string | null;
    }
  | {
      /** The core's Rust layer refused the request or could not carry it out. */
      kind: 'local';
      /** The human-readable English reason. */
      message: string;
    }
  | {
      /**
       * The connected daemon has not settled on a wire revision this request
       * needs, so the core's Rust layer refused to send it.
       */
      kind: 'unsupported';
      /** The wire tag of the request that was refused. */
      requestType: string;
    }
  | {
      /** No structure stands behind the failure: only text was reported. */
      kind: 'bare';
      /** The text the failure was reported with. */
      message: string;
    };

/** Raised when a value from the core does not match the boundary contract. */
export class BoundaryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'BoundaryError';
  }
}

/**
 * The `Error` message a {@link WindowOpenFailureError} carries: the reason
 * text for the three shapes that have one, or a diagnostic sentence built
 * from the wire tag for the gate's refusal, which carries no reason text of
 * its own. Diagnostic only -- not the rendered display line, which never
 * names the wire tag (`window-open-failure-model.ts` builds that instead).
 */
function windowOpenFailureMessage(failure: WindowOpenFailure): string {
  if (failure.kind === 'unsupported') {
    return `the connected daemon has not settled a wire revision for ${failure.requestType}`;
  }
  return failure.message;
}

/**
 * Raised when `open_window` fails, carrying the failure in a shape the
 * callers branch on. The `Error` message stays the reason text, so a caller
 * that only reads `message` sees what it saw before.
 */
export class WindowOpenFailureError extends Error {
  /** Why the attempt failed. */
  readonly failure: WindowOpenFailure;

  constructor(failure: WindowOpenFailure) {
    super(windowOpenFailureMessage(failure));
    this.name = 'WindowOpenFailureError';
    this.failure = failure;
  }
}

/**
 * Validates a `session://exit` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowExitPayload(value: unknown): ExitPayload {
  const context = SESSION_EVENT.exit;
  const record = requireRecord(value, context);
  return {
    exit_code: requireInteger(record, 'exit_code', context),
    window_id: requireString(record, 'window_id', context),
  };
}

/**
 * Validates one frame part as `next_frames` returns it: the daemon's frame
 * fields without the message's `type` tag. Fields serde leaves out when empty
 * take their empty value. What only the whole grid can judge (an attribute
 * index past `attrs`, wide indices, a row wider than the grid) is left to the
 * applier.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
export function narrowFrame(value: unknown): Frame {
  const context = 'next_frames.frame';
  const record = requireRecord(value, context);
  const seq = requireNonNegativeInteger(record, 'seq', context);
  const cols = requireNonNegativeInteger(record, 'cols', context);
  const rows = requireNonNegativeInteger(record, 'rows', context);
  if (!isValidSize({ cols, rows })) {
    throw new BoundaryError(`${context}: ${cols}x${rows} is not a window size`);
  }
  const reset = requireBoolean(record, 'reset', context);
  const last = requireBoolean(record, 'last', context);
  const attrs = requireArray(readOwnField(record, 'attrs'), `${context}.attrs`).map((entry, i) =>
    narrowCellAttr(entry, `${context}.attrs[${i}]`)
  );
  const lines = requireArray(readOwnField(record, 'lines'), `${context}.lines`).map((entry, i) =>
    narrowFrameLine(entry, rows, `${context}.lines[${i}]`)
  );
  const cursor = narrowCursor(readOwnField(record, 'cursor'), cols, rows, `${context}.cursor`);
  const view = narrowViewState(readOwnField(record, 'view'), `${context}.view`);
  const rawOverrides = readOwnField(record, 'overrides');
  const overrides =
    rawOverrides === undefined
      ? []
      : requireArray(rawOverrides, `${context}.overrides`).map((entry, i) =>
          narrowColorOverride(entry, `${context}.overrides[${i}]`)
        );
  return { seq, cols, rows, reset, last, attrs, lines, cursor, view, overrides };
}

/**
 * Validates the result of the `copy_window_selection` command: the selected
 * text, or `null` when nothing is selected.
 *
 * @throws {BoundaryError} when the result is neither a string nor `null`.
 */
export function narrowSelectionText(value: unknown): string | null {
  if (value === null || typeof value === 'string') {
    return value;
  }
  throw new BoundaryError('copy_window_selection: expected a string or null');
}

const CELL_FLAG_VALUES: ReadonlySet<string> = new Set<CellFlag>([
  'bold',
  'dim',
  'italic',
  'underline',
  'strikeout',
  'inverse',
  'hidden',
  'selected',
]);

const CURSOR_SHAPE_VALUES: ReadonlySet<string> = new Set<CursorShape>([
  'block',
  'underline',
  'bar',
  'hollow_block',
]);

const NAMED_SLOT_VALUES: ReadonlySet<string> = new Set(['foreground', 'background', 'cursor']);

const MAX_COLOR_COMPONENT = 255;

function narrowCellAttr(value: unknown, context: string): CellAttr {
  const record = requireRecord(value, context);
  const rawFlags = readOwnField(record, 'flags');
  const flags =
    rawFlags === undefined
      ? []
      : requireStringList(rawFlags, `${context}.flags`).map((flag) => {
          if (!CELL_FLAG_VALUES.has(flag)) {
            throw new BoundaryError(`${context}.flags: unrecognized flag "${flag}"`);
          }
          return flag as CellFlag;
        });
  return {
    fg: narrowOptionalCellColor(readOwnField(record, 'fg'), `${context}.fg`),
    bg: narrowOptionalCellColor(readOwnField(record, 'bg'), `${context}.bg`),
    flags,
  };
}

function narrowOptionalCellColor(value: unknown, context: string): CellColor {
  if (value === undefined || value === 'default') {
    return 'default';
  }
  const record = requireRecord(value, context);
  if (readOwnField(record, 'indexed') !== undefined) {
    return { indexed: requireColorComponent(record, 'indexed', context) };
  }
  return { rgb: narrowRgb(readOwnField(record, 'rgb'), `${context}.rgb`) };
}

function narrowRgb(value: unknown, context: string): Rgb {
  const parts = requireArray(value, context);
  if (parts.length !== 3) {
    throw new BoundaryError(`${context}: expected three components`);
  }
  const [red, green, blue] = parts.map((part) => {
    if (typeof part !== 'number' || !Number.isInteger(part) || part < 0) {
      throw new BoundaryError(`${context}: expected a color component`);
    }
    if (part > MAX_COLOR_COMPONENT) {
      throw new BoundaryError(`${context}: ${part} is past ${MAX_COLOR_COMPONENT}`);
    }
    return part;
  });
  return [red ?? 0, green ?? 0, blue ?? 0];
}

function requireColorComponent(record: UntrustedRecord, key: string, context: string): number {
  const value = requireNonNegativeInteger(record, key, context);
  if (value > MAX_COLOR_COMPONENT) {
    throw new BoundaryError(`${context}.${key}: ${value} is past ${MAX_COLOR_COMPONENT}`);
  }
  return value;
}

function narrowFrameLine(value: unknown, rows: number, context: string): FrameLine {
  const record = requireRecord(value, context);
  const row = requireNonNegativeInteger(record, 'row', context);
  if (row >= rows) {
    throw new BoundaryError(`${context}.row: ${row} is outside ${rows} rows`);
  }
  const rawWrapped = readOwnField(record, 'wrapped');
  const wrapped = rawWrapped === undefined ? false : requireBoolean(record, 'wrapped', context);
  const runs = requireArray(readOwnField(record, 'runs'), `${context}.runs`).map((entry, i) =>
    narrowFrameRun(entry, `${context}.runs[${i}]`)
  );
  return { row, wrapped, runs };
}

function narrowFrameRun(value: unknown, context: string): FrameRun {
  const record = requireRecord(value, context);
  const rawWide = readOwnField(record, 'wide');
  const wide =
    rawWide === undefined
      ? []
      : requireArray(rawWide, `${context}.wide`).map((index) => {
          if (typeof index !== 'number' || !Number.isInteger(index) || index < 0) {
            throw new BoundaryError(`${context}.wide: expected non-negative integers`);
          }
          return index;
        });
  return {
    attr: requireNonNegativeInteger(record, 'attr', context),
    cells: requireStringArray(record, 'cells', context),
    wide,
  };
}

function narrowCursor(value: unknown, cols: number, rows: number, context: string): CursorState {
  const record = requireRecord(value, context);
  const row = requireNonNegativeInteger(record, 'row', context);
  const col = requireNonNegativeInteger(record, 'col', context);
  if (row >= rows || col > cols) {
    throw new BoundaryError(`${context}: (${row}, ${col}) is outside ${cols}x${rows}`);
  }
  const shape = requireString(record, 'shape', context);
  if (!CURSOR_SHAPE_VALUES.has(shape)) {
    throw new BoundaryError(`${context}.shape: unrecognized shape "${shape}"`);
  }
  return {
    row,
    col,
    visible: requireBoolean(record, 'visible', context),
    shape: shape as CursorShape,
  };
}

function narrowViewState(value: unknown, context: string): ViewState {
  const record = requireRecord(value, context);
  return {
    display_offset: requireNonNegativeInteger(record, 'display_offset', context),
    history_rows: requireNonNegativeInteger(record, 'history_rows', context),
    mouse_reporting: requireBoolean(record, 'mouse_reporting', context),
    alt_screen: requireBoolean(record, 'alt_screen', context),
  };
}

function narrowColorOverride(value: unknown, context: string): ColorOverride {
  const record = requireRecord(value, context);
  return {
    slot: narrowPaletteSlot(readOwnField(record, 'slot'), `${context}.slot`),
    rgb: narrowRgb(readOwnField(record, 'rgb'), `${context}.rgb`),
  };
}

function narrowPaletteSlot(value: unknown, context: string): PaletteSlot {
  if (typeof value === 'string') {
    if (!NAMED_SLOT_VALUES.has(value)) {
      throw new BoundaryError(`${context}: unrecognized slot "${value}"`);
    }
    return value as PaletteSlot;
  }
  const record = requireRecord(value, context);
  return { indexed: requireColorComponent(record, 'indexed', context) };
}

/**
 * Validates a `session://disconnected` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowDisconnectedPayload(value: unknown): DisconnectedPayload {
  const context = SESSION_EVENT.disconnected;
  const record = requireRecord(value, context);
  return {
    reason: requireString(record, 'reason', context),
    window_id: requireString(record, 'window_id', context),
  };
}

/**
 * Validates a `session://superseded` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowSupersededPayload(value: unknown): SupersededPayload {
  const context = SESSION_EVENT.superseded;
  const record = requireRecord(value, context);
  return {
    reason: requireString(record, 'reason', context),
    window_id: requireString(record, 'window_id', context),
  };
}

/**
 * Validates a `session://daemon-spawn-warning` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowDaemonSpawnWarningPayload(value: unknown): DaemonSpawnWarningPayload {
  const context = SESSION_EVENT.daemonSpawnWarning;
  const record = requireRecord(value, context);
  return { message: requireString(record, 'message', context) };
}

/**
 * Validates a `session://status` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowStatusPayload(value: unknown): StatusPayload {
  const context = SESSION_EVENT.status;
  const record = requireRecord(value, context);
  const base: StatusPayload = {
    window_id: requireString(record, 'window_id', context),
    detected: readDetectedStatus(record, context, 'refuse'),
  };
  const status_rev = readOptionalGeneration(record, 'status_rev', context, 'refuse');
  return status_rev === null ? base : { ...base, status_rev };
}

/**
 * Validates a `session://daemon-recovery` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowDaemonRecoveryPayload(value: unknown): DaemonRecoveryPayload {
  const context = SESSION_EVENT.daemonRecovery;
  const record = requireRecord(value, context);
  return {
    status: requireDaemonRecoveryStatus(record, 'status', context),
    rev: requireNonNegativeInteger(record, 'rev', context),
  };
}

/**
 * Validates a `get_recovery_status` result.
 *
 * @throws {BoundaryError} when the result does not match the contract.
 */
export function narrowRecoveryStatusReport(value: unknown): RecoveryStatusReport {
  const context = 'get_recovery_status';
  const record = requireRecord(value, context);
  return {
    status: requireDaemonRecoveryStatus(record, 'status', context),
    rev: requireNonNegativeInteger(record, 'rev', context),
    config_failure: readOptionalClientConfigFailure(record, 'config_failure', context),
    startup_env_failure: readOptionalStartupEnvFailure(record, 'startup_env_failure', context),
  };
}

const DAEMON_PROBLEM_KINDS: ReadonlySet<string> = new Set<DaemonProblemKind>([
  'windows_unavailable',
]);

/**
 * Validates a `get_daemon_health` result. A problem of a kind this build does
 * not know is dropped rather than failing the reply: a newer daemon may name
 * more, and showing nothing for one is better than hiding the others.
 *
 * @throws {BoundaryError} when the result is not `{ problems: [...] }` or a
 * problem is not an object with a string `kind` and `detail`.
 */
export function narrowDaemonHealth(value: unknown): DaemonProblem[] {
  const context = 'get_daemon_health';
  const record = requireRecord(value, context);
  const problems: DaemonProblem[] = [];
  for (const raw of requireArray(readOwnField(record, 'problems'), `${context}.problems`)) {
    const entry = requireRecord(raw, `${context}.problems`);
    const kind = requireString(entry, 'kind', `${context}.problems`);
    const detail = requireString(entry, 'detail', `${context}.problems`);
    if (DAEMON_PROBLEM_KINDS.has(kind)) {
      problems.push({ kind: kind as DaemonProblemKind, detail: sanitizeForDisplay(detail) });
    }
  }
  return problems;
}

/**
 * Validates a `session://approval-requested` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowApprovalRequestedPayload(value: unknown): ApprovalRequestedPayload {
  const context = SESSION_EVENT.approvalRequested;
  const record = requireRecord(value, context);
  return { request: narrowApprovalRequest(readOwnField(record, 'request'), `${context}.request`) };
}

/**
 * Validates a `session://approval-withdrawn` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowApprovalWithdrawnPayload(value: unknown): ApprovalWithdrawnPayload {
  const context = SESSION_EVENT.approvalWithdrawn;
  const record = requireRecord(value, context);
  return {
    request_id: requireString(record, 'request_id', context),
    reason: normalizeWithdrawReason(requireString(record, 'reason', context)),
  };
}

/**
 * Validates the `decide_approval` command's own return value.
 *
 * @throws {BoundaryError} when the value is not a recognized outcome.
 */
export function narrowDecideApprovalOutcome(value: unknown): DecideApprovalOutcome {
  if (value === 'decided' || value === 'not_pending') {
    return value;
  }
  throw new BoundaryError(`decide_approval: unrecognized outcome (${JSON.stringify(value)})`);
}

/**
 * Validates the `close_window` command's own return value.
 *
 * @throws {BoundaryError} when the value is not a recognized outcome.
 */
export function narrowCloseOutcome(value: unknown): CloseOutcome {
  if (value === 'closed' || value === 'not_found' || value === 'unreachable') {
    return value;
  }
  throw new BoundaryError(`close_window: unrecognized outcome (${JSON.stringify(value)})`);
}

/**
 * Validates a `session://approval-registration-refused` payload.
 *
 * @throws {BoundaryError} when the payload is not an object.
 */
export function narrowApprovalRegistrationRefusedPayload(
  value: unknown
): ApprovalRegistrationRefusedPayload {
  requireRecord(value, SESSION_EVENT.approvalRegistrationRefused);
  return {};
}

/**
 * Validates a `session://approval-reset` payload, whose `requests` replace the
 * queue wholesale.
 *
 * @throws {BoundaryError} when the payload, or any request in it, does not
 * match the contract.
 */
export function narrowApprovalResetPayload(value: unknown): ApprovalResetPayload {
  const context = SESSION_EVENT.approvalReset;
  const record = requireRecord(value, context);
  return {
    requests: narrowApprovalRequestList(readOwnField(record, 'requests'), `${context}.requests`),
  };
}

/**
 * Validates a pending list, whichever route it arrived by: the reset event
 * and the `sync_approvals` command carry the same array, so they validate it
 * the same way.
 *
 * @throws {BoundaryError} when the value is not an array, or an entry does
 * not match the contract.
 */
export function narrowApprovalRequestList(value: unknown, context: string): ApprovalRequest[] {
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${context}: expected an array`);
  }
  return (value as unknown[]).map((entry) => narrowApprovalRequest(entry, context));
}

/**
 * Validates the `sync_approvals` command's own return value, a union
 * discriminated by `status`. Like {@link narrowDecideApprovalOutcome} this is
 * a reply to a request this side made, so an unrecognized status is a
 * contract violation rather than a fail-safe fallback.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
export function narrowApprovalSyncOutcome(value: unknown): ApprovalSyncOutcome {
  const context = 'sync_approvals';
  const record = requireRecord(value, context);
  const status = requireString(record, 'status', context);
  if (status === 'registered') {
    return {
      status,
      pending: narrowApprovalRequestList(readOwnField(record, 'pending'), `${context}.pending`),
    };
  }
  if (status === 'refused') {
    return { status };
  }
  throw new BoundaryError(`${context}: unrecognized status (${JSON.stringify(status)})`);
}

/**
 * Validates one pending approval request.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
export function narrowApprovalRequest(
  value: unknown,
  context = 'approval_request'
): ApprovalRequest {
  const record = requireRecord(value, context);
  const request_id = requireString(record, 'request_id', context);
  const tool = requireString(record, 'tool', context);
  const requester = narrowApprovalRequester(
    readOwnField(record, 'requester'),
    `${context}.requester`
  );
  const rawTarget = readOwnField(record, 'target');
  const target =
    rawTarget === null || rawTarget === undefined
      ? null
      : narrowApprovalTarget(rawTarget, `${context}.target`);
  const rawRoom = readOwnField(record, 'room');
  const room =
    rawRoom === null || rawRoom === undefined ? null : narrowRoomRef(rawRoom, `${context}.room`);
  const text = readOptionalString(record, 'text', context);
  const base: ApprovalRequest = { request_id, tool, requester, text };
  const withTarget = target === null ? base : { ...base, target };
  return room === null ? withTarget : { ...withTarget, room };
}

/**
 * Validates a room reference: the room a pending request is addressed to, and
 * the room a listed window belongs to, are read the same way.
 *
 * Both fields are refused when empty, here rather than in {@link requireString}:
 * an empty id matches no room and an empty name renders as a heading with
 * nothing in it, so neither is a value the view can act on.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
function narrowRoomRef(value: unknown, context: string): RoomRef {
  const record = requireRecord(value, context);
  const roomRef: RoomRef = {
    room_id: requireString(record, 'room_id', context),
    name: requireString(record, 'name', context),
  };
  for (const key of ['room_id', 'name'] as const) {
    if (roomRef[key] === '') {
      throw new BoundaryError(`${context}.${key}: expected a non-empty string`);
    }
  }
  return roomRef;
}

/**
 * Validates the requester of a pending request. `window_id` and `name` are
 * required for a `"window"` requester and forced to `null` for every other
 * kind, so a kind the daemon adds later cannot smuggle in an identity this
 * side would then display as a window's.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
function narrowApprovalRequester(value: unknown, context: string): ApprovalRequester {
  const record = requireRecord(value, context);
  const kind = normalizeRequesterKind(requireString(record, 'kind', context));
  if (kind !== 'window') {
    return { kind, window_id: null, name: null };
  }
  return {
    kind,
    window_id: requireString(record, 'window_id', context),
    name: requireString(record, 'name', context),
  };
}

/** Normalizes an unrecognized wire `kind` to `'unknown'` (fail-safe). */
function normalizeRequesterKind(kind: string): ApprovalRequesterKind {
  return kind === 'window' || kind === 'external' ? kind : 'unknown';
}

/** Normalizes an unrecognized wire `reason` to `'unknown'` (fail-safe). */
function normalizeWithdrawReason(reason: string): WithdrawReason {
  return reason === 'resolved' || reason === 'timeout' || reason === 'requester_disconnected'
    ? reason
    : 'unknown';
}

/**
 * Validates the target window of a pending request.
 *
 * @throws {BoundaryError} when the value does not match the contract.
 */
function narrowApprovalTarget(value: unknown, context: string): ApprovalTarget {
  const record = requireRecord(value, context);
  return {
    window_id: requireString(record, 'window_id', context),
    name: requireString(record, 'name', context),
    status: requireWindowStatus(record, 'status', context),
  };
}

/**
 * Validates the reply to `send_chat` and `broadcast_chat`, which answer with
 * the same shape.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowChatPosted(value: unknown): ChatPosted {
  const context = 'chat_posted';
  const record = requireRecord(value, context);
  return {
    room_id: requireString(record, 'room_id', context),
    seq: requireInteger(record, 'seq', context),
    targets: requireStringArray(record, 'targets', context),
    user: requireBoolean(record, 'user', context),
    unresolved: requireStringArray(record, 'unresolved', context),
    halted: requireBoolean(record, 'halted', context),
  };
}

/**
 * Validates the reply to `read_chat`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowChatMessages(value: unknown): ChatMessages {
  const context = 'chat_messages';
  const record = requireRecord(value, context);
  const messagesContext = `${context}.messages`;
  const rosterContext = `${context}.roster`;
  return {
    room_id: requireString(record, 'room_id', context),
    messages: requireArray(readOwnField(record, 'messages'), messagesContext).map((entry) =>
      narrowChatMessage(entry, messagesContext)
    ),
    next_after_seq: requireInteger(record, 'next_after_seq', context),
    halted: narrowChatHalt(readOwnField(record, 'halted'), `${context}.halted`),
    needs_user: requireBoolean(record, 'needs_user', context),
    roster: requireArray(readOwnField(record, 'roster'), rosterContext).map((entry) =>
      narrowChatParticipant(entry, rosterContext)
    ),
  };
}

/**
 * Validates the reply to `list_chat_rooms`: the envelope `{ rooms: [...] }`
 * the daemon's own `ChatRooms` message carries, not a bare array.
 *
 * @throws {BoundaryError} when the reply, or any row in it, does not match
 * the contract.
 */
export function narrowChatRooms(value: unknown): ChatRoomInfo[] {
  const context = 'list_chat_rooms';
  const record = requireRecord(value, context);
  const roomsContext = `${context}.rooms`;
  return requireArray(readOwnField(record, 'rooms'), roomsContext).map((entry) =>
    narrowChatRoomInfo(entry, roomsContext)
  );
}

/**
 * Validates one row of the `list_chat_rooms` result. `halted` always keeps
 * its key here, folding the wire's absent-or-`null` into `null`: only the
 * minimal placeholder row `applyChatChangedToRooms` builds (`chat-store.ts`)
 * omits the key.
 *
 * @throws {BoundaryError} when the row does not match the contract.
 */
function narrowChatRoomInfo(value: unknown, context: string): ChatRoomInfo {
  const record = requireRecord(value, context);
  return {
    room_id: requireString(record, 'room_id', context),
    name: requireString(record, 'name', context),
    participant_count: requireNonNegativeInteger(record, 'participant_count', context),
    needs_user: requireBoolean(record, 'needs_user', context),
    halted: narrowChatHalt(readOwnField(record, 'halted'), `${context}.halted`),
    last_at_ms: readOptionalInteger(record, 'last_at_ms', context),
  };
}

/**
 * Validates a `session://chat-changed` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowChatChanged(value: unknown): ChatChanged {
  const context = SESSION_EVENT.chatChanged;
  const record = requireRecord(value, context);
  return {
    room_id: requireString(record, 'room_id', context),
    seq: requireInteger(record, 'seq', context),
    needs_user: requireBoolean(record, 'needs_user', context),
  };
}

/**
 * Validates one message of a room's history. `from_kind` and a delivery's
 * `state` are closed sets, like `target.status`: an unrecognized value is
 * refused rather than normalized, so a kind added later never reaches the
 * view as one this build believes it can render.
 *
 * @throws {BoundaryError} when the message does not match the contract.
 */
function narrowChatMessage(value: unknown, context: string): ChatMessage {
  const record = requireRecord(value, context);
  const deliveriesContext = `${context}.deliveries`;
  return {
    seq: requireInteger(record, 'seq', context),
    at_ms: requireInteger(record, 'at_ms', context),
    from_kind: requireChatFromKind(record, 'from_kind', context),
    from_window_id: readOptionalString(record, 'from_window_id', context),
    from_name: requireString(record, 'from_name', context),
    body: requireString(record, 'body', context),
    mentions: requireStringArray(record, 'mentions', context),
    broadcast: requireBoolean(record, 'broadcast', context),
    deliveries: requireArray(readOwnField(record, 'deliveries'), deliveriesContext).map((entry) =>
      narrowChatDeliveryInfo(entry, deliveriesContext)
    ),
  };
}

/**
 * Validates one delivery record of a chat message.
 *
 * @throws {BoundaryError} when the record does not match the contract.
 */
function narrowChatDeliveryInfo(value: unknown, context: string): ChatDeliveryInfo {
  const record = requireRecord(value, context);
  return {
    window_id: requireString(record, 'window_id', context),
    ...readChatDeliveryOutcome(record, context),
  };
}

/**
 * Validates one roster entry.
 *
 * @throws {BoundaryError} when the entry does not match the contract.
 */
function narrowChatParticipant(value: unknown, context: string): ChatParticipant {
  const record = requireRecord(value, context);
  return {
    window_id: requireString(record, 'window_id', context),
    name: requireString(record, 'name', context),
  };
}

/**
 * Validates the halt a room reports, which is absent (`null`) while the room
 * is running.
 *
 * @throws {BoundaryError} when a present halt does not match the contract.
 */
function narrowChatHalt(value: unknown, context: string): ChatHalt | null {
  if (value === null || value === undefined) {
    return null;
  }
  const record = requireRecord(value, context);
  return {
    reason: requireString(record, 'reason', context),
    since_ms: requireInteger(record, 'since_ms', context),
  };
}

/**
 * Validates the result of the `attach` command.
 *
 * @throws {BoundaryError} when the result does not match the contract.
 */
export function narrowAttachResponse(value: unknown): AttachResponse {
  const context = 'attach';
  const record = requireRecord(value, context);
  return {
    child_exited: requireBoolean(record, 'child_exited', context),
    exit_code: readOptionalInteger(record, 'exit_code', context),
  };
}

/**
 * Validates one `WindowInfo` entry of the `list_windows` result.
 *
 * @throws {BoundaryError} when the entry does not match the contract.
 */
export function narrowWindowInfo(value: unknown, context = 'window_info'): WindowInfo {
  const record = requireRecord(value, context);
  const base: WindowInfo = {
    window_id: requireString(record, 'window_id', context),
    name: requireString(record, 'name', context),
    vendor: readOptionalString(record, 'vendor', context),
    cwd: requireString(record, 'cwd', context),
    detected: readDetectedStatus(record, context, 'drop'),
    exit_code: readOptionalInteger(record, 'exit_code', context),
  };
  const rawLayoutJson = readOwnField(record, 'layout_json');
  const info =
    rawLayoutJson === null || rawLayoutJson === undefined
      ? base
      : { ...base, layout_json: requireString(record, 'layout_json', context) };
  const rawRoom = readOwnField(record, 'room');
  const withRoom =
    rawRoom === null || rawRoom === undefined
      ? info
      : { ...info, room: narrowRoomRef(rawRoom, `${context}.room`) };
  const detection_warning = readOptionalString(record, 'detection_warning', context);
  const withWarning = detection_warning === null ? withRoom : { ...withRoom, detection_warning };
  const status_rev = readOptionalGeneration(record, 'status_rev', context, 'drop');
  const withRev = status_rev === null ? withWarning : { ...withWarning, status_rev };
  const rawMcpConnected = readOwnField(record, 'mcp_connected');
  const withMcp =
    rawMcpConnected === null || rawMcpConnected === undefined
      ? withRev
      : { ...withRev, mcp_connected: requireBoolean(record, 'mcp_connected', context) };
  const rawReportsMissing = readOwnField(record, 'status_reports_missing');
  return rawReportsMissing === null || rawReportsMissing === undefined
    ? withMcp
    : {
        ...withMcp,
        status_reports_missing: requireBoolean(record, 'status_reports_missing', context),
      };
}

/**
 * Validates the result of the `list_windows` command.
 *
 * @throws {BoundaryError} when the result is not a list of valid entries.
 */
export function narrowWindowInfoList(value: unknown): WindowInfo[] {
  const context = 'list_windows';
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${context}: expected an array`);
  }
  return value.map((entry) => narrowWindowInfo(entry, context));
}

/** Payload of `session://window-list`. */
export interface WindowListPayload {
  /** Every window the daemon reported on this poll. */
  windows: WindowInfo[];
  /**
   * The wire revision the connected daemon settled on, `null` while this
   * connection has not settled one yet.
   */
  protocol_revision: number | null;
}

/**
 * Validates a `session://window-list` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowWindowListPayload(value: unknown): WindowListPayload {
  const context = 'session://window-list';
  const record = requireRecord(value, context);
  return {
    windows: narrowWindowInfoList(readOwnField(record, 'windows')),
    protocol_revision: readOptionalInteger(record, 'protocol_revision', context),
  };
}

/** True if `value` claims to be one of the tagged failures the command returns. */
function namesFailureKind(value: unknown): value is UntrustedRecord {
  const record = asRecord(value);
  return record !== null && ownsField(record, 'kind');
}

/**
 * A value that claims no tag, as reason text. Objects are encoded rather than
 * stringified, which would flatten every one of them to the same word.
 */
function failureText(value: unknown): string {
  if (typeof value === 'object' && value !== null) {
    try {
      return JSON.stringify(value) ?? '';
    } catch {
      // A circular reference or a BigInt makes JSON.stringify throw. Losing
      // the text beats losing the failure.
      return '';
    }
  }
  return String(value);
}

/**
 * Validates the value an `open_window` rejection carries.
 *
 * Several kinds of value arrive here: the tagged shapes `WindowOpenFailure`
 * (`commands.rs`) serializes, a bare string when Tauri refused the call
 * before the command ran, and an `Error` when the invoke bridge itself threw.
 * Only a value that claims a tag is held to the contract; one that claims
 * none is read for its text, so wording that used to reach the user still
 * does. The tagged field names are the ones on that wire, not this layer's
 * camelCase.
 *
 * @throws {BoundaryError} when a value claims a tag but does not match it.
 */
export function narrowWindowOpenFailure(value: unknown): WindowOpenFailure {
  const context = 'open_window.failure';
  if (value instanceof Error) {
    return { kind: 'bare', message: value.message };
  }
  if (!namesFailureKind(value)) {
    return { kind: 'bare', message: failureText(value) };
  }
  const kind = requireString(value, 'kind', context);
  if (kind === 'rejected') {
    return {
      kind: 'rejected',
      code: requireString(value, 'code', context),
      message: requireString(value, 'message', context),
      rejectedArgument: readOptionalString(value, 'rejected_argument', context),
    };
  }
  if (kind === 'local') {
    return { kind: 'local', message: requireString(value, 'message', context) };
  }
  if (kind === 'unsupported') {
    return { kind: 'unsupported', requestType: requireString(value, 'request_type', context) };
  }
  throw new BoundaryError(`${context}.kind: unrecognized failure kind "${kind}"`);
}

// ---------------------------------------------------------------------------
// The untrusted-store choke point for the restore UI. Candidate rows come from
// the daemon's store rather than from running child processes, so a row can be
// malformed in ways `WindowInfo` never is: it is dropped only when an
// execution-value field (`position` / `command` / `cwd` / `name`) has the wrong
// type, while the remaining fields degrade to `null`.
// ---------------------------------------------------------------------------

/** Longest display field kept before truncation (code points, not bytes). */
export const MAX_DISPLAY_FIELD_CHARS = 4096;

/**
 * C0/C1 control characters and DEL, stripped entirely from display fields.
 * The Unicode `Cc` (Control) category covers all three ranges in one class,
 * avoiding a literal control-character range in the pattern (no-control-regex).
 * Narrower than `describeOperationFailure` (`render.ts`), and it strips where
 * that one folds a run to a single space.
 */
const DISPLAY_CONTROL_CHARS = /\p{Cc}/gu;

/**
 * Strips control characters and caps the result at
 * {@link MAX_DISPLAY_FIELD_CHARS} code points. Exported so every display of
 * daemon-supplied text passes through the same treatment.
 * @param value the text to display
 */
export function sanitizeForDisplay(value: string): string {
  const stripped = value.replace(DISPLAY_CONTROL_CHARS, '');
  const codePoints = Array.from(stripped);
  if (codePoints.length <= MAX_DISPLAY_FIELD_CHARS) {
    return stripped;
  }
  return `${codePoints.slice(0, MAX_DISPLAY_FIELD_CHARS).join('')}…`;
}

/**
 * Control characters other than the line feed, and the bidirectional controls
 * (U+061C, U+200E/F, U+202A-E, U+2066-9) that reorder what is drawn.
 */
const APPROVAL_BODY_HIDDEN_CHARS = /[\p{Cc}\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu;

/**
 * Prepares an approval request's body for display. The body is several lines
 * (command, directory, purpose), so line feeds are kept. Every other control
 * character, and every bidirectional control, is written out as `\u{202E}`
 * rather than removed or drawn: an approver must see everything that will run,
 * and a hidden character could change what the text appears to say. Capped at
 * {@link MAX_DISPLAY_FIELD_CHARS} code points before escaping, so an escape is
 * never cut in half.
 * @param value the body text to display
 */
export function formatApprovalBody(value: string): string {
  const codePoints = Array.from(value);
  const capped =
    codePoints.length <= MAX_DISPLAY_FIELD_CHARS
      ? value
      : `${codePoints.slice(0, MAX_DISPLAY_FIELD_CHARS).join('')}…`;
  return capped.replace(APPROVAL_BODY_HIDDEN_CHARS, (char) => {
    if (char === '\n') {
      return char;
    }
    const hex = (char.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
    return `\\u{${hex}}`;
  });
}

/** One restore candidate, execution-value fields plus their display\* derivatives. */
export interface RestoreEntry {
  position: number;
  command: string[];
  cwd: string;
  name: string;
  vendor: string | null;
  detection: string | null;
  layout: LayoutV1 | null;
  displayCommand: string;
  displayCwd: string;
  displayName: string;
  displayVendor: string | null;
  displayDetection: string | null;
  /** The launch profile this candidate was opened from, `null` for an argv launch. */
  profile: string | null;
  /** The `model` launch option recorded with the launch, `null` when none was. */
  model: string | null;
  /** The `effort` launch option recorded with the launch, `null` when none was. */
  effort: string | null;
}

/** `value` as a `string[]`, or `null` when it is not an array of strings. */
function readStringArrayOrNull(value: unknown): string[] | null {
  if (!Array.isArray(value)) {
    return null;
  }
  const result: string[] = [];
  for (const item of value) {
    if (typeof item !== 'string') {
      return null;
    }
    result.push(item);
  }
  return result;
}

/** `value` as a string, or `null` for any other type (never throws: lenient field). */
function readLenientOptionalString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

/** `record.layout_json` narrowed to a {@link LayoutV1}, `null` for any absent/invalid value. */
function readCandidateLayout(record: UntrustedRecord): LayoutV1 | null {
  const raw = readOwnField(record, 'layout_json');
  return typeof raw === 'string' ? parseLayoutJson(raw) : null;
}

/**
 * Narrows one raw candidate row. Returns `null` (drop the row) when
 * `position`, `command`, `cwd` or `name` has the wrong type; every other
 * field degrades to `null` instead of dropping the row.
 */
function tryNarrowRestoreCandidate(value: unknown): RestoreEntry | null {
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  const position = readOwnField(record, 'position');
  if (typeof position !== 'number' || !Number.isInteger(position)) {
    return null;
  }
  const command = readStringArrayOrNull(readOwnField(record, 'command'));
  if (command === null) {
    return null;
  }
  const cwd = readOwnField(record, 'cwd');
  if (typeof cwd !== 'string') {
    return null;
  }
  const name = readOwnField(record, 'name');
  if (typeof name !== 'string') {
    return null;
  }
  const vendor = readLenientOptionalString(readOwnField(record, 'vendor'));
  const detection = readLenientOptionalString(readOwnField(record, 'detection'));
  const profile = readLenientOptionalString(readOwnField(record, 'profile'));
  const model = readLenientOptionalString(readOwnField(record, 'model'));
  const effort = readLenientOptionalString(readOwnField(record, 'effort'));
  return {
    position,
    command,
    cwd,
    name,
    vendor,
    detection,
    layout: readCandidateLayout(record),
    displayCommand: sanitizeForDisplay(command.join(' ')),
    displayCwd: sanitizeForDisplay(cwd),
    displayName: sanitizeForDisplay(name),
    displayVendor: vendor === null ? null : sanitizeForDisplay(vendor),
    displayDetection: detection === null ? null : sanitizeForDisplay(detection),
    profile,
    model,
    effort,
  };
}

/**
 * Validates the `windows` field of a `list_restore_candidates` reply.
 *
 * @throws {BoundaryError} when the value itself is not an array (a malformed
 * row inside it is dropped instead of throwing -- see the module doc above).
 */
export function narrowRestoreCandidateList(value: unknown): RestoreEntry[] {
  const context = 'list_restore_candidates.windows';
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${context}: expected an array`);
  }
  const entries: RestoreEntry[] = [];
  for (const raw of value) {
    const entry = tryNarrowRestoreCandidate(raw);
    if (entry !== null) {
      entries.push(entry);
    }
  }
  return entries;
}

/**
 * Validates a command result that is a bare string (a window id, a name).
 *
 * @throws {BoundaryError} when the result is not a string.
 */
export function narrowStringResult(value: unknown, context: string): string {
  if (typeof value !== 'string') {
    throw new BoundaryError(`${context}: expected a string`);
  }
  return value;
}

/** Reports whether a PTY size is inside the range the core accepts. */
export function isValidSize(size: { cols: number; rows: number }): boolean {
  return withinDimension(size.cols, MIN_PTY_COLS) && withinDimension(size.rows, MIN_PTY_ROWS);
}

function withinDimension(value: number, min: number): boolean {
  return Number.isInteger(value) && value >= min && value <= MAX_PTY_DIMENSION;
}

/**
 * Marks {@link UntrustedRecord}. Ambient and never defined: nothing reads it
 * at runtime, it exists so that type carries no field a reader can reach.
 */
declare const UNTRUSTED_RECORD: unique symbol;

/**
 * A record that crossed the IPC edge, in a shape whose fields cannot be
 * reached directly: indexing, dotting and destructuring one are all type
 * errors, so a read has to go through {@link readOwnField} (or
 * {@link ownEntries}), which answer only with what the value owns itself.
 *
 * Declared as an `interface` rather than a type alias: only the former
 * withholds the implicit index signature that would let an assertion to
 * `Record<string, unknown>` open it. An assertion to a type that asks for no
 * index signature still converts, so this is a narrower guarantee than it
 * looks.
 *
 * Narrowing by `in` escapes all of this -- it types the dotted read that
 * follows and answers for the prototype chain. The eslint rule configured for
 * this module flags that operator in an expression when lint runs; what holds
 * at runtime is the seal the entry point installs, not the rule.
 */
export interface UntrustedRecord {
  readonly [UNTRUSTED_RECORD]: never;
}

/**
 * The one assertion that builds an {@link UntrustedRecord}, so the check that
 * a value is a plain object is never separated from the type that claims it:
 * arrays and `null` are excluded, as an array would index by number and
 * `null` owns nothing.
 * @param value the value received from the core
 * @returns the opaque view, or `null` when `value` is not a plain object
 */
export function asRecord(value: unknown): UntrustedRecord | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null;
  }
  return value as UntrustedRecord;
}

/**
 * The opaque record as an indexable one. Private, and the only route back:
 * every value it hands out passes an ownership test -- or `Object.entries`,
 * which answers with own keys alone -- before it reaches a caller.
 */
function fieldsOf(record: UntrustedRecord): Record<string, unknown> {
  return record as unknown as Record<string, unknown>;
}

/**
 * True when `record` carries `key` itself. A field the value does not own is
 * not a field the core sent, so a polluted `Object.prototype` must not be
 * able to stand in for one.
 */
function ownsField(record: UntrustedRecord, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(record, key);
}

/**
 * The value `record` carries under `key` itself, `undefined` when it owns
 * none. Every reader below goes through here, so no route reads a field off
 * the prototype chain.
 */
export function readOwnField(record: UntrustedRecord, key: string): unknown {
  return ownsField(record, key) ? fieldsOf(record)[key] : undefined;
}

/**
 * The record's own fields, for the few replies whose keys are data rather
 * than a fixed contract. `Object.entries` answers with own enumerable keys
 * only, so what it yields is what the core sent.
 * @param record the reply to iterate
 */
export function ownEntries(record: UntrustedRecord): [string, unknown][] {
  return Object.entries(fieldsOf(record));
}

/**
 * One reply as an {@link UntrustedRecord}.
 *
 * @throws {BoundaryError} when it is not one (an array included: the replies
 * this validates are objects, and an array would index by number).
 */
export function requireRecord(value: unknown, context: string): UntrustedRecord {
  const record = asRecord(value);
  if (record === null) {
    throw new BoundaryError(`${context}: expected an object`);
  }
  return record;
}

/**
 * Reads a required string field.
 *
 * @throws {BoundaryError} when the record owns no such field, or owns one
 * that is not a string.
 */
export function requireString(record: UntrustedRecord, key: string, context: string): string {
  const value = readOwnField(record, key);
  if (typeof value !== 'string') {
    throw new BoundaryError(`${context}.${key}: expected a string`);
  }
  return value;
}

/**
 * The boolean `record` owns under `key`.
 *
 * @throws {BoundaryError} when the record owns no such field, or owns one
 * that is not a boolean.
 */
export function requireBoolean(record: UntrustedRecord, key: string, context: string): boolean {
  const value = readOwnField(record, key);
  if (typeof value !== 'boolean') {
    throw new BoundaryError(`${context}.${key}: expected a boolean`);
  }
  return value;
}

function requireInteger(record: UntrustedRecord, key: string, context: string): number {
  const value = readOwnField(record, key);
  if (typeof value !== 'number' || !Number.isInteger(value)) {
    throw new BoundaryError(`${context}.${key}: expected an integer`);
  }
  return value;
}

/**
 * Reads a required integer field that must not be negative.
 *
 * @throws {BoundaryError} when the record owns no such field, or owns one
 * that is not a non-negative integer.
 */
export function requireNonNegativeInteger(
  record: UntrustedRecord,
  key: string,
  context: string
): number {
  const value = readOwnField(record, key);
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new BoundaryError(`${context}.${key}: expected a non-negative integer`);
  }
  return value;
}

function requireWindowStatus(record: UntrustedRecord, key: string, context: string): WindowStatus {
  const value = requireString(record, key, context);
  if (!WINDOW_STATUS_VALUES.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized status "${value}"`);
  }
  return value as WindowStatus;
}

function requireDaemonRecoveryStatus(
  record: UntrustedRecord,
  key: string,
  context: string
): DaemonRecoveryStatus {
  const value = requireString(record, key, context);
  if (!DAEMON_RECOVERY_STATUS_VALUES.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized status "${value}"`);
  }
  return value as DaemonRecoveryStatus;
}

/** Reads `config_failure`, `null` for a missing key or JSON `null`. */
function readOptionalClientConfigFailure(
  record: UntrustedRecord,
  key: string,
  context: string
): ClientConfigFailure | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value !== 'string' || !CLIENT_CONFIG_FAILURE_VALUES.has(value)) {
    throw new BoundaryError(
      `${context}.${key}: unrecognized config_failure (${JSON.stringify(value)})`
    );
  }
  return value as ClientConfigFailure;
}

/** Reads `startup_env_failure`, `null` for a missing key or JSON `null`. */
function readOptionalStartupEnvFailure(
  record: UntrustedRecord,
  key: string,
  context: string
): StartupEnvFailure | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value !== 'string' || !STARTUP_ENV_FAILURE_VALUES.has(value)) {
    throw new BoundaryError(
      `${context}.${key}: unrecognized startup_env_failure (${JSON.stringify(value)})`
    );
  }
  return value as StartupEnvFailure;
}

/**
 * One value as an array whose entries are still unvalidated.
 *
 * @throws {BoundaryError} when the value is not an array.
 */
export function requireArray(value: unknown, context: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new BoundaryError(`${context}: expected an array`);
  }
  return value as unknown[];
}

/**
 * Reads a required field that is an array of strings.
 *
 * @throws {BoundaryError} when the record owns no such field, or owns one
 * that is not an array of strings.
 */
export function requireStringArray(
  record: UntrustedRecord,
  key: string,
  context: string
): string[] {
  return requireStringList(readOwnField(record, key), `${context}.${key}`);
}

/**
 * One value as an array of strings.
 *
 * @throws {BoundaryError} when the value is not an array, or holds an entry
 * that is not a string.
 */
export function requireStringList(value: unknown, context: string): string[] {
  return requireArray(value, context).map((entry) => {
    if (typeof entry !== 'string') {
      throw new BoundaryError(`${context}: expected an array of strings`);
    }
    return entry;
  });
}

function requireChatFromKind(
  record: UntrustedRecord,
  key: string,
  context: string
): ChatMessageFromKind {
  const value = requireString(record, key, context);
  if (!CHAT_FROM_KIND_VALUES.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized kind "${value}"`);
  }
  return value as ChatMessageFromKind;
}

function requireChatDeliveryState(
  record: UntrustedRecord,
  key: string,
  context: string
): ChatDeliveryState {
  const value = requireString(record, key, context);
  if (!CHAT_DELIVERY_STATE_VALUES.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized delivery state "${value}"`);
  }
  return value as ChatDeliveryState;
}

/**
 * Reads a string field that the core serializes as `null` -- or omits -- when
 * it has no value. Both answers narrow to `null`.
 *
 * @throws {BoundaryError} when the field is present with any other type.
 */
export function readOptionalString(
  record: UntrustedRecord,
  key: string,
  context: string
): string | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  return requireString(record, key, context);
}

/**
 * What a route does with a value it decides it cannot keep: refuse the whole
 * record, or drop the field alone. The status-change route carries a single
 * record and refuses, since a record with a value it cannot reconcile is a
 * shape no reader should see; a route that carries a list drops the field
 * alone, so one malformed entry never costs the window or the message it
 * belongs to. Shared by an out-of-vocabulary `unknown_reason`
 * ({@link readArmReason}) and a negative `status_rev`
 * ({@link readOptionalGeneration}): both are values that are present but
 * unusable, split the same way by route.
 */
type FieldMismatchPolicy = 'refuse' | 'drop';

/**
 * Reads the `unknown_reason` the core serializes as `null` -- or omits -- when
 * it has none to report, for a record whose arm either may or may not hold
 * one. A non-string value is refused on every route: it is malformed rather
 * than merely unexpected.
 *
 * @throws {BoundaryError} when the value is not a string, or when the policy
 * is `'refuse'` and the reason does not belong on this arm.
 */
function readArmReason(
  record: UntrustedRecord,
  context: string,
  unknownArm: boolean,
  onMismatch: FieldMismatchPolicy
): UnknownReason | null {
  const value = readOwnField(record, 'unknown_reason');
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value !== 'string') {
    throw new BoundaryError(`${context}.unknown_reason: expected a string`);
  }
  if (unknownArm && UNKNOWN_REASON_VALUES.has(value)) {
    return value as UnknownReason;
  }
  if (onMismatch === 'refuse') {
    throw new BoundaryError(
      `${context}.unknown_reason: ${JSON.stringify(value)} does not belong on this arm`
    );
  }
  console.warn(
    `${context}.unknown_reason: dropped ${JSON.stringify(value)}, which does not belong on this arm`
  );
  return null;
}

/**
 * Assembles a window's status and its reason into the one value the view
 * holds. Every route that reports a status goes through here, so the pair is
 * built in one place.
 *
 * @throws {BoundaryError} when the status is not one this build knows, or
 * when {@link readArmReason} refuses the reason.
 */
function readDetectedStatus(
  record: UntrustedRecord,
  context: string,
  onMismatch: FieldMismatchPolicy
): DetectedStatusView {
  const status = requireWindowStatus(record, 'status', context);
  const reason = readArmReason(record, context, status === 'unknown', onMismatch);
  if (status === 'unknown') {
    return { status, reason };
  }
  return { status };
}

/**
 * Assembles a delivery's outcome and its reason into one value, the same way
 * {@link readDetectedStatus} does for a status.
 *
 * @throws {BoundaryError} when the delivery state is not one this build knows.
 */
function readChatDeliveryOutcome(record: UntrustedRecord, context: string): ChatDeliveryOutcome {
  const state = requireChatDeliveryState(record, 'state', context);
  const reason = readArmReason(record, context, state === 'undelivered_unknown', 'drop');
  if (state === 'undelivered_unknown') {
    return { state, reason };
  }
  return { state };
}

/**
 * Reads an integer field that the core serializes as `null` -- or omits --
 * when it has no value. Both answers narrow to `null`.
 *
 * @throws {BoundaryError} when the field is present with any other type.
 */
export function readOptionalInteger(
  record: UntrustedRecord,
  key: string,
  context: string
): number | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  return requireInteger(record, key, context);
}

/**
 * Reads an optional field that is a status generation rather than a plain
 * integer: a generation counts observations, so a negative value is not one
 * and must never reach {@link import('../stores/window-registry').withObservedPair}'s
 * high-water-mark comparison as though it were. Built on
 * {@link readOptionalInteger} rather than folded into it, since that
 * function's other callers (`exit_code`, `last_at_ms`) legitimately read
 * negative values.
 *
 * @throws {BoundaryError} when the value is negative and the policy is `'refuse'`.
 */
function readOptionalGeneration(
  record: UntrustedRecord,
  key: string,
  context: string,
  onNegative: FieldMismatchPolicy
): number | null {
  const value = readOptionalInteger(record, key, context);
  if (value === null || value >= 0) {
    return value;
  }
  if (onNegative === 'refuse') {
    throw new BoundaryError(`${context}.${key}: expected a non-negative generation`);
  }
  console.warn(`${context}.${key}: dropped a negative generation (${value})`);
  return null;
}
