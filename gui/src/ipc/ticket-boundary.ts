/**
 * The WebView-side boundary for the ticket board: the shapes the ticket
 * commands reply with, and the `session://ticket-changed` payload.
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test. Ticket text is written by an AI and read by a
 * person, so every part that is shown is prepared here, through the makers in
 * `display/untrusted-text`; a part that is also matched or sent back keeps its
 * value beside the prepared one.
 */
import {
  formatPathForDisplay,
  formatTicketText,
  sanitizeForDisplay,
  type DisplayBlock,
  type DisplayLine,
} from '../display/untrusted-text';
import {
  BoundaryError,
  ownEntries,
  readOptionalInteger,
  readOptionalPrepared,
  readOptionalString,
  readOwnField,
  requireArray,
  requireBoolean,
  requireInteger,
  requireNonNegativeInteger,
  requireRecord,
  requireString,
  requireStringArray,
  SESSION_EVENT,
  type UntrustedRecord,
} from './session-boundary';

export { BoundaryError };

/** One workspace that has tickets. */
export interface TicketWorkspace {
  workspace_id: number;
  /** How the workspace is keyed (an opaque string, such as `git`). */
  kind: string;
  /** The last name of the workspace's path, as a room name is shown. */
  name: DisplayLine;
  /** The workspace's path, written out the way every path is shown. */
  path: DisplayLine;
}

/** The name of a ticket field: matched as it is, shown prepared. */
export interface TicketFieldName {
  name: string;
  displayName: DisplayLine;
}

/** The value a ticket holds for one of its fields. */
export interface TicketFieldValue extends TicketFieldName {
  value: DisplayBlock;
}

/** One state of the user's ticket settings, in board column order. */
export interface TicketStateInfo {
  name: string;
  displayName: DisplayLine;
  description: string;
  /** The fields a ticket in this state must have. */
  requires: TicketFieldName[];
}

/** One field a ticket may carry. */
export interface TicketFieldInfo {
  name: string;
  description: string;
}

/** The user's ticket settings, as written in `tickets.toml`. */
export interface TicketConfig {
  /** The rules the user set for the board, several lines. */
  guide: string;
  states: TicketStateInfo[];
  fields: TicketFieldInfo[];
}

/** One milestone of a workspace. */
export interface TicketMilestone {
  /** Matched against a ticket's milestone and sent back as the filter. */
  name: string;
  displayName: DisplayLine;
  /** Where the milestone sorts; equal positions sort by name. */
  position: number;
  title: DisplayLine;
  description: string | null;
}

/** One ticket of a list, in board order. */
export interface TicketSummary {
  /** The ticket's number in its workspace, shown as `T-<n>`. */
  number: number;
  title: DisplayLine;
  /** The ticket's state, which may name a state the settings no longer have. */
  state: string;
  displayState: DisplayLine;
  milestone: string | null;
  displayMilestone: DisplayLine | null;
  /** When the ticket last changed, in milliseconds since the Unix epoch. */
  updated_at: number;
  /** How many plans were made from the ticket. */
  plan_count: number;
  /** Values of the fields the ticket's current state requires; empty when it has none. */
  fields: TicketFieldValue[];
}

/** The reply of `list_tickets`. */
export interface TicketList {
  /** The workspace read, `null` when the daemon did not name it. */
  workspace_id: number | null;
  /** The user's ticket settings, `null` when they could not be read. */
  config: TicketConfig | null;
  /** Why the settings could not be read, `null` when they were. */
  config_error: DisplayLine | null;
  milestones: TicketMilestone[];
  tickets: TicketSummary[];
  /** Whether more tickets matched than were returned. */
  truncated: boolean;
}

/** One result recorded against a ticket. */
export interface TicketResult {
  seq: number;
  /** When the daemon recorded it, in milliseconds since the Unix epoch. */
  at: number;
  text: DisplayBlock;
}

