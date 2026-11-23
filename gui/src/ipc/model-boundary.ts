/**
 * The WebView-side boundary for a profile's model aliases: the shapes the
 * model-alias commands reply with and the `session://model-choices` payload.
 *
 * The field names mirror the Rust wire types in `yagura-protocol` verbatim
 * (snake_case). Every reply is accepted as `unknown` and validated here with
 * the same readers `session-boundary` uses, so a field is only ever read
 * through an ownership test.
 */
import {
  foldExternalText,
  formatCommandLine,
  sanitizeForDisplay,
  type DisplayLine,
} from '../display/untrusted-text';
import {
  BoundaryError,
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

/** One alias of a profile's `model` launch option and the value it stands for. */
export interface LaunchOptionAlias {
  alias: string;
  value: string;
}

/**
 * Where a profile's model list came from. A closed set, like the Rust enum it
 * mirrors: a `kind` outside it is refused rather than guessed at.
 */
export type ModelChoiceSource =
  { kind: 'values' } | { kind: 'command'; displayCommand: DisplayLine };

/** One model an alias may be set to. */
export interface ModelChoice {
  /** The value stored for the alias. */
  id: string;
  /** `id` for display. */
  displayId: DisplayLine;
  /** A display name, `null` when the source offers none. */
  label: DisplayLine | null;
}

/** The immediate reply to `list_model_choices`: the request was accepted. */
export interface ModelChoicesRequested {
  request_id: number;
  profile: string;
}

/** Payload of `session://model-choices`: the list a request asked for. */
export interface ModelChoicesPayload {
  request_id: number;
  profile: string;
  /** Where the list came from, `null` when the profile declares no source. */
  source: ModelChoiceSource | null;
  choices: ModelChoice[];
  /** Why the list could not be taken, folded onto one line; `null` on success. */
  error: DisplayLine | null;
}

/** The reply to `save_model_aliases`: the aliases as stored. */
export interface ModelAliasesSaved {
  profile: string;
  aliases: LaunchOptionAlias[];
}

/** What a shipped profile says about its `model` launch option. */
export interface ShippedModelProfile {
  aliases: LaunchOptionAlias[];
}

/** The shipped aliases of each profile the Models tab has read, keyed by profile id. */
export type ShippedModelProfiles = Record<string, ShippedModelProfile>;

/**
 * Validates the reply to `list_model_choices`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowModelChoicesRequested(value: unknown): ModelChoicesRequested {
  const context = 'list_model_choices';
  const record = requireRecord(value, context);
  return {
    request_id: requireNonNegativeInteger(record, 'request_id', context),
    profile: requireString(record, 'profile', context),
  };
}

/**
 * Validates a `session://model-choices` payload. `source`, `error` and a
 * choice's `label` may be absent or `null`, both read as `null`.
 *
 * @throws {BoundaryError} when the payload does not match the contract.
 */
export function narrowModelChoicesPayload(value: unknown): ModelChoicesPayload {
  const context = SESSION_EVENT.modelChoices;
  const record = requireRecord(value, context);
  const choicesContext = `${context}.choices`;
  return {
    request_id: requireNonNegativeInteger(record, 'request_id', context),
    profile: requireString(record, 'profile', context),
    source: readOptionalSource(record, context),
    choices: requireArray(readOwnField(record, 'choices'), choicesContext).map((entry) => {
      const choice = requireRecord(entry, choicesContext);
      const id = requireString(choice, 'id', choicesContext);
      const label = readOptionalString(choice, 'label', choicesContext);
      return {
        id,
        displayId: sanitizeForDisplay(id),
        label: label === null ? null : sanitizeForDisplay(label),
      };
    }),
    error: foldedOrNull(readOptionalString(record, 'error', context)),
  };
}

function foldedOrNull(text: string | null): DisplayLine | null {
  return text === null ? null : foldExternalText(text);
}

/**
 * Validates the reply to `save_model_aliases`.
 *
 * @throws {BoundaryError} when the reply does not match the contract.
 */
export function narrowModelAliasesSaved(value: unknown): ModelAliasesSaved {
  const context = 'save_model_aliases';
  const record = requireRecord(value, context);
  return {
    profile: requireString(record, 'profile', context),
    aliases: narrowAliases(readOwnField(record, 'aliases'), `${context}.aliases`),
  };
}

function narrowAliases(value: unknown, context: string): LaunchOptionAlias[] {
  return requireArray(value, context).map((entry) => {
    const row = requireRecord(entry, context);
    return {
      alias: requireString(row, 'alias', context),
      value: requireString(row, 'value', context),
    };
  });
}

/** Reads a push's `source`; the daemon omits the key when there is none. */
function readOptionalSource(record: UntrustedRecord, context: string): ModelChoiceSource | null {
  const value = readOwnField(record, 'source');
  if (value === null || value === undefined) {
    return null;
  }
  const sourceContext = `${context}.source`;
  const source = requireRecord(value, sourceContext);
  const kind = requireString(source, 'kind', sourceContext);
  switch (kind) {
    case 'values':
      return { kind };
    case 'command': {
      return {
        kind,
        displayCommand: formatCommandLine(requireStringArray(source, 'argv', sourceContext)),
      };
    }
    default:
      throw new BoundaryError(`${sourceContext}.kind: unrecognized kind "${kind}"`);
  }
}
