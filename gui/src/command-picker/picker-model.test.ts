/**
 * The command picker's rows are decided here, away from the DOM: which windows
 * and actions are listed, in what order and under which heading, what part of
 * each visible text is marked, and where the selection goes. The visible texts
 * and whether an action can run are handed in, so these tests build them as
 * plain strings.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DetectedStatusView, WindowStatus } from '../ipc/session-boundary';
import type { ActionBlockReason, ActionVerdict } from '../keymap/action-availability';
import { ACTION_IDS, type ActionId } from '../keymap/key-binding';
import {
  chosenRow,
  moveSelection,
  openPicker,
  openingInput,
  pointAt,
  pressOpenKey,
  refreshPicker,
  typeInput,
} from './picker-model';
import type {
  PickerAction,
  PickerRow,
  PickerSource,
  PickerState,
  PickerWindow,
} from './picker-model';

const STATUS_WORDS: Record<WindowStatus, string> = {
  waiting_approval: 'Needs approval',
  waiting_input: 'Needs input',
  unknown: 'Unknown',
  interrupted: 'Interrupted',
  running: 'Running',
  idle: 'Idle',
  exited: 'Exited',
};

interface WindowOptions {
  name?: string;
  place?: string;
  cwd?: string;
  vendor?: string | null;
  /** A task window whose report still stands: waiting on its owner, not on the user. */
  standing?: boolean;
}

function win(id: string, status: WindowStatus, options: WindowOptions = {}): PickerWindow {
  const detected: DetectedStatusView = status === 'unknown' ? { status, reason: null } : { status };
  return {
    windowId: id,
    name: options.name ?? `name-${id}`,
    place: options.place ?? 'room',
    cwd: options.cwd ?? `/w/${id}`,
    vendor: options.vendor === undefined ? 'vendor' : options.vendor,
    statusWords: STATUS_WORDS[status],
    detected,
    ...(options.standing === true ? { taskReport: { seq: 1, state: 'standing' as const } } : {}),
  };
}

const ACTION_NAMES: Record<ActionId, string> = {
  'open-window-picker': 'Go to window',
  'open-command-palette': 'Commands',
  'focus-next-window': 'Next window',
  'focus-previous-window': 'Previous window',
  'focus-next-attention': 'Next window needing you',
  'open-window-form': 'New window',
  'close-window': 'Close window',
  'interrupt-window': 'Interrupt window',
  'rename-window': 'Rename window',
  'minimize-window': 'Minimize window',
  'toggle-maximize-window': 'Maximize window',
  'toggle-work-mode': 'Switch Focus or Canvas',
  'tile-windows': 'Tile windows',
  'toggle-sidebar-focus': 'Sidebar',
  'toggle-needs-you-filter': 'Needs you filter',
  'toggle-chat': 'Chat',
  'toggle-plans': 'Board',
  'toggle-settings': 'Settings',
};

const FIND = 'Go to and commands';
const WINDOWS = 'Windows';
const PANELS = 'Focus and panels';

function sectionOf(action: ActionId): string {
  const index = ACTION_IDS.indexOf(action);
  if (index < ACTION_IDS.indexOf('focus-next-window')) {
    return FIND;
  }
  return index < ACTION_IDS.indexOf('toggle-sidebar-focus') ? WINDOWS : PANELS;
}

const KEYWORDS: Partial<Record<ActionId, string>> = {
  'interrupt-window': 'stop cancel',
  'toggle-plans': 'plans tickets',
};

/**
 * Every action, handed in reversed so the order of the rows has to come from
 * the action order rather than from the input. `blocked` names the actions that
 * cannot run now and why.
 */
function actionsOf(blocked: Partial<Record<ActionId, ActionBlockReason>> = {}): PickerAction[] {
  return [...ACTION_IDS].reverse().map((id) => {
    const reason = blocked[id];
    const verdict: ActionVerdict = reason === undefined ? { ok: true } : { ok: false, reason };
    return {
      id,
      name: ACTION_NAMES[id],
      keywords: KEYWORDS[id] ?? '',
      section: sectionOf(id),
      verdict,
    };
  });
}

