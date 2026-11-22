/**
 * How a failed command is read once it is caught: the failure behind the
 * caught value and which line the user sees.
 *
 * The line is picked by the failure's kind alone. A request the gate refused
 * for the daemon's wire revision is caused by the version difference, so
 * every operation shows the same incompatible-daemon sentence; any other
 * failure is the operation's own and shows the operation's line.
 */

import { caughtValueText } from './display/untrusted-text';
import { CommandFailureError, type CommandFailure } from './ipc/session-boundary';
import { banners } from './ui-messages';

/**
 * The failure behind a caught value. A value no command door raised -- a
 * malformed reply, or anything the work around the call threw -- still has
 * text worth showing, so it becomes the failure that carries text alone.
 *
 * @param error the value a failed operation rejected with
 */
export function commandFailureOf(error: unknown): CommandFailure {
  if (error instanceof CommandFailureError) {
    return error.failure;
  }
  return { kind: 'bare', text: caughtValueText(error) };
}

/**
 * The `error.code` of a request the daemon refused, `null` for any other
 * failure.
 *
 * @param error the value a failed operation rejected with
 */
export function commandRejectionCode(error: unknown): string | null {
  const failure = commandFailureOf(error);
  return failure.kind === 'rejected' ? failure.code : null;
}

/**
 * The line shown for a failed operation.
 *
 * @param error the value the operation rejected with
 * @param describe builds the operation's own line, for every failure the
 * daemon's wire revision did not cause
 */
export function commandFailureLine(error: unknown, describe: () => string): string {
  return lineForFailure(commandFailureOf(error), describe);
}

/**
 * The line shown for a failure already read out of what was caught, by the
 * same rule as {@link commandFailureLine}.
 *
 * @param failure why the operation failed
 * @param describe builds the operation's own line
 */
export function lineForFailure(failure: CommandFailure, describe: () => string): string {
  return failure.kind === 'unsupported' ? banners.daemonIncompatible : describe();
}
