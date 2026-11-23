/**
 * Facade over the i18n layer: binds the resolved locale once and
 * re-exports it under the API shape its consumers import.
 */
import { resolveMessages } from './i18n/messages';
import {
  describeActionBlock as renderDescribeActionBlock,
  describeAnnouncement as renderDescribeAnnouncement,
  describeAttentionBadge as renderDescribeAttentionBadge,
  describeChatPostedAt as renderDescribeChatPostedAt,
  describeDeliveryState as renderDescribeDeliveryState,
  describeDetectionWarning as renderDescribeDetectionWarning,
  describeDiscardedKeymapEntries as renderDescribeDiscardedKeymapEntries,
  describeEventTime as renderDescribeEventTime,
  describeExitedWindow as renderDescribeExitedWindow,
  describeExitedWindowParts as renderDescribeExitedWindowParts,
  describeGoToWindow as renderDescribeGoToWindow,
  describeHaltReason as renderDescribeHaltReason,
  describeInterruptHint as renderDescribeInterruptHint,
  describeNoticeAndMore as renderDescribeNoticeAndMore,
  describeOperationFailure as renderDescribeOperationFailure,
  describePaneNotice as renderDescribePaneNotice,
  describePickerActionRow as renderDescribePickerActionRow,
  describePickerEmpty as renderDescribePickerEmpty,
  describePickerResults as renderDescribePickerResults,
  describePickerWindowRow as renderDescribePickerWindowRow,
  describeQueuedMessages as renderDescribeQueuedMessages,
  describeRecordedTime as renderDescribeRecordedTime,
  describeReopenOffer as renderDescribeReopenOffer,
  describeReopenNotice as renderDescribeReopenNotice,
  describeRestoreSelectionCount as renderDescribeRestoreSelectionCount,
  describeRestoreConfirm as renderDescribeRestoreConfirm,
  describeRestoreOrigin as renderDescribeRestoreOrigin,
  describeRoomTitle as renderDescribeRoomTitle,
  describeStatus as renderDescribeStatus,
  describeStatusLabel as renderDescribeStatusLabel,
  describeThemeIssues as renderDescribeThemeIssues,
  describeThemeName as renderDescribeThemeName,
  describeUndeliveredUnknownReason as renderDescribeUndeliveredUnknownReason,
  describeUnknownExplanation as renderDescribeUnknownExplanation,
  describeUnknownReasonSuffix as renderDescribeUnknownReasonSuffix,
  describeWindowCall as renderDescribeWindowCall,
  describeWindowCallParts as renderDescribeWindowCallParts,
  describeWindowHeader as renderDescribeWindowHeader,
  describeWindowLocation as renderDescribeWindowLocation,
  describeWindowRow as renderDescribeWindowRow,
  foldedFailureReason,
  type PickerWindowRowFacts,
  type WindowHeaderFacts,
} from './i18n/render';
import type { TitleParts } from './i18n/messages';
import type { UiText } from './display/ui-text';
import {
  EXTERNAL_TEXT_MAX_CODE_POINTS,
  foldExternalText,
  type DisplayLine,
} from './display/untrusted-text';
import type { PickerEmpty, PickerMode } from './command-picker/picker-model';
import type { ChatDeliveryOutcome, DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import type { ActionBlockReason } from './keymap/action-availability';
import type { ConfirmTarget } from './session/confirm-target';

const messages = resolveMessages();

/**
 * A resolved locale, and the resource shape a locale written outside `i18n/`
 * (a test's) fills before it stands in for one.
 */
export type { Messages, MessagesSource } from './i18n/messages';

/** A sentence split around the name it holds. */
export type { TitleParts };

/** The parts as a locale resource writes them, for a locale written outside `i18n/`. */
export type { TitleParts as TitlePartsSource } from './i18n/messages-en';

/** Status display strings. */
export const statusLabels = messages.statusLabels;

/** Button labels. */
export const buttons = messages.buttons;

/** Dialog messages. */
export const dialogs = messages.dialogs;

/** Form labels. */
export const forms = messages.forms;

/** Theme selection labels. */
export const themes = messages.themes;

/** Error messages. */
export const errors = messages.errors;

/** Notices' text, wherever a notice shows. */
export const banners = messages.banners;

/** Tooltips and helper text. */
export const hints = messages.hints;

/** The `⋯` window menu's item labels. */
export const menus = messages.menus;

/** Accessible names for the navigation rail's icons. */
export const nav = messages.nav;

/** The sidebar's window list: its header, its room headings and its rows. */
export const sidebar = messages.sidebar;

/** The chat panel's text: its chrome, its chips, its banner and its badge. */
export const chat = messages.chat;

/** The plans panel's text: its tabs, board, owner banner and role editor. */
export const plans = messages.plans;

/** The plans panel's Tickets tab: the ticket board and one ticket's details. */
export const tickets = messages.tickets;

/** The settings panel's text: its tabs and the Profiles tab's rows. */
export const settings = messages.settings;

/** The work area's text while it shows no window. */
export const workArea = messages.workArea;

/** The go-to-window picker and the command palette. */
export const picker = messages.picker;

/** How a key combination is written: the modifier names and the chord they make. */
export const keyNames = messages.keyNames;

/**
 * Describes the card that stands in for a window that ended without this GUI
 * closing it.
 * @param name the window's display name
 * @param exitCode the exit code, or null if not yet determined
 */
export function describeExitedWindow(name: string, exitCode: number | null): UiText {
  return renderDescribeExitedWindow(messages, name, exitCode);
}

/**
 * The exited window's sentence split around its name.
 * @param name the window's display name
 * @param exitCode the exit code, or null if not yet determined
 */
export function describeExitedWindowParts(name: string, exitCode: number | null): TitleParts {
  return renderDescribeExitedWindowParts(messages, name, exitCode);
}

/**
 * Describes the button that goes to another window.
 * @param name the window's display name
 */
export function describeGoToWindow(name: string): UiText {
  return renderDescribeGoToWindow(messages, name);
}

/**
 * Describes a window that calls the user, as its lane card says it.
 * @param name the window's display name
 * @param status which call it is
 */
export function describeWindowCall(
  name: string,
  status: Extract<WindowStatus, 'waiting_approval' | 'waiting_input'>
): UiText {
  return renderDescribeWindowCall(messages, name, status);
}

/**
 * A calling window's sentence split around its name.
 * @param name the window's display name
 * @param status which call it is
 */
export function describeWindowCallParts(
  name: string,
  status: Extract<WindowStatus, 'waiting_approval' | 'waiting_input'>
): TitleParts {
  return renderDescribeWindowCallParts(messages, name, status);
}

/**
 * Describes a lane card that later events of its kind were folded into.
 * @param sentence the latest event's sentence
 * @param count how many other events the card holds
 */
export function describeNoticeAndMore(sentence: string, count: number): UiText {
  return renderDescribeNoticeAndMore(messages, sentence, count);
}

/**
 * Describes when a notification event was recorded.
 * @param date the moment it was recorded
 */
export function describeEventTime(date: Date): UiText {
  return renderDescribeEventTime(messages, date);
}

/**
 * Describes when a chat message was posted.
 * @param atMs the moment it was posted, in Unix epoch milliseconds
 */
export function describeChatPostedAt(atMs: number): UiText {
  return renderDescribeChatPostedAt(messages, atMs);
}

/**
 * Joins what one update reads out into one text.
 * @param sentences what is read, in order
 */
export function describeAnnouncement(sentences: readonly string[]): UiText {
  return renderDescribeAnnouncement(messages, sentences);
}

/**
 * Describes when something was recorded; `null` for no plausible recording time.
 * @param epochS the moment, in Unix epoch seconds
 */
export function describeRecordedTime(epochS: number): UiText | null {
  return renderDescribeRecordedTime(messages, epochS);
}

/**
 * Describes the empty work area's offer to reopen the windows of the last session.
 * @param count the number of restore candidates
 * @param savedAtEpochS when that session was saved, in Unix epoch seconds; `null` when unknown
 */
export function describeReopenOffer(count: number, savedAtEpochS: number | null): UiText {
  return renderDescribeReopenOffer(messages, count, savedAtEpochS);
}

/**
 * Describes how one chat message landed in one addressed window.
 * @param state the delivery state the daemon reported
 */
export function describeDeliveryState(state: string): UiText {
  return renderDescribeDeliveryState(messages, state);
}

/**
 * Describes why a chat room stopped.
 * @param reason the halt reason the daemon reported
 */
export function describeHaltReason(reason: string): UiText {
  return renderDescribeHaltReason(messages, reason);
}

/**
 * Describes a theme name for a select option. A user theme is shown under the
 * name its author gave it, prepared for display; only the built-in ones are translated.
 * @param name the theme name
 */
export function describeThemeName(name: string): UiText | DisplayLine {
  return renderDescribeThemeName(messages, name);
}

/**
 * Re-exported so callers reach the failure reason, built in `i18n/`, and the
 * fold it uses (`display/untrusted-text.ts`) where they read their messages.
 */
export { EXTERNAL_TEXT_MAX_CODE_POINTS, foldExternalText, foldedFailureReason };

/**
 * Builds the notice shown when a window operation fails: the summary, and the
 * detail the daemon reported when anything survives {@link foldExternalText}.
 * @param summary the operation-specific message, returned as is when no detail is left
 * @param detail the rejected value the operation failed with
 */
export function describeOperationFailure<S extends UiText | DisplayLine>(
  summary: S,
  detail: unknown
): S | UiText {
  return renderDescribeOperationFailure(messages, summary, detail);
}

/**
 * Builds the notice shown when a window opened but its detection definition
 * failed to load, so that window's status stays `Unknown`.
 * @param detail the reason the daemon reported
 */
export function describeDetectionWarning(detail: unknown): UiText {
  return renderDescribeDetectionWarning(messages, detail);
}

/**
 * Describes a sidebar row as its accessible name.
 * @param name the window's display name
 * @param status the window's status
 * @param isAwaitingPermission whether the window waits for the user's permission
 */
export function describeWindowRow(
  name: DisplayLine,
  status: string,
  isAwaitingPermission: boolean
): UiText {
  return renderDescribeWindowRow(messages, name, status, isAwaitingPermission);
}

export type { WindowHeaderFacts };

/**
 * Describes a window header as its accessible description.
 * @param facts where the window runs and which marks its header shows
 */
export function describeWindowHeader(facts: WindowHeaderFacts): UiText {
  return renderDescribeWindowHeader(messages, facts);
}

/**
 * Describes where a window runs.
 * @param facts the window's vendor and working directory, prepared for display
 */
export function describeWindowLocation(facts: Pick<WindowHeaderFacts, 'vendor' | 'cwd'>): UiText {
  return renderDescribeWindowLocation(messages, facts);
}

/**
 * Describes a window status for display.
 * @param status the status reported by the daemon
 */
export function describeStatus(status: string): UiText {
  return renderDescribeStatus(messages, status);
}

/**
 * Describes a status label: the status word, followed by the short reason it
 * is unknown when there is one.
 * @param status the status reported by the daemon
 * @param reason the short reason, `null` when the label names none
 */
export function describeStatusLabel(status: string, reason: UiText | null): UiText {
  return renderDescribeStatusLabel(messages, status, reason);
}

/**
 * Describes what is wrong with one window's connection, `null` while nothing
 * is.
 * @param phase the session phase of that window
 */
export function describePaneNotice(phase: string): UiText | null {
  return renderDescribePaneNotice(messages, phase);
}

/**
 * Describes how many keymap entries were rejected while resolving the
 * effective keymap.
 * @param count the number of discarded entries
 */
export function describeDiscardedKeymapEntries(count: number): UiText {
  return renderDescribeDiscardedKeymapEntries(messages, count);
}

/**
 * Describes how many themes-file entries were rejected while parsing the
 * user's themes.
 * @param count the number of rejected entries
 */
export function describeThemeIssues(count: number): UiText {
  return renderDescribeThemeIssues(messages, count);
}

/**
 * Describes the count shown at every "needs you" display location.
 * @param count the number of windows that need the user's attention
 */
export function describeAttentionBadge(count: number): UiText {
  return renderDescribeAttentionBadge(messages, count);
}

/**
 * Describes the reopen offer's text: how many windows from the last
 * session can be reopened, and when that configuration was recorded.
 * @param count the number of restore candidates
 * @param savedAtEpochS when that session was saved, in Unix epoch seconds; `null` when unknown
 */
export function describeReopenNotice(count: number, savedAtEpochS: number | null): UiText {
  return renderDescribeReopenNotice(messages, count, savedAtEpochS);
}

/**
 * Describes the restore dialog's live selection count.
 * @param selected the number of candidates currently checked
 * @param total the total number of candidates offered
 */
export function describeRestoreSelectionCount(selected: number, total: number): UiText {
  return renderDescribeRestoreSelectionCount(messages, selected, total);
}

/**
 * Describes where a restore candidate came from; `null` when it recorded neither.
 * @param profile the candidate's profile, prepared for display
 * @param vendor the candidate's vendor, prepared for display
 */
export function describeRestoreOrigin(
  profile: DisplayLine | null,
  vendor: DisplayLine | null
): UiText | null {
  return renderDescribeRestoreOrigin(messages, profile, vendor);
}

/**
 * Describes the label of the button that reopens the checked windows.
 * @param count the number of candidates currently checked
 */
export function describeRestoreConfirm(count: number): UiText {
  return renderDescribeRestoreConfirm(messages, count);
}

/**
 * Describes a room heading together with how many windows it holds.
 * @param name the room's display name
 * @param count the number of windows in the room
 */
export function describeRoomTitle(name: DisplayLine, count: number): UiText {
  return renderDescribeRoomTitle(messages, name, count);
}

/** Describes the tooltip of a window's waiting-messages mark. */
export function describeQueuedMessages(count: number): UiText {
  return renderDescribeQueuedMessages(messages, count);
}

/**
 * Describes the short reason a status label is followed by when the pair
 * names the `'unknown'` arm; `null` otherwise.
 * @param detected the window's status and the reason it is `'unknown'`
 */
export function describeUnknownReasonSuffix(detected: DetectedStatusView): UiText | null {
  return renderDescribeUnknownReasonSuffix(messages, detected);
}

/**
 * Describes why a window's status is `'unknown'`, for the title-bar tooltip;
 * `null` when the pair names no reason.
 * @param detected the window's status and the reason it is `'unknown'`
 * @param warning the detection-load warning carried alongside the pair
 */
export function describeUnknownExplanation(
  detected: DetectedStatusView,
  warning: DisplayLine | null
): UiText | null {
  return renderDescribeUnknownExplanation(messages, detected, warning);
}

/**
 * Describes how one delivery ended, naming the reason when the outcome is the
 * `'undelivered_unknown'` arm and carries one.
 * @param outcome how the delivery ended, with the reason that arm carries
 */
export function describeUndeliveredUnknownReason(outcome: ChatDeliveryOutcome): UiText {
  return renderDescribeUndeliveredUnknownReason(messages, outcome);
}

/**
 * Describes the interrupt button's tooltip, from the gate's own verdict.
 * @param target the gate's verdict for `interrupt` on that window
 */
export function describeInterruptHint(target: ConfirmTarget): UiText {
  return renderDescribeInterruptHint(messages, target);
}

/**
 * Describes why an action does nothing right now.
 * @param reason the reason the availability judgment gave
 */
export function describeActionBlock(reason: ActionBlockReason): UiText {
  return renderDescribeActionBlock(messages, reason);
}

export type { PickerWindowRowFacts };

/**
 * Describes a picker window row as its accessible name.
 * @param facts the window's name, status and place, and whether it is the current one
 */
export function describePickerWindowRow(facts: PickerWindowRowFacts): UiText {
  return renderDescribePickerWindowRow(messages, facts);
}

/**
 * Describes a palette action row as its accessible name.
 * @param name the action's name
 * @param key the key as the row shows it, `null` when the action has none
 */
export function describePickerActionRow(name: UiText, key: UiText | null): UiText {
  return renderDescribePickerActionRow(messages, name, key);
}

/**
 * Describes why the picker lists nothing, or only the New window offer.
 * @param empty the picker's empty state
 */
export function describePickerEmpty(empty: NonNullable<PickerEmpty>): {
  title: UiText;
  hint: UiText | null;
} {
  return renderDescribePickerEmpty(messages, empty);
}

/**
 * Describes what the picker lists, for the screen reader.
 * @param mode the mode the rows are in
 * @param rowCount how many rows are listed
 * @param empty the picker's empty state
 */
export function describePickerResults(
  mode: PickerMode,
  rowCount: number,
  empty: PickerEmpty
): UiText {
  return renderDescribePickerResults(messages, mode, rowCount, empty);
}