function source(
  windows: readonly PickerWindow[],
  currentId: string | null = null,
  actions: readonly PickerAction[] = actionsOf()
): PickerSource<PickerWindow> {
  return { windows, currentId, actions };
}

function rowsOf(state: PickerState<PickerWindow>): PickerRow<PickerWindow>[] {
  return state.sections.flatMap((section) => section.rows);
}

function idOf(row: PickerRow<PickerWindow>): string {
  return row.kind === 'window' ? row.window.windowId : row.action.id;
}

function idsOf(state: PickerState<PickerWindow>): string[] {
  return rowsOf(state).map(idOf);
}

function sectionIds(state: PickerState<PickerWindow>): string[][] {
  return state.sections.map((section) => section.rows.map(idOf));
}

function rowOf(state: PickerState<PickerWindow>, id: string): PickerRow<PickerWindow> {
  const row = rowsOf(state).find((candidate) => idOf(candidate) === id);
  if (row === undefined) {
    throw new Error(`no row for ${id}`);
  }
  return row;
}

function keyOf(state: PickerState<PickerWindow>, id: string): string {
  return rowOf(state, id).key;
}

function selectedId(state: PickerState<PickerWindow>): string | null {
  const row = rowsOf(state).find((candidate) => candidate.key === state.selected);
  return row === undefined ? null : idOf(row);
}

function windowRowOf(state: PickerState<PickerWindow>, id: string) {
  const row = rowOf(state, id);
  if (row.kind !== 'window') {
    throw new Error(`${id} is not a window row`);
  }
  return row;
}

function actionRowOf(state: PickerState<PickerWindow>, id: ActionId) {
  const row = rowOf(state, id);
  if (row.kind !== 'action') {
    throw new Error(`${id} is not an action row`);
  }
  return row;
}

function defined<V>(value: V | null): V {
  if (value === null) {
    throw new Error('expected a value');
  }
  return value;
}

function typed(src: PickerSource<PickerWindow>, input: string): PickerState<PickerWindow> {
  return typeInput(openPicker('windows', src), input, src);
}

/** Windows of every status, in sidebar order. */
const MIXED: PickerWindow[] = [
  win('w1', 'running'),
  win('w2', 'waiting_input'),
  win('w3', 'unknown'),
  win('w4', 'waiting_approval'),
  win('w5', 'idle'),
  win('w6', 'waiting_input', { standing: true }),
  win('w7', 'exited'),
  win('w8', 'interrupted'),
  win('w9', 'waiting_approval'),
  win('w10', 'waiting_input'),
];

describe('picker-model: the windows with nothing typed', () => {
  it('lists Needs you by status rank then sidebar order, and the other windows unknown first', () => {
    const state = openPicker('windows', source(MIXED));

    expect(state.mode).toBe('windows');
    expect(state.sections.map((section) => section.heading)).toEqual([
      { kind: 'needs-you' },
      { kind: 'other-windows' },
    ]);
    expect(sectionIds(state)).toEqual([
      ['w4', 'w9', 'w2', 'w10'],
      ['w3', 'w6', 'w8', 'w1', 'w5', 'w7'],
    ]);
    expect(state.empty).toBeNull();
  });

  it('puts the current window last in its own section', () => {
    expect(sectionIds(openPicker('windows', source(MIXED, 'w4')))).toEqual([
      ['w9', 'w2', 'w10', 'w4'],
      ['w3', 'w6', 'w8', 'w1', 'w5', 'w7'],
    ]);
    const otherCurrent = openPicker('windows', source(MIXED, 'w3'));
    expect(sectionIds(otherCurrent)).toEqual([
      ['w4', 'w9', 'w2', 'w10'],
      ['w6', 'w8', 'w1', 'w5', 'w7', 'w3'],
    ]);
    expect(windowRowOf(otherCurrent, 'w3').isCurrent).toBe(true);
    expect(windowRowOf(otherCurrent, 'w6').isCurrent).toBe(false);
  });

  it('leaves out a section with no window, heading and all', () => {
    const quiet = openPicker('windows', source([win('a', 'idle'), win('b', 'running')]));
    expect(quiet.sections.map((section) => section.heading)).toEqual([{ kind: 'other-windows' }]);
    expect(sectionIds(quiet)).toEqual([['b', 'a']]);

    const waiting = openPicker('windows', source([win('a', 'waiting_input')]));
    expect(waiting.sections.map((section) => section.heading)).toEqual([{ kind: 'needs-you' }]);
  });
});

