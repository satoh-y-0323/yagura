import { describe, it, expect } from 'vitest';

import type { WindowStatus } from '../ipc/session-boundary';
import { emptyArrivalMemory, needsYou, observeNeedsYouArrivals } from '../status-tone';
import type { NeedsYouToastState, NeedsYouToastWindow } from './needs-you-toasts';

interface StepInput {
  arrived?: readonly string[];
  windows: readonly NeedsYouToastWindow[];
  seenWindowId?: string | null;
  dismissed?: string;
}

async function load() {
  return await import('./needs-you-toasts');
}

/** A window as the shell lists it; `needs` defaults to true for the two waiting statuses. */
function win(windowId: string, status: WindowStatus, needs?: boolean): NeedsYouToastWindow {
  const waiting = status === 'waiting_approval' || status === 'waiting_input';
  return { windowId, status, needsYou: needs ?? waiting };
}

async function run(steps: readonly StepInput[]): Promise<NeedsYouToastState> {
  const { stepNeedsYouToasts, emptyNeedsYouToastState } = await load();
  let state = emptyNeedsYouToastState();
  for (const step of steps) {
    state = stepNeedsYouToasts(state, {
      arrived: step.arrived ?? [],
      windows: step.windows,
      seenWindowId: step.seenWindowId ?? null,
      ...(step.dismissed === undefined ? {} : { dismissed: step.dismissed }),
    });
  }
  return state;
}

function ids(state: NeedsYouToastState): string[] {
  return state.toasts.map((toast) => toast.windowId);
}

function toastOf(state: NeedsYouToastState, windowId: string) {
  const toast = state.toasts.find((item) => item.windowId === windowId);
  if (toast === undefined) {
    throw new Error(`no toast for ${windowId}`);
  }
  return toast;
}

describe('a window arriving', () => {
  it('puts one toast up for the window that arrived and none for the rest', async () => {
    const state = await run([
      {
        arrived: ['a'],
        windows: [win('a', 'waiting_approval'), win('b', 'waiting_input'), win('c', 'running')],
      },
    ]);
    expect(ids(state)).toEqual(['a']);
    expect(toastOf(state, 'a').status).toBe('waiting_approval');
  });

  it('shows nothing before anything has arrived', async () => {
    const state = await run([{ windows: [win('a', 'waiting_approval')] }]);
    expect(state.toasts).toEqual([]);
  });

  it('keeps the toast on later steps with no new arrival', async () => {
    const windows = [win('a', 'waiting_approval')];
    const state = await run([{ arrived: ['a'], windows }, { windows }, { windows }]);
    expect(ids(state)).toEqual(['a']);
  });

  it('keeps a call through an unknown observation, as the arrival rule passes unknown over', async () => {
    const { stepNeedsYouToasts, emptyNeedsYouToastState } = await load();
    let memory = emptyArrivalMemory();
    let state = emptyNeedsYouToastState();
    const states: NeedsYouToastState[] = [];
    for (const status of ['waiting_approval', 'unknown', 'waiting_approval'] as const) {
      const entry = {
        windowId: 'w',
        detected: status === 'unknown' ? { status, reason: null } : { status },
        statusBoot: null,
      };
      const observed = observeNeedsYouArrivals(memory, [entry]);
      memory = observed.memory;
      state = stepNeedsYouToasts(state, {
        arrived: observed.arrived,
        windows: [{ windowId: 'w', status, needsYou: needsYou(entry) }],
        seenWindowId: null,
      });
      states.push(state);
    }
    const [first, ...rest] = states;
    expect(first === undefined ? [] : ids(first)).toEqual(['w']);
    expect(rest).toEqual([first, first]);
  });

  it('makes no toast for a window that arrived but does not need the user', async () => {
    const state = await run([{ arrived: ['a'], windows: [win('a', 'waiting_input', false)] }]);
    expect(state.toasts).toEqual([]);
  });

  it('makes no toast for a window that is not in the list', async () => {
    const state = await run([{ arrived: ['ghost'], windows: [win('a', 'running')] }]);
    expect(state.toasts).toEqual([]);
  });
});

