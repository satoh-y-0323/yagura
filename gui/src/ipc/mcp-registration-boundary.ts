/**
 * The WebView-side boundary for MCP registration: the shapes
 * `list_mcp_registrations` / `run_mcp_registration` reply with and the two
 * pushes the core forwards (`session://mcp-registration-status`,
 * `session://mcp-registration-result`).
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test. The text the Integrations tab draws is prepared
 * for display here, so no caller can draw the daemon's text unprepared.
 */
import {
  foldExternalText,
  formatCommandLine,
  formatOutputBlock,
  formatPathForDisplay,
  sanitizeForDisplay,
  type DisplayBlock,
  type DisplayLine,
} from '../display/untrusted-text';
import {
  BoundaryError,
  readOptionalInteger,
  readOptionalString,
  readOwnField,
  requireArray,
  requireNonNegativeInteger,
  requireRecord,
  requireString,
  requireStringArray,
  SESSION_EVENT,
  type UntrustedRecord,
} from './session-boundary';

export { BoundaryError };

/** How a profile's registration currently compares to the CLI's own record. */
export type McpRegistrationState =
  'registered' | 'differs' | 'not_registered' | 'not_installed' | 'unknown';

const MCP_REGISTRATION_STATE_VALUES: ReadonlySet<string> = new Set<McpRegistrationState>([
  'registered',
  'differs',
  'not_registered',
  'not_installed',
  'unknown',
]);

/** What `run_mcp_registration` may be asked to do. A closed set: the request's own intent must never be guessed at. */
export type McpRegistrationAction = 'register' | 'update' | 'remove';

const MCP_REGISTRATION_ACTION_VALUES: ReadonlySet<string> = new Set<McpRegistrationAction>([
  'register',
  'update',
  'remove',
]);

/** A CLI invocation, sliced at the element the display should start from. */
export interface McpRegistrationCommand {
  argv: string[];
  programAt: number;
  /** `argv` from `programAt` on, for display. */
  displayCommand: DisplayLine;
  /** The whole `argv`, for display. */
  displayArgv: DisplayLine;
}

/**
 * One change `toml_set` would make (or made) to a host settings file. Every
 * part is written into the file, so each is shown with every hidden character
 * written out.
 */
export interface McpRegistrationTomlSet {
  file: DisplayLine;
  table: DisplayLine[];
  values: { key: DisplayLine; value: DisplayLine }[];
}

/** One entry registration writes into a host file, written out like {@link McpRegistrationTomlSet}. */
export interface McpRegistrationEntry {
  file: DisplayLine;
  name: DisplayLine;
  content: DisplayLine;
}

/** One row the daemon lists: a profile that declares an MCP registration table. */
export interface McpRegistrationRow {
  profile: string;
  displayName: DisplayLine;
  group: DisplayLine | null;
  /** `null` when the registration only writes files. */
  add: McpRegistrationCommand | null;
  remove: McpRegistrationCommand | null;
  tomlSet: McpRegistrationTomlSet | null;
  /** Empty when the profile writes none or the daemon predates the field. */
  entries: McpRegistrationEntry[];
  /**
   * The sentence the profile asks to show with the row; `null` (or absent in
   * a hand-built row) when it writes none or the daemon predates the field.
   */
  notice?: DisplayLine | null;
}

/** Reply to `list_mcp_registrations`. */
export interface McpRegistrationsListed {
  requestId: number;
  bridge: DisplayLine | null;
  rows: McpRegistrationRow[];
}

/**
 * How one profile's registration currently reads, `differsIn` empty unless `state` is `'differs'`.
 * `differsIn` keeps every part the daemon names, including ones this build has no wording for.
 */
export interface McpRegistrationStatusInfo {
  profile: string;
  state: McpRegistrationState;
  differsIn: string[];
  /** `differsIn` for display, in the same order. */
  displayDiffersIn: DisplayLine[];
  /** Why the state could not be read, folded onto one line. */
  reason: DisplayLine | null;
  add: McpRegistrationCommand | null;
  remove: McpRegistrationCommand | null;
  /** The pointer of the claim that makes Register and Update refuse; `null` when nothing refuses. */
  refusedByClaim: DisplayLine | null;
}

/** Payload of `session://mcp-registration-status`. */
export interface McpRegistrationStatusPayload {
  requestId: number;
  status: McpRegistrationStatusInfo;
}

/** The immediate reply to `run_mcp_registration`: the request was accepted. */
export interface McpRegistrationRequested {
  requestId: number;
  profile: string;
  action: McpRegistrationAction;
}

