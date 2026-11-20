import type { WindowStatus } from '../ipc/session-boundary';
import type { NoticeClass } from './notice-model';

/** The statuses a window calls the user with. */
export type NeedsYouToastStatus = Extract<WindowStatus, 'waiting_approval' | 'waiting_input'>;

/** What a step reads of one window as the shell lists it. */
export interface NeedsYouToastWindow {
  windowId: string;
  status: WindowStatus;
  /** The window's `needsYou` decision (`status-tone.ts`), taken as given. */
  needsYou: boolean;
}

/** One window's call for the user. */
export interface NeedsYouToast {
  windowId: string;
  status: NeedsYouToastStatus;
  tone: NoticeClass['tone'];
  /** Whether the toast goes with time; an approval wait stays until acted on. */
  timed: boolean;
  /** Changes each time the window arrives, so a running timer starts over. */
  arrival: number;
  /** Changes only when the status changed, so the sentence is announced again only then. */
  sentenceRev: number;
  /** The order the window first came in; a replacement keeps it. */
  entered: number;
}

/** The toasts calling the user, in display order. */
export interface NeedsYouToastState {
  /** Approval waits before input waits, each in the order the windows came in. */
  readonly toasts: readonly NeedsYouToast[];
  /** The next value handed out for `arrival`, `sentenceRev` and `entered`; it only grows. */
  readonly nextMark: number;
}

/** What one step is decided from. */
export interface NeedsYouToastInput {
  /** The windows `observeNeedsYouArrivals` named in this observation. */
  arrived: readonly string[];
  windows: readonly NeedsYouToastWindow[];
  /** The window being looked at, `null` when none is. */
  seenWindowId: string | null;
  /** The window whose toast the user closed. */
  dismissed?: string;
}

/** The state a shell starts with: no toast. */
export function emptyNeedsYouToastState(): NeedsYouToastState {
  return { toasts: [], nextMark: 0 };
}

function isCallStatus(status: WindowStatus): status is NeedsYouToastStatus {
  return status === 'waiting_approval' || status === 'waiting_input';
}

function callOrder(status: NeedsYouToastStatus): number {
  return status === 'waiting_approval' ? 0 : 1;
}

/**
 * Applies one observation to the toasts. A toast is put up or replaced only
 * when its window arrives, so a closed toast stays away until the next
 * arrival without any memory of the close. A toast goes when its window is
 * being looked at, no longer needs the user, or is gone from the list.
 * @param state the state returned by the previous step
 * @param input this observation
 */
export function stepNeedsYouToasts(
  state: NeedsYouToastState,
  input: NeedsYouToastInput
): NeedsYouToastState {
  const byId = new Map(input.windows.map((window) => [window.windowId, window]));
  let nextMark = state.nextMark;
  // The close is applied before this step's arrivals: an arrival in the same
  // step is newer than the toast the user closed, so it puts one up again.
  const toasts = new Map(
    state.toasts
      .filter((toast) => toast.windowId !== input.dismissed)
      .map((toast) => [toast.windowId, toast])
  );

  for (const windowId of input.arrived) {
    const window = byId.get(windowId);
    if (window === undefined || !window.needsYou || !isCallStatus(window.status)) {
      continue;
    }
    const before = toasts.get(windowId);
    const arrival = nextMark++;
    const status = window.status;
    const timed = status === 'waiting_input';
    toasts.set(windowId, {
      windowId,
      status,
      // An input wait passes with time; an approval wait holds until the user acts on it.
      tone: timed ? 'info' : 'warning',
      timed,
      arrival,
      sentenceRev: before !== undefined && before.status === status ? before.sentenceRev : arrival,
      entered: before?.entered ?? arrival,
    });
  }

  const kept = [...toasts.values()].filter((toast) => {
    const window = byId.get(toast.windowId);
    return window !== undefined && window.needsYou && toast.windowId !== input.seenWindowId;
  });
  kept.sort((a, b) => callOrder(a.status) - callOrder(b.status) || a.entered - b.entered);
  return { toasts: kept, nextMark };
}
