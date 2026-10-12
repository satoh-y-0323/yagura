/**
 * What each shortcut action does once the dispatcher has matched it: which
 * window it acts on, where the next / previous window is, and where keyboard
 * focus goes afterwards.
 */
import { describe, expect, it } from 'vitest';

import type { PlacedWindow } from '../session/current-window';
import type { RoomGroup } from '../sidebar/room-groups';
import { ACTION_IDS, type ActionId } from './key-binding';
import { createShortcutRunner, type ShortcutRunnerDeps } from './shortcut-runner';

interface Win extends PlacedWindow {
  roomId: string;
}

function win(
  windowId: string,
  zOrder: number,
  roomId: string,
  state: { isMinimized?: boolean; isMaximized?: boolean } = {}
): Win {
  return {
    windowId,
    roomId,
    display: {
      zOrder,
      isMinimized: state.isMinimized ?? false,
      isMaximized: state.isMaximized ?? false,
    },
  };
}

interface Setup {
  windows: Win[];
  /** The sidebar order; defaults to `windows`. */
  order?: Win[];
  attentionTarget?: Win | null;
  /** Whether a close removes the window (false models a refused confirmation). */
  closeRemoves?: boolean;
  /** Holds each close until the test releases it. */
  holdClose?: boolean;
}

interface Harness {
  run: (action: ActionId) => Promise<void>;
  calls: string[];
  releaseClose: () => void;
}

/** The sidebar's rooms over `order`, each room placed where its first window is. */
function groupsOf(order: readonly Win[]): RoomGroup<Win>[] {
  const groups: RoomGroup<Win>[] = [];
  for (const entry of order) {
    const found = groups.find((group) => group.roomId === entry.roomId);
    if (found !== undefined) {
      found.entries.push(entry);
    } else {
      groups.push({
        roomId: entry.roomId,
        name: entry.roomId,
        entries: [entry],
        attention: 0,
        unknown: 0,
        minimized: 0,
        collapsed: true,
      });
    }
  }
  return groups;
}

function harness(setup: Setup): Harness {
  const calls: string[] = [];
  let live: Win[] = [...setup.windows];
  const order = setup.order ?? setup.windows;
  let releaseClose: () => void = () => {};
  const deps: ShortcutRunnerDeps<Win> = {
    windows: (): readonly Win[] => live,
    displayOrder: (): readonly Win[] => order.filter((entry) => live.includes(entry)),
    groups: (): readonly RoomGroup<Win>[] =>
      groupsOf(order.filter((entry) => live.includes(entry))),
    expandRoom: (roomId: string): void => {
      calls.push(`expand:${roomId}`);
    },
    actions: {
      close: async (entry: Win): Promise<void> => {
        calls.push(`close:${entry.windowId}`);
        if (setup.holdClose === true) {
          await new Promise<void>((resolve) => {
            releaseClose = resolve;
          });
        }
        if (setup.closeRemoves !== false) {
          live = live.filter((other) => other.windowId !== entry.windowId);
        }
      },
      interrupt: async (entry: Win): Promise<void> => {
        calls.push(`interrupt:${entry.windowId}`);
        await Promise.resolve();
      },
      minimize: (entry: Win): void => {
        calls.push(`minimize:${entry.windowId}`);
      },
      maximize: (entry: Win): void => {
        calls.push(`maximize:${entry.windowId}`);
      },
      focus: (entry: Win): void => {
        calls.push(`focus:${entry.windowId}`);
      },
    },
    goToNextWindowNeedingYou: (): Win | null => {
      calls.push('attention');
      return setup.attentionTarget ?? null;
    },
    focusTerminal: (windowId: string): void => {
      calls.push(`terminal:${windowId}`);
    },
    restoreFocus: (): void => {
      calls.push('restore');
    },
    openWindowForm: (): void => {
      calls.push('open-form');
    },
    toggleChat: async (): Promise<void> => {
      calls.push('chat');
      await Promise.resolve();
    },
    togglePlans: async (): Promise<void> => {
      calls.push('plans');
      await Promise.resolve();
    },
    toggleSettings: async (): Promise<void> => {
      calls.push('settings');
      await Promise.resolve();
    },
    toggleSidebarFocus: (): void => {
      calls.push('sidebar');
    },
  };
  const run = createShortcutRunner(deps);
  return { run, calls, releaseClose: () => releaseClose() };
}

// B is the current window (highest zOrder) and sits between A and C in the sidebar.
const A = win('wa-k3f9xa', 1, 'room-a');
const B = win('wb-p0d8qz', 3, 'room-b');
const C = win('wc-m7t2rv', 2, 'room-c');

const EXPECTED_CALLS: Readonly<Record<ActionId, readonly string[]>> = {
  'focus-next-window': ['expand:room-c', `focus:${C.windowId}`, `terminal:${C.windowId}`],
  'focus-previous-window': ['expand:room-a', `focus:${A.windowId}`, `terminal:${A.windowId}`],
  'focus-next-attention': ['attention'],
  'open-window-form': ['open-form'],
  'close-window': [`close:${B.windowId}`, 'restore'],
  'interrupt-window': [`interrupt:${B.windowId}`],
  'minimize-window': [`minimize:${B.windowId}`, 'restore'],
  'toggle-maximize-window': [`maximize:${B.windowId}`],
  'toggle-sidebar-focus': ['sidebar'],
  'toggle-chat': ['chat'],
  'toggle-plans': ['plans'],
  'toggle-settings': ['settings'],
};

