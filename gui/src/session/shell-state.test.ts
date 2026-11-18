/**
 * The work area's state: the work mode, the window Focus shows, the view the
 * current window is derived from, and the window Focus shows at startup.
 *
 * The module is read through `import.meta.glob` so that, while it does not
 * exist, every case below reports its own unmet contract instead of the whole
 * file failing collection.
 */
import { describe, it, expect } from 'vitest';

import type { DetectedStatusView, TaskReportMark, WindowStatus } from '../ipc/session-boundary';

type WorkMode = 'focus' | 'canvas';

type WorkView = { mode: 'canvas' } | { mode: 'focus'; focusedWindowId: string | null };

/** A window as the sidebar listed it; the order the sidebar showed is what a next window is read from. */
interface OrderedWindow {
  windowId: string;
  name: string;
  exitCode: number | null;
}

interface ExitedCard {
  windowId: string;
  name: string;
  exitCode: number | null;
  next: string | null;
  nextWindowId: string | null;
}

interface ShellState {
  workMode: WorkMode;
  focusedWindowId: string | null;
  closingHere: ReadonlySet<string>;
  exitedCard: ExitedCard | null;
  pendingExitCodes: ReadonlyMap<string, number>;
}

type WindowGoneEvent = {
  type: 'windowGone';
  windowId: string;
  orderBefore: readonly OrderedWindow[];
  exitCode: number | null;
};

type ShellEvent =
  | { type: 'enterFocus'; canvasCurrent: { windowId: string } | null }
  | { type: 'enterCanvas' }
  | { type: 'select'; windowId: string }
  | { type: 'closeRequested'; windowId: string }
  | { type: 'closeFailed'; windowId: string }
  | WindowGoneEvent
  | { type: 'exitCodeReceived'; windowId: string; exitCode: number }
  | { type: 'openedByUser'; windowId: string }
  | { type: 'windowsAppeared'; windowIds: readonly string[] }
  | { type: 'exitedCardGoTo' };

type ShellEffect =
  { type: 'bringToFront'; windowId: string } | { type: 'focusTerminal'; windowId: string };

interface ShellStep {
  state: ShellState;
  effects: readonly ShellEffect[];
  /** Set only for `windowGone`: whether this GUI closed the window. */
  goneByUser?: boolean;
}

interface StartupWindow {
  windowId: string;
  detected: DetectedStatusView;
  taskReport?: TaskReportMark;
}

interface ShellStateModule {
  initialShellState: (workMode: WorkMode) => ShellState;
  reduceShell: (state: ShellState, event: ShellEvent) => ShellStep;
  diffGoneWindows: (
    previous: readonly OrderedWindow[],
    current: readonly OrderedWindow[]
  ) => WindowGoneEvent[];
  diffAppearedWindows: (
    previous: readonly OrderedWindow[],
    current: readonly OrderedWindow[]
  ) => ShellEvent[];
  workViewOf: (state: ShellState) => WorkView;
  readWorkMode: (storage: { getItem: (key: string) => string | null }) => WorkMode;
  writeWorkMode: (storage: Pick<Storage, 'setItem'>, mode: WorkMode) => void;
  firstWindowAtStartup: <T extends StartupWindow>(
    order: readonly T[],
    canvasCurrent: T | null
  ) => T | null;
}

const shellStateModules = import.meta.glob<ShellStateModule>('./shell-state.ts', {
  eager: true,
});

function shellStateModule(): ShellStateModule {
  const found = Object.values(shellStateModules)[0];
  if (found === undefined) {
    throw new Error('gui/src/session/shell-state.ts could not be read');
  }
  return found;
}

/** A Focus state showing `windowId`, reached through the reducer itself. */
function focusShowing(windowId: string): ShellState {
  const { initialShellState, reduceShell } = shellStateModule();
  return reduceShell(initialShellState('focus'), { type: 'select', windowId }).state;
}

