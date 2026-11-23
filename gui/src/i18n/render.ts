import type { Messages, TitleParts } from './messages';
import type { UiText } from '../display/ui-text';
import type { PickerEmpty, PickerMode } from '../command-picker/picker-model';
import type {
  ChatDeliveryOutcome,
  DetectedStatusView,
  WindowStatus,
} from '../ipc/session-boundary';
import {
  caughtValueText,
  foldExternalText,
  sanitizeForDisplay,
  type DisplayLine,
} from '../display/untrusted-text';
import type { ActionBlockReason } from '../keymap/action-availability';
// Type-only: the gate itself reaches this module through `ui-messages.ts`, so
// a value import here would close that loop at runtime.
import type { ConfirmTarget, InterruptBlockReason } from '../session/confirm-target';

/**
 * The entry `record` holds under `key` when that is an own key, else
 * `undefined`. An externally supplied string picks an entry this way without
 * being claimed to be one of the keys.
 */
function ownEntry<V>(record: Readonly<Record<string, V>>, key: string): V | undefined {
  return Object.prototype.hasOwnProperty.call(record, key) ? record[key] : undefined;
}

/** Describes a window status for display. */
export function describeStatus(messages: Messages, status: string): UiText {
  return ownEntry(messages.statusLabels, status) ?? messages.statusLabels.unknown;
}

/**
 * Describes the card that stands in for a window that ended without this GUI
 * closing it: the window's name, and its exit code once known.
 *
 * @param name the window's display name
 */
export function describeExitedWindow(
  messages: Messages,
  name: DisplayLine,
  exitCode: number | null
): UiText {
  return exitCode === null
    ? messages.workArea.windowExited(name)
    : messages.workArea.windowExitedWithCode(name, exitCode);
}

/**
 * The exited window's sentence split around its name, for a card that cuts only the name short.
 *
 * @param name the window's display name
 */
export function describeExitedWindowParts(
  messages: Messages,
  name: DisplayLine,
  exitCode: number | null
): TitleParts {
  return exitCode === null
    ? messages.workArea.windowExitedParts(name)
    : messages.workArea.windowExitedWithCodeParts(name, exitCode);
}

/**
 * Describes the button that goes to another window.
 *
 * @param name the window's display name
 */
export function describeGoToWindow(messages: Messages, name: DisplayLine): UiText {
  return messages.workArea.goToWindow(name);
}

/**
 * Describes a window that calls the user, as its lane card says it.
 *
 * @param name the window's display name
 */
export function describeWindowCall(
  messages: Messages,
  name: DisplayLine,
  status: Extract<WindowStatus, 'waiting_approval' | 'waiting_input'>
): UiText {
  return status === 'waiting_approval'
    ? messages.banners.windowNeedsApproval(name)
    : messages.banners.windowNeedsInput(name);
}

/**
 * A calling window's sentence split around its name, for a card that cuts only the name short.
 *
 * @param name the window's display name
 */
export function describeWindowCallParts(
  messages: Messages,
  name: DisplayLine,
  status: Extract<WindowStatus, 'waiting_approval' | 'waiting_input'>
): TitleParts {
  return status === 'waiting_approval'
    ? messages.banners.windowNeedsApprovalParts(name)
    : messages.banners.windowNeedsInputParts(name);
}

/**
 * Describes a lane card that later events of its kind were folded into.
 *
 * @param sentence the latest event's sentence
 * @param count how many other events the card holds
 */
export function describeNoticeAndMore(
  messages: Messages,
  sentence: UiText | DisplayLine,
  count: number
): UiText {
  return messages.banners.noticeAndMore(sentence, count);
}

/** Describes when a notification event was recorded. */
export function describeEventTime(messages: Messages, date: Date): UiText {
  return messages.formats.eventTime(date);
}

/** Describes when a chat message was posted. */
export function describeChatPostedAt(messages: Messages, atMs: number): UiText {
  return messages.formats.chatPostedAt(atMs);
}

/** Joins what one update reads out into one text. */
export function describeAnnouncement(
  messages: Messages,
  sentences: readonly (UiText | DisplayLine)[]
): UiText {
  return messages.formats.sentences(sentences);
}

/**
 * Describes when something was recorded; `null` for a value that is no
 * plausible recording time.
 *
 * @param epochS the moment, in Unix epoch seconds
 */
