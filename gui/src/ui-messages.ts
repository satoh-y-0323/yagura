/**
 * Facade over the i18n layer: binds the resolved locale once and
 * re-exports it under the API shape its consumers import.
 */
import { resolveMessages } from './i18n/messages';
import {
  describeActionBlock as renderDescribeActionBlock,
  describeAnnouncement as renderDescribeAnnouncement,
  describeAttentionBadge as renderDescribeAttentionBadge,
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
  describeOperationFailure,
  describePaneNotice as renderDescribePaneNotice,
  describePickerActionRow as renderDescribePickerActionRow,
  describePickerEmpty as renderDescribePickerEmpty,
  describePickerResults as renderDescribePickerResults,
  describePickerWindowRow as renderDescribePickerWindowRow,
  describeQueuedMessages as renderDescribeQueuedMessages,
  describeReopenOffer as renderDescribeReopenOffer,
  describeReopenNotice as renderDescribeReopenNotice,
  describeRestoreSelectionCount as renderDescribeRestoreSelectionCount,
  describeRestoreConfirm as renderDescribeRestoreConfirm,
  describeRoomTitle as renderDescribeRoomTitle,
  describeStatus as renderDescribeStatus,
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
  EXTERNAL_TEXT_MAX_CODE_POINTS,
  foldExternalText,
  foldedFailureReason,
  type PickerWindowRowFacts,
  type WindowHeaderFacts,
} from './i18n/render';
import type { TitleParts } from './i18n/messages-en';
import type { PickerEmpty, PickerMode } from './command-picker/picker-model';
import type { ChatDeliveryOutcome, DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import type { ActionBlockReason } from './keymap/action-availability';
import type { ConfirmTarget } from './session/confirm-target';

const messages = resolveMessages();

/** The shape every locale fills, for a locale written outside `i18n/` such as a test's. */
export type { Messages } from './i18n/messages';

/** A sentence split around the name it holds. */
export type { TitleParts };

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

/**
 * Describes the card that stands in for a window that ended without this GUI
 * closing it.
 * @param name the window's display name
 * @param exitCode the exit code, or null if not yet determined
 */
export function describeExitedWindow(name: string, exitCode: number | null): string {
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
export function describeGoToWindow(name: string): string {
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
): string {
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
export function describeNoticeAndMore(sentence: string, count: number): string {
  return renderDescribeNoticeAndMore(messages, sentence, count);
}

/**
 * Describes when a notification event was recorded.
 * @param date the moment it was recorded
 */
export function describeEventTime(date: Date): string {
  return renderDescribeEventTime(messages, date);
}

/**
 * Joins what one update reads out into one text.
 * @param sentences what is read, in order
 */
export function describeAnnouncement(sentences: readonly string[]): string {
  return renderDescribeAnnouncement(messages, sentences);
}

/**
 * Describes the empty work area's offer to reopen the windows of the last session.
 * @param count the number of restore candidates
 * @param timestamp the formatted recording time, `null` when unknown
 */
export function describeReopenOffer(count: number, timestamp: string | null): string {
  return renderDescribeReopenOffer(messages, count, timestamp);
}

/**
 * Describes how one chat message landed in one addressed window.
 * @param state the delivery state the daemon reported
 */
export function describeDeliveryState(state: string): string {
  return renderDescribeDeliveryState(messages, state);
}

/**
 * Describes why a chat room stopped.
 * @param reason the halt reason the daemon reported
 */
export function describeHaltReason(reason: string): string {
  return renderDescribeHaltReason(messages, reason);
}

/**
 * Describes a theme name for a select option. A user theme is shown under the
 * name its author gave it; only the built-in ones are translated.
 * @param name the theme name
 */
export function describeThemeName(name: string): string {
  return renderDescribeThemeName(messages, name);
}

/**
 * Re-exported so callers outside `i18n/` reach the sanitizer through this
 * facade: folding external text is not message resolution, but the boundary
 * contract keeps `i18n/` imports to this one module.
 */
export {
  describeOperationFailure,
  EXTERNAL_TEXT_MAX_CODE_POINTS,
  foldExternalText,
  foldedFailureReason,
};

/**
 * Builds the notice shown when a window opened but its detection definition
 * failed to load, so that window's status stays `Unknown`.
 * @param detail the reason the daemon reported
 */
export function describeDetectionWarning(detail: unknown): string {
  return renderDescribeDetectionWarning(messages, detail);
}

/**
 * Describes a sidebar row as its accessible name.
 * @param name the window's display name
 * @param status the window's status
 * @param isAwaitingPermission whether the window waits for the user's permission
 */
export function describeWindowRow(
  name: string,
  status: string,
  isAwaitingPermission: boolean
): string {
  return renderDescribeWindowRow(messages, name, status, isAwaitingPermission);
}

export type { WindowHeaderFacts };

/**
 * Describes a window header as its accessible description.
 * @param facts where the window runs and which marks its header shows
 */
export function describeWindowHeader(facts: WindowHeaderFacts): string {
  return renderDescribeWindowHeader(messages, facts);
}

/**
 * Describes where a window runs.
 * @param facts the window's vendor and working directory, prepared for display
 */
export function describeWindowLocation(facts: Pick<WindowHeaderFacts, 'vendor' | 'cwd'>): string {
  return renderDescribeWindowLocation(messages, facts);
}

/**
 * Describes a window status for display.
 * @param status the status reported by the daemon
 */
export function describeStatus(status: string): string {
  return renderDescribeStatus(messages, status);
}

/**
 * Describes what is wrong with one window's connection, `null` while nothing
 * is.
 * @param phase the session phase of that window
 */
export function describePaneNotice(phase: string): string | null {
  return renderDescribePaneNotice(messages, phase);
}

/**
 * Describes how many keymap entries were rejected while resolving the
 * effective keymap.
 * @param count the number of discarded entries
 */
export function describeDiscardedKeymapEntries(count: number): string {
  return renderDescribeDiscardedKeymapEntries(messages, count);
}

/**
 * Describes how many themes-file entries were rejected while parsing the
 * user's themes.
 * @param count the number of rejected entries
 */
export function describeThemeIssues(count: number): string {
  return renderDescribeThemeIssues(messages, count);
}

/**
 * Describes the count shown at every "needs you" display location.
 * @param count the number of windows that need the user's attention
 */
export function describeAttentionBadge(count: number): string {
  return renderDescribeAttentionBadge(messages, count);
}

/**
 * Describes the reopen offer's text: how many windows from the last
 * session can be reopened, and when that configuration was recorded.
 * @param count the number of restore candidates
 * @param timestamp the formatted recording time, `null` when unknown
 */
export function describeReopenNotice(count: number, timestamp: string | null): string {
  return renderDescribeReopenNotice(messages, count, timestamp);
}

/**
 * Describes the restore dialog's live selection count.
 * @param selected the number of candidates currently checked
 * @param total the total number of candidates offered
 */
export function describeRestoreSelectionCount(selected: number, total: number): string {
  return renderDescribeRestoreSelectionCount(messages, selected, total);
}

/**
 * Describes the label of the button that reopens the checked windows.
 * @param count the number of candidates currently checked
 */
export function describeRestoreConfirm(count: number): string {
  return renderDescribeRestoreConfirm(messages, count);
}

/**
 * Describes a room heading together with how many windows it holds.
 * @param name the room's display name
 * @param count the number of windows in the room
 */
export function describeRoomTitle(name: string, count: number): string {
  return renderDescribeRoomTitle(messages, name, count);
}

/** Describes the tooltip of a window's waiting-messages mark. */
export function describeQueuedMessages(count: number): string {
  return renderDescribeQueuedMessages(messages, count);
}

/**
 * Describes the suffix a status label appends when the pair names the
 * `'unknown'` arm.
 * @param detected the window's status and the reason it is `'unknown'`
 */
export function describeUnknownReasonSuffix(detected: DetectedStatusView): string {
  return renderDescribeUnknownReasonSuffix(messages, detected);
}

/**
 * Describes why a window's status is `'unknown'`, for the title-bar tooltip.
 * @param detected the window's status and the reason it is `'unknown'`
 * @param warning the detection-load warning carried alongside the pair
 */
export function describeUnknownExplanation(
  detected: DetectedStatusView,
  warning: string | null
): string {
  return renderDescribeUnknownExplanation(messages, detected, warning);
}

/**
 * Describes how one delivery ended, naming the reason when the outcome is the
 * `'undelivered_unknown'` arm and carries one.
 * @param outcome how the delivery ended, with the reason that arm carries
 */
export function describeUndeliveredUnknownReason(outcome: ChatDeliveryOutcome): string {
  return renderDescribeUndeliveredUnknownReason(messages, outcome);
}

/**
 * Describes the interrupt button's tooltip, from the gate's own verdict.
 * @param target the gate's verdict for `interrupt` on that window
 */
export function describeInterruptHint(target: ConfirmTarget): string {
  return renderDescribeInterruptHint(messages, target);
}

/**
 * Describes why an action does nothing right now.
 * @param reason the reason the availability judgment gave
 */
export function describeActionBlock(reason: ActionBlockReason): string {
  return renderDescribeActionBlock(messages, reason);
}

export type { PickerWindowRowFacts };

/**
 * Describes a picker window row as its accessible name.
 * @param facts the window's name, status and place, and whether it is the current one
 */
export function describePickerWindowRow(facts: PickerWindowRowFacts): string {
  return renderDescribePickerWindowRow(messages, facts);
}

/**
 * Describes a palette action row as its accessible name.
 * @param name the action's name
 * @param key the key as the row shows it, `null` when the action has none
 */
export function describePickerActionRow(name: string, key: string | null): string {
  return renderDescribePickerActionRow(messages, name, key);
}

/**
 * Describes why the picker lists nothing, or only the New window offer.
 * @param empty the picker's empty state
 */
export function describePickerEmpty(empty: NonNullable<PickerEmpty>): {
  title: string;
  hint: string | null;
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
): string {
  return renderDescribePickerResults(messages, mode, rowCount, empty);
}
