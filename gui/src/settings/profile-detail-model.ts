/**
 * Pure decisions behind a profile's details in the Profiles tab: which
 * section each value belongs to, how it reads, and whether an edited command
 * may be saved. No IPC, no DOM.
 */
import type { ProfileDetail, ProfileDetailEntry } from '../ipc/profile-detail-boundary';

/** One line of the details. */
export interface DetailRow {
  /** What the line is about, as the profile file names it. */
  key: string;
  /** The value in use, as text. */
  text: string;
  /** The shipped value, only when the user's own value differs from it. */
  shippedText: string | null;
  /** Whether the user's own file sets the value. */
  changed: boolean;
}

/** The details split into the sections the tab shows. */
export interface DetailSections {
  /** What the window is opened with, besides the command. */
  launch: DetailRow[];
  /** Launch option aliases, edited in Plans → Models. */
  aliases: DetailRow[];
  /** Environment variables, shown hidden until asked for. */
  environment: DetailRow[];
  /** What yagura hands the CLI, one line per table. */
  provided: DetailRow[];
}

/** Top-level keys shown elsewhere in the row: the switch and the command editor. */
const SHOWN_ELSEWHERE = new Set(['schema', 'enabled', 'command']);

/** Tables that describe what yagura hands the CLI rather than how it starts. */
const PROVIDED_TABLES = new Set(['mcp', 'hooks', 'mcp_registration']);

/** A JSON value as one line of text. */
export function valueText(value: unknown): string {
  if (typeof value === 'string') {
    return value;
  }
  if (Array.isArray(value) && value.every((item) => typeof item === 'string')) {
    return value.join(' ');
  }
  return JSON.stringify(value) ?? '';
}

function row(key: string, entry: ProfileDetailEntry): DetailRow {
  return {
    key,
    text: valueText(entry.value),
    shippedText:
      entry.changed && entry.shipped !== undefined && entry.shipped !== null
        ? valueText(entry.shipped)
        : null,
    changed: entry.changed,
  };
}

/** The table a `provided` line stands for, or null for any other value. */
function providedTable(top: string, second: string | undefined): string | null {
  if (PROVIDED_TABLES.has(top)) {
    return top;
  }
  if (second === 'choices') {
    return `${top}.choices`;
  }
  return null;
}

/**
 * Sorts every value of `detail` into its section, keeping the profile's own
 * order. A `provided` table is one line, changed when any value under it is.
 */
export function detailSections(detail: ProfileDetail): DetailSections {
  const sections: DetailSections = { launch: [], aliases: [], environment: [], provided: [] };
  const provided = new Map<string, DetailRow>();
  for (const entry of detail.entries) {
    const [top, second, third] = entry.path;
    if (top === undefined || SHOWN_ELSEWHERE.has(top)) {
      continue;
    }
    const table = providedTable(top, second);
    if (table !== null) {
      const existing = provided.get(table);
      if (existing === undefined) {
        const line: DetailRow = { key: table, text: '', shippedText: null, changed: entry.changed };
        provided.set(table, line);
        sections.provided.push(line);
      } else {
        existing.changed = existing.changed || entry.changed;
      }
    } else if (top === 'env' && second !== undefined) {
      sections.environment.push(row(second, entry));
    } else if (second === 'aliases' && third !== undefined) {
      sections.aliases.push(row(`${top} ${third}`, entry));
    } else {
      sections.launch.push(row(entry.path.join('.'), entry));
    }
  }
  return sections;
}

/** Whether an edited argv differs from the saved one. */
export function isCommandDirty(edited: readonly string[], saved: readonly string[]): boolean {
  return edited.length !== saved.length || edited.some((item, index) => item !== saved[index]);
}

/**
 * Whether an edited argv can be saved: it changed, has at least one
 * argument, and no argument is empty (the daemon refuses an empty one).
 */
export function canSaveCommand(edited: readonly string[], saved: readonly string[]): boolean {
  return (
    isCommandDirty(edited, saved) && edited.length > 0 && edited.every((item) => item.length > 0)
  );
}

/** What the Profiles tab needs to show one opened profile's details. */
export interface ProfileDetailsState {
  /** Whether the daemon answers profile-detail requests at all. */
  isAvailable: boolean;
  /** The profile whose details are open, or null. */
  openId: string | null;
  /** The open profile's details, null while they are being read. */
  detail: ProfileDetail | null;
  /** Why the open profile's details could not be read. */
  failure: string | null;
  /** Whether a command save or reset for the open profile is in flight. */
  isSavingCommand: boolean;
  /** Why the last command save or reset failed. */
  commandFailure: string | null;
  /** The host operating system's name, as the command heading says it. */
  platformName: string;
}

/** What the Profiles tab reports back about details. */
export interface ProfileDetailActions {
  onToggle: (id: string) => void;
  onSaveCommand: (id: string, argv: string[]) => void;
  onResetCommand: (id: string) => void;
  onOpenModels: (id: string) => void;
}

/** The operating system name a keyboard platform stands for. */
export function platformName(platform: 'windows' | 'mac' | 'linux'): string {
  return { windows: 'Windows', mac: 'macOS', linux: 'Linux' }[platform];
}