describe('the same window arriving again', () => {
  it('stays one toast and takes the new status', async () => {
    const state = await run([
      { arrived: ['a'], windows: [win('a', 'waiting_input')] },
      { arrived: ['a'], windows: [win('a', 'waiting_approval')] },
    ]);
    expect(ids(state)).toEqual(['a']);
    expect(toastOf(state, 'a').status).toBe('waiting_approval');
    expect(toastOf(state, 'a').timed).toBe(false);
  });

  it('marks the replacement so a running timer starts over, even with the status unchanged', async () => {
    const windows = [win('a', 'waiting_input')];
    const first = await run([{ arrived: ['a'], windows }]);
    const second = await run([
      { arrived: ['a'], windows },
      { arrived: ['a'], windows },
    ]);
    const quiet = await run([{ arrived: ['a'], windows }, { windows }]);
    expect(ids(second)).toEqual(['a']);
    expect(toastOf(second, 'a').arrival).not.toBe(toastOf(first, 'a').arrival);
    expect(toastOf(quiet, 'a').arrival).toBe(toastOf(first, 'a').arrival);
  });

  it('marks the sentence as changed only when the status changed', async () => {
    const input = [win('a', 'waiting_input')];
    const approval = [win('a', 'waiting_approval')];
    const base = await run([{ arrived: ['a'], windows: input }]);
    const same = await run([
      { arrived: ['a'], windows: input },
      { arrived: ['a'], windows: input },
    ]);
    const changed = await run([
      { arrived: ['a'], windows: input },
      { arrived: ['a'], windows: approval },
    ]);
    expect(toastOf(same, 'a').sentenceRev).toBe(toastOf(base, 'a').sentenceRev);
    expect(toastOf(changed, 'a').sentenceRev).not.toBe(toastOf(base, 'a').sentenceRev);
  });

  it('does not touch the toasts of the windows that did not arrive again', async () => {
    const windows = [win('a', 'waiting_approval'), win('b', 'waiting_approval')];
    const before = await run([{ arrived: ['a', 'b'], windows }]);
    const after = await run([
      { arrived: ['a', 'b'], windows },
      { arrived: ['a'], windows },
    ]);
    expect(toastOf(after, 'b')).toEqual(toastOf(before, 'b'));
    expect(toastOf(after, 'a').arrival).not.toBe(toastOf(before, 'a').arrival);
  });
});

describe('the window being looked at', () => {
  it('gets no toast when it arrives', async () => {
    const state = await run([
      {
        arrived: ['a', 'b'],
        windows: [win('a', 'waiting_approval'), win('b', 'waiting_approval')],
        seenWindowId: 'a',
      },
    ]);
    expect(ids(state)).toEqual(['b']);
  });

  it('loses its toast once it becomes the window being looked at', async () => {
    const windows = [win('a', 'waiting_approval'), win('b', 'waiting_input')];
    // `a` is unknown when it becomes the window being looked at: the call kept through unknown still goes.
    const unknownA = [win('a', 'unknown'), win('b', 'waiting_input')];
    const state = await run([
      { arrived: ['a', 'b'], windows },
      { windows: unknownA },
      { windows: unknownA, seenWindowId: 'a' },
    ]);
    expect(ids(state)).toEqual(['b']);
  });

  it('leaves the other windows alone while one is looked at', async () => {
    const windows = [win('a', 'waiting_approval'), win('b', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { arrived: ['b'], windows, seenWindowId: 'c' },
    ]);
    expect(ids(state)).toEqual(['a', 'b']);
  });
});

describe('a window leaving the needs-you set', () => {
  it('loses its toast when it no longer needs the user', async () => {
    const state = await run([
      { arrived: ['a', 'b'], windows: [win('a', 'waiting_input'), win('b', 'waiting_input')] },
      { windows: [win('a', 'unknown'), win('b', 'waiting_input')] },
      { windows: [win('a', 'running'), win('b', 'waiting_input')] },
    ]);
    expect(ids(state)).toEqual(['b']);
  });

  it('loses its toast when the list says it does not need the user although its status is waiting', async () => {
    const state = await run([
      { arrived: ['a'], windows: [win('a', 'waiting_input')] },
      { windows: [win('a', 'waiting_input', false)] },
    ]);
    expect(state.toasts).toEqual([]);
  });

  it('loses its toast when the window is gone from the list', async () => {
    const state = await run([
      {
        arrived: ['a', 'b'],
        windows: [win('a', 'waiting_approval'), win('b', 'waiting_approval')],
      },
      { windows: [win('b', 'waiting_approval')] },
    ]);
    expect(ids(state)).toEqual(['b']);
  });
});