describe('workViewOf: the view the current window is derived from', () => {
  it('carries the window Focus shows', () => {
    const { workViewOf } = shellStateModule();

    expect(workViewOf(focusShowing('w-a'))).toEqual({ mode: 'focus', focusedWindowId: 'w-a' });
  });

  it('carries no window in Canvas, where the placement decides', () => {
    const { initialShellState, workViewOf } = shellStateModule();

    expect(workViewOf(initialShellState('canvas'))).toEqual({ mode: 'canvas' });
  });
});

describe('reduceShell: choosing a window and switching the work mode', () => {
  it('in Focus, choosing a window changes only the window Focus shows and asks for nothing else', () => {
    const { reduceShell } = shellStateModule();
    const before = focusShowing('w-a');

    const step = reduceShell(before, { type: 'select', windowId: 'w-b' });

    expect(step.state).toEqual({ ...before, focusedWindowId: 'w-b' });
    expect(step.effects).toEqual([]);
  });

  it('Canvas to Focus shows the window that was current on the canvas, without touching the placement', () => {
    const { initialShellState, reduceShell } = shellStateModule();
    const before = initialShellState('canvas');

    const step = reduceShell(before, { type: 'enterFocus', canvasCurrent: { windowId: 'w-c' } });

    expect(step.state).toEqual({ ...before, workMode: 'focus', focusedWindowId: 'w-c' });
    expect(step.effects).toEqual([]);
  });

  it('Canvas to Focus with no current window on the canvas shows no window', () => {
    const { initialShellState, reduceShell } = shellStateModule();

    const step = reduceShell(initialShellState('canvas'), {
      type: 'enterFocus',
      canvasCurrent: null,
    });

    expect(step.state.workMode).toBe('focus');
    expect(step.state.focusedWindowId).toBeNull();
  });

  it('Focus to Canvas changes only the mode and asks the canvas to bring the Focus window to the front', () => {
    const { reduceShell } = shellStateModule();
    const before = focusShowing('w-a');

    const step = reduceShell(before, { type: 'enterCanvas' });

    expect(step.state).toEqual({ ...before, workMode: 'canvas' });
    expect(step.effects).toEqual([{ type: 'bringToFront', windowId: 'w-a' }]);
  });
});

describe('readWorkMode: the saved work mode', () => {
  function storageHolding(value: string | null): {
    getItem: (key: string) => string | null;
    keys: string[];
  } {
    const keys: string[] = [];
    return {
      keys,
      getItem: (key: string): string | null => {
        keys.push(key);
        return value;
      },
    };
  }

  it('reads the saved mode from the injected storage under yagura.work-mode', () => {
    const { readWorkMode } = shellStateModule();
    const canvas = storageHolding('canvas');
    const focus = storageHolding('focus');

    expect(readWorkMode(canvas)).toBe('canvas');
    expect(readWorkMode(focus)).toBe('focus');
    expect(canvas.keys).toEqual(['yagura.work-mode']);
  });

  it.each<string | null>([null, 'tabs', '', 'Canvas'])(
    'falls back to Focus for %j',
    (value: string | null) => {
      const { readWorkMode } = shellStateModule();

      expect(readWorkMode(storageHolding(value))).toBe('focus');
    }
  );

  it('reads back the mode writeWorkMode saved under yagura.work-mode', () => {
    const { readWorkMode, writeWorkMode } = shellStateModule();
    const saved = new Map<string, string>();
    const storage = {
      getItem: (key: string): string | null => saved.get(key) ?? null,
      setItem: (key: string, value: string): void => {
        saved.set(key, value);
      },
    };

    writeWorkMode(storage, 'canvas');
    expect(readWorkMode(storage)).toBe('canvas');
    expect([...saved.keys()]).toEqual(['yagura.work-mode']);

    writeWorkMode(storage, 'focus');
    expect(readWorkMode(storage)).toBe('focus');
  });
});

interface Win extends StartupWindow {
  label: string;
}

