import type { Messages } from './messages';
import type { ChatDeliveryOutcome, DetectedStatusView } from '../ipc/session-boundary';
// Type-only: the gate itself reaches this module through `ui-messages.ts`, so
// a value import here would close that loop at runtime.
import type { ConfirmTarget } from '../session/confirm-target';

/**
 * True if `key` is an own key of `record`, narrowing an externally supplied
 * string into that resource object's key set (no index signature, so a
 * plain bracket access would not type-check without this).
 */
function isKeyOf<T extends object>(record: T, key: string): key is Extract<keyof T, string> {
  return Object.prototype.hasOwnProperty.call(record, key);
}

/** Describes a window status for display. */
export function describeStatus(messages: Messages, status: string): string {
  return isKeyOf(messages.statusLabels, status)
    ? messages.statusLabels[status]
    : messages.statusLabels.unknown;
}

/** Describes an exit code for display (session-phase label, with the code appended once known). */
export function describeExit(messages: Messages, exitCode: number | null): string {
  const exitLabel = messages.sessionPhaseLabels.exited;
  if (exitCode === null) {
    return exitLabel;
  }
  return `${exitLabel} ${messages.formats.exitCode(exitCode)}`;
}

/** Describes an exit code shown next to a window status that already says the process has exited. */
export function describeExitCode(messages: Messages, exitCode: number): string {
  return messages.formats.exitCode(exitCode);
}

/** Describes the window list heading together with how many windows are open. */
export function describeWindowListTitle(messages: Messages, count: number): string {
  return messages.formats.windowListTitle(messages.hints.windowList, count);
}

/** Describes what is wrong with one window's connection, `null` while nothing is. */
export function describePaneNotice(messages: Messages, phase: string): string | null {
  if (phase === 'failed') {
    return messages.banners.connectionFailed;
  }
  if (phase === 'superseded') {
    return messages.banners.windowSuperseded;
  }
  return null;
}

/** Describes how many keymap entries were rejected while resolving the effective keymap. */
export function describeDiscardedKeymapEntries(messages: Messages, count: number): string {
  return messages.banners.discardedKeymapEntries(count);
}

/** Describes how many themes-file entries were rejected while parsing the user's themes. */
export function describeThemeIssues(messages: Messages, count: number): string {
  return messages.banners.themeIssues(count);
}

/**
 * Describes the count shown at every "needs you" display location:
 * the row indicator, the badges and the rail all read this same rendering.
 */
export function describeAttentionBadge(messages: Messages, count: number): string {
  return messages.formats.attentionBadge(count);
}

/** Describes a room heading together with how many windows it holds. */
export function describeRoomTitle(messages: Messages, name: string, count: number): string {
  return messages.formats.windowListTitle(name, count);
}

/** Describes the room heading's Unknown-count badge. */
export function describeUnknownBadge(messages: Messages, count: number): string {
  return messages.formats.unknownBadge(count);
}

/**
 * Describes the suffix a status label appends when the pair names the
 * `'unknown'` arm: the connector plus the reason's short label. Empty for
 * any other arm, and for the unknown arm with no reason (fail-safe: a
 * window with no reason to show has nothing to append).
 *
 * Named without `describeStatus` as a substring on purpose:
 * the sidebar's verbatim source scan rejects that literal occurring anywhere
 * else in the file.
 */
export function describeUnknownReasonSuffix(
  messages: Messages,
  detected: DetectedStatusView
): string {
  if (detected.status !== 'unknown' || detected.reason === null) {
    return '';
  }
  return ` · ${messages.unknownReasons.short[detected.reason]}`;
}

/**
 * Describes why a window's status is `'unknown'`, for the title-bar tooltip:
 * the reason's explanation, with the detection-load warning appended in
 * parentheses when it is non-blank. Empty for an arm that
 * names no reason, the same fail-safe shape
 * {@link describeUnknownReasonSuffix} takes.
 */
export function describeUnknownExplanation(
  messages: Messages,
  detected: DetectedStatusView,
  warning: string | null
): string {
  if (detected.status !== 'unknown' || detected.reason === null) {
    return '';
  }
  const explanation = messages.unknownReasons.explanation[detected.reason];
  return warning === null || warning.trim() === '' ? explanation : `${explanation} (${warning})`;
}

/**
 * Describes how one delivery ended, naming the reason when the outcome is the
 * `'undelivered_unknown'` arm and carries one. Every other arm -- and that arm
 * with no reason -- renders exactly as
 * {@link describeDeliveryState} does, so the outcome the record names, not the
 * presence of a reason, decides the wording.
 */
export function describeUndeliveredUnknownReason(
  messages: Messages,
  outcome: ChatDeliveryOutcome
): string {
  if (outcome.state !== 'undelivered_unknown' || outcome.reason === null) {
    return describeDeliveryState(messages, outcome.state);
  }
  return messages.chat.deliveryUndeliveredUnknownReason(
    messages.unknownReasons.chatWord[outcome.reason]
  );
}

/**
 * Describes the restore banner's text: how many windows from the last
 * session are available, and when that configuration was recorded.
 */
export function describeRestoreBanner(
  messages: Messages,
  count: number,
  timestamp: string | null
): string {
  return messages.banners.restoreAvailable(count, timestamp);
}