describe('closing a toast', () => {
  it('removes it and nothing else', async () => {
    const windows = [win('a', 'waiting_approval'), win('b', 'waiting_approval')];
    const state = await run([
      { arrived: ['a', 'b'], windows },
      { windows, dismissed: 'a' },
    ]);
    expect(ids(state)).toEqual(['b']);
  });

  it('keeps it away on later steps until the window arrives again', async () => {
    const windows = [win('a', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { windows, dismissed: 'a' },
      { windows },
      { windows },
    ]);
    expect(state.toasts).toEqual([]);
  });

  it('lets the next arrival of that window put a toast up again', async () => {
    const windows = [win('a', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { windows, dismissed: 'a' },
      { windows },
      { arrived: ['a'], windows },
    ]);
    expect(ids(state)).toEqual(['a']);
  });

  it('does not carry the close over to another window', async () => {
    const windows = [win('a', 'waiting_approval'), win('b', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { windows, dismissed: 'a' },
      { arrived: ['b'], windows },
    ]);
    expect(ids(state)).toEqual(['b']);
  });

  it('is harmless for a window that has no toast', async () => {
    const windows = [win('a', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { windows, dismissed: 'ghost' },
    ]);
    expect(ids(state)).toEqual(['a']);
  });

  it('shows the toast again when the window arrives in the same step it was closed', async () => {
    const windows = [win('a', 'waiting_approval')];
    const state = await run([
      { arrived: ['a'], windows },
      { arrived: ['a'], windows, dismissed: 'a' },
    ]);
    expect(ids(state)).toEqual(['a']);
  });
});

describe('how a toast lasts', () => {
  it('stays until acted on for an approval wait, with no time mark', async () => {
    const state = await run([{ arrived: ['a'], windows: [win('a', 'waiting_approval')] }]);
    expect(toastOf(state, 'a').timed).toBe(false);
    expect(toastOf(state, 'a').tone).toBe('warning');
  });

  it('is an info toast that goes with time for an input wait', async () => {
    const state = await run([{ arrived: ['a'], windows: [win('a', 'waiting_input')] }]);
    expect(toastOf(state, 'a').timed).toBe(true);
    expect(toastOf(state, 'a').tone).toBe('info');
  });
});

describe('the order of the toasts', () => {
  it('lists approval waits before input waits', async () => {
    const windows = [
      win('i1', 'waiting_input'),
      win('a1', 'waiting_approval'),
      win('i2', 'waiting_input'),
    ];
    const state = await run([{ arrived: ['i1', 'a1', 'i2'], windows }]);
    expect(ids(state)).toEqual(['a1', 'i1', 'i2']);
  });

  it('lists each kind in the order the windows came in, across steps', async () => {
    const windows = [
      win('a1', 'waiting_approval'),
      win('i1', 'waiting_input'),
      win('a2', 'waiting_approval'),
      win('i2', 'waiting_input'),
    ];
    const state = await run([
      { arrived: ['i1'], windows },
      { arrived: ['a2'], windows },
      { arrived: ['i2', 'a1'], windows },
    ]);
    expect(ids(state)).toEqual(['a2', 'a1', 'i1', 'i2']);
  });

  it('keeps a toast in its place when its window arrives again', async () => {
    const windows = [win('a1', 'waiting_approval'), win('a2', 'waiting_approval')];
    const state = await run([
      { arrived: ['a1'], windows },
      { arrived: ['a2'], windows },
      { arrived: ['a1'], windows },
    ]);
    expect(ids(state)).toEqual(['a1', 'a2']);
  });
});

describe('the state it is given', () => {
  it('is not changed by a step', async () => {
    const { stepNeedsYouToasts, emptyNeedsYouToastState } = await load();
    const windows = [win('a', 'waiting_approval')];
    const start = stepNeedsYouToasts(emptyNeedsYouToastState(), {
      arrived: ['a'],
      windows,
      seenWindowId: null,
    });
    const snapshot = JSON.stringify(start);
    stepNeedsYouToasts(start, { arrived: ['a'], windows, seenWindowId: 'a', dismissed: 'a' });
    expect(JSON.stringify(start)).toBe(snapshot);
  });
});