function win(windowId: string, status: WindowStatus, taskReport?: TaskReportMark): Win {
  const detected: DetectedStatusView = status === 'unknown' ? { status, reason: null } : { status };
  return taskReport === undefined
    ? { windowId, label: windowId, detected }
    : { windowId, label: windowId, detected, taskReport };
}

describe('firstWindowAtStartup: the window Focus shows when the app starts', () => {
  it('picks the window needing the user that ranks first, ahead of the sidebar order and the canvas', () => {
    const { firstWindowAtStartup } = shellStateModule();
    const order = [
      win('w-run', 'running'),
      win('w-input', 'waiting_input'),
      win('w-approval', 'waiting_approval'),
    ];

    const picked = firstWindowAtStartup(order, order[0] ?? null);

    expect(picked).toBe(order[2]);
  });

  it('picks a window needing the user over the current window of the canvas', () => {
    const { firstWindowAtStartup } = shellStateModule();
    const order = [win('w-idle', 'idle'), win('w-input', 'waiting_input')];

    expect(firstWindowAtStartup(order, order[0] ?? null)?.windowId).toBe('w-input');
  });

  it('falls back to the current window of the canvas when no window needs the user', () => {
    const { firstWindowAtStartup } = shellStateModule();
    const order = [win('w-a', 'running'), win('w-b', 'idle'), win('w-c', 'unknown')];

    expect(firstWindowAtStartup(order, order[1] ?? null)).toBe(order[1]);
  });

  it('does not count a task window that only waits for its owner as needing the user', () => {
    const { firstWindowAtStartup } = shellStateModule();
    const order = [
      win('w-a', 'running'),
      win('w-task', 'waiting_input', { seq: 1, standing: true }),
      win('w-b', 'idle'),
    ];

    expect(firstWindowAtStartup(order, order[2] ?? null)?.windowId).toBe('w-b');
  });

  it('falls back to the top of the sidebar when nothing needs the user and the canvas has no current window', () => {
    const { firstWindowAtStartup } = shellStateModule();
    const order = [win('w-top', 'running'), win('w-next', 'idle')];

    expect(firstWindowAtStartup(order, null)).toBe(order[0]);
  });

  it('returns null when there is no window', () => {
    const { firstWindowAtStartup } = shellStateModule();

    expect(firstWindowAtStartup([], null)).toBeNull();
  });
});

function ordered(windowId: string, exitCode: number | null = null): OrderedWindow {
  return { windowId, name: `name-${windowId}`, exitCode };
}

const ORDER_ABC = [ordered('a'), ordered('b'), ordered('c')];

/** A Focus state showing `windowId`, with `closing` already asked to close. */
function focusShowingWhileClosing(windowId: string, closing: readonly string[]): ShellState {
  const { reduceShell } = shellStateModule();
  return closing.reduce(
    (state, id) => reduceShell(state, { type: 'closeRequested', windowId: id }).state,
    focusShowing(windowId)
  );
}

function gone(
  windowId: string,
  orderBefore: readonly OrderedWindow[],
  exitCode: number | null = null
): WindowGoneEvent {
  return { type: 'windowGone', windowId, orderBefore, exitCode };
}

/** Runs the events through the reducer, collecting the effects and each `goneByUser`. */
function replay(
  start: ShellState,
  events: readonly ShellEvent[]
): { state: ShellState; effects: ShellEffect[]; goneByUser: boolean[] } {
  const { reduceShell } = shellStateModule();
  let state = start;
  const effects: ShellEffect[] = [];
  const goneByUser: boolean[] = [];
  for (const event of events) {
    const step = reduceShell(state, event);
    state = step.state;
    effects.push(...step.effects);
    if (step.goneByUser !== undefined) {
      goneByUser.push(step.goneByUser);
    }
  }
  return { state, effects, goneByUser };
}