/** A plan made from a ticket: opened by its id, which is shown when the plan list has no title. */
export interface TicketPlanRef {
  planId: string;
  displayPlanId: DisplayLine;
}

/** One ticket in full, as `get_ticket` replies with it. */
export interface Ticket {
  number: number;
  title: DisplayLine;
  body: DisplayBlock;
  state: string;
  displayState: DisplayLine;
  milestone: string | null;
  displayMilestone: DisplayLine | null;
  /** Field values, including fields the settings no longer define. */
  fields: TicketFieldValue[];
  /** Oldest first. */
  results: TicketResult[];
  /** The plans made from the ticket. */
  plans: TicketPlanRef[];
  created_at: number;
  updated_at: number;
}

/** Payload of `session://ticket-changed`. */
export interface TicketChanged {
  /** The workspace whose tickets or milestones changed. */
  workspace_id: number;
}

/**
 * Validates the reply to `list_ticket_workspaces`: the envelope
 * `{ workspaces: [...] }` the daemon's own `TicketWorkspaceList` carries.
 *
 * @throws {BoundaryError} when the reply, or any row in it, does not match
 * the contract.
 */
export function narrowTicketWorkspaceList(value: unknown): TicketWorkspace[] {
  const context = 'list_ticket_workspaces';
  const record = requireRecord(value, context);
  const rowsContext = `${context}.workspaces`;
  return requireArray(readOwnField(record, 'workspaces'), rowsContext).map((entry) => {
    const row = requireRecord(entry, rowsContext);
    const path = requireString(row, 'path', rowsContext);
    return {
      workspace_id: requireInteger(row, 'workspace_id', rowsContext),
      kind: requireString(row, 'kind', rowsContext),
      name: workspaceName(path),
      path: formatPathForDisplay(path),
    };
  });
}

/**
 * Validates the reply to `list_tickets`. A frame without settings carries
 * the reason in `config_error` instead, and still lists the tickets.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowTicketList(value: unknown): TicketList {
  const context = 'list_tickets';
  const record = requireRecord(value, context);
  const configValue = readOwnField(record, 'config');
  const configError = readOptionalPrepared(record, 'config_error', context, sanitizeForDisplay);
  const milestonesContext = `${context}.milestones`;
  const ticketsContext = `${context}.tickets`;
  return {
    workspace_id: readOptionalInteger(record, 'workspace_id', context),
    config:
      configValue === null || configValue === undefined
        ? null
        : narrowTicketConfig(configValue, `${context}.config`),
    config_error: configError,
    milestones: requireArray(readOwnField(record, 'milestones'), milestonesContext).map((entry) =>
      narrowMilestone(entry, milestonesContext)
    ),
    tickets: requireArray(readOwnField(record, 'tickets'), ticketsContext).map((entry) =>
      narrowTicketSummary(entry, ticketsContext)
    ),
    truncated: requireBoolean(record, 'truncated', context),
  };
}

/**
 * Validates the reply to `get_ticket`: the ticket itself.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowTicketDetail(value: unknown): Ticket {
  const context = 'ticket';
  const record = requireRecord(value, context);
  const resultsContext = `${context}.results`;
  return {
    number: requireNonNegativeInteger(record, 'number', context),
    title: sanitizeForDisplay(requireString(record, 'title', context)),
    body: formatTicketText(requireString(record, 'body', context)),
    ...stateAndMilestone(record, context),
    fields: narrowFieldValues(readOwnField(record, 'fields'), `${context}.fields`),
    results: requireArray(readOwnField(record, 'results'), resultsContext).map((entry) => {
      const row = requireRecord(entry, resultsContext);
      return {
        seq: requireNonNegativeInteger(row, 'seq', resultsContext),
        at: requireNonNegativeInteger(row, 'at', resultsContext),
        text: formatTicketText(requireString(row, 'text', resultsContext)),
      };
    }),
    plans: requireStringArray(record, 'plans', context).map((planId) => ({
      planId,
      displayPlanId: sanitizeForDisplay(planId),
    })),
    created_at: requireNonNegativeInteger(record, 'created_at', context),
    updated_at: requireNonNegativeInteger(record, 'updated_at', context),
  };
}

/**
 * Validates a `session://ticket-changed` payload.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowTicketChanged(value: unknown): TicketChanged {
  const context = SESSION_EVENT.ticketChanged;
  const record = requireRecord(value, context);
  return { workspace_id: requireInteger(record, 'workspace_id', context) };
}

/**
 * The name shown for a workspace: the last segment of its raw path, written
 * out as a path is. The segment is taken first, since a written-out character
 * starts with a backslash.
 */
