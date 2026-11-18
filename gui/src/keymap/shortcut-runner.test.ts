/**
 * What each shortcut action does once the dispatcher has matched it: which
 * window it acts on, where the next / previous window is, and where keyboard
 * focus goes afterwards. The current window is injected; how it is derived is
 * measured in `current-window.test.ts`.
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
  /** The current window of the work view; defaults to none. */
  current?: Win | null;
  /** Defaults to Canvas. */
  workMode?: 'focus' | 'canvas';
  attentionTarget?: Win | null;
}

interface Harness {
  run: (action: ActionId) => Promise<void>;
  calls: string[];
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
        minimized: 0,
        collapsed: true,
      });
    }
  }
  return groups;
}

function harness(setup: Setup): Harness {
  const calls: string[] = [];
  const live: Win[] = [...setup.windows];
  const order = setup.order ?? setup.windows;
  const deps: ShortcutRunnerDeps<Win> = {
    current: (): Win | null => setup.current ?? null,
    workMode: (): 'focus' | 'canvas' => setup.workMode ?? 'canvas',
    displayOrder: (): readonly Win[] => order.filter((entry) => live.includes(entry)),
    groups: (): readonly RoomGroup<Win>[] =>
      groupsOf(order.filter((entry) => live.includes(entry))),
    expandRoom: (roomId: string): void => {
      calls.push(`expand:${roomId}`);
    },
    actions: {
      close: async (entry: Win): Promise<void> => {
        calls.push(`close:${entry.windowId}`);
        await Promise.resolve();
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
  return { run, calls };
}

// B sits between A and C in the sidebar; C holds the highest zOrder, so a
// runner deriving the target from the placement would act on C instead of B.
const A = win('wa-k3f9xa', 1, 'room-a');
const B = win('wb-p0d8qz', 2, 'room-b');
const C = win('wc-m7t2rv', 3, 'room-c');

const EXPECTED_CALLS: Readonly<Record<ActionId, readonly string[]>> = {
  'focus-next-window': ['expand:room-c', `focus:${C.windowId}`, `terminal:${C.windowId}`],
  'focus-previous-window': ['expand:room-a', `focus:${A.windowId}`, `terminal:${A.windowId}`],
  'focus-next-attention': ['attention'],
  'open-window-form': ['open-form'],
  'close-window': [`close:${B.windowId}`],
  'interrupt-window': [`interrupt:${B.windowId}`],
  'minimize-window': [`minimize:${B.windowId}`],
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
    const h = harness({ windows: [A, B, C], current: B, attentionTarget: C });
    await h.run(action);
    expect(h.calls).toEqual(EXPECTED_CALLS[action]);
  });
});

describe('next and previous window', () => {
  it('walk the sidebar order, not the order windows were opened in', async () => {
    const order = [A, C, B];
    const next = harness({ windows: [A, B, C], order, current: B });
    await next.run('focus-next-window');
    expect(next.calls).toContain(`focus:${A.windowId}`);

    const previous = harness({ windows: [A, B, C], order, current: B });
    await previous.run('focus-previous-window');
    expect(previous.calls).toContain(`focus:${C.windowId}`);
  });

  it('wrap around at both ends', async () => {
    const top = win('wt-h1s5ld', 9, 'room-a');
    const next = harness({ windows: [A, C, top], order: [A, C, top], current: top });
    await next.run('focus-next-window');
    expect(next.calls).toContain(`focus:${A.windowId}`);

    const first = win('wf-q8n3bc', 9, 'room-a');
    const previous = harness({ windows: [first, A, C], order: [first, A, C], current: first });
    await previous.run('focus-previous-window');
    expect(previous.calls).toContain(`focus:${C.windowId}`);
  });

  it('start from the ends when there is no current window, and bring a minimized target back', async () => {
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
  it('is the current window it is given, whatever the placement', async () => {
    const maximized = win('wm-r4c6yu', 9, 'room-a', { isMaximized: true });
    const h = harness({ windows: [maximized, A, C], current: A });
    await h.run('close-window');
    await h.run('interrupt-window');
    expect(h.calls).toEqual([`close:${A.windowId}`, `interrupt:${A.windowId}`]);
  });

  it.each<ActionId>([
    'close-window',
    'interrupt-window',
    'minimize-window',
    'toggle-maximize-window',
  ])('%s does nothing when there is no current window', async (action: ActionId) => {
    const h = harness({ windows: [A, B, C], current: null });
    await h.run(action);
    expect(h.calls).toEqual([]);
  });
});

describe('in Focus', () => {
  it.each<ActionId>(['minimize-window', 'toggle-maximize-window'])(
    '%s does nothing (Focus has no minimize or maximize)',
    async (action: ActionId) => {
      const h = harness({ windows: [A, B, C], current: B, workMode: 'focus' });
      await h.run(action);
      expect(h.calls).toEqual([]);
    }
  );

  it.each<ActionId>(['close-window', 'interrupt-window', 'focus-next-window'])(
    '%s still acts as in Canvas',
    async (action: ActionId) => {
      const h = harness({ windows: [A, B, C], current: B, workMode: 'focus' });
      await h.run(action);
      expect(h.calls).toEqual(EXPECTED_CALLS[action]);
    }
  );
});

describe('focus after an operation', () => {
  it('leaves the terminal focus after the attention jump to the jump itself', async () => {
    const found = harness({ windows: [A, B, C], current: B, attentionTarget: C });
    await found.run('focus-next-attention');
    expect(found.calls).toEqual(['attention']);

    const none = harness({ windows: [A, B], current: B, attentionTarget: null });
    await none.run('focus-next-attention');
    expect(none.calls).toEqual(['attention']);
  });
});
