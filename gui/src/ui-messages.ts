/**
 * Facade over the i18n layer: binds the resolved locale once and
 * re-exports it under the API shape its consumers import.
 */
import { resolveMessages } from './i18n/messages';
import {
  describeAttentionBadge as renderDescribeAttentionBadge,
  describeDeliveryState as renderDescribeDeliveryState,
  describeDetectionWarning as renderDescribeDetectionWarning,
  describeDiscardedKeymapEntries as renderDescribeDiscardedKeymapEntries,
  describeExit as renderDescribeExit,
  describeExitCode as renderDescribeExitCode,
  describeHaltReason as renderDescribeHaltReason,
  describeInterruptHint as renderDescribeInterruptHint,
  describeOperationFailure,
  describePaneNotice as renderDescribePaneNotice,
  describeRestoreBanner as renderDescribeRestoreBanner,
  describeRestoreSelectionCount as renderDescribeRestoreSelectionCount,
  describeRoomTitle as renderDescribeRoomTitle,
  describeStatus as renderDescribeStatus,
  describeThemeIssues as renderDescribeThemeIssues,
  describeThemeName as renderDescribeThemeName,
  describeUndeliveredUnknownReason as renderDescribeUndeliveredUnknownReason,
  describeUnknownBadge as renderDescribeUnknownBadge,
  describeUnknownExplanation as renderDescribeUnknownExplanation,
  describeUnknownReasonSuffix as renderDescribeUnknownReasonSuffix,
  describeWindowListTitle as renderDescribeWindowListTitle,
  foldExternalText,
} from './i18n/render';
import type { ChatDeliveryOutcome, DetectedStatusView } from './ipc/session-boundary';
import type { ConfirmTarget } from './session/confirm-target';

const messages = resolveMessages();

/** Status display strings. */
export const statusLabels = messages.statusLabels;

/** Session phase display strings. */
export const sessionPhaseLabels = messages.sessionPhaseLabels;

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

/** Banner messages. */
export const banners = messages.banners;

/** Tooltips and helper text. */
export const hints = messages.hints;

/** The sidebar's short-form status names. */
export const windowList = messages.windowList;

/** The `⋯` window menu's item labels. */
export const menus = messages.menus;

/** Accessible names for the navigation rail's icons. */
export const nav = messages.nav;

/** The chat panel's text: its chrome, its chips, its banner and its badge. */
export const chat = messages.chat;

/** The plans panel's text: its tabs, board, owner banner and role editor. */
export const plans = messages.plans;

/** The plans panel's Tickets tab: the ticket board and one ticket's details. */
export const tickets = messages.tickets;

/** The settings panel's text: its tabs and the Profiles tab's rows. */
export const settings = messages.settings;

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
export { describeOperationFailure, foldExternalText };

/**
 * Builds the notice shown when a window opened but its detection definition
 * failed to load, so that window's status stays `Unknown`.
 * @param detail the reason the daemon reported
 */
export function describeDetectionWarning(detail: unknown): string {
  return renderDescribeDetectionWarning(messages, detail);
}

/**
 * Describes the window list heading together with how many windows are open.
 * @param count the number of open windows
 */
export function describeWindowListTitle(count: number): string {
  return renderDescribeWindowListTitle(messages, count);
}

/**
 * Describes an exit code for display.
 * @param exitCode the exit code, or null if not yet determined
 */
export function describeExit(exitCode: number | null): string {
  return renderDescribeExit(messages, exitCode);
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
 * Describes an exit code shown next to a window status that already says the
 * process has exited.
 * @param exitCode the exit code of the child process
 */
export function describeExitCode(exitCode: number): string {
  return renderDescribeExitCode(messages, exitCode);
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
 * Describes the restore banner's text: how many windows from the last
 * session are available, and when that configuration was recorded.
 * @param count the number of restore candidates
 * @param timestamp the formatted recording time, `null` when unknown
 */
export function describeRestoreBanner(count: number, timestamp: string | null): string {
  return renderDescribeRestoreBanner(messages, count, timestamp);
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
 * Describes a room heading together with how many windows it holds.
 * @param name the room's display name
 * @param count the number of windows in the room
 */
export function describeRoomTitle(name: string, count: number): string {
  return renderDescribeRoomTitle(messages, name, count);
}

/**
 * Describes the room heading's Unknown-count badge.
 * @param count the number of windows in an unknown state
 */
export function describeUnknownBadge(count: number): string {
  return renderDescribeUnknownBadge(messages, count);
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