/** One step of an operation `run_mcp_registration` carried out. */
export interface McpRegistrationStep {
  argv: string[];
  programAt: number;
  /** `argv` from `programAt` on, for display. */
  displayCommand: DisplayLine;
  exitCode: number | null;
  output: DisplayBlock;
}

/** Payload of `session://mcp-registration-result`. */
export interface McpRegistrationResultPayload {
  requestId: number;
  profile: string;
  action: McpRegistrationAction;
  steps: McpRegistrationStep[];
  error: DisplayBlock | null;
  status: McpRegistrationStatusInfo;
}

/**
 * Normalizes an unrecognized wire `state` to `'unknown'` (fail-safe): a state
 * this build does not know is exactly as actionable as one the daemon
 * reported it could not read.
 */
function normalizeMcpRegistrationState(value: string): McpRegistrationState {
  return MCP_REGISTRATION_STATE_VALUES.has(value) ? (value as McpRegistrationState) : 'unknown';
}

/**
 * Validates a `run_mcp_registration` action. Unlike {@link normalizeMcpRegistrationState}
 * this is a reply to a request this side made itself, so an unrecognized
 * value is a contract violation rather than a fail-safe fallback (the same
 * discipline `narrowDecideApprovalOutcome` follows).
 *
 * @throws {BoundaryError} when the value is not a known action.
 */
function requireMcpRegistrationAction(
  record: UntrustedRecord,
  key: string,
  context: string
): McpRegistrationAction {
  const value = requireString(record, key, context);
  if (!MCP_REGISTRATION_ACTION_VALUES.has(value)) {
    throw new BoundaryError(`${context}.${key}: unrecognized action "${value}"`);
  }
  return value as McpRegistrationAction;
}

function narrowMcpRegistrationCommand(value: unknown, context: string): McpRegistrationCommand {
  const record = requireRecord(value, context);
  const argv = requireStringArray(record, 'argv', context);
  const programAt = requireNonNegativeInteger(record, 'program_at', context);
  return {
    argv,
    programAt,
    displayCommand: formatCommandLine(argv.slice(programAt)),
    displayArgv: formatCommandLine(argv),
  };
}

function readOptionalLine(
  record: UntrustedRecord,
  key: string,
  context: string,
  prepare: (value: string) => DisplayLine
): DisplayLine | null {
  const value = readOptionalString(record, key, context);
  return value === null ? null : prepare(value);
}

/** Reads a command field the core serializes as `null`, or leaves out, when the row carries none. */
function readOptionalCommand(
  record: UntrustedRecord,
  key: string,
  context: string
): McpRegistrationCommand | null {
  const value = readOwnField(record, key);
  if (value === null || value === undefined) {
    return null;
  }
  return narrowMcpRegistrationCommand(value, `${context}.${key}`);
}

function narrowMcpRegistrationTomlSet(value: unknown, context: string): McpRegistrationTomlSet {
  const record = requireRecord(value, context);
  const valuesContext = `${context}.values`;
  return {
    file: formatPathForDisplay(requireString(record, 'file', context)),
    table: requireStringArray(record, 'table', context).map(formatPathForDisplay),
    values: requireArray(readOwnField(record, 'values'), valuesContext).map((entry) => {
      const row = requireRecord(entry, valuesContext);
      return {
        key: formatPathForDisplay(requireString(row, 'key', valuesContext)),
        value: formatPathForDisplay(requireString(row, 'value', valuesContext)),
      };
    }),
  };
}

function readOptionalTomlSet(
  record: UntrustedRecord,
  context: string
): McpRegistrationTomlSet | null {
  const value = readOwnField(record, 'toml_set');
  if (value === null || value === undefined) {
    return null;
  }
  return narrowMcpRegistrationTomlSet(value, `${context}.toml_set`);
}

function readEntries(record: UntrustedRecord, context: string): McpRegistrationEntry[] {
  const value = readOwnField(record, 'entries');
  if (value === null || value === undefined) {
    return [];
  }
  const entriesContext = `${context}.entries`;
  return requireArray(value, entriesContext).map((entry) => {
    const row = requireRecord(entry, entriesContext);
    return {
      file: formatPathForDisplay(requireString(row, 'file', entriesContext)),
      name: formatPathForDisplay(requireString(row, 'name', entriesContext)),
      content: formatPathForDisplay(requireString(row, 'content', entriesContext)),
    };
  });
}