export function describeRecordedTime(messages: Messages, epochS: number): UiText | null {
  return messages.formats.recordedTime(epochS);
}

/**
 * Describes the empty work area's offer to reopen the windows of the last session.
 *
 * @param savedAtEpochS when that session was saved, `null` when unknown
 */
export function describeReopenOffer(
  messages: Messages,
  count: number,
  savedAtEpochS: number | null
): UiText {
  return messages.workArea.reopenFromLastSession(count, savedAtEpochS);
}

/**
 * Describes a sidebar row as its accessible name: the window's name and its
 * status word, and whether the window waits for the user's permission.
 *
 * @param name the window's display name
 */
export function describeWindowRow(
  messages: Messages,
  name: DisplayLine,
  status: string,
  isAwaitingPermission: boolean
): UiText {
  const label = messages.formats.windowRow(name, describeStatus(messages, status));
  return isAwaitingPermission ? messages.formats.windowRowAwaitingPermission(label) : label;
}

/** What a window header says beyond what it shows as text; see {@link describeWindowHeader}. */
export interface WindowHeaderFacts {
  /** The window's vendor; `null` when it has none. */
  vendor: DisplayLine | null;
  /** The window's working directory. */
  cwd: DisplayLine;
  /** Whether the header shows the MCP connection mark. */
  showsMcpMark: boolean;
  /** Whether the header shows the mark that status reports are not arriving. */
  showsStatusReportMark: boolean;
  /** The inbox count the header shows, `undefined` while it shows none. */
  queuedCount: number | undefined;
}

function windowLocationSentence(
  messages: Messages,
  facts: Pick<WindowHeaderFacts, 'vendor' | 'cwd'>
): UiText {
  return facts.vendor === null
    ? messages.formats.windowHeaderCwd(facts.cwd)
    : messages.formats.windowHeaderVendorCwd(facts.vendor, facts.cwd);
}

/** Describes where a window runs: the sentence a header and a hover card both open with. */
export function describeWindowLocation(
  messages: Messages,
  facts: Pick<WindowHeaderFacts, 'vendor' | 'cwd'>
): UiText {
  return messages.formats.sentences([windowLocationSentence(messages, facts)]);
}

/**
 * Describes a window header as its accessible description: where the window
 * runs, then what each mark it shows means, so the marks are not icon-only.
 */
export function describeWindowHeader(messages: Messages, facts: WindowHeaderFacts): UiText {
  const sentences = [windowLocationSentence(messages, facts)];
  if (facts.showsMcpMark) {
    sentences.push(messages.hints.mcpConnected);
  }
  if (facts.showsStatusReportMark) {
    sentences.push(messages.hints.statusReportsMissing);
  }
  if (facts.queuedCount !== undefined) {
    sentences.push(messages.formats.queuedMessages(facts.queuedCount));
  }
  return messages.formats.sentences(sentences);
}

/** Describes what is wrong with one window's connection, `null` while nothing is. */
export function describePaneNotice(messages: Messages, phase: string): UiText | null {
  if (phase === 'failed') {
    return messages.banners.connectionFailed;
  }
  if (phase === 'superseded') {
    return messages.banners.windowSuperseded;
  }
  return null;
}

/** Describes how many keymap entries were rejected while resolving the effective keymap. */
export function describeDiscardedKeymapEntries(messages: Messages, count: number): UiText {
  return messages.banners.discardedKeymapEntries(count);
}

/** Describes how many themes-file entries were rejected while parsing the user's themes. */
export function describeThemeIssues(messages: Messages, count: number): UiText {
  return messages.banners.themeIssues(count);
}

/**
 * Describes the count shown at every "needs you" display location:
 * the row indicator, the badges and the rail all read this same rendering.
 */
export function describeAttentionBadge(messages: Messages, count: number): UiText {
  return messages.formats.attentionBadge(count);
}

/** Describes a room heading together with how many windows it holds. */
export function describeRoomTitle(messages: Messages, name: DisplayLine, count: number): UiText {
  return messages.formats.windowListTitle(name, count);
}

/** Describes the tooltip of a window's waiting-messages mark. */
export function describeQueuedMessages(messages: Messages, count: number): UiText {
  return messages.formats.queuedMessages(count);
}

/**
 * Describes the reason's short label a status label is followed by when the
 * pair names the `'unknown'` arm. `null` for any other arm, and for the
 * unknown arm with no reason (fail-safe: a window with no reason to show has
 * nothing to append).
 */
