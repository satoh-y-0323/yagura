/**
 * What each shortcut action does once the dispatcher has matched it: which
 * window it acts on, where the next / previous window is, and where keyboard
 * focus goes afterwards. The current window is injected; how it is derived is
 * measured in `current-window.test.ts`.
 */
import { describe, expect, it } from 'vitest';

import type { WindowStatus } from '../ipc/session-boundary';
import type { PlacedWindow } from '../session/current-window';
import type { ExitedCard } from '../session/shell-state';
import type { RoomGroup } from '../sidebar/room-groups';
import { actionAvailability, type ActionBlockReason } from './action-availability';
import { ACTION_IDS, type ActionId } from './key-binding';
import {
  availabilityContext,
  createShortcutRunner,
  type ShortcutRunnerDeps,
} from './shortcut-runner';

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
  /** The order next / previous walk; defaults to `windows`. */
  order?: Win[];
  /** The sidebar's whole order when a filter narrows `order`; defaults to `order`. */
  sidebar?: Win[];
  /** The current window of the work view; defaults to none. */
  current?: Win | null;
  /** Defaults to Canvas. */
  workMode?: 'focus' | 'canvas';
  attentionTarget?: Win | null;
  /** The card of an ended window Focus shows; defaults to none. */
  exitedCard?: ExitedCard | null;
  /** Each window's status by id; a window not listed is running. */
  statuses?: Readonly<Record<string, WindowStatus>>;
}

interface Harness {
  run: (action: ActionId) => Promise<void>;
  calls: string[];
  deps: ShortcutRunnerDeps<Win>;
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
        kind: 'folder',
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
  const sidebar = setup.sidebar ?? order;
  const deps: ShortcutRunnerDeps<Win> = {
    current: (): Win | null => setup.current ?? null,
    workMode: (): 'focus' | 'canvas' => setup.workMode ?? 'canvas',
    exitedCard: (): ExitedCard | null => setup.exitedCard ?? null,
    displayOrder: (): readonly Win[] => order.filter((entry) => live.includes(entry)),
    groups: (): readonly RoomGroup<Win>[] =>
      groupsOf(sidebar.filter((entry) => live.includes(entry))),
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
      goTo: (entry: Win): void => {
        calls.push(`goTo:${entry.windowId}`);
      },
    },
    goToNextWindowNeedingYou: (): Win | null => {
      calls.push('attention');
      return setup.attentionTarget ?? null;
    },
    nextAttention: (): Win | null => setup.attentionTarget ?? null,
    statusOf: (entry: Win): WindowStatus => setup.statuses?.[entry.windowId] ?? 'running',
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
    openPicker: (mode: 'windows' | 'commands'): void => {
      calls.push(`picker:${mode}`);
    },
    renameWindow: (entry: Win): void => {
      calls.push(`rename:${entry.windowId}`);
    },
    toggleWorkMode: (): void => {
      calls.push('work-mode');
    },
    tileWindows: (): void => {
      calls.push('tile');
    },
    toggleNeedsYouFilter: (): void => {
      calls.push('needs-you-filter');
    },
  };
  const run = createShortcutRunner(deps);
  return { run, calls, deps };
}

// B sits between A and C in the sidebar; C holds the highest zOrder, so a
// runner deriving the target from the placement would act on C instead of B.
const A = win('wa-k3f9xa', 1, 'room-a');
const B = win('wb-p0d8qz', 2, 'room-b');
const C = win('wc-m7t2rv', 3, 'room-c');

const EXPECTED_CALLS: Readonly<Record<ActionId, readonly string[]>> = {
  'open-window-picker': ['picker:windows'],
  'open-command-palette': ['picker:commands'],
  'focus-next-window': ['expand:room-c', `goTo:${C.windowId}`, `terminal:${C.windowId}`],
  'focus-previous-window': ['expand:room-a', `goTo:${A.windowId}`, `terminal:${A.windowId}`],
  'focus-next-attention': ['attention'],
  'open-window-form': ['open-form'],
  'close-window': [`close:${B.windowId}`],
  'interrupt-window': [`interrupt:${B.windowId}`],
  'rename-window': [`rename:${B.windowId}`],
  'minimize-window': [`minimize:${B.windowId}`],
  'toggle-maximize-window': [`maximize:${B.windowId}`],
  'toggle-work-mode': ['work-mode'],
  'tile-windows': ['tile'],
  'toggle-sidebar-focus': ['sidebar'],
  'toggle-needs-you-filter': ['needs-you-filter'],
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
    const done = h.run(action);
    expect(h.calls, 'before the returned promise is awaited').toEqual(EXPECTED_CALLS[action]);
    await done;
    expect(h.calls).toEqual(EXPECTED_CALLS[action]);
  });
});