describe('picker-model: the windows as the user types', () => {
  it('ranks the whole name over its start, a word start, the middle and the other fields', () => {
    const state = typed(
      source([
        win('e', 'idle', { name: 'other', place: 'rev-room' }),
        win('d', 'idle', { name: 'prevent' }),
        win('c', 'idle', { name: 'my-review' }),
        win('b', 'idle', { name: 'review-a' }),
        win('a', 'idle', { name: 'rev' }),
      ]),
      'rev'
    );

    expect(idsOf(state)).toEqual(['a', 'b', 'c', 'd', 'e']);
    expect(state.sections.map((section) => section.heading)).toEqual([null]);
    expect(windowRowOf(state, 'd').marks.name).toEqual([{ start: 1, end: 4 }]);
    expect(windowRowOf(state, 'e').marks.name).toEqual([]);
    expect(windowRowOf(state, 'e').marks.place).toEqual([{ start: 0, end: 3 }]);
  });

  it('puts reviewer above a reviewer-2 that needs the user once reviewer is typed in full', () => {
    const src = source([
      win('r2', 'waiting_approval', { name: 'reviewer-2' }),
      win('r', 'idle', { name: 'reviewer' }),
    ]);
    expect(idsOf(typed(src, 'reviewer'))[0]).toBe('r');
  });

  it('matches the whole name with the words as typed, repeated words and runs of spaces included', () => {
    const repeated = source([
      win('short', 'waiting_approval', { name: 'test' }),
      win('twice', 'idle', { name: 'test test' }),
    ]);
    const twice = typed(repeated, 'test test');
    expect(idsOf(twice)[0]).toBe('twice');
    expect(windowRowOf(twice, 'twice').marks.name).toEqual([{ start: 0, end: 9 }]);

    const spaced = source([
      win('near', 'waiting_approval', { name: 'a bc' }),
      win('wide', 'idle', { name: 'a  b' }),
    ]);
    for (const input of ['a b', 'a  b', ' a b ', 'a　b']) {
      const state = typed(spaced, input);
      expect(idsOf(state)[0], JSON.stringify(input)).toBe('wide');
      expect(windowRowOf(state, 'wide').marks.name, JSON.stringify(input)).toEqual([
        { start: 0, end: 4 },
      ]);
    }
  });

  it('keeps only the windows every word is found in', () => {
    const state = typed(
      source([
        win('both', 'idle', { name: 'reviewer', place: 'yagura' }),
        win('name-only', 'idle', { name: 'reviewer', place: 'other' }),
        win('room-only', 'idle', { name: 'builder', place: 'yagura' }),
      ]),
      'yagura rev'
    );
    expect(idsOf(state)).toEqual(['both']);
  });

  it('finds every window whose status reads Needs, the ones needing the user first', () => {
    const state = typed(source(MIXED), 'needs');
    expect(idsOf(state)).toEqual(['w4', 'w9', 'w2', 'w10', 'w6']);
  });

  it('orders one stage as needs you, unknown, the reported input wait and then the quiet statuses, current last', () => {
    const inYagura = MIXED.map((w) => ({ ...w, place: 'yagura' }));
    expect(idsOf(typed(source(inYagura, 'w9'), 'yagura'))).toEqual([
      'w4',
      'w2',
      'w10',
      'w3',
      'w6',
      'w8',
      'w1',
      'w5',
      'w7',
      'w9',
    ]);
  });

  it('marks the union of the places each word is found at', () => {
    const state = typed(source([win('a', 'idle', { name: 'd5-wc' })]), 'd5 5-w');
    expect(windowRowOf(state, 'a').marks.name).toEqual([{ start: 0, end: 4 }]);
  });
});

