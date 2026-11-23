/**
 * The pure model behind the Settings panel's Integrations tab: how the rows
 * the daemon listed are grouped, which buttons each registration state
 * offers, the command each row shows, the one line explaining why the tab
 * cannot act, and the text an operation's result leaves behind. No IPC, no
 * DOM: the component renders these values and hands the switch back to its
 * caller.
 */
import {
  joinDisplayBlocks,
  sanitizeForDisplay,
  type DisplayBlock,
  type DisplayLine,
} from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import type {
  McpRegistrationCommand,
  McpRegistrationEntry,
  McpRegistrationResultPayload,
  McpRegistrationRow,
  McpRegistrationState,
  McpRegistrationsListed,
  McpRegistrationStatusInfo,
} from '../ipc/mcp-registration-boundary';
import { settings } from '../ui-messages';

/** The entries a command-less registration writes into one of its CLI's files. */
export interface IntegrationFileWrite {
  /** The file as the daemon names it, to tell apart files that read alike. */
  fileKey: string;
  file: DisplayLine;
  names: readonly DisplayLine[];
}

/** What one row of the Integrations tab shows. */
export interface IntegrationRowView {
  /** The profile's wire id. */
  profile: string;
  /** The profile's display name. */
  name: DisplayLine;
  /** The registration-state text this row currently shows. */
  stateLabel: UiText;
  /** Which color the state line should render in. */
  stateTone: 'ok' | 'warn' | 'none';
  /** The command this row shows, from `programAt` onward; `null` for a row that runs none. */
  command: DisplayLine | null;
  /** The full command (from argv[0]); `null` for a row that runs none. */
  commandTitle: DisplayLine | null;
  /** The primary button this row offers, `null` when it offers none. */
  primaryAction: 'register' | 'update' | null;
  /** Whether the primary button must stay unpressable. */
  isPrimaryDisabled: boolean;
  /** Whether this row offers a Remove button at all. */
  canRemove: boolean;
  /** Whether the Remove button must stay unpressable. */
  isRemoveDisabled: boolean;
  /** Why the state could not be read, `null` for every other state. */
  reason: DisplayLine | null;
  /** The settings-file change this row's registration also makes, `null` for a row with none. */
  tomlSetNote: UiText | null;
  /** The entries this row's registration writes into its CLI's files, `null` for a row with none. */
  entriesNote: UiText | null;
  /**
   * Every such entry written out in full, one line each; `null` alongside a
   * `null` note. Lines, so that no one-line place can run them together.
   */
  entriesTitle: readonly UiText[] | null;
  /**
   * The files a registration that runs no command writes, with the entries in
   * each; `null` for a row that runs a command.
   */
  fileWrites: IntegrationFileWrite[] | null;
  /** Why Register / Update would be refused before it is pressed, `null` when it would not. */
  refusal: UiText | null;
  /** The profile's own sentence for this row, shown in every state; `null` when it writes none. */
  notice: DisplayLine | null;
}

/** One heading and the rows under it. */
export interface IntegrationGroup {
  /** Tells the groups apart whatever their headings read; a named group may read like the host's. */
  key: string;
  heading: UiText | DisplayLine;
  rows: IntegrationRowView[];
}

// The two key spaces cannot meet: only the host key lacks the prefix.
const HOST_GROUP_KEY = 'host';
const NAMED_GROUP_KEY_PREFIX = 'group:';

/**
 * The one line explaining why the Integrations tab cannot act right now,
 * `null` when there is nothing to say.
 */
export function integrationsNotice(listed: McpRegistrationsListed | null): UiText | null {
  if (listed === null) {
    return null;
  }
  if (listed.bridge === null) {
    return settings.integrationsNoBridge;
  }
  return null;
}

/**
 * The name a failure to run `profile`'s registration is told by: the listed
 * row's display name, or the profile id itself when no listed row has it.
 */
export function integrationRunName(
  listed: McpRegistrationsListed | null,
  profile: string
): DisplayLine {
  return (
    listed?.rows.find((row) => row.profile === profile)?.displayName ?? sanitizeForDisplay(profile)
  );
}

/**
 * The command a row shows: the settled command a status push carried, if
 * any, else the one the listing itself carried. `remove` for a registered
 * row, `add` for every other state; `null` for a registration that runs no
 * command.
 */
function commandFor(
  row: McpRegistrationRow,
  status: McpRegistrationStatusInfo | undefined
): McpRegistrationCommand | null {
  if (status?.state === 'registered') {
    return status.remove ?? row.remove;
  }
  return status?.add ?? row.add;
}

/** The primary button a state offers, `null` for a row with nothing to register or update. */
function primaryActionFor(state: McpRegistrationState | null): 'register' | 'update' | null {
  switch (state) {
    case 'differs':
      return 'update';
    case 'not_registered':
    case 'not_installed':
    case 'unknown':
      return 'register';
    default:
      return null;
  }
}

/** Whether a state offers a Remove button at all. */
function canRemoveFor(state: McpRegistrationState | null): boolean {
  return state === 'registered' || state === 'differs' || state === 'unknown';
}

