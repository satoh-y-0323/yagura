/**
 * The WebView-side boundary for the ticket board: the shapes the ticket
 * commands reply with, the `session://ticket-changed` payload, and the
 * display functions for ticket text.
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test. Ticket text is written by an AI and read by a
 * person, so it is drawn only through the display functions below.
 */
import {
  BoundaryError,
  formatMultilineText,
  ownEntries,
  readOptionalInteger,
  readOptionalString,
  readOwnField,
  requireArray,
  requireBoolean,
  requireInteger,
  requireNonNegativeInteger,
  requireRecord,
  requireString,
  requireStringArray,
  sanitizeForDisplay,
  SESSION_EVENT,
  type UntrustedRecord,
} from './session-boundary';

export { BoundaryError };

/**
 * Longest ticket body or result kept for display, in code points. The daemon
 * caps a body at 16 KiB of UTF-8, which is never more code points than this.
 */
export const MAX_TICKET_TEXT_CHARS = 16 * 1024;

/** One workspace that has tickets. */
export interface TicketWorkspace {
  workspace_id: number;
  /** How the workspace is keyed (an opaque string, such as `git`). */
  kind: string;
  /** The workspace's path, for display. */
  path: string;
}

/** One state of the user's ticket settings, in board column order. */
export interface TicketStateInfo {
  name: string;
  description: string;
  /** The fields a ticket in this state must have. */
  requires: string[];
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
  name: string;
  /** Where the milestone sorts; equal positions sort by name. */
  position: number;
  title: string;
  description: string | null;
}

/** One ticket of a list, in board order. */
export interface TicketSummary {
  /** The ticket's number in its workspace, shown as `T-<n>`. */
  number: number;
  title: string;
  /** The ticket's state, which may name a state the settings no longer have. */
  state: string;
  milestone: string | null;
  /** When the ticket last changed, in milliseconds since the Unix epoch. */
  updated_at: number;
  /** How many plans were made from the ticket. */
  plan_count: number;
  /** Values of the fields the ticket's current state requires; empty when it has none. */
  fields: Record<string, string>;
}

/** The reply of `list_tickets`. */
export interface TicketList {
  /** The workspace read, `null` when the daemon did not name it. */
  workspace_id: number | null;
  /** The user's ticket settings, `null` when they could not be read. */
  config: TicketConfig | null;
  /** Why the settings could not be read, `null` when they were. */
  config_error: string | null;
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
  text: string;
}

/** One ticket in full, as `get_ticket` replies with it. */
export interface Ticket {
  number: number;
  title: string;
  body: string;
  state: string;
  milestone: string | null;
  /** Field values by field name, including fields the settings no longer define. */
  fields: Record<string, string>;
  /** Oldest first. */
  results: TicketResult[];
  /** The plans made from the ticket. */
  plans: string[];
  created_at: number;
  updated_at: number;
}

/** Payload of `session://ticket-changed`. */
export interface TicketChanged {
  /** The workspace whose tickets or milestones changed. */
  workspace_id: number;
}

/** The one-line parts of a ticket summary, ready to draw as text. */
export interface TicketSummaryDisplay {
  /** `T-<n>`. */
  label: string;
  title: string;
  state: string;
  milestone: string | null;
  updated_at: number;
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
    return {
      workspace_id: requireInteger(row, 'workspace_id', rowsContext),
      kind: requireString(row, 'kind', rowsContext),
      path: requireString(row, 'path', rowsContext),
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
  const milestonesContext = `${context}.milestones`;
  const ticketsContext = `${context}.tickets`;
  return {
    workspace_id: readOptionalInteger(record, 'workspace_id', context),
    config:
      configValue === null || configValue === undefined
        ? null
        : narrowTicketConfig(configValue, `${context}.config`),
    config_error: readOptionalString(record, 'config_error', context),
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
    title: requireString(record, 'title', context),
    body: requireString(record, 'body', context),
    state: requireString(record, 'state', context),
    milestone: readOptionalString(record, 'milestone', context),
    fields: narrowFieldValues(readOwnField(record, 'fields'), `${context}.fields`),
    results: requireArray(readOwnField(record, 'results'), resultsContext).map((entry) => {
      const row = requireRecord(entry, resultsContext);
      return {
        seq: requireNonNegativeInteger(row, 'seq', resultsContext),
        at: requireNonNegativeInteger(row, 'at', resultsContext),
        text: requireString(row, 'text', resultsContext),
      };
    }),
    plans: requireStringArray(record, 'plans', context),
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

const TICKET_TEXT_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n', '\t']);

/**
 * Prepares a ticket's body, a result, or other text of several lines for
 * display. Escaped like an approval body, except that tabs are kept and the
 * cap is {@link MAX_TICKET_TEXT_CHARS}.
 * @param value the text to display
 */
export function formatTicketText(value: string): string {
  return formatMultilineText(value, MAX_TICKET_TEXT_CHARS, TICKET_TEXT_KEPT_CONTROLS);
}

/**
 * The one-line parts of a ticket summary, each through `sanitizeForDisplay`.
 * @param summary the summary to display
 */
export function displayTicketSummary(summary: TicketSummary): TicketSummaryDisplay {
  return {
    label: `T-${summary.number}`,
    title: sanitizeForDisplay(summary.title),
    state: sanitizeForDisplay(summary.state),
    milestone: summary.milestone === null ? null : sanitizeForDisplay(summary.milestone),
    updated_at: summary.updated_at,
  };
}

/**
 * A workspace row with its kind and path through `sanitizeForDisplay`.
 * @param workspace the workspace to display
 */
export function displayTicketWorkspace(workspace: TicketWorkspace): TicketWorkspace {
  return {
    workspace_id: workspace.workspace_id,
    kind: sanitizeForDisplay(workspace.kind),
    path: sanitizeForDisplay(workspace.path),
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
      return {
        name: requireString(row, 'name', statesContext),
        description: requireString(row, 'description', statesContext),
        requires: requireStringArray(row, 'requires', statesContext),
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
  return {
    name: requireString(record, 'name', context),
    position: requireInteger(record, 'position', context),
    title: requireString(record, 'title', context),
    description: readOptionalString(record, 'description', context),
  };
}

function narrowTicketSummary(value: unknown, context: string): TicketSummary {
  const record = requireRecord(value, context);
  return {
    number: requireNonNegativeInteger(record, 'number', context),
    title: requireString(record, 'title', context),
    state: requireString(record, 'state', context),
    milestone: readOptionalString(record, 'milestone', context),
    updated_at: requireNonNegativeInteger(record, 'updated_at', context),
    plan_count: requireNonNegativeInteger(record, 'plan_count', context),
    fields: narrowFieldValues(readOwnField(record, 'fields'), `${context}.fields`),
  };
}

/** Reads the field-name-to-value map, whose keys are data rather than a fixed contract. */
function narrowFieldValues(value: unknown, context: string): Record<string, string> {
  const record: UntrustedRecord = requireRecord(value, context);
  return Object.fromEntries(
    ownEntries(record).map(([name, fieldValue]) => {
      if (typeof fieldValue !== 'string') {
        throw new BoundaryError(`${context}.${name}: expected a string`);
      }
      return [name, fieldValue];
    })
  );
}