describe('picker-model: folding what is typed and what is shown', () => {
  it('matches full-width letters and digits to their half-width forms, marked on the shown text', () => {
    const half = typed(source([win('a', 'idle', { name: 'd5-wc' })]), 'ｄ５');
    expect(windowRowOf(half, 'a').marks.name).toEqual([{ start: 0, end: 2 }]);

    const full = typed(source([win('b', 'idle', { name: 'ｄ５-wag2' })]), 'd5');
    expect(windowRowOf(full, 'b').marks.name).toEqual([{ start: 0, end: 2 }]);
  });

  it.each([
    ['a decomposed ka with a voiced mark', '/w/ガx', { start: 3, end: 5 }],
    ['a half-width ka with a voiced mark', '/w/ｶﾞ', { start: 3, end: 5 }],
    ['a decomposed hangul syllable', '/w/각', { start: 3, end: 6 }],
  ])('finds the composed character in %s and marks the whole cluster', (_, cwd, mark) => {
    const query = cwd.includes('ᄀ') ? '각' : 'ガ';
    const state = typed(source([win('a', 'idle', { name: 'alpha', cwd })]), query);
    expect(idsOf(state)).toEqual(['a']);
    expect(windowRowOf(state, 'a').marks.cwd).toEqual([mark]);
  });

  it('marks by code point on a cwd with a hidden character written out', () => {
    const src = source([win('a', 'idle', { name: 'alpha', cwd: '/tmp/\\u{202E}evil' })]);
    expect(windowRowOf(typed(src, 'evil'), 'a').marks.cwd).toEqual([{ start: 13, end: 17 }]);
    expect(windowRowOf(typed(src, '202e'), 'a').marks.cwd).toEqual([{ start: 8, end: 12 }]);
  });
});

describe('picker-model: folding without Intl.Segmenter', () => {
  const original = Object.getOwnPropertyDescriptor(Intl, 'Segmenter');

  afterEach(() => {
    if (original) {
      Object.defineProperty(Intl, 'Segmenter', original);
    }
    vi.resetModules();
  });

  // Loaded after removing Segmenter so the result does not depend on whether
  // the module looks for it on import or on each call.
  async function loadWithoutSegmenter(): Promise<typeof import('./picker-model')> {
    Object.defineProperty(Intl, 'Segmenter', {
      value: undefined,
      configurable: true,
      writable: true,
    });
    vi.resetModules();
    return import('./picker-model');
  }

  it.each([
    ['a decomposed ka with a voiced mark', '/w/ガx'],
    ['a half-width ka with a voiced mark', '/w/ｶﾞ'],
  ])('still finds ga in %s and marks the whole cluster', async (_, cwd) => {
    const model = await loadWithoutSegmenter();
    const src = source([win('a', 'idle', { name: 'alpha', cwd })]);
    const state = model.typeInput(model.openPicker('windows', src), 'ガ', src);
    expect(idsOf(state)).toEqual(['a']);
    expect(windowRowOf(state, 'a').marks.cwd).toEqual([{ start: 3, end: 5 }]);
  });
});