function workspaceName(path: string): DisplayLine {
  const segments = path.split(/[\\/]/).filter((segment) => segment !== '');
  return formatPathForDisplay(segments[segments.length - 1] ?? path);
}

function fieldName(name: string): TicketFieldName {
  return { name, displayName: sanitizeForDisplay(name) };
}

function stateAndMilestone(
  record: UntrustedRecord,
  context: string
): Pick<TicketSummary, 'state' | 'displayState' | 'milestone' | 'displayMilestone'> {
  const state = requireString(record, 'state', context);
  const milestone = readOptionalString(record, 'milestone', context);
  return {
    state,
    displayState: sanitizeForDisplay(state),
    milestone,
    displayMilestone: milestone === null ? null : sanitizeForDisplay(milestone),
  };
}

function narrowTicketConfig(value: unknown, context: string): TicketConfig {
  const record = requireRecord(value, context);
  const statesContext = `${context}.states`;
  const fieldsContext = `${context}.fields`;
  return {
    guide: requireString(record, 'guide', context),
    states: requireArray(readOwnField(record, 'states'), statesContext).map((entry) => {
      const row = requireRecord(entry, statesContext);
      const name = requireString(row, 'name', statesContext);
      return {
        name,
        displayName: sanitizeForDisplay(name),
        description: requireString(row, 'description', statesContext),
        requires: requireStringArray(row, 'requires', statesContext).map(fieldName),
      };
    }),
    fields: requireArray(readOwnField(record, 'fields'), fieldsContext).map((entry) => {
      const row = requireRecord(entry, fieldsContext);
      return {
        name: requireString(row, 'name', fieldsContext),
        description: requireString(row, 'description', fieldsContext),
      };
    }),
  };
}

function narrowMilestone(value: unknown, context: string): TicketMilestone {
  const record = requireRecord(value, context);
  const name = requireString(record, 'name', context);
  return {
    name,
    displayName: sanitizeForDisplay(name),
    position: requireInteger(record, 'position', context),
    title: sanitizeForDisplay(requireString(record, 'title', context)),
    description: readOptionalString(record, 'description', context),
  };
}

function narrowTicketSummary(value: unknown, context: string): TicketSummary {
  const record = requireRecord(value, context);
  return {
    number: requireNonNegativeInteger(record, 'number', context),
    title: sanitizeForDisplay(requireString(record, 'title', context)),
    ...stateAndMilestone(record, context),
    updated_at: requireNonNegativeInteger(record, 'updated_at', context),
    plan_count: requireNonNegativeInteger(record, 'plan_count', context),
    fields: narrowFieldValues(readOwnField(record, 'fields'), `${context}.fields`),
  };
}

/** Reads the field-name-to-value map, whose keys are data rather than a fixed contract. */
function narrowFieldValues(value: unknown, context: string): TicketFieldValue[] {
  const record: UntrustedRecord = requireRecord(value, context);
  return ownEntries(record).map(([name, fieldValue]) => {
    if (typeof fieldValue !== 'string') {
      throw new BoundaryError(`${context}.${name}: expected a string`);
    }
    return { ...fieldName(name), value: formatTicketText(fieldValue) };
  });
}