function narrowMcpRegistrationRow(value: unknown, context: string): McpRegistrationRow {
  const record = requireRecord(value, context);
  return {
    profile: requireString(record, 'profile', context),
    displayName: sanitizeForDisplay(requireString(record, 'display_name', context)),
    group: readOptionalLine(record, 'group', context, sanitizeForDisplay),
    add: readOptionalCommand(record, 'add', context),
    remove: readOptionalCommand(record, 'remove', context),
    tomlSet: readOptionalTomlSet(record, context),
    entries: readEntries(record, context),
    // A notice may run past one reason's length, so it is not folded and capped like one.
    notice: readOptionalLine(record, 'notice', context, sanitizeForDisplay),
  };
}

/**
 * Validates the reply to `list_mcp_registrations`.
 *
 * @throws {BoundaryError} when the reply, or any row in it, does not match
 * the contract.
 */
export function narrowMcpRegistrationsListed(value: unknown): McpRegistrationsListed {
  const context = 'list_mcp_registrations';
  const record = requireRecord(value, context);
  const rowsContext = `${context}.rows`;
  return {
    requestId: requireNonNegativeInteger(record, 'request_id', context),
    bridge: readOptionalLine(record, 'bridge', context, formatPathForDisplay),
    rows: requireArray(readOwnField(record, 'rows'), rowsContext).map((entry) =>
      narrowMcpRegistrationRow(entry, rowsContext)
    ),
  };
}

/**
 * Validates the reply to `run_mcp_registration`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowMcpRegistrationRequested(value: unknown): McpRegistrationRequested {
  const context = 'run_mcp_registration';
  const record = requireRecord(value, context);
  return {
    requestId: requireNonNegativeInteger(record, 'request_id', context),
    profile: requireString(record, 'profile', context),
    action: requireMcpRegistrationAction(record, 'action', context),
  };
}

function narrowMcpRegistrationStatusInfo(
  value: unknown,
  context: string
): McpRegistrationStatusInfo {
  const record = requireRecord(value, context);
  const differsIn = requireStringArray(record, 'differs_in', context);
  return {
    profile: requireString(record, 'profile', context),
    state: normalizeMcpRegistrationState(requireString(record, 'state', context)),
    differsIn,
    displayDiffersIn: differsIn.map(formatPathForDisplay),
    reason: readOptionalLine(record, 'reason', context, foldExternalText),
    add: readOptionalCommand(record, 'add', context),
    remove: readOptionalCommand(record, 'remove', context),
    refusedByClaim: readOptionalLine(record, 'refused_by_claim', context, formatPathForDisplay),
  };
}

/**
 * Validates a `session://mcp-registration-status` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowMcpRegistrationStatusPayload(value: unknown): McpRegistrationStatusPayload {
  const context = SESSION_EVENT.mcpRegistrationStatus;
  const record = requireRecord(value, context);
  return {
    requestId: requireNonNegativeInteger(record, 'request_id', context),
    status: narrowMcpRegistrationStatusInfo(readOwnField(record, 'status'), `${context}.status`),
  };
}

function narrowMcpRegistrationStep(value: unknown, context: string): McpRegistrationStep {
  const record = requireRecord(value, context);
  const argv = requireStringArray(record, 'argv', context);
  const programAt = requireNonNegativeInteger(record, 'program_at', context);
  return {
    argv,
    programAt,
    displayCommand: formatCommandLine(argv.slice(programAt)),
    exitCode: readOptionalInteger(record, 'exit_code', context),
    output: formatOutputBlock(requireString(record, 'output', context)),
  };
}

function readOptionalBlock(
  record: UntrustedRecord,
  key: string,
  context: string
): DisplayBlock | null {
  const value = readOptionalString(record, key, context);
  return value === null ? null : formatOutputBlock(value);
}

/**
 * Validates a `session://mcp-registration-result` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowMcpRegistrationResultPayload(value: unknown): McpRegistrationResultPayload {
  const context = SESSION_EVENT.mcpRegistrationResult;
  const record = requireRecord(value, context);
  const stepsContext = `${context}.steps`;
  return {
    requestId: requireNonNegativeInteger(record, 'request_id', context),
    profile: requireString(record, 'profile', context),
    action: requireMcpRegistrationAction(record, 'action', context),
    steps: requireArray(readOwnField(record, 'steps'), stepsContext).map((entry) =>
      narrowMcpRegistrationStep(entry, stepsContext)
    ),
    error: readOptionalBlock(record, 'error', context),
    status: narrowMcpRegistrationStatusInfo(readOwnField(record, 'status'), `${context}.status`),
  };
}