/** What the shell sends the reducer as the window list changes from one snapshot to the next. */
function goneEventsAcross(snapshots: readonly (readonly OrderedWindow[])[]): WindowGoneEvent[] {
  const { diffGoneWindows } = shellStateModule();
  const events: WindowGoneEvent[] = [];
  for (let i = 1; i < snapshots.length; i += 1) {
    events.push(...diffGoneWindows(snapshots[i - 1] ?? [], snapshots[i] ?? []));
  }
  return events;
}

describe('reduceShell: closing a window from this GUI', () => {
  it('remembers the window from the request to close it, and forgets it when the close fails', () => {
    const { initialShellState, reduceShell } = shellStateModule();

    const asked = reduceShell(initialShellState('focus'), {
      type: 'closeRequested',
      windowId: 'w-a',
    });
    const failed = reduceShell(asked.state, { type: 'closeFailed', windowId: 'w-a' });

    expect(asked.state.closingHere.has('w-a')).toBe(true);
    expect(failed.state.closingHere.has('w-a')).toBe(false);
  });
});

describe('diffGoneWindows: the windows that left the list since the last one', () => {
  it('reports each vanished window once, with the order the sidebar showed before and its exit code', () => {
    const { diffGoneWindows } = shellStateModule();
    const before = [ordered('a'), ordered('b', 3), ordered('c')];

    const events = diffGoneWindows(before, [ordered('a'), ordered('c')]);

    expect(events).toEqual([gone('b', before, 3)]);
  });

  it('reports nothing for windows still listed or never listed before', () => {
    const { diffGoneWindows } = shellStateModule();

    const events = diffGoneWindows(ORDER_ABC, [...ORDER_ABC, ordered('new')]);

    expect(events).toEqual([]);
  });

  it('reports nothing the second time the same list is seen', () => {
    const { diffGoneWindows } = shellStateModule();
    const afterClose = [ordered('a'), ordered('c')];

    const first = diffGoneWindows(ORDER_ABC, afterClose);
    const second = diffGoneWindows(afterClose, afterClose);

    expect(first).toHaveLength(1);
    expect(second).toEqual([]);
  });
});

