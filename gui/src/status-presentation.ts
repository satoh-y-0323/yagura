import type { IconName } from './icons/icon-paths';
import type { WindowStatus } from './ipc/session-boundary';

/** The colour family of a status: it names the `--st-<role>` token its icon is drawn in. */
export type StatusRole = 'approval' | 'input' | 'running' | 'halted' | 'unknown' | 'idle';

export interface StatusPresentation {
  /** Attention rank: 0 is the most urgent. Ranks are distinct across the seven statuses. */
  rank: number;
  icon: IconName;
  role: StatusRole;
  /** Only a running window moves. */
  motion: 'spin' | 'none';
  /** Whether the status asks for the user. The one place that decides it. */
  needsYou: boolean;
}

/**
 * How every status looks. Written as a `Record`, so adding a status fails to
 * compile here instead of silently falling through. The status words live in
 * the i18n resources (`describeStatus`).
 */
export const STATUS_PRESENTATION: Record<WindowStatus, StatusPresentation> = {
  waiting_approval: {
    rank: 0,
    icon: 'shield-exclamation-point',
    role: 'approval',
    motion: 'none',
    needsYou: true,
  },
  waiting_input: {
    rank: 1,
    icon: 'message-circle',
    role: 'input',
    motion: 'none',
    needsYou: true,
  },
  // Ranked above the quiet statuses because a wait may hide behind it, yet it
  // does not count as needing the user: an unconfirmed state must not call anyone.
  unknown: { rank: 2, icon: 'circle-dashed', role: 'unknown', motion: 'none', needsYou: false },
  interrupted: { rank: 3, icon: 'circle-stop', role: 'halted', motion: 'none', needsYou: false },
  running: { rank: 4, icon: 'loader-circle', role: 'running', motion: 'spin', needsYou: false },
  idle: { rank: 5, icon: 'circle', role: 'idle', motion: 'none', needsYou: false },
  exited: { rank: 6, icon: 'circle-x', role: 'idle', motion: 'none', needsYou: false },
};