describe('every action', () => {
  it('has an expectation for each action id and none other', () => {
    expect(Object.keys(EXPECTED_CALLS).sort()).toEqual([...ACTION_IDS].sort());
  });

  it.each([...ACTION_IDS])('%s reaches what it stands for, in order', async (action: ActionId) => {
    const h = harness({ windows: [A, B, C], attentionTarget: C });
    await h.run(action);
    expect(h.calls).toEqual(EXPECTED_CALLS[action]);
  });
});

describe('next and previous window', () => {
  it('walk the sidebar order, not the order windows were opened in', async () => {
    const order = [A, C, B];
    const next = harness({ windows: [A, B, C], order });
    await next.run('focus-next-window');
    expect(next.calls).toContain(`focus:${A.windowId}`);

    const previous = harness({ windows: [A, B, C], order });
    await previous.run('focus-previous-window');
    expect(previous.calls).toContain(`focus:${C.windowId}`);
  });

  it('wrap around at both ends', async () => {
    const top = win('wt-h1s5ld', 9, 'room-a');
    const next = harness({ windows: [A, C, top], order: [A, C, top] });
    await next.run('focus-next-window');
    expect(next.calls).toContain(`focus:${A.windowId}`);

    const first = win('wf-q8n3bc', 9, 'room-a');
    const previous = harness({ windows: [first, A, C], order: [first, A, C] });
    await previous.run('focus-previous-window');
    expect(previous.calls).toContain(`focus:${C.windowId}`);
  });

  it('start from the ends when no window is on screen, and bring a minimized target back', async () => {
    const hiddenA = win('wa-k3f9xa', 1, 'room-a', { isMinimized: true });
    const hiddenC = win('wc-m7t2rv', 2, 'room-c', { isMinimized: true });
    const next = harness({ windows: [hiddenA, hiddenC] });
    await next.run('focus-next-window');
    expect(next.calls).toEqual([
      'expand:room-a',
      `focus:${hiddenA.windowId}`,
      `terminal:${hiddenA.windowId}`,
    ]);

    const previous = harness({ windows: [hiddenA, hiddenC] });
    await previous.run('focus-previous-window');
    expect(previous.calls).toEqual([
      'expand:room-c',
      `focus:${hiddenC.windowId}`,
      `terminal:${hiddenC.windowId}`,
    ]);
  });

  it('do nothing when there is no window at all', async () => {
    const h = harness({ windows: [] });
    await h.run('focus-next-window');
    await h.run('focus-previous-window');
    expect(h.calls).toEqual([]);
  });
});

describe('the window an operation acts on', () => {
  it('is the maximized window even when another has a higher zOrder', async () => {
    const maximized = win('wm-r4c6yu', 1, 'room-a', { isMaximized: true });
    const h = harness({ windows: [maximized, B] });
    await h.run('interrupt-window');
    expect(h.calls).toEqual([`interrupt:${maximized.windowId}`]);
  });

  it('is never a minimized window, whatever its zOrder', async () => {
    const minimized = win('wn-z2e7wo', 9, 'room-a', { isMinimized: true });
    const h = harness({ windows: [minimized, C] });
    await h.run('toggle-maximize-window');
    expect(h.calls).toEqual([`maximize:${C.windowId}`]);
  });

  it.each<ActionId>([
    'close-window',
    'interrupt-window',
    'minimize-window',
    'toggle-maximize-window',
  ])('%s does nothing when no window is on screen', async (action: ActionId) => {
    const hidden = win('wh-g5v1pk', 4, 'room-a', { isMinimized: true });
    const h = harness({ windows: [hidden] });
    await h.run(action);
    expect(h.calls).toEqual([]);
  });
});

describe('focus after an operation', () => {
  it('returns focus after a close only when the window is gone', async () => {
    const refused = harness({ windows: [A, B], closeRemoves: false });
    await refused.run('close-window');
    expect(refused.calls).toEqual([`close:${B.windowId}`]);
  });

  it('returns focus only after the close has finished', async () => {
    const h = harness({ windows: [A, B], holdClose: true });
    const running = h.run('close-window');
    await Promise.resolve();
    expect(h.calls).toEqual([`close:${B.windowId}`]);

    h.releaseClose();
    await running;
    expect(h.calls).toEqual([`close:${B.windowId}`, 'restore']);
  });

  it('leaves the terminal focus after the attention jump to the jump itself', async () => {
    const found = harness({ windows: [A, B, C], attentionTarget: C });
    await found.run('focus-next-attention');
    expect(found.calls).toEqual(['attention']);

    const none = harness({ windows: [A, B], attentionTarget: null });
    await none.run('focus-next-attention');
    expect(none.calls).toEqual(['attention']);
  });
});