describe('reduceShell: a window vanished while Focus shows it', () => {
  it('after this GUI closed it, shows the window after it and moves the keyboard to its terminal', () => {
    const { reduceShell } = shellStateModule();
    const before = focusShowingWhileClosing('b', ['b']);

    const step = reduceShell(before, gone('b', ORDER_ABC));

    expect(step.state.focusedWindowId).toBe('c');
    expect(step.state.exitedCard).toBeNull();
    expect(step.state.closingHere.has('b')).toBe(false);
    expect(step.effects).toEqual([{ type: 'focusTerminal', windowId: 'c' }]);
    expect(step.goneByUser).toBe(true);
  });

  it('after this GUI closed the last window of the order, shows the one before it', () => {
    const { reduceShell } = shellStateModule();

    const step = reduceShell(focusShowingWhileClosing('c', ['c']), gone('c', ORDER_ABC));

    expect(step.state.focusedWindowId).toBe('b');
    expect(step.effects).toEqual([{ type: 'focusTerminal', windowId: 'b' }]);
  });

  it('after this GUI closed the only window, shows no window and moves nothing', () => {
    const { reduceShell } = shellStateModule();
    const only = [ordered('a')];

    const step = reduceShell(focusShowingWhileClosing('a', ['a']), gone('a', only));

    expect(step.state.focusedWindowId).toBeNull();
    expect(step.state.exitedCard).toBeNull();
    expect(step.effects).toEqual([]);
  });

  it('when it ended on its own, keeps showing it, leaves a card and moves the keyboard nowhere', () => {
    const { reduceShell } = shellStateModule();

    const step = reduceShell(focusShowing('b'), gone('b', ORDER_ABC, 2));

    expect(step.state.focusedWindowId).toBe('b');
    expect(step.state.exitedCard).toMatchObject({
      windowId: 'b',
      name: 'name-b',
      exitCode: 2,
      next: 'name-c',
      nextWindowId: 'c',
    });
    expect(step.effects).toEqual([]);
    expect(step.goneByUser).toBe(false);
  });

  it('names the window before it on the card when the one that ended was last, and none when it was alone', () => {
    const { reduceShell } = shellStateModule();

    const last = reduceShell(focusShowing('c'), gone('c', ORDER_ABC));
    const alone = reduceShell(focusShowing('a'), gone('a', [ordered('a')]));

    expect(last.state.exitedCard).toMatchObject({ next: 'name-b', nextWindowId: 'b' });
    expect(alone.state.exitedCard).toMatchObject({ next: null, nextWindowId: null });
  });

  it('writes the exit code onto the card when it arrives after the card', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const late = reduceShell(carded, { type: 'exitCodeReceived', windowId: 'b', exitCode: 7 });
    const other = reduceShell(carded, { type: 'exitCodeReceived', windowId: 'c', exitCode: 7 });

    expect(carded.exitedCard?.exitCode).toBeNull();
    expect(late.state.exitedCard?.exitCode).toBe(7);
    expect(other.state.exitedCard?.exitCode).toBeNull();
  });

  it('leaves no card and no move when the window that vanished was not the one shown', () => {
    const { reduceShell } = shellStateModule();
    const before = focusShowingWhileClosing('a', ['b']);

    const closed = reduceShell(before, gone('b', ORDER_ABC));
    const ended = reduceShell(focusShowing('a'), gone('c', ORDER_ABC));

    expect(closed.state.focusedWindowId).toBe('a');
    expect(closed.state.exitedCard).toBeNull();
    expect(closed.state.closingHere.has('b')).toBe(false);
    expect(closed.effects).toEqual([]);
    expect(closed.goneByUser).toBe(true);
    expect(ended.state.focusedWindowId).toBe('a');
    expect(ended.state.exitedCard).toBeNull();
    expect(ended.goneByUser).toBe(false);
  });

  it('on the canvas changes neither the window nor the placement', () => {
    const { initialShellState, reduceShell } = shellStateModule();
    const before = reduceShell(initialShellState('canvas'), {
      type: 'closeRequested',
      windowId: 'b',
    }).state;

    const step = reduceShell(before, gone('b', ORDER_ABC));

    expect(step.state.workMode).toBe('canvas');
    expect(step.state.focusedWindowId).toBeNull();
    expect(step.state.exitedCard).toBeNull();
    expect(step.state.closingHere.has('b')).toBe(false);
    expect(step.effects).toEqual([]);
  });

  it('treats a close that failed and then a window that ended on its own as ended on its own', () => {
    const closeFailedThenEnded: ShellEvent[] = [
      { type: 'closeRequested', windowId: 'b' },
      { type: 'closeFailed', windowId: 'b' },
      gone('b', ORDER_ABC, 1),
    ];

    const result = replay(focusShowing('b'), closeFailedThenEnded);

    expect(result.goneByUser).toEqual([false]);
    expect(result.state.focusedWindowId).toBe('b');
    expect(result.state.exitedCard).not.toBeNull();
    expect(result.effects).toEqual([]);
  });
});

describe('the verdict on who closed a window does not depend on which news arrives first', () => {
  const afterClose = [ordered('a'), ordered('c')];

  it('is one verdict whether the exit push or the close reply removes the window first', () => {
    const request: ShellEvent = { type: 'closeRequested', windowId: 'b' };
    // Both the push and the reply remove the window; the second removal leaves the list as it is.
    const pushFirst = replay(focusShowing('b'), [
      request,
      ...goneEventsAcross([ORDER_ABC, afterClose, afterClose]),
    ]);
    const replyFirst = replay(focusShowing('b'), [
      request,
      ...goneEventsAcross([ORDER_ABC, afterClose]),
    ]);

    expect(pushFirst.goneByUser).toEqual([true]);
    expect(pushFirst).toEqual(replyFirst);
    expect(pushFirst.state.focusedWindowId).toBe('c');
  });

  it('gives the same card whether the exit code came with the vanishing or after it', () => {
    const { reduceShell } = shellStateModule();
    const withCode = reduceShell(focusShowing('b'), gone('b', ORDER_ABC, 9)).state;
    const early = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;
    const afterwards = reduceShell(early, {
      type: 'exitCodeReceived',
      windowId: 'b',
      exitCode: 9,
    }).state;

    expect(afterwards.exitedCard).toEqual(withCode.exitedCard);
  });
});

