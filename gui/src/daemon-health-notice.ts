import type { DaemonProblem, DaemonProblemKind } from './ipc/session-boundary';
import { banners } from './ui-messages';

/**
 * The sentence the banner shows for each kind of problem. The daemon's own
 * `detail` is not shown: it may name private paths, and the sentence already
 * says what to do.
 */
const NOTICES: Readonly<Record<DaemonProblemKind, string>> = {
  windows_unavailable: banners.daemonWindowsUnavailable,
};

/** The sentence the banner shows for each problem the daemon reports. */
export function daemonProblemNotices(problems: readonly DaemonProblem[]): string[] {
  return problems.map((problem) => NOTICES[problem.kind]);
}