export function describeUnknownReasonSuffix(
  messages: Messages,
  detected: DetectedStatusView
): UiText | null {
  if (detected.status !== 'unknown' || detected.reason === null) {
    return null;
  }
  return messages.unknownReasons.short[detected.reason];
}

/**
 * Describes a status label: the status word, followed by the short reason
 * {@link describeUnknownReasonSuffix} gives when there is one.
 *
 * @param reason the short reason, `null` when the label names none
 */
export function describeStatusLabel(
  messages: Messages,
  status: string,
  reason: UiText | null
): UiText {
  const word = describeStatus(messages, status);
  return reason === null ? word : messages.formats.statusWithUnknownReason(word, reason);
}

/**
 * Describes why a window's status is `'unknown'`, for the title-bar tooltip:
 * the reason's explanation, with the detection-load warning appended in
 * parentheses when it is non-blank. `null` for an arm that names no reason,
 * the same fail-safe shape {@link describeUnknownReasonSuffix} takes.
 */
export function describeUnknownExplanation(
  messages: Messages,
  detected: DetectedStatusView,
  warning: DisplayLine | null
): UiText | null {
  if (detected.status !== 'unknown' || detected.reason === null) {
    return null;
  }
  const explanation = messages.unknownReasons.explanation[detected.reason];
  return warning === null || warning.trim() === ''
    ? explanation
    : messages.formats.unknownExplanationWithWarning(explanation, warning);
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
): UiText {
  if (outcome.state !== 'undelivered_unknown' || outcome.reason === null) {
    return describeDeliveryState(messages, outcome.state);
  }
  return messages.chat.deliveryUndeliveredUnknownReason(
    messages.unknownReasons.chatWord[outcome.reason]
  );
}

/**
 * Describes the reopen offer's text: how many windows from the last
 * session can be reopened, and when that configuration was recorded.
 */
export function describeReopenNotice(
  messages: Messages,
  count: number,
  savedAtEpochS: number | null
): UiText {
  return messages.banners.restoreAvailable(count, savedAtEpochS);
}

/** Describes the restore dialog's live selection count. */
export function describeRestoreSelectionCount(
  messages: Messages,
  selected: number,
  total: number
): UiText {
  return messages.formats.restoreSelectionCount(selected, total);
}

/**
 * Describes where a restore candidate came from: its profile and its vendor,
 * whichever it recorded; `null` when it recorded neither.
 */
export function describeRestoreOrigin(
  messages: Messages,
  profile: DisplayLine | null,
  vendor: DisplayLine | null
): UiText | null {
  const parts = [profile, vendor].flatMap((part) => (part === null || part === '' ? [] : [part]));
  return parts.length === 0 ? null : messages.formats.restoreOrigin(parts);
}

/** Describes the label of the button that reopens the checked windows. */
export function describeRestoreConfirm(messages: Messages, count: number): UiText {
  return messages.formats.restoreConfirm(count);
}

/**
 * Describes how one chat message landed in one addressed window. A state this
 * build does not know falls back to the unknown wording, the same way an
 * unrecognized status does.
 */