describe('reduceShell: a window appeared', () => {
  it('goes to a window this GUI opened and moves the keyboard to its terminal', () => {
    const { reduceShell } = shellStateModule();

    const step = reduceShell(focusShowing('a'), { type: 'openedByUser', windowId: 'n' });

    expect(step.state.focusedWindowId).toBe('n');
    expect(step.effects).toEqual([{ type: 'focusTerminal', windowId: 'n' }]);
  });

  it('keeps showing the current window when windows appear from anywhere else, and moves nothing', () => {
    const { reduceShell } = shellStateModule();
    const before = focusShowing('a');

    const step = reduceShell(before, { type: 'windowsAppeared', windowIds: ['x', 'y'] });

    expect(step.state).toEqual(before);
    expect(step.effects).toEqual([]);
  });

  it('shows the first of them when there was no window to show, still without moving the keyboard', () => {
    const { initialShellState, reduceShell } = shellStateModule();

    const step = reduceShell(initialShellState('focus'), {
      type: 'windowsAppeared',
      windowIds: ['x', 'y'],
    });

    expect(step.state.focusedWindowId).toBe('x');
    expect(step.effects).toEqual([]);
  });

  it('does not leave the card of an ended window for a window that appeared on its own', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const step = reduceShell(carded, { type: 'windowsAppeared', windowIds: ['x'] });

    expect(step.state.focusedWindowId).toBe('b');
    expect(step.state.exitedCard).toEqual(carded.exitedCard);
    expect(step.effects).toEqual([]);
  });
});

describe('reduceShell: the card of an ended window', () => {
  it('goes to the window named on the card, moves the keyboard to it and clears the card', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const step = reduceShell(carded, { type: 'exitedCardGoTo' });

    expect(step.state.focusedWindowId).toBe('c');
    expect(step.state.exitedCard).toBeNull();
    expect(step.effects).toEqual([{ type: 'focusTerminal', windowId: 'c' }]);
  });

  it('is cleared when the user chooses another window by another way', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const step = reduceShell(carded, { type: 'select', windowId: 'a' });

    expect(step.state.focusedWindowId).toBe('a');
    expect(step.state.exitedCard).toBeNull();
  });
});

describe('diffGoneWindows: several windows leaving in the same observation', () => {
  const four = [ordered('a'), ordered('b'), ordered('c'), ordered('d')];
  const afterBoth = [ordered('a'), ordered('d')];

  it('reads each one against the order that holds it and the windows still listed, not the others that left', () => {
    const { diffGoneWindows } = shellStateModule();

    const events = diffGoneWindows(four, afterBoth);

    expect(events.map((event) => event.windowId)).toEqual(['b', 'c']);
    expect(events.map((event) => event.orderBefore.map((entry) => entry.windowId))).toEqual([
      ['a', 'b', 'd'],
      ['a', 'c', 'd'],
    ]);
  });

  it('moves on to the window after the one closed that is still listed, else the one before', () => {
    const closedBoth: ShellEvent[] = [
      { type: 'closeRequested', windowId: 'b' },
      { type: 'closeRequested', windowId: 'c' },
    ];
    const gonesFromFour = goneEventsAcross([four, afterBoth]);

    const showingB = replay(focusShowing('b'), [...closedBoth, ...gonesFromFour]);
    const showingC = replay(focusShowing('c'), [...closedBoth, ...gonesFromFour]);
    const tail = replay(focusShowing('c'), [
      ...closedBoth,
      ...goneEventsAcross([ORDER_ABC, [ordered('a')]]).filter((event) => event.windowId === 'c'),
    ]);

    expect(showingB.state.focusedWindowId).toBe('d');
    expect(showingB.effects).toEqual([{ type: 'focusTerminal', windowId: 'd' }]);
    expect(showingC.state.focusedWindowId).toBe('d');
    expect(tail.state.focusedWindowId).toBe('a');
    expect(tail.effects).toEqual([{ type: 'focusTerminal', windowId: 'a' }]);
  });
});