/** The state line's text and color for one row. */
function stateView(status: McpRegistrationStatusInfo | undefined): {
  label: UiText;
  tone: 'ok' | 'warn' | 'none';
} {
  if (status === undefined) {
    return { label: settings.integrationChecking, tone: 'none' };
  }
  switch (status.state) {
    case 'registered':
      return { label: settings.integrationRegistered, tone: 'ok' };
    case 'not_registered':
      return { label: settings.integrationNotRegistered, tone: 'none' };
    case 'not_installed':
      return { label: settings.integrationNotInstalled, tone: 'none' };
    case 'unknown':
      return { label: settings.integrationUnknown, tone: 'warn' };
    case 'differs':
      return status.differsIn.includes('command')
        ? { label: settings.integrationDiffers, tone: 'warn' }
        : {
            label: settings.integrationDiffersIn(status.displayDiffersIn[0] ?? null),
            tone: 'warn',
          };
  }
}

/** Builds the one row `integrationGroups` shows for one listed row. */
function buildRowView(
  row: McpRegistrationRow,
  status: McpRegistrationStatusInfo | undefined,
  running: string | null
): IntegrationRowView {
  const state = status?.state ?? null;
  const { label, tone } = stateView(status);
  const operationRunning = running !== null;
  const command = commandFor(row, status);
  const writesFiles = command === null && row.entries.length > 0;
  const primaryAction = primaryActionFor(state);
  const claim = status?.refusedByClaim ?? null;
  const refused = primaryAction !== null && claim !== null;
  return {
    profile: row.profile,
    name: row.displayName,
    stateLabel: label,
    stateTone: tone,
    command: command?.displayCommand ?? null,
    commandTitle: command?.displayArgv ?? null,
    primaryAction,
    isPrimaryDisabled: state === 'not_installed' || operationRunning || refused,
    canRemove: canRemoveFor(state),
    isRemoveDisabled: operationRunning,
    reason: status?.reason ?? null,
    tomlSetNote:
      row.tomlSet === null
        ? null
        : settings.integrationTomlSetNote(row.tomlSet.file, row.tomlSet.table, row.tomlSet.values),
    entriesNote: writesFiles ? null : entriesNote(row.entries),
    entriesTitle:
      row.entries.length === 0
        ? null
        : row.entries.map((entry) => settings.integrationEntryLine(entry.name, entry.content)),
    fileWrites: writesFiles ? fileWrites(row.entries) : null,
    refusal: refused ? settings.integrationRefusedByClaim(claim) : null,
    notice: row.notice ?? null,
  };
}

/** The entries grouped by file, in the order the files first appear. */
function fileWrites(entries: readonly McpRegistrationEntry[]): IntegrationFileWrite[] {
  const byFile = new Map<string, [file: DisplayLine, names: DisplayLine[]]>();
  for (const entry of entries) {
    const found = byFile.get(entry.fileKey);
    if (found === undefined) {
      byFile.set(entry.fileKey, [entry.file, [entry.name]]);
    } else {
      found[1].push(entry.name);
    }
  }
  return [...byFile].map(([fileKey, [file, names]]) => ({ fileKey, file, names }));
}

/** One sentence per file the entries go into, in the order the files first appear. */
function entriesNote(entries: readonly McpRegistrationEntry[]): UiText | null {
  if (entries.length === 0) {
    return null;
  }
  return settings.integrationHookEntriesNotes(
    fileWrites(entries).map(({ file, names }) => settings.integrationHookEntriesNote(file, names))
  );
}

/**
 * Groups the daemon's listed rows: every row without a `group` first, under
 * the host heading, then each named group in the order its first row
 * appeared. A group with no ungrouped row carries no host heading at all.
 */
export function integrationGroups(
  listed: McpRegistrationsListed,
  statuses: ReadonlyMap<string, McpRegistrationStatusInfo>,
  running: string | null
): IntegrationGroup[] {
  const hostRows: IntegrationRowView[] = [];
  // Keyed by the raw name, since two names may read alike once prepared; a Map keeps
  // the order in which each group first appeared.
  const namedGroups = new Map<string, { heading: DisplayLine; rows: IntegrationRowView[] }>();

  for (const row of listed.rows) {
    const view = buildRowView(row, statuses.get(row.profile), running);
    if (row.group === null) {
      hostRows.push(view);
      continue;
    }
    const named = namedGroups.get(row.group.key);
    if (named === undefined) {
      namedGroups.set(row.group.key, { heading: row.group.heading, rows: [view] });
    } else {
      named.rows.push(view);
    }
  }

  const groups: IntegrationGroup[] = [];
  if (hostRows.length > 0) {
    groups.push({ key: HOST_GROUP_KEY, heading: settings.integrationsHostGroup, rows: hostRows });
  }
  for (const [key, { heading, rows }] of namedGroups) {
    groups.push({ key: `${NAMED_GROUP_KEY_PREFIX}${key}`, heading, rows });
  }
  return groups;
}

/**
 * The text an operation's result leaves in the row's result area, and whether
 * it failed.
 */
export function resultLines(result: McpRegistrationResultPayload): {
  text: DisplayBlock;
  isFailure: boolean;
} {
  const lines: (UiText | DisplayBlock)[] = [];
  for (const step of result.steps) {
    if (step.displayCommand !== null) {
      lines.push(settings.integrationResultCommand(step.displayCommand));
    }
    if (step.output !== '') {
      lines.push(step.output);
    }
  }
  if (result.error !== null) {
    lines.push(result.error);
  }
  return { text: joinDisplayBlocks(lines), isFailure: result.error !== null };
}