export function describeDeliveryState(messages: Messages, state: string): UiText {
  switch (state) {
    case 'queued':
      return messages.chat.deliveryPending;
    case 'delivered':
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
export function describeHaltReason(messages: Messages, reason: string): UiText {
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
 * name its author gave it, prepared for display since it comes from the
 * user's themes file; only the built-in ones are translated.
 */
export function describeThemeName(messages: Messages, name: string): UiText | DisplayLine {
  return ownEntry(messages.builtInThemeLabels, name) ?? sanitizeForDisplay(name);
}

// ---------------------------------------------------------------------------
// Sanitization
// ---------------------------------------------------------------------------

/**
 * The one display line a caught value is told by: its text
 * (`caughtValueText`) folded and capped by {@link foldExternalText}, both in
 * `display/untrusted-text.ts`, since the reason may carry the daemon's words.
 *
 * @param error the value a failed operation threw or rejected with
 */
export function foldedFailureReason(error: unknown): DisplayLine {
  return foldExternalText(caughtValueText(error));
}

/**
 * Builds the notice shown when a window operation fails: the summary, and the
 * detail the daemon reported (an approval-gate rejection, for one) when
 * anything survives {@link foldExternalText}.
 *
 * @param summary the operation-specific message from `errors`, or the display name of what
 * failed; returned as is when no detail is left
 * @param detail the rejected value the operation failed with
 */
export function describeOperationFailure<S extends UiText | DisplayLine>(
  messages: Messages,
  summary: S,
  detail: unknown
): S | UiText {
  const folded = foldedFailureReason(detail);
  return folded === '' ? summary : messages.errors.operationFailure(summary, folded);
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
export function describeDetectionWarning(messages: Messages, detail: unknown): UiText {
  return describeOperationFailure(messages, messages.errors.detectionLoadFailed, detail);
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
export function describeInterruptHint(messages: Messages, target: ConfirmTarget): UiText {
  return target.allowed
    ? messages.hints.interruptWindow
    : describeInterruptBlock(messages, target.reason);
}

function describeInterruptBlock(messages: Messages, reason: InterruptBlockReason): UiText {
  switch (reason) {
    case 'exited':
      return messages.hints.interruptUnavailableExited;
    case 'unknown':
      return messages.hints.interruptUnavailableUnknown;
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

/**
 * Describes why an action does nothing right now, for the palette's row and the
 * menu item that stands for it.
 *
 * @param messages the resolved locale
 * @param reason the reason the availability judgment gave
 */
export function describeActionBlock(messages: Messages, reason: ActionBlockReason): UiText {
  switch (reason) {
    case 'no-window-on-screen':
      return messages.actionBlocks.noWindowOnScreen;
    case 'canvas-only':
      return messages.sidebar.canvasOnly;
    case 'no-windows':
      return messages.actionBlocks.noWindows;
    case 'nowhere-to-go':
      return messages.actionBlocks.nowhereToGo;
    case 'nothing-needs-you':
      return messages.actionBlocks.nothingNeedsYou;
    case 'exited':
    case 'unknown':
      return describeInterruptBlock(messages, reason);
    default: {
      const unreachable: never = reason;
      return unreachable;
    }
  }
}

/** What a picker window row is read out as; see {@link describePickerWindowRow}. */
export interface PickerWindowRowFacts {
  /** The window's display name. */
  name: DisplayLine;
  status: string;
  isAwaitingPermission: boolean;
  /** The room's display name, or the folder for a window in no room. */
  place: DisplayLine;
  isCurrent: boolean;
}

/**
 * Describes a picker window row as its accessible name: the sidebar row's name,
 * then where the window is, then whether it is the current one.
 */
export function describePickerWindowRow(messages: Messages, facts: PickerWindowRowFacts): UiText {
  const row = describeWindowRow(messages, facts.name, facts.status, facts.isAwaitingPermission);
  const placed = messages.picker.windowRowPlace(row, facts.place);
  return facts.isCurrent ? messages.picker.windowRowCurrent(placed) : placed;
}

/**
 * Describes a palette action row as its accessible name: the action's name, and
 * its key when it has one.
 *
 * @param key the key as the row shows it, `null` when the action has none
 */
export function describePickerActionRow(
  messages: Messages,
  name: UiText,
  key: UiText | null
): UiText {
  return key === null ? name : messages.picker.actionRowKey(name, key);
}

/**
 * Describes why the picker lists nothing, or only the New window offer: what
 * the user sees, and how to search the other mode when there is a way.
 */
export function describePickerEmpty(
  messages: Messages,
  empty: NonNullable<PickerEmpty>
): { title: UiText; hint: UiText | null } {
  switch (empty.kind) {
    case 'no-windows':
      return { title: messages.picker.noWindows, hint: null };
    case 'no-window-match':
      return {
        title: messages.picker.noWindowMatch(empty.query),
        hint: messages.picker.noWindowMatchHint,
      };
    case 'no-command-match':
      return {
        title: messages.picker.noCommandMatch(empty.query),
        hint: messages.picker.noCommandMatchHint,
      };
    default: {
      const unreachable: never = empty;
      return unreachable;
    }
  }
}

/**
 * Describes what the picker lists, for the screen reader once typing pauses:
 * how many rows there are, or why there are none.
 */
export function describePickerResults(
  messages: Messages,
  mode: PickerMode,
  rowCount: number,
  empty: PickerEmpty
): UiText {
  if (empty !== null) {
    return describePickerEmpty(messages, empty).title;
  }
  return mode === 'commands'
    ? messages.picker.commandCount(rowCount)
    : messages.picker.windowCount(rowCount);
}