describe('diffAppearedWindows: the windows that joined the list since the last one', () => {
  it('reports the new windows together, in the list order, and none that were listed before', () => {
    const { diffAppearedWindows } = shellStateModule();

    const events = diffAppearedWindows(
      [ordered('a'), ordered('b')],
      [ordered('a'), ordered('x'), ordered('b'), ordered('y')]
    );

    expect(events).toEqual([{ type: 'windowsAppeared', windowIds: ['x', 'y'] }]);
  });

  it('reports nothing when no window joined, including when some only left', () => {
    const { diffAppearedWindows } = shellStateModule();

    expect(diffAppearedWindows(ORDER_ABC, ORDER_ABC)).toEqual([]);
    expect(diffAppearedWindows(ORDER_ABC, [ordered('a')])).toEqual([]);
  });
});

describe('reduceShell: an exit code heard before its window left the list', () => {
  it('shows it on the card the window leaves, and prefers the code the list carried', () => {
    const { reduceShell } = shellStateModule();
    const heard = reduceShell(focusShowing('b'), {
      type: 'exitCodeReceived',
      windowId: 'b',
      exitCode: 5,
    }).state;

    const fromHeard = reduceShell(heard, gone('b', ORDER_ABC));
    const fromList = reduceShell(heard, gone('b', ORDER_ABC, 9));

    expect(fromHeard.state.exitedCard?.exitCode).toBe(5);
    expect(fromList.state.exitedCard?.exitCode).toBe(9);
  });

  it('is forgotten once the window has left the list', () => {
    const { reduceShell } = shellStateModule();
    const heard = reduceShell(focusShowing('a'), {
      type: 'exitCodeReceived',
      windowId: 'b',
      exitCode: 5,
    }).state;

    const after = reduceShell(heard, gone('b', ORDER_ABC));

    expect(heard.pendingExitCodes.get('b')).toBe(5);
    expect(after.state.pendingExitCodes.has('b')).toBe(false);
  });
});

describe('reduceShell: the card of an ended window when the window it offers also leaves', () => {
  it('offers the window next to that one instead, and keeps the offer when another window leaves', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const unrelated = reduceShell(carded, gone('z', [ordered('z'), ordered('a'), ordered('c')]));
    const offered = reduceShell(carded, gone('c', [ordered('a'), ordered('c')]));
    const taken = reduceShell(offered.state, { type: 'exitedCardGoTo' });

    expect(unrelated.state.exitedCard).toMatchObject({ next: 'name-c', nextWindowId: 'c' });
    expect(offered.state.exitedCard).toMatchObject({ next: 'name-a', nextWindowId: 'a' });
    expect(offered.state.focusedWindowId).toBe('b');
    expect(taken.state.focusedWindowId).toBe('a');
    expect(taken.effects).toEqual([{ type: 'focusTerminal', windowId: 'a' }]);
  });

  it('offers nothing when no window is left to offer', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', [ordered('b'), ordered('c')])).state;

    const offered = reduceShell(carded, gone('c', [ordered('c')]));
    const pressed = reduceShell(offered.state, { type: 'exitedCardGoTo' });

    expect(offered.state.exitedCard).toMatchObject({ next: null, nextWindowId: null });
    expect(pressed.state).toEqual(offered.state);
    expect(pressed.effects).toEqual([]);
  });
});

