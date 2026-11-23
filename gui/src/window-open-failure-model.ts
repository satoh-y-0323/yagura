/**
 * How a refused window launch is presented, and what it costs the launch
 * memory. Both are value -> value mappings kept out of the components, which
 * have no rendering harness to measure them through.
 *
 * Display wording is taken as an argument: only `ui-messages.ts` may reach
 * the i18n resources.
 */

import { lineForFailure } from './command-failure-model';
import type { UiText } from './display/ui-text';
import type { DisplayLine } from './display/untrusted-text';
import { NAME_TAKEN_CODE, type CommandFailure } from './ipc/session-boundary';
// The same folding and capping every other operation failure goes through:
// the reason is external text of the same kind, so it gets the same
// treatment rather than a second copy of it. All are read through the facade,
// never i18n/ directly.
import { describeOperationFailure, errors } from './ui-messages';

/** The argument name the core reports for a refused working directory. */
const WORKING_DIRECTORY_ARGUMENT = 'cwd';

/**
 * Builds the line shown when a window could not be opened.
 *
 * A rejection shows the daemon's own reason rather than the whole sentence,
 * and one whose reason carries nothing falls back to the code the daemon
 * answered with: the summary alone would name no cause, while the code is the
 * one handle left on the refusal. Whether the reason carries anything is
 * decided by the formatter rather than restated here.
 *
 * A taken name is followed by how to get past it: an empty name gets the
 * default name, which the daemon numbers instead of refusing.
 *
 * @param summary the operation-specific wording, or the window's display name, supplied by the caller
 * @param failure why the attempt failed
 */
export function describeWindowOpenFailure(
  summary: UiText | DisplayLine,
  failure: CommandFailure
): UiText | DisplayLine {
  return lineForFailure(failure, () => describeLaunchFailure(summary, failure));
}

/** The launch's own line for a failure the daemon's wire revision did not cause. */
function describeLaunchFailure(
  summary: UiText | DisplayLine,
  failure: CommandFailure
): UiText | DisplayLine {
  if (failure.kind !== 'rejected') {
    return describeOperationFailure(summary, failure.text);
  }
  const line = describeOperationFailure(summary, failure.message);
  const reason = line !== summary ? line : describeOperationFailure(summary, failure.code);
  return failure.code === NAME_TAKEN_CODE ? errors.nameTakenOpenFailure(reason) : reason;
}

/**
 * Whether the remembered working directory is still worth offering after a
 * failed launch.
 *
 * Only the argument the daemon named decides this. The reason text is not
 * consulted: a refusal that merely mentions a directory is not a refusal of
 * the one that was sent, and a refusal of the one that was sent need not
 * mention it.
 *
 * @param failure why the attempt failed
 */
export function shouldDropRememberedCwd(failure: CommandFailure): boolean {
  return failure.kind === 'rejected' && failure.rejectedArgument === WORKING_DIRECTORY_ARGUMENT;
}
