/**
 * The WebView-side boundary for MCP registration: the shapes
 * `list_mcp_registrations` / `run_mcp_registration` reply with and the two
 * pushes the core forwards (`session://mcp-registration-status`,
 * `session://mcp-registration-result`).
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test.
 */
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
}

/** One change `toml_set` would make (or made) to a host settings file. */
export interface McpRegistrationTomlSet {
  file: string;
  table: string[];
  values: { key: string; value: string }[];
}

/** One entry registration writes into a host file. */
export interface McpRegistrationEntry {
  file: string;
  name: string;
  content: string;
}

/** One row the daemon lists: a profile that declares an MCP registration table. */
export interface McpRegistrationRow {
  profile: string;
  displayName: string;
  group: string | null;
  /** `null` when the registration only writes files. */
  add: McpRegistrationCommand | null;
  remove: McpRegistrationCommand | null;
  tomlSet: McpRegistrationTomlSet | null;
  /** Empty when the profile writes none or the daemon predates the field. */
  entries: McpRegistrationEntry[];
  /**
   * The sentence the profile asks to show with the row, verbatim; `null` (or
   * absent in a hand-built row) when it writes none or the daemon predates the field.
   */
  notice?: string | null;
}

/** Reply to `list_mcp_registrations`. */
export interface McpRegistrationsListed {
  requestId: number;
  bridge: string | null;
  rows: McpRegistrationRow[];
}

/** How one profile's registration currently reads, `differsIn` empty unless `state` is `'differs'`. */
export interface McpRegistrationStatusInfo {
  profile: string;
  state: McpRegistrationState;
  differsIn: string[];
  reason: string | null;
  add: McpRegistrationCommand | null;
  remove: McpRegistrationCommand | null;
  /** The pointer of the claim that makes Register and Update refuse; `null` when nothing refuses. */
  refusedByClaim: string | null;
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
  exitCode: number | null;
  output: string;
}

/** Payload of `session://mcp-registration-result`. */
export interface McpRegistrationResultPayload {
  requestId: number;
  profile: string;
  action: McpRegistrationAction;
  steps: McpRegistrationStep[];
  error: string | null;
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
  return {
    argv: requireStringArray(record, 'argv', context),
    programAt: requireNonNegativeInteger(record, 'program_at', context),
  };
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
    file: requireString(record, 'file', context),
    table: requireStringArray(record, 'table', context),
    values: requireArray(readOwnField(record, 'values'), valuesContext).map((entry) => {
      const row = requireRecord(entry, valuesContext);
      return {
        key: requireString(row, 'key', valuesContext),
        value: requireString(row, 'value', valuesContext),
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
      file: requireString(row, 'file', entriesContext),
      name: requireString(row, 'name', entriesContext),
      content: requireString(row, 'content', entriesContext),
    };
  });
}

function narrowMcpRegistrationRow(value: unknown, context: string): McpRegistrationRow {
  const record = requireRecord(value, context);
  return {
    profile: requireString(record, 'profile', context),
    displayName: requireString(record, 'display_name', context),
    group: readOptionalString(record, 'group', context),
    add: readOptionalCommand(record, 'add', context),
    remove: readOptionalCommand(record, 'remove', context),
    tomlSet: readOptionalTomlSet(record, context),
    entries: readEntries(record, context),
    notice: readOptionalString(record, 'notice', context),
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
    bridge: readOptionalString(record, 'bridge', context),
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
  return {
    profile: requireString(record, 'profile', context),
    state: normalizeMcpRegistrationState(requireString(record, 'state', context)),
    differsIn: requireStringArray(record, 'differs_in', context),
    reason: readOptionalString(record, 'reason', context),
    add: readOptionalCommand(record, 'add', context),
    remove: readOptionalCommand(record, 'remove', context),
    refusedByClaim: readOptionalString(record, 'refused_by_claim', context),
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
  return {
    argv: requireStringArray(record, 'argv', context),
    programAt: requireNonNegativeInteger(record, 'program_at', context),
    exitCode: readOptionalInteger(record, 'exit_code', context),
    output: requireString(record, 'output', context),
  };
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
    error: readOptionalString(record, 'error', context),
    status: narrowMcpRegistrationStatusInfo(readOwnField(record, 'status'), `${context}.status`),
  };
}