describe('picker-model: the mode it opens in and the open keys while open', () => {
  it('opens the commands with > typed and the caret after it, and the windows empty', () => {
    expect(openingInput('commands')).toEqual({ input: '>', caret: 1 });
    expect(openingInput('windows')).toEqual({ input: '', caret: 0 });

    const commands = openPicker('commands', source(MIXED));
    expect(commands.input).toBe('>');
    expect(commands.mode).toBe('commands');
    const windows = openPicker('windows', source(MIXED));
    expect(windows.input).toBe('');
    expect(windows.mode).toBe('windows');
  });

  it('switches on the other mode key, opening as that mode opens, and ignores its own key', () => {
    const src = source(MIXED);
    const typedWindows = typeInput(openPicker('windows', src), 'abc', src);

    const toCommands = defined(pressOpenKey(typedWindows, 'commands', src));
    expect(toCommands.mode).toBe('commands');
    expect(toCommands.input).toBe(openingInput('commands').input);
    expect(idsOf(toCommands)).toEqual(idsOf(openPicker('commands', src)));
    expect(toCommands.selected).toBe(rowsOf(toCommands)[0]?.key);
    expect(pressOpenKey(typedWindows, 'windows', src)).toBeNull();

    const typedCommands = typeInput(openPicker('commands', src), '>clo', src);
    const toWindows = defined(pressOpenKey(typedCommands, 'windows', src));
    expect(toWindows.mode).toBe('windows');
    expect(toWindows.input).toBe(openingInput('windows').input);
    expect(sectionIds(toWindows)).toEqual(sectionIds(openPicker('windows', src)));
    expect(toWindows.selected).toBe(rowsOf(toWindows)[0]?.key);
    expect(pressOpenKey(typedCommands, 'commands', src)).toBeNull();
  });

  it('moves between the modes as > is typed and deleted', () => {
    const src = source(MIXED);
    const commands = typeInput(openPicker('windows', src), '>', src);
    expect(commands.mode).toBe('commands');
    expect(typeInput(commands, '', src).mode).toBe('windows');
  });
});

describe('picker-model: the actions', () => {
  it('lists every action but the palette itself in the action order and sections, nothing typed', () => {
    const state = openPicker(
      'commands',
      source([], null, actionsOf({ 'close-window': 'no-window-on-screen' }))
    );

    expect(idsOf(state)).toEqual(ACTION_IDS.filter((id) => id !== 'open-command-palette'));
    expect(state.sections.map((section) => section.heading)).toEqual([
      { kind: 'actions', name: FIND },
      { kind: 'actions', name: WINDOWS },
      { kind: 'actions', name: PANELS },
    ]);
    expect(actionRowOf(state, 'close-window').blocked).toBe('no-window-on-screen');
    expect(actionRowOf(state, 'focus-next-window').blocked).toBeNull();
    expect(actionRowOf(state, 'close-window').showsSection).toBe(false);
  });

  it('orders typed matches by stage, then the ones that can run, then the action order', () => {
    const src = source(
      [],
      null,
      actionsOf({
        'focus-previous-window': 'nowhere-to-go',
        'close-window': 'no-window-on-screen',
        'tile-windows': 'canvas-only',
      })
    );

    const byWindow = typeInput(openPicker('commands', src), '>window', src);
    expect(idsOf(byWindow)).toEqual([
      'open-window-picker',
      'focus-next-window',
      'focus-next-attention',
      'open-window-form',
      'interrupt-window',
      'rename-window',
      'minimize-window',
      'toggle-maximize-window',
      'focus-previous-window',
      'close-window',
      'tile-windows',
      'toggle-work-mode',
    ]);
    expect(byWindow.sections.map((section) => section.heading)).toEqual([null]);
    expect(rowsOf(byWindow).every((row) => row.kind === 'action' && row.showsSection)).toBe(true);

    const ti = typeInput(openPicker('commands', src), '>ti', src);
    expect(idsOf(ti)).toEqual(['tile-windows', 'toggle-settings', 'toggle-plans']);
    expect(actionRowOf(ti, 'tile-windows').marks.name).toEqual([{ start: 0, end: 2 }]);
    expect(actionRowOf(ti, 'toggle-settings').marks.name).toEqual([{ start: 3, end: 5 }]);
    expect(actionRowOf(ti, 'toggle-plans').marks.name).toEqual([]);
  });

  it('never lists the palette itself, even when its name is typed', () => {
    const src = source([]);
    expect(idsOf(typeInput(openPicker('commands', src), '>commands', src))).toEqual([
      'open-window-picker',
    ]);
  });
});

