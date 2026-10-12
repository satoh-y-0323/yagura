/**
 * Pure logic of the Models tab: which profiles offer an alias editor, the
 * rows it shows, whether a row differs from the shipped value or from what
 * was saved, which model-list push to take, and the line shown when no list
 * could be read.
 */
import type { LaunchOptionAlias, ModelChoice, ModelChoiceSource } from '../ipc/model-boundary';
import type { ProfileDetail } from '../ipc/profile-detail-boundary';
import type { ProfileEntry } from '../ipc/session-client';
import { plans } from '../ui-messages';

/** One alias as the editor shows it. */
export interface AliasRow {
  alias: string;
  /** The value shown, including an edit not yet saved. */
  value: string;
  /** The value the shipped profile gives the alias, `null` when it gives none. */
  shippedValue: string | null;
  /** Whether `value` differs from `shippedValue`. */
  edited: boolean;
  /**
   * Whether a fetched model list exists and does not contain `value`. Always
   * `false` while no list is known, so an unknown list never marks a value.
   */
  valueMissingFromChoices: boolean;
}

/** Why no model list can be shown. */
export type ChoicesFailure =
  /** A reason given by the daemon or the core, shown as it was given. */
  | { kind: 'reported'; reason: string }
  /** The requested list did not arrive within {@link CHOICES_FETCH_TIMEOUT_MS}. */
  | { kind: 'overdue' };

/** Where one profile's model list stands. */
export type ModelChoicesEntry =
  /** Requested and not answered yet. */
  | { kind: 'fetching'; startedAtMs: number }
  /** The daemon's answer, as it was pushed. */
  | {
      kind: 'answered';
      source: ModelChoiceSource | null;
      choices: ModelChoice[];
      error: string | null;
      fetchedAtMs: number;
    }
  /** The request was refused or the answer did not arrive in time. */
  | { kind: 'failed'; failure: ChoicesFailure };

/**
 * How long a requested model list may take before the tab stops waiting for
 * it. Longer than the daemon's own limit on the listing command (20 seconds),
 * so a push carrying the daemon's time-out reason normally arrives first.
 */
export const CHOICES_FETCH_TIMEOUT_MS = 25_000;

/** The profiles whose `model` launch option declares at least one alias. */
export function editableProfiles(profiles: readonly ProfileEntry[]): ProfileEntry[] {
  return profiles.filter((profile) => profile.model !== null && profile.model.aliases.length > 0);
}

/**
 * Whether `value` differs from the shipped value. A profile the shipped set
 * does not cover has nothing to differ from.
 */
export function isEdited(value: string, shippedValue: string | null): boolean {
  return shippedValue !== null && value !== shippedValue;
}

/**
 * One row per alias of `current`, in its order, compared against the shipped
 * aliases and the fetched model list.
 */
export function buildAliasRows(
  current: readonly LaunchOptionAlias[],
  shipped: readonly LaunchOptionAlias[],
  choices: readonly ModelChoice[]
): AliasRow[] {
  const shippedValues = new Map(shipped.map((entry) => [entry.alias, entry.value]));
  return current.map((entry) =>
    aliasRow(entry.alias, entry.value, shippedValues.get(entry.alias) ?? null, choices)
  );
}

/**
 * Puts the row for `alias` back to its shipped value. Only the rows change:
 * nothing is saved until the aliases are stored. `choices` is the fetched
 * model list, if any, to judge whether the restored value is in it.
 */
export function resetToShipped(
  rows: readonly AliasRow[],
  alias: string,
  choices: readonly ModelChoice[] = []
): AliasRow[] {
  return rows.map((row) =>
    row.alias === alias && row.shippedValue !== null
      ? aliasRow(row.alias, row.shippedValue, row.shippedValue, choices)
      : row
  );
}

/** Whether any row's value differs from the saved value of its alias. */
export function isDirty(rows: readonly AliasRow[], saved: readonly LaunchOptionAlias[]): boolean {
  if (rows.length !== saved.length) {
    return true;
  }
  const savedValues = new Map(saved.map((entry) => [entry.alias, entry.value]));
  return rows.some((row) => savedValues.get(row.alias) !== row.value);
}

/** The aliases to store, in row order. */
export function aliasesToSave(rows: readonly AliasRow[]): LaunchOptionAlias[] {
  return rows.map((row) => ({ alias: row.alias, value: row.value }));
}

/**
 * A request id different from `lastIssued`. Wraps to zero before leaving the
 * range of integers a number holds exactly, since the core reads it as one.
 */
export function nextChoicesRequestId(lastIssued: number): number {
  return Number.isSafeInteger(lastIssued) && lastIssued >= 0 && lastIssued < Number.MAX_SAFE_INTEGER
    ? lastIssued + 1
    : 0;
}

/**
 * Whether a model-list push answers the latest request issued for its
 * profile. The id is recorded when the request is issued, so a push that
 * arrives before the request's acknowledgement is still taken.
 */
export function acceptsChoicesPush(
  latestRequestIds: ReadonlyMap<string, number>,
  payload: { profile: string; request_id: number }
): boolean {
  return latestRequestIds.get(payload.profile) === payload.request_id;
}

/** Whether a request started at `startedAtMs` has waited its full time by `nowMs`. */
export function isChoicesFetchOverdue(startedAtMs: number, nowMs: number): boolean {
  return nowMs - startedAtMs >= CHOICES_FETCH_TIMEOUT_MS;
}

/** The line shown in place of the model list when none could be read. */
export function choicesFailureLine(failure: ChoicesFailure): string {
  const reason = failure.kind === 'reported' ? failure.reason : plans.modelChoicesOverdueReason;
  return plans.modelChoicesFailed(reason);
}

function aliasRow(
  alias: string,
  value: string,
  shippedValue: string | null,
  choices: readonly ModelChoice[]
): AliasRow {
  return {
    alias,
    value,
    shippedValue,
    edited: isEdited(value, shippedValue),
    valueMissingFromChoices: choices.length > 0 && !choices.some((choice) => choice.id === value),
  };
}

/**
 * The shipped values of a profile's `model` aliases, read from its details:
 * one per alias the shipped profile gives a value, in the profile's order.
 */
export function shippedAliasesOf(detail: ProfileDetail): LaunchOptionAlias[] {
  return detail.entries.flatMap((entry) => {
    const [table, key, alias] = entry.path;
    return table === 'model' &&
      key === 'aliases' &&
      alias !== undefined &&
      entry.path.length === 3 &&
      typeof entry.shipped === 'string'
      ? [{ alias, value: entry.shipped }]
      : [];
  });
}