describe('next and previous window', () => {
  it('walk the sidebar order, not the order windows were opened in', async () => {
    const order = [A, C, B];
    const next = harness({ windows: [A, B, C], order, current: B });
    await next.run('focus-next-window');
    expect(next.calls).toContain(`goTo:${A.windowId}`);

    const previous = harness({ windows: [A, B, C], order, current: B });
    await previous.run('focus-previous-window');
    expect(previous.calls).toContain(`goTo:${C.windowId}`);
  });

  it('start from the ends when there is no current window, and bring a minimized target back', async () => {
    const hiddenA = win('wa-k3f9xa', 1, 'room-a', { isMinimized: true });
    const hiddenC = win('wc-m7t2rv', 2, 'room-c', { isMinimized: true });
    const next = harness({ windows: [hiddenA, hiddenC] });
    await next.run('focus-next-window');
    expect(next.calls).toEqual([
      'expand:room-a',
      `goTo:${hiddenA.windowId}`,
      `terminal:${hiddenA.windowId}`,
    ]);

    const previous = harness({ windows: [hiddenA, hiddenC] });
    await previous.run('focus-previous-window');
    expect(previous.calls).toEqual([
      'expand:room-c',
      `goTo:${hiddenC.windowId}`,
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
});

/** The card of a window that left; the ends of the list do not stand in for its place. */
function cardFor(windowId: string, next: Win | null, previous: Win | null): ExitedCard {
  return {
    windowId,
    name: windowId,
    exitCode: 1,
    next: next?.windowId ?? null,
    nextWindowId: next?.windowId ?? null,
    previousWindowId: previous?.windowId ?? null,
  };
}

interface Refusal {
  when: string;
  action: ActionId;
  setup: Setup;
  reason: ActionBlockReason;
}

const NO_ROWS_SHOWN: Setup = { windows: [A, B, C], order: [], sidebar: [A, B, C], current: B };
const LONE_CARD: Setup = {
  windows: [A],
  workMode: 'focus',
  exitedCard: cardFor('wgone-b', null, null),
};

const REFUSALS: readonly Refusal[] = [
  {
    when: 'no current window',
    action: 'close-window',
    setup: { windows: [A, B, C] },
    reason: 'no-window-on-screen',
  },
  {
    when: 'no current window',
    action: 'rename-window',
    setup: { windows: [A, B, C] },
    reason: 'no-window-on-screen',
  },
  {
    when: 'no current window',
    action: 'interrupt-window',
    setup: { windows: [A, B, C] },
    reason: 'no-window-on-screen',
  },
  {
    when: 'the current window is unknown',
    action: 'interrupt-window',
    setup: { windows: [A, B, C], current: B, statuses: { [B.windowId]: 'unknown' } },
    reason: 'unknown',
  },
  {
    when: 'the current window has exited',
    action: 'interrupt-window',
    setup: { windows: [A, B, C], current: B, statuses: { [B.windowId]: 'exited' } },
    reason: 'exited',
  },
  {
    when: 'no current window',
    action: 'minimize-window',
    setup: { windows: [A, B, C] },
    reason: 'no-window-on-screen',
  },
  {
    when: 'no current window',
    action: 'toggle-maximize-window',
    setup: { windows: [A, B, C] },
    reason: 'no-window-on-screen',
  },
  {
    when: 'in Focus',
    action: 'minimize-window',
    setup: { windows: [A, B, C], current: B, workMode: 'focus' },
    reason: 'canvas-only',
  },
  {
    when: 'in Focus',
    action: 'toggle-maximize-window',
    setup: { windows: [A, B, C], current: B, workMode: 'focus' },
    reason: 'canvas-only',
  },
  {
    when: 'in Focus',
    action: 'tile-windows',
    setup: { windows: [A, B, C], current: B, workMode: 'focus' },
    reason: 'canvas-only',
  },
  {
    when: 'no window is open',
    action: 'tile-windows',
    setup: { windows: [] },
    reason: 'no-windows',
  },
  {
    when: 'the Needs you filter shows no rows',
    action: 'focus-next-window',
    setup: NO_ROWS_SHOWN,
    reason: 'nowhere-to-go',
  },
  {
    when: 'the Needs you filter shows no rows',
    action: 'focus-previous-window',
    setup: NO_ROWS_SHOWN,
    reason: 'nowhere-to-go',
  },
  {
    when: "the ended window's card has no neighbour",
    action: 'focus-next-window',
    setup: LONE_CARD,
    reason: 'nowhere-to-go',
  },
  {
    when: "the ended window's card has no neighbour",
    action: 'focus-previous-window',
    setup: LONE_CARD,
    reason: 'nowhere-to-go',
  },
  {
    when: 'no window needs the user',
    action: 'focus-next-attention',
    setup: { windows: [A, B, C], current: B, attentionTarget: null },
    reason: 'nothing-needs-you',
  },
];

describe('an action with nothing to act on', () => {
  it.each(REFUSALS)(
    '$action does nothing when $when, and is refused as $reason',
    async ({ action, setup, reason }: Refusal) => {
      const h = harness(setup);
      await h.run(action);
      expect(h.calls).toEqual([]);
      expect(actionAvailability(action, availabilityContext(h.deps))).toEqual({
        ok: false,
        reason,
      });
    }
  );

  it('has no next window when the Needs you filter shows no rows', () => {
    const h = harness(NO_ROWS_SHOWN);
    expect(availabilityContext(h.deps).hasNeighbor.next).toBe(false);
  });
});

describe('in Focus', () => {
  it.each<ActionId>(['close-window', 'interrupt-window', 'rename-window', 'focus-next-window'])(
    '%s still acts as in Canvas',
    async (action: ActionId) => {
      const h = harness({ windows: [A, B, C], current: B, workMode: 'focus' });
      await h.run(action);
      expect(h.calls).toEqual(EXPECTED_CALLS[action]);
    }
  );
});

describe('next and previous window with the Needs you filter on', () => {
  it('walk the visible rows in the view order, settled and unknown rows included', async () => {
    // The view order is measured in needs-you-view.test.ts; here the order the filter shows is given as it stands: b (settled), c, a, then the unknown u.
    const a = win('a', 1, 'room-x');
    const b = win('b', 2, 'room-x');
    const c = win('c', 3, 'room-x');
    const u = win('u', 4, 'room-x');
    const order = [b, c, a, u];

    const forward = harness({ windows: [a, b, c, u], order, current: c });
    await forward.run('focus-next-window');
    expect(forward.calls).toContain('goTo:a');

    const backward = harness({ windows: [a, b, c, u], order, current: b });
    await backward.run('focus-previous-window');
    expect(backward.calls).toContain('goTo:u');
  });
});

describe("next and previous window while an ended window's card is shown", () => {
  it('go to the windows on either side of where the card stands, not to the ends of the list', async () => {
    const between = cardFor('wgone-b', C, A);
    const next = harness({ windows: [A, C], workMode: 'focus', exitedCard: between });
    await next.run('focus-next-window');
    expect(next.calls).toEqual(['expand:room-c', `goTo:${C.windowId}`, `terminal:${C.windowId}`]);

    const previous = harness({ windows: [A, C], workMode: 'focus', exitedCard: between });
    await previous.run('focus-previous-window');
    expect(previous.calls).toEqual([
      'expand:room-a',
      `goTo:${A.windowId}`,
      `terminal:${A.windowId}`,
    ]);
  });

  it('reach a window the Needs you filter hides, and do nothing when the card has none', async () => {
    const hiddenByFilter = harness({
      windows: [A, C],
      order: [A],
      sidebar: [A, C],
      workMode: 'focus',
      exitedCard: cardFor('wgone-b', C, A),
    });
    await hiddenByFilter.run('focus-next-window');
    expect(hiddenByFilter.calls).toContain(`goTo:${C.windowId}`);

    const alone = harness({
      windows: [A],
      workMode: 'focus',
      exitedCard: cardFor('wgone-b', null, null),
    });
    await alone.run('focus-next-window');
    await alone.run('focus-previous-window');
    expect(alone.calls).toEqual([]);
  });
});