describe('picker-model: when there is nothing to list', () => {
  it('offers New window when no window is open', () => {
    const state = openPicker('windows', source([]));
    expect(state.empty).toEqual({ kind: 'no-windows' });
    expect(idsOf(state)).toEqual(['open-window-form']);
    expect(state.selected).toBe(keyOf(state, 'open-window-form'));
    expect(chosenRow(state)?.key).toBe(keyOf(state, 'open-window-form'));

    const stillNone = refreshPicker(state, source([]));
    expect(stillNone.empty).toEqual({ kind: 'no-windows' });
    expect(idsOf(stillNone)).toEqual(['open-window-form']);
    expect(stillNone.selected).toBe(state.selected);
  });

  it('says what was typed, and offers no New window, when it is typed with no window open', () => {
    const state = typed(source([]), 'no such');
    expect(state.empty).toEqual({ kind: 'no-window-match', query: 'no such' });
    expect(rowsOf(state)).toEqual([]);
    expect(state.selected).toBeNull();

    const refreshed = refreshPicker(state, source([]));
    expect(refreshed.empty).toEqual({ kind: 'no-window-match', query: 'no such' });
    expect(rowsOf(refreshed)).toEqual([]);
  });

  it('says what was typed when no window and when no action matches', () => {
    const src = source(MIXED);
    const noWindow = typed(src, 'no such');
    expect(noWindow.empty).toEqual({ kind: 'no-window-match', query: 'no such' });
    expect(rowsOf(noWindow)).toEqual([]);
    expect(noWindow.selected).toBeNull();

    const noAction = typeInput(openPicker('commands', src), '>zzz', src);
    expect(noAction.empty).toEqual({ kind: 'no-command-match', query: 'zzz' });
    expect(rowsOf(noAction)).toEqual([]);
  });
});

