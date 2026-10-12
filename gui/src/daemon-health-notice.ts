import type { DaemonProblem } from './ipc/session-boundary';
import { banners } from './ui-messages';

/**
 * The sentence the banner shows for each problem the daemon reports. The
 * daemon's own `detail` is shown only where the sentence has nothing else to
 * say about the cause.
 */
export function daemonProblemNotices(problems: readonly DaemonProblem[]): string[] {
  return problems.map((problem) =>
    problem.kind === 'windows_unavailable'
      ? banners.daemonWindowsUnavailable
      : banners.daemonInitialWindowFailed(problem.detail)
  );
}
