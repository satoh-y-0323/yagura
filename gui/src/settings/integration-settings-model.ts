/**
 * The pure model behind the Settings panel's Integrations tab: how the rows
 * the daemon listed are grouped, which buttons each registration state
 * offers, the command each row shows, the one line explaining why the tab
 * cannot act, and the text an operation's result leaves behind. No IPC, no
 * DOM: the component renders these values and hands the switch back to its
 * caller.
 */
import { MCP_REGISTRATION_MIN_REVISION } from '../ipc/session-boundary';
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
  file: string;
  names: string[];
}

/** What one row of the Integrations tab shows. */
export interface IntegrationRowView {
  /** The profile's wire id. */
  profile: string;
  /** The profile's display name. */
  name: string;
  /** The registration-state text this row currently shows. */
  stateLabel: string;
  /** Which color the state line should render in. */
  stateTone: 'ok' | 'warn' | 'none';
  /** The command this row shows, from `programAt` onward. */
  command: string;
  /** The full command (from argv[0]), for the `title` attribute. */
  commandTitle: string;
  /** The primary button this row offers, `null` when it offers none. */
  primaryAction: 'register' | 'update' | null;
  /** Whether the primary button must stay unpressable. */
  isPrimaryDisabled: boolean;
  /** Whether this row offers a Remove button at all. */
  canRemove: boolean;
  /** Whether the Remove button must stay unpressable. */
  isRemoveDisabled: boolean;
  /** Why the state could not be read, `null` for every other state. */
  reason: string | null;
  /** The settings-file change this row's registration also makes, `null` for a row with none. */
  tomlSetNote: string | null;
  /** The entries this row's registration writes into its CLI's files, `null` for a row with none. */
  entriesNote: string | null;
  /** Every such entry written out in full, for a tooltip; `null` alongside a `null` note. */
  entriesTitle: string | null;
  /**
   * The files a registration that runs no command writes, with the entries in
   * each; `null` for a row that runs a command.
   */
  fileWrites: IntegrationFileWrite[] | null;
  /** Why Register / Update would be refused before it is pressed, `null` when it would not. */
  refusal: string | null;
}

/** One heading and the rows under it. */
export interface IntegrationGroup {
  heading: string;
  rows: IntegrationRowView[];
}

/**
 * Whether the connected daemon has settled on a wire revision that carries
 * MCP registration. An unreported revision is not unsupported: the daemon's
 * own refusal is the authority, so this is only ever a negative signal
 * (mirrors `isApprovalSettingsSupported`'s discipline).
 */
export function isIntegrationsSupported(protocolRevision: number | null): boolean {
  return protocolRevision === null || protocolRevision >= MCP_REGISTRATION_MIN_REVISION;
}

/**
 * The one line explaining why the Integrations tab cannot act right now,
 * `null` when there is nothing to say. An unsupported daemon is judged
 * before `listed`, since an old daemon's rows (if somehow present) are not
 * the live list this tab would otherwise show.
 */
export function integrationsNotice(
  listed: McpRegistrationsListed | null,
  protocolRevision: number | null
): string | null {
  if (!isIntegrationsSupported(protocolRevision)) {
    return settings.integrationsNeedNewerDaemon;
  }
  if (listed === null) {
    return null;
  }
  if (listed.bridge === null) {
    return settings.integrationsNoBridge;
  }
  return null;
}

/** Joins argv elements with spaces, quoting an element that contains whitespace. */
function joinArgv(argv: readonly string[]): string {
  return argv.map((part) => (/\s/.test(part) ? `"${part}"` : part)).join(' ');
}

/** The command a row (or a result step) shows, sliced at `programAt`. */
export function displayCommand(command: McpRegistrationCommand): string {
  return joinArgv(command.argv.slice(command.programAt));
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
  label: string;
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
        : { label: settings.integrationDiffersIn(status.differsIn[0] ?? ''), tone: 'warn' };
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
    command: command === null ? '' : displayCommand(command),
    commandTitle: command === null ? '' : joinArgv(command.argv),
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
        : row.entries.map((entry) => `${entry.name} = ${entry.content}`).join('\n'),
    fileWrites: writesFiles ? fileWrites(row.entries) : null,
    refusal: refused ? settings.integrationRefusedByClaim(claim) : null,
  };
}

/** The entries grouped by file, in the order the files first appear. */
function fileWrites(entries: readonly McpRegistrationEntry[]): IntegrationFileWrite[] {
  const namesByFile = new Map<string, string[]>();
  for (const entry of entries) {
    const names = namesByFile.get(entry.file) ?? [];
    names.push(entry.name);
    namesByFile.set(entry.file, names);
  }
  return [...namesByFile].map(([file, names]) => ({ file, names }));
}

/** One sentence per file the entries go into, in the order the files first appear. */
function entriesNote(entries: readonly McpRegistrationEntry[]): string | null {
  if (entries.length === 0) {
    return null;
  }
  return fileWrites(entries)
    .map(({ file, names }) => settings.integrationHookEntriesNote(file, names))
    .join(' ');
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
  // A Map keeps the order in which each group first appeared.
  const groupRows = new Map<string, IntegrationRowView[]>();

  for (const row of listed.rows) {
    const view = buildRowView(row, statuses.get(row.profile), running);
    if (row.group === null) {
      hostRows.push(view);
      continue;
    }
    let bucket = groupRows.get(row.group);
    if (bucket === undefined) {
      bucket = [];
      groupRows.set(row.group, bucket);
    }
    bucket.push(view);
  }

  const groups: IntegrationGroup[] = [];
  if (hostRows.length > 0) {
    groups.push({ heading: settings.integrationsHostGroup, rows: hostRows });
  }
  for (const [heading, rows] of groupRows) {
    groups.push({ heading, rows });
  }
  return groups;
}

/** The text an operation's result leaves in the row's result area, and whether it failed. */
export function resultLines(result: McpRegistrationResultPayload): {
  text: string;
  isFailure: boolean;
} {
  const lines: string[] = [];
  for (const step of result.steps) {
    if (step.argv.length > 0) {
      lines.push(`$ ${displayCommand(step)}`);
    }
    if (step.output !== '') {
      lines.push(step.output);
    }
  }
  if (result.error !== null) {
    lines.push(result.error);
  }
  return { text: lines.join('\n'), isFailure: result.error !== null };
}