describe('picker-model: while the picker is open', () => {
  const FIRST = [
    win('a', 'waiting_approval'),
    win('b', 'waiting_input'),
    win('c', 'running'),
    win('d', 'idle'),
  ];

  it('keeps the rows where they were when statuses or verdicts change, showing them as they are now', () => {
    const opened = openPicker('windows', source(FIRST, 'b'));
    const changed = [
      win('a', 'running'),
      win('b', 'waiting_input'),
      win('c', 'waiting_approval'),
      win('d', 'idle'),
    ];

    const state = refreshPicker(opened, source(changed, 'b'));
    expect(sectionIds(state)).toEqual([
      ['a', 'b'],
      ['c', 'd'],
    ]);
    expect(windowRowOf(state, 'a').window.statusWords).toBe('Running');
    expect(state.selected).toBe(opened.selected);

    // Typed so that whether an action can run orders the rows: building them
    // again from the new verdicts would move close-window up and rename-window down.
    const before = source(
      [],
      null,
      actionsOf({ 'close-window': 'no-window-on-screen', 'tile-windows': 'canvas-only' })
    );
    const after = source(
      [],
      null,
      actionsOf({ 'rename-window': 'no-window-on-screen', 'tile-windows': 'canvas-only' })
    );
    const byWindow = typeInput(openPicker('commands', before), '>window', before);
    expect(idsOf(typeInput(byWindow, '>window', after))).not.toEqual(idsOf(byWindow));

    const commands = refreshPicker(byWindow, after);
    expect(idsOf(commands)).toEqual(idsOf(byWindow));
    expect(commands.sections.map((section) => section.heading)).toEqual(
      byWindow.sections.map((section) => section.heading)
    );
    expect(actionRowOf(commands, 'close-window').action.verdict).toEqual({ ok: true });
    expect(actionRowOf(commands, 'close-window').blocked).toBeNull();
    expect(actionRowOf(commands, 'rename-window').blocked).toBe('no-window-on-screen');
    expect(commands.selected).toBe(byWindow.selected);
  });

  it('adds a new window at the end of the section it would join now, with nothing typed', () => {
    const opened = openPicker('windows', source(FIRST, 'b'));
    const state = refreshPicker(
      opened,
      source([...FIRST, win('e', 'waiting_input'), win('f', 'idle')], 'b')
    );
    expect(sectionIds(state)).toEqual([
      ['a', 'b', 'e'],
      ['c', 'd', 'f'],
    ]);
    expect(state.selected).toBe(opened.selected);
  });

  it('adds the heading with the first window of a section that was empty', () => {
    const quiet = [win('c', 'running'), win('d', 'idle')];
    const state = refreshPicker(
      openPicker('windows', source(quiet)),
      source([...quiet, win('e', 'waiting_approval')])
    );
    const needs = state.sections.find((section) => section.heading?.kind === 'needs-you');
    expect(needs?.rows.map(idOf)).toEqual(['e']);
    const other = state.sections.find((section) => section.heading?.kind === 'other-windows');
    expect(other?.rows.map(idOf)).toEqual(['c', 'd']);

    const offered = openPicker('windows', source([]));
    expect(idsOf(offered)).toEqual(['open-window-form']);
    const first = refreshPicker(offered, source([win('e', 'idle')]));
    expect(first.sections.map((section) => section.heading)).toEqual([{ kind: 'other-windows' }]);
    expect(sectionIds(first)).toEqual([['e']]);
    expect(first.empty).toBeNull();
  });

  it('puts a Needs you section that appears while open above the other windows, leaving the rows and selection', () => {
    const quiet = [win('c', 'running'), win('d', 'idle')];
    const opened = openPicker('windows', source(quiet));
    const onD = pointAt(opened, keyOf(opened, 'd'));
    const state = refreshPicker(onD, source([...quiet, win('e', 'waiting_approval')]));
    expect(state.sections.map((section) => section.heading)).toEqual([
      { kind: 'needs-you' },
      { kind: 'other-windows' },
    ]);
    expect(sectionIds(state)).toEqual([['e'], ['c', 'd']]);
    expect(state.selected).toBe(onD.selected);
  });

  it('does not add a window that was open all along when a status change makes it match what is typed', () => {
    const windows = [win('a', 'running'), win('b', 'waiting_input')];
    const typedNeeds = typed(source(windows), 'needs');
    expect(idsOf(typedNeeds)).toEqual(['b']);

    const state = refreshPicker(
      typedNeeds,
      source([win('a', 'waiting_input'), win('b', 'waiting_input')])
    );
    expect(idsOf(state)).toEqual(['b']);
  });

  it('adds a new window only when it matches what is typed', () => {
    const src = source([
      win('a1', 'idle', { name: 'alpha one' }),
      win('b', 'idle', { name: 'beta' }),
    ]);
    const state = refreshPicker(
      typed(src, 'alp'),
      source([
        ...src.windows,
        win('g', 'waiting_approval', { name: 'gamma' }),
        win('a2', 'idle', { name: 'alpha two' }),
      ])
    );
    expect(idsOf(state)).toEqual(['a1', 'a2']);
  });

  it('drops the row of a window that is gone', () => {
    const state = refreshPicker(
      openPicker('windows', source(FIRST)),
      source(FIRST.filter((w) => w.windowId !== 'b'))
    );
    expect(sectionIds(state)).toEqual([['a'], ['c', 'd']]);

    // The last window going leaves the New window offer, as opening with none does.
    const lastGone = refreshPicker(openPicker('windows', source([win('e', 'idle')])), source([]));
    expect(lastGone.empty).toEqual({ kind: 'no-windows' });
    expect(idsOf(lastGone)).toEqual(['open-window-form']);
  });
});

