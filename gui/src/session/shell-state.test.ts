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

interface ShellState {
  workMode: WorkMode;
  focusedWindowId: string | null;
}

type ShellEvent =
  | { type: 'enterFocus'; canvasCurrent: { windowId: string } | null }
  | { type: 'enterCanvas' }
  | { type: 'select'; windowId: string };

type ShellEffect = { type: 'bringToFront'; windowId: string };

interface ShellStep {
  state: ShellState;
  effects: readonly ShellEffect[];
}

interface StartupWindow {
  windowId: string;
  detected: DetectedStatusView;
  taskReport?: TaskReportMark;
}

interface ShellStateModule {
  initialShellState: (workMode: WorkMode) => ShellState;
  reduceShell: (state: ShellState, event: ShellEvent) => ShellStep;
  workViewOf: (state: ShellState) => WorkView;
  readWorkMode: (storage: { getItem: (key: string) => string | null }) => WorkMode;
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
