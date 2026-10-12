/**
 * The WebView-side boundary for a profile's details: every value a profile
 * uses with the layer it comes from, and the host's launch argv.
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the readers `session-boundary` uses. A value is any JSON the profile holds;
 * it is kept as `unknown` and only ever turned into text for display.
 */
import {
  BoundaryError,
  readOwnField,
  requireArray,
  requireBoolean,
  requireRecord,
  requireString,
  requireStringArray,
  type UntrustedRecord,
} from './session-boundary';

export { BoundaryError };

/**
 * The wire revision a daemon must have settled on before the profile-detail
 * requests reach it (mirrors `PROFILE_DETAIL_MIN_REVISION`, `yagura-protocol`).
 * Informational only: it decides whether the Profiles tab offers details,
 * never what a request may send.
 */
export const PROFILE_DETAIL_MIN_REVISION = 16;

/** The argv a profile launches on the daemon's host, with its layer. */
export interface ProfileCommandDetail {
  argv: string[];
  /** The shipped profile's argv; absent when there is no shipped profile. */
  shipped: string[] | null;
  /** Whether the user's own file has a `[command]` table. */
  changed: boolean;
}

/** One value of a profile as the daemon uses it. */
export interface ProfileDetailEntry {
  /** The key path, outermost table first. */
  path: string[];
  value: unknown;
  /** The shipped profile's value at this path; `undefined` when it has none. */
  shipped: unknown;
  /** Whether the user's own file sets this value. */
  changed: boolean;
}

/** The reply to `get_profile_detail`. */
export interface ProfileDetail {
  profile: string;
  /** Whether the profile is laid over a shipped profile. */
  shippedBase: boolean;
  command: ProfileCommandDetail;
  entries: ProfileDetailEntry[];
}

/** The reply to `set_profile_command`: the argv in use after the change. */
export interface ProfileCommandSet {
  profile: string;
  command: ProfileCommandDetail;
}

function narrowCommand(value: unknown, context: string): ProfileCommandDetail {
  const record = requireRecord(value, context);
  const shipped = readOwnField(record, 'shipped');
  return {
    argv: requireStringArray(record, 'argv', context),
    shipped:
      shipped === undefined || shipped === null
        ? null
        : requireStringArray(record, 'shipped', context),
    changed: requireBoolean(record, 'changed', context),
  };
}

function narrowEntry(value: unknown, context: string): ProfileDetailEntry {
  const record: UntrustedRecord = requireRecord(value, context);
  return {
    path: requireStringArray(record, 'path', context),
    value: readOwnField(record, 'value'),
    shipped: readOwnField(record, 'shipped'),
    changed: requireBoolean(record, 'changed', context),
  };
}

/**
 * Validates the reply to `get_profile_detail`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowProfileDetail(value: unknown): ProfileDetail {
  const context = 'get_profile_detail';
  const record = requireRecord(value, context);
  return {
    profile: requireString(record, 'profile', context),
    shippedBase: requireBoolean(record, 'shipped_base', context),
    command: narrowCommand(readOwnField(record, 'command'), `${context}.command`),
    entries: requireArray(readOwnField(record, 'entries'), `${context}.entries`).map(
      (entry, index) => narrowEntry(entry, `${context}.entries[${index}]`)
    ),
  };
}

/**
 * Validates the reply to `set_profile_command`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowProfileCommandSet(value: unknown): ProfileCommandSet {
  const context = 'set_profile_command';
  const record = requireRecord(value, context);
  return {
    profile: requireString(record, 'profile', context),
    command: narrowCommand(readOwnField(record, 'command'), `${context}.command`),
  };
}
