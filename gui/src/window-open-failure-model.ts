/**
 * How a refused window launch is presented, and what it costs the launch
 * memory. Both are value -> value mappings kept out of the components, which
 * have no rendering harness to measure them through.
 *
 * Display wording is taken as an argument: only `ui-messages.ts` may reach
 * the i18n resources.
 */

import { WindowOpenFailureError, type WindowOpenFailure } from './ipc/session-boundary';
// The same folding and capping every other operation failure goes through:
// the reason is external text of the same kind, so it gets the same
// treatment rather than a second copy of it. `errors` is the fixed line for
// the wire-revision gate's refusal; both are read through
// the facade, never i18n/ directly.
import { describeOperationFailure, errors } from './ui-messages';

/** The argument name the core reports for a refused working directory. */
const WORKING_DIRECTORY_ARGUMENT = 'cwd';

/**
 * The failure behind a caught error.
 *
 * An error the open-window boundary did not raise -- a malformed reply, or
 * anything the work around the call threw -- still has text worth showing, so
 * it becomes the arm that carries text alone rather than being discarded.
 *
 * @param error the value a failed launch rejected with
 */
export function windowOpenFailureOf(error: unknown): WindowOpenFailure {
  if (error instanceof WindowOpenFailureError) {
    return error.failure;
  }
  return { kind: 'bare', message: error instanceof Error ? error.message : String(error) };
}

/**
 * Builds the line shown when a window could not be opened.
 *
 * A rejection whose reason text carries nothing falls back to the code the
 * daemon answered with: the summary alone would name no cause, while the code
 * is the one handle left on the refusal. Whether the reason carries anything
 * is decided by the formatter rather than restated here.
 *
 * @param summary the operation-specific wording, supplied by the caller
 * @param failure why the attempt failed
 */
export function describeWindowOpenFailure(summary: string, failure: WindowOpenFailure): string {
  if (failure.kind === 'unsupported') {
    // The wire tag this arm carries (`requestType`) is machine-readable, not
    // for display, so the line comes from a fixed i18n string rather than
    // the summary/detail join every other shape goes through.
    return describeOperationFailure(summary, errors.launchOptionUnsupported);
  }
  const line = describeOperationFailure(summary, failure.message);
  if (failure.kind !== 'rejected' || line !== summary) {
    return line;
  }
  return describeOperationFailure(summary, failure.code);
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
export function shouldDropRememberedCwd(failure: WindowOpenFailure): boolean {
  return failure.kind === 'rejected' && failure.rejectedArgument === WORKING_DIRECTORY_ARGUMENT;
}