/** Describes the restore dialog's live selection count. */
export function describeRestoreSelectionCount(
  messages: Messages,
  selected: number,
  total: number
): string {
  return messages.formats.restoreSelectionCount(selected, total);
}

/**
 * Describes how one chat message landed in one addressed window. A state this
 * build does not know falls back to the unknown wording, the same way an
 * unrecognized status does.
 */
export function describeDeliveryState(messages: Messages, state: string): string {
  switch (state) {
    case 'pending':
      return messages.chat.deliveryPending;
    case 'taken':
      return messages.chat.deliveryTaken;
    case 'undelivered_unknown':
      return messages.chat.deliveryUndeliveredUnknown;
    case 'undelivered_halted':
      return messages.chat.deliveryUndeliveredHalted;
    case 'undelivered_exhausted':
      return messages.chat.deliveryUndeliveredExhausted;
    case 'undelivered_restart':
      return messages.chat.deliveryUndeliveredRestart;
    default:
      return messages.chat.deliveryUnknown;
  }
}

/**
 * Describes why a chat room stopped. A reason this build does not know still
 * says the room stopped, so a halt never goes unsaid.
 */
export function describeHaltReason(messages: Messages, reason: string): string {
  switch (reason) {
    case 'repetition':
      return messages.chat.haltedRepetition;
    case 'monologue':
      return messages.chat.haltedMonologue;
    case 'turn_budget':
      return messages.chat.haltedTurnBudget;
    default:
      return messages.chat.haltedUnknown;
  }
}

/**
 * Describes a theme name for a select option. A user theme is shown under the
 * name its author gave it; only the built-in ones are translated.
 */
export function describeThemeName(messages: Messages, name: string): string {
  return isKeyOf(messages.builtInThemeLabels, name) ? messages.builtInThemeLabels[name] : name;
}

// ---------------------------------------------------------------------------
// Sanitization
// ---------------------------------------------------------------------------

/** Longest externally supplied text kept, in code points. */
const EXTERNAL_TEXT_MAX_CODE_POINTS = 200;

/**
 * The rejected value as text. Tauri commands reject with a plain string;
 * anything else reached the WebView as decoded JSON and can be encoded back.
 */
function detailText(detail: unknown): string {
  if (typeof detail === 'string') {
    return detail;
  }
  if (detail instanceof Error) {
    return detail.message;
  }
  try {
    return JSON.stringify(detail) ?? '';
  } catch {
    // A circular reference or a BigInt makes JSON.stringify throw. The notice
    // has to survive that: losing the detail beats losing the whole banner.
    return '';
  }
}

/**
 * Reduces text that came from outside the WebView to one display line: line
 * breaks, control characters and non-breaking whitespace are folded into
 * single spaces, the ends are trimmed, and the rest is capped so that one
 * message cannot take over the notice area.
 *
 * The folding here is broader than `DISPLAY_CONTROL_CHARS`
 * (`session-boundary.ts`), which covers fewer classes and strips rather than
 * folds.
 *
 * @param text the external text, as it arrived
 */
export function foldExternalText(text: string): string {
  const flattened = text.replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Zs}]+/gu, ' ').trim();
  // Cut by code point: half of a surrogate pair would render as a stray glyph.
  const codePoints = Array.from(flattened);
  return codePoints.length > EXTERNAL_TEXT_MAX_CODE_POINTS
    ? `${codePoints.slice(0, EXTERNAL_TEXT_MAX_CODE_POINTS).join('')}…`
    : flattened;
}

/**
 * Builds the notice shown when a window operation fails: the summary, and the
 * detail the daemon reported (an approval-gate rejection, for one) when
 * anything survives {@link foldExternalText}.
 *
 * @param summary the operation-specific message from `errors`
 * @param detail the rejected value the operation failed with
 */
export function describeOperationFailure(summary: string, detail: unknown): string {
  const folded = foldExternalText(detailText(detail));
  return folded === '' ? summary : `${summary}: ${folded}`;
}

/**
 * Builds the notice shown when a window opened but its detection definition
 * failed to load, so that window's status stays `Unknown`.
 *
 * The detail is the daemon's English reason text, folded and capped exactly
 * like an operation failure's detail: it is external text of the same kind,
 * so it goes through the same treatment rather than a second copy of it.
 *
 * @param messages the resolved locale, for the `errors.detectionLoadFailed` summary
 * @param detail the reason the daemon reported
 */
export function describeDetectionWarning(messages: Messages, detail: unknown): string {
  return describeOperationFailure(messages.errors.detectionLoadFailed, detail);
}
/**
 * Describes the interrupt button's tooltip: what the button does while the
 * gate permits it, and why it cannot be used while the gate refuses it.
 *
 * It takes the gate's verdict rather than the window's status, so the reason
 * is read off the decision that produced it instead of being judged a second
 * time here.
 *
 * @param messages the resolved locale
 * @param target the gate's verdict for `interrupt` on that window
 */
export function describeInterruptHint(messages: Messages, target: ConfirmTarget): string {
  if (target.allowed) {
    return messages.hints.interruptWindow;
  }
  switch (target.reason) {
    case 'exited':
      return messages.hints.interruptUnavailableExited;
    case 'unknown':
      return messages.hints.interruptUnavailableUnknown;
    default: {
      const unreachable: never = target.reason;
      return unreachable;
    }
  }
}