describe('picker-model: where the selection goes', () => {
  /** Needs you holds n1 and n2, the other windows o1 to o3. */
  const ROWS = [
    win('n1', 'waiting_approval'),
    win('n2', 'waiting_input'),
    win('o1', 'running'),
    win('o2', 'idle'),
    win('o3', 'exited'),
  ];
  const src = source(ROWS);

  it('starts on the first row, and on none when nothing is listed', () => {
    expect(selectedId(openPicker('windows', src))).toBe('n1');
    expect(typed(src, 'no such').selected).toBeNull();
  });

  it('goes back to the first row whenever the rows are built again', () => {
    const moved = moveSelection(openPicker('windows', src), 'down', 3);
    expect(selectedId(moved)).toBe('o2');
    const retyped = typeInput(moved, 'o', src);
    expect(retyped.selected).toBe(rowsOf(retyped)[0]?.key);
  });

  it('moves one row or a page across the headings and stops at either end', () => {
    let state = openPicker('windows', src);
    state = moveSelection(state, 'down', 1);
    expect(selectedId(state)).toBe('n2');
    state = moveSelection(state, 'down', 1);
    expect(selectedId(state)).toBe('o1');
    state = moveSelection(state, 'down', 10);
    expect(selectedId(state)).toBe('o3');
    state = moveSelection(state, 'down', 1);
    expect(selectedId(state)).toBe('o3');
    state = moveSelection(state, 'up', 2);
    expect(selectedId(state)).toBe('o1');
    state = moveSelection(state, 'up', 10);
    expect(selectedId(state)).toBe('n1');
    state = moveSelection(state, 'up', 1);
    expect(selectedId(state)).toBe('n1');
  });

  it('clears the selection when its window goes, so Enter does nothing; down then goes to the first row', () => {
    const onO2 = moveSelection(openPicker('windows', src), 'down', 3);
    const gone = refreshPicker(onO2, source(ROWS.filter((row) => row.windowId !== 'o2')));
    expect(gone.selected).toBeNull();
    expect(chosenRow(gone)).toBeNull();
    expect(selectedId(moveSelection(gone, 'down', 1))).toBe('n1');
    expect(selectedId(moveSelection(gone, 'down', 10))).toBe('n1');
    expect(selectedId(moveSelection(gone, 'up', 1))).toBe('o3');
    expect(selectedId(moveSelection(gone, 'up', 10))).toBe('o3');
  });

  it('keeps the selection when another window goes or a window is added', () => {
    const onO2 = moveSelection(openPicker('windows', src), 'down', 3);
    const otherGone = refreshPicker(onO2, source(ROWS.filter((row) => row.windowId !== 'n2')));
    expect(selectedId(otherGone)).toBe('o2');
    const added = refreshPicker(onO2, source([...ROWS, win('n3', 'waiting_input')]));
    expect(selectedId(added)).toBe('o2');

    const none = refreshPicker(onO2, source(ROWS.filter((row) => row.windowId !== 'o2')));
    const addedToNone = refreshPicker(none, source([...ROWS, win('n3', 'waiting_input')]));
    expect(addedToNone.selected).toBeNull();
  });

  it('follows the pointer, and Enter takes the row selected when it can run', () => {
    const opened = openPicker('windows', src);
    const pointed = pointAt(opened, keyOf(opened, 'o2'));
    expect(selectedId(pointed)).toBe('o2');
    expect(chosenRow(pointed)?.key).toBe(pointed.selected);

    const actions = source([], null, actionsOf({ 'close-window': 'no-window-on-screen' }));
    const commands = openPicker('commands', actions);
    const onBlocked = pointAt(commands, keyOf(commands, 'close-window'));
    expect(selectedId(onBlocked)).toBe('close-window');
    expect(chosenRow(onBlocked)).toBeNull();
    const onAllowed = pointAt(commands, keyOf(commands, 'rename-window'));
    expect(chosenRow(onAllowed)?.key).toBe(keyOf(commands, 'rename-window'));
  });
});