describe('reduceShell: going to the canvas while a card is shown', () => {
  it('drops the card and asks to bring nothing forward, for the window has left the list', () => {
    const { reduceShell } = shellStateModule();
    const carded = reduceShell(focusShowing('b'), gone('b', ORDER_ABC)).state;

    const step = reduceShell(carded, { type: 'enterCanvas' });

    expect(step.state.workMode).toBe('canvas');
    expect(step.state.exitedCard).toBeNull();
    expect(step.effects).toEqual([]);
  });
});

/**
 * The key-driven neighbour of an ended window's card. No name is fixed for it
 * anywhere yet, so it is looked up inside each case and only these cases fail
 * while it is missing.
 */
type CardNeighbor = (card: ExitedCard, step: 1 | -1) => string | null;

function cardNeighborOf(card: ExitedCard | null, step: 1 | -1): string | null {
  const found = (shellStateModule() as unknown as Record<string, unknown>)['exitedCardNeighbor'];
  if (typeof found !== 'function' || card === null) {
    throw new Error('exitedCardNeighbor (gui/src/session/shell-state.ts) is not provided');
  }
  return (found as CardNeighbor)(card, step);
}

function cardedAmong(windowId: string, order: readonly OrderedWindow[]): ShellState {
  const { reduceShell } = shellStateModule();
  return reduceShell(focusShowing(windowId), gone(windowId, order)).state;
}

describe('the card of an ended window: Next window and Previous window start from where it stood', () => {
  it('Next window goes where the card offers to go, and Previous window to the window before it', () => {
    const { reduceShell } = shellStateModule();
    const carded = cardedAmong('b', ORDER_ABC);

    const next = cardNeighborOf(carded.exitedCard, 1);
    const previous = cardNeighborOf(carded.exitedCard, -1);
    const pressed = reduceShell(carded, { type: 'exitedCardGoTo' });

    expect(next).toBe('c');
    expect(next).toBe(pressed.state.focusedWindowId);
    expect(previous).toBe('a');
  });

  it('treats the ends as the card does: no wrapping, the window on the other side instead', () => {
    const { reduceShell } = shellStateModule();
    const last = cardedAmong('c', ORDER_ABC);
    const first = cardedAmong('a', ORDER_ABC);

    expect(cardNeighborOf(last.exitedCard, 1)).toBe('b');
    expect(cardNeighborOf(last.exitedCard, 1)).toBe(
      reduceShell(last, { type: 'exitedCardGoTo' }).state.focusedWindowId
    );
    expect(cardNeighborOf(last.exitedCard, -1)).toBe('b');
    expect(cardNeighborOf(first.exitedCard, 1)).toBe('b');
    expect(cardNeighborOf(first.exitedCard, -1)).toBe('b');
  });

  it('has no neighbour when no window is left', () => {
    const alone = cardedAmong('b', [ordered('b')]);

    expect(cardNeighborOf(alone.exitedCard, 1)).toBeNull();
    expect(cardNeighborOf(alone.exitedCard, -1)).toBeNull();
  });

  it('keeps both keys on windows still listed when the window on either side leaves too', () => {
    const { reduceShell } = shellStateModule();
    const carded = cardedAmong('b', ORDER_ABC);

    const afterCLeft = reduceShell(carded, gone('c', [ordered('a'), ordered('c')])).state;
    const afterALeft = reduceShell(carded, gone('a', [ordered('a'), ordered('c')])).state;

    expect(cardNeighborOf(afterCLeft.exitedCard, 1)).toBe('a');
    expect(cardNeighborOf(afterCLeft.exitedCard, -1)).toBe('a');
    expect(cardNeighborOf(afterALeft.exitedCard, 1)).toBe('c');
    expect(cardNeighborOf(afterALeft.exitedCard, -1)).toBe('c');
  });
});
