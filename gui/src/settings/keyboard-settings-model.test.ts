/**
 * The pure model behind the Settings panel's Keyboard tab: the rows, what a
 * recorded chord does, when editing is refused, the notices a row shows, and
 * the controller that turns a press on Change / Reset / Remove into a save.
 * The component only renders these values, so every behaviour is measured here
 * without a DOM.
 */
import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';

import tauriConf from '../../src-tauri/tauri.conf.json';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';
import type { BindingRejection } from '../keymap/binding-rules';
import { defaultKeysFor, type DefaultKeys } from '../keymap/default-keymap';
import { ACTION_IDS, type ActionId } from '../keymap/key-binding';
import {
  assignChord,
  effectiveKeys,
  removeAction,
  resetAction,
  type Keys,
} from '../keymap/keymap-model';
import type { LoadKeymapResult } from '../ipc/session-client';
import {
  applyRecordedChord,
  createKeyboardTabController,
  keyboardRows,
  keymapEditBlock,
  keymapFileLocation,
  rowNotices,
  type KeymapEditBlock,
  type KeymapLoadStatus,
  type KeyboardTabController,
} from './keyboard-settings-model';

const PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];

function defaultKeys(platform: KeyboardPlatform): Keys {
  return effectiveKeys(defaultKeysFor(platform), {}, platform).keys;
}

/** A table in which two actions have no default chord, as the built-in table may have. */
const NO_DEFAULT: ActionId = 'minimize-window';
const ALSO_NO_DEFAULT: ActionId = 'toggle-maximize-window';
const WITH_GAPS: DefaultKeys = {
  ...defaultKeysFor('windows'),
  [NO_DEFAULT]: null,
  [ALSO_NO_DEFAULT]: null,
};

/** The group each row belongs to, in the order the mock shows them. */
const EXPECTED_GROUPS: Readonly<Record<ActionId, 'find' | 'windows' | 'focus-and-panels'>> = {
  'open-window-picker': 'find',
  'open-command-palette': 'find',
  'focus-next-window': 'windows',
  'focus-previous-window': 'windows',
  'focus-next-attention': 'windows',
  'open-window-form': 'windows',
  'close-window': 'windows',
  'interrupt-window': 'windows',
  'rename-window': 'windows',
  'minimize-window': 'windows',
  'toggle-maximize-window': 'windows',
  'toggle-work-mode': 'windows',
  'tile-windows': 'windows',
  'toggle-sidebar-focus': 'focus-and-panels',
  'toggle-needs-you-filter': 'focus-and-panels',
  'toggle-chat': 'focus-and-panels',
  'toggle-plans': 'focus-and-panels',
  'toggle-settings': 'focus-and-panels',
  'toggle-notifications': 'focus-and-panels',
};

describe('keyboardRows', () => {
  it('has one row per action in ACTION_IDS order', () => {
    const rows = keyboardRows(defaultKeys('windows'), defaultKeysFor('windows'));
    expect(rows.map((row) => row.action)).toEqual([...ACTION_IDS]);
    expect(rows).toHaveLength(ACTION_IDS.length);
  });

  it('puts each row in its group and keeps the rows of a group together, so no heading shows twice', () => {
    const rows = keyboardRows(defaultKeys('windows'), defaultKeysFor('windows'));
    for (const row of rows) {
      expect(row.group, row.action).toBe(EXPECTED_GROUPS[row.action]);
    }
    const headings = rows
      .filter((row, index) => rows[index - 1]?.group !== row.group)
      .map((row) => row.group);
    expect(new Set(headings).size).toBe(headings.length);
  });

  it('opens with the window picker and the command palette, the only rows of the find group', () => {
    const rows = keyboardRows(defaultKeys('windows'), defaultKeysFor('windows'));
    expect(rows.slice(0, 2).map(({ action, group }) => ({ action, group }))).toEqual([
      { action: 'open-window-picker', group: 'find' },
      { action: 'open-command-palette', group: 'find' },
    ]);
    expect(rows.filter((row) => row.group === 'find')).toHaveLength(2);
  });

  it.each(PLATFORMS)(
    'shows the default table on %s as unchanged, resettable never, removable where a default chord exists',
    (p) => {
      const rows = keyboardRows(defaultKeys(p), defaultKeysFor(p));
      for (const row of rows) {
        expect(row.chord, row.action).toBe(defaultKeysFor(p)[row.action]);
        expect(row.isChanged, row.action).toBe(false);
        expect(row.canReset, row.action).toBe(false);
        expect(row.canRemove, row.action).toBe(defaultKeysFor(p)[row.action] !== null);
      }
    }
  );

  it('shows a row without a default chord as unchanged and with nothing to reset or remove, until it gets a chord', () => {
    const untouched = effectiveKeys(WITH_GAPS, {}, 'windows').keys;
    const before = keyboardRows(untouched, WITH_GAPS).find((each) => each.action === NO_DEFAULT);
    expect(before?.chord).toBeNull();
    expect(before?.isChanged).toBe(false);
    expect(before?.canReset).toBe(false);
    expect(before?.canRemove).toBe(false);

    const given = assignChord(untouched, NO_DEFAULT, 'Ctrl+Shift+q').keys;
    const after = keyboardRows(given, WITH_GAPS).find((each) => each.action === NO_DEFAULT);
    expect(after?.isChanged).toBe(true);
    expect(after?.canReset).toBe(true);
    expect(after?.canRemove).toBe(true);
  });

  it('marks only the row whose chord differs from its default as changed and resettable', () => {
    const keys = assignChord(defaultKeys('windows'), 'toggle-chat', 'Ctrl+Shift+q').keys;
    const rows = keyboardRows(keys, defaultKeysFor('windows'));
    const changed = rows.filter((row) => row.isChanged).map((row) => row.action);
    expect(changed).toEqual(['toggle-chat']);
    const row = rows.find((each) => each.action === 'toggle-chat');
    expect(row?.chord).toBe('Ctrl+Shift+q');
    expect(row?.canReset).toBe(true);
    expect(row?.canRemove).toBe(true);
  });

  it('shows an action without a chord as not removable, but changed and resettable', () => {
    const keys = removeAction(defaultKeys('windows'), 'toggle-plans');
    const row = keyboardRows(keys, defaultKeysFor('windows')).find(
      (each) => each.action === 'toggle-plans'
    );
    expect(row?.chord).toBeNull();
    expect(row?.canRemove).toBe(false);
    expect(row?.isChanged).toBe(true);
    expect(row?.canReset).toBe(true);
  });

  it('is unchanged again once the default chord is back', () => {
    const changed = assignChord(defaultKeys('windows'), 'toggle-chat', 'Ctrl+Shift+q').keys;
    const back = resetAction(changed, defaultKeysFor('windows'), 'toggle-chat').keys;
    const row = keyboardRows(back, defaultKeysFor('windows')).find(
      (each) => each.action === 'toggle-chat'
    );
    expect(row?.isChanged).toBe(false);
    expect(row?.canReset).toBe(false);
  });

  it('compares against the defaults it is given, not against a fixed platform', () => {
    const macKeys = defaultKeys('mac');
    const againstMac = keyboardRows(macKeys, defaultKeysFor('mac'));
    const againstWindows = keyboardRows(macKeys, defaultKeysFor('windows'));
    expect(againstMac.some((row) => row.isChanged)).toBe(false);
    expect(againstWindows.some((row) => row.isChanged)).toBe(true);
  });
});

describe('applyRecordedChord', () => {
  const REJECTIONS: ReadonlyArray<[KeyboardPlatform, string, BindingRejection]> = [
    ['windows', 'Ctrl+r', 'reaches-terminal'],
    ['linux', 'Ctrl+r', 'reaches-terminal'],
    ['windows', 'Ctrl+Alt+k', 'types-characters'],
    ['windows', 'Ctrl+Alt+1', 'types-characters'],
    ['windows', 'Alt+k', 'alt-reaches-terminal'],
    ['windows', 'k', 'needs-modifier'],
    ['windows', 'Ctrl+Shift+c', 'reserved-for-clipboard'],
    ['windows', 'Ctrl+Shift+v', 'reserved-for-clipboard'],
    ['mac', 'Meta+w', 'reserved-by-app-menu'],
  ];

  it.each(REJECTIONS)(
    'rejects %s %s with the reason %s and names the chord it refused',
    (platform, chord, reason) => {
      const keys = defaultKeys(platform);
      const outcome = applyRecordedChord(keys, 'toggle-chat', chord, platform);
      expect(outcome.kind).toBe('rejected');
      if (outcome.kind === 'rejected') {
        expect(outcome.reason).toBe(reason);
        expect(outcome.chord).toBe(chord);
      }
    }
  );

  it('carries no table for a rejection, and leaves the table it was given as it was', () => {
    const keys = defaultKeys('windows');
    const before = JSON.stringify(keys);
    const outcome = applyRecordedChord(keys, 'toggle-chat', 'Ctrl+r', 'windows');
    expect('keys' in outcome).toBe(false);
    expect(JSON.stringify(keys)).toBe(before);
  });

  it('accepts a free chord and returns the table assignChord would', () => {
    const keys = defaultKeys('windows');
    const outcome = applyRecordedChord(keys, 'toggle-chat', 'Ctrl+Shift+q', 'windows');
    expect(outcome.kind).toBe('assigned');
    if (outcome.kind === 'assigned') {
      const expected = assignChord(keys, 'toggle-chat', 'Ctrl+Shift+q');
      expect(outcome.keys).toEqual(expected.keys);
      expect(outcome.movedFrom).toBeNull();
      expect(outcome.chord).toBe('Ctrl+Shift+q');
    }
  });

  it('accepts a chord that another action holds and reports whose it was', () => {
    const keys = defaultKeys('windows');
    const taken = keys['toggle-chat'];
    expect(taken).not.toBeNull();
    const outcome = applyRecordedChord(keys, 'toggle-plans', taken ?? '', 'windows');
    expect(outcome.kind).toBe('assigned');
    if (outcome.kind === 'assigned') {
      expect(outcome.movedFrom).toBe('toggle-chat');
      expect(outcome.keys['toggle-chat']).toBeNull();
      expect(outcome.keys['toggle-plans']).toBe(taken);
    }
  });

  it('judges a chord for the platform it is given: Meta+Shift+j is fine on mac, Ctrl+Shift+c is not on linux', () => {
    expect(applyRecordedChord(defaultKeys('mac'), 'toggle-chat', 'Shift+Meta+j', 'mac').kind).toBe(
      'assigned'
    );
    expect(
      applyRecordedChord(defaultKeys('linux'), 'toggle-chat', 'Ctrl+Shift+c', 'linux').kind
    ).toBe('rejected');
  });
});

describe('keymapEditBlock', () => {
  const loaded = (result: LoadKeymapResult): KeymapLoadStatus => ({ kind: 'loaded', result });

  it('lets a file that was read be edited', () => {
    expect(keymapEditBlock(loaded({ keys: {}, legacy_entries: 0 }))).toBeNull();
    expect(
      keymapEditBlock(loaded({ keys: { 'toggle-chat': 'Ctrl+Shift+q' }, legacy_entries: 2 }))
    ).toBeNull();
  });

  it('lets a missing file be edited: that is the normal way of running on the defaults', () => {
    expect(
      keymapEditBlock(loaded({ keys: {}, legacy_entries: 0, failure: 'not_found' }))
    ).toBeNull();
  });

  it.each<[LoadKeymapResult['failure'] & string, KeymapEditBlock]>([
    ['read_failed', 'read_failed'],
    ['invalid_toml', 'invalid_toml'],
    ['too_large', 'too_large'],
  ])('stops editing when the load failed with %s', (failure, block) => {
    expect(keymapEditBlock(loaded({ keys: {}, legacy_entries: 0, failure }))).toBe(block);
  });

  it('stops editing before the file has been loaded and when the load call was rejected', () => {
    expect(keymapEditBlock({ kind: 'not-loaded' })).toBe('not_loaded');
    expect(keymapEditBlock({ kind: 'rejected' })).toBe('rejected');
  });
});

describe('keymapFileLocation', () => {
  it('names the documented keymap.toml path of each platform', () => {
    expect(keymapFileLocation('windows')).toBe(
      `%LOCALAPPDATA%\\${tauriConf.identifier}\\keymap.toml`
    );
    expect(keymapFileLocation('mac')).toBe('~/Library/Application Support/yagura/keymap.toml');
    expect(keymapFileLocation('linux')).toBe('~/.config/yagura/keymap.toml');
  });
});

describe('rowNotices', () => {
  it('shows a rejection on the row that was being changed and nowhere else', () => {
    const notices = rowNotices('toggle-chat', {
      kind: 'rejected',
      chord: 'Ctrl+r',
      reason: 'reaches-terminal',
    });
    expect(notices).toEqual({
      'toggle-chat': { kind: 'rejected', chord: 'Ctrl+r', reason: 'reaches-terminal' },
    });
  });

  it('shows nothing for an assignment that took no chord from anyone', () => {
    expect(
      rowNotices('toggle-chat', { kind: 'assigned', chord: 'Ctrl+Shift+q', movedFrom: null })
    ).toEqual({});
  });

  it('shows a move on both rows: the one that received the chord and the one that lost it', () => {
    const notices = rowNotices('toggle-plans', {
      kind: 'assigned',
      chord: 'Ctrl+Shift+l',
      movedFrom: 'toggle-chat',
    });
    expect(notices['toggle-plans']).toEqual({
      kind: 'took',
      chord: 'Ctrl+Shift+l',
      from: 'toggle-chat',
    });
    expect(notices['toggle-chat']).toEqual({
      kind: 'lost',
      chord: 'Ctrl+Shift+l',
      to: 'toggle-plans',
    });
    expect(Object.keys(notices).sort()).toEqual(['toggle-chat', 'toggle-plans']);
  });
});

/** A dispatcher and a store that record what the controller asks of them. */
function harness(
  options: {
    block?: KeymapEditBlock | null;
    platform?: KeyboardPlatform;
    defaults?: DefaultKeys;
  } = {}
) {
  const platform = options.platform ?? 'windows';
  const defaults = options.defaults ?? defaultKeysFor(platform);
  const state = {
    keys: effectiveKeys(defaults, {}, platform).keys,
    block: options.block ?? null,
    began: [] as Array<(chord: string | null) => void>,
    cancels: 0,
    edits: [] as Array<(keys: Keys) => Keys>,
    settle: [] as Array<(saved: boolean) => void>,
  };
  const controller: KeyboardTabController = createKeyboardTabController({
    platform,
    defaults,
    keys: () => state.keys,
    editBlock: () => state.block,
    beginRecording: (onResult) => {
      state.began.push(onResult);
    },
    cancelRecording: () => {
      state.cancels += 1;
    },
    commit: (edit) =>
      new Promise<boolean>((resolve) => {
        state.edits.push(edit);
        state.settle.push(resolve);
      }),
  });
  return { state, controller, platform };
}

async function flush(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

function lastResult(state: ReturnType<typeof harness>['state']): (chord: string | null) => void {
  const report = state.began[state.began.length - 1];
  if (report === undefined) throw new Error('recording was never started');
  return report;
}

describe('the Keyboard tab controller: recording', () => {
  it('starts a recording when Change is pressed and shows that row as recording', () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    expect(state.began).toHaveLength(1);
    expect(get(controller).recording).toBe('toggle-chat');
  });

  it('starts with nothing recording and no notices', () => {
    const { controller } = harness();
    expect(get(controller)).toEqual({ recording: null, notices: {} });
  });

  it('stops showing the row as recording when the recording ends without a chord', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)(null);
    await flush();
    expect(get(controller).recording).toBeNull();
    expect(state.edits).toHaveLength(0);
    expect(get(controller).notices).toEqual({});
  });

  it('stops showing the row as recording the moment a chord arrives, before the save finishes', () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+Shift+q');
    expect(state.edits).toHaveLength(1);
    expect(get(controller).recording).toBeNull();
  });

  it('stops showing the row as recording after a rejected chord, saves nothing, and tells the row why', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+r');
    await flush();
    expect(get(controller).recording).toBeNull();
    expect(state.edits).toHaveLength(0);
    expect(get(controller).notices['toggle-chat']).toEqual({
      kind: 'rejected',
      chord: 'Ctrl+r',
      reason: 'reaches-terminal',
    });
  });

  it('follows the row pressed last: an earlier recording that reports late changes nothing', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    const stale = lastResult(state);
    controller.change('toggle-plans');
    expect(get(controller).recording).toBe('toggle-plans');
    stale('Ctrl+Shift+q');
    await flush();
    expect(state.edits).toHaveLength(0);
    expect(get(controller).recording).toBe('toggle-plans');
  });
});

describe('the Keyboard tab controller: saving a recorded chord', () => {
  it('hands the store an edit that gives the action the chord', () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+Shift+q');
    const edit = state.edits[0];
    expect(edit).toBeDefined();
    const expected = applyRecordedChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q', 'windows');
    expect(expected.kind).toBe('assigned');
    if (expected.kind === 'assigned') {
      expect(edit?.(state.keys)).toEqual(expected.keys);
    }
  });

  it('hands the store an edit, not a table: a change made in between is kept', () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+Shift+q');
    const meanwhile = removeAction(state.keys, 'toggle-plans');
    const result = state.edits[0]?.(meanwhile);
    expect(result?.['toggle-chat']).toBe('Ctrl+Shift+q');
    expect(result?.['toggle-plans']).toBeNull();
  });

  it('shows the move on both rows once the save succeeded', async () => {
    const { state, controller } = harness();
    const taken = state.keys['toggle-chat'] ?? '';
    controller.change('toggle-plans');
    lastResult(state)(taken);
    expect(get(controller).notices).toEqual({});
    state.settle[0]?.(true);
    await flush();
    const notices = get(controller).notices;
    expect(notices['toggle-plans']).toEqual({ kind: 'took', chord: taken, from: 'toggle-chat' });
    expect(notices['toggle-chat']).toEqual({ kind: 'lost', chord: taken, to: 'toggle-plans' });
  });

  it('shows no move when the save failed, because nothing moved', async () => {
    const { state, controller } = harness();
    const taken = state.keys['toggle-chat'] ?? '';
    controller.change('toggle-plans');
    lastResult(state)(taken);
    state.settle[0]?.(false);
    await flush();
    expect(get(controller).notices).toEqual({});
    expect(get(controller).recording).toBeNull();
  });
});

describe('the Keyboard tab controller: Reset and Remove', () => {
  it('saves a Reset as an edit that restores the default chord', async () => {
    const { state, controller } = harness();
    state.keys = assignChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q').keys;
    const pending = controller.reset('toggle-chat');
    expect(state.edits).toHaveLength(1);
    expect(state.edits[0]?.(state.keys)).toEqual(
      resetAction(state.keys, defaultKeysFor('windows'), 'toggle-chat').keys
    );
    state.settle[0]?.(true);
    await pending;
  });

  it('hands the store edits, not tables, for Reset and Remove: a change made in between is kept', () => {
    const { state, controller } = harness();
    state.keys = assignChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q').keys;
    void controller.reset('toggle-chat');
    void controller.remove('toggle-settings');
    const meanwhile = removeAction(state.keys, 'toggle-plans');
    const afterReset = state.edits[0]?.(meanwhile);
    expect(afterReset?.['toggle-chat']).toBe(defaultKeysFor('windows')['toggle-chat']);
    expect(afterReset?.['toggle-plans']).toBeNull();
    const afterRemove = state.edits[1]?.(meanwhile);
    expect(afterRemove?.['toggle-settings']).toBeNull();
    expect(afterRemove?.['toggle-plans']).toBeNull();
    expect(afterRemove?.['toggle-chat']).toBe('Ctrl+Shift+q');
  });

  it('saves nothing for a Reset on a row that is already at its default', async () => {
    const { state, controller } = harness();
    await controller.reset('toggle-chat');
    expect(state.edits).toHaveLength(0);
  });

  it('shows the move when a Reset takes its default chord back from another row', async () => {
    const { state, controller } = harness();
    const defaultChat = state.keys['toggle-chat'] ?? '';
    state.keys = assignChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q').keys;
    state.keys = assignChord(state.keys, 'toggle-plans', defaultChat).keys;
    const pending = controller.reset('toggle-chat');
    state.settle[0]?.(true);
    await pending;
    const notices = get(controller).notices;
    expect(notices['toggle-chat']).toEqual({
      kind: 'took',
      chord: defaultChat,
      from: 'toggle-plans',
    });
    expect(notices['toggle-plans']).toEqual({
      kind: 'lost',
      chord: defaultChat,
      to: 'toggle-chat',
    });
  });

  it('saves a Reset on a row without a default chord as its Remove, shows no notice, and saves nothing once the row has no chord', async () => {
    const { state, controller } = harness({ defaults: WITH_GAPS });
    expect(state.keys[ALSO_NO_DEFAULT]).toBeNull();
    state.keys = assignChord(state.keys, NO_DEFAULT, 'Ctrl+Shift+q').keys;
    const pending = controller.reset(NO_DEFAULT);
    expect(state.edits).toHaveLength(1);
    expect(state.edits[0]?.(state.keys)).toEqual(removeAction(state.keys, NO_DEFAULT));
    state.settle[0]?.(true);
    await pending;
    expect(get(controller).notices).toEqual({});

    await controller.reset(ALSO_NO_DEFAULT);
    expect(state.edits).toHaveLength(1);
  });

  it('saves a Remove as an edit that leaves the action without a chord', async () => {
    const { state, controller } = harness();
    const pending = controller.remove('toggle-chat');
    expect(state.edits).toHaveLength(1);
    expect(state.edits[0]?.(state.keys)).toEqual(removeAction(state.keys, 'toggle-chat'));
    state.settle[0]?.(true);
    await pending;
  });

  it('saves nothing for a Remove on a row that has no chord', async () => {
    const { state, controller } = harness();
    state.keys = removeAction(state.keys, 'toggle-chat');
    await controller.remove('toggle-chat');
    expect(state.edits).toHaveLength(0);
  });

  it('ends a recording that is under way when Reset or Remove is pressed', async () => {
    const { state, controller } = harness();
    state.keys = assignChord(state.keys, 'toggle-plans', 'Ctrl+Shift+q').keys;
    controller.change('toggle-chat');
    const pending = controller.reset('toggle-plans');
    expect(get(controller).recording).toBeNull();
    expect(state.cancels).toBe(1);
    state.settle[0]?.(true);
    await pending;

    controller.change('toggle-chat');
    const removal = controller.remove('toggle-chat');
    expect(get(controller).recording).toBeNull();
    expect(state.cancels).toBe(2);
    state.settle[1]?.(true);
    await removal;
  });
});

describe('the Keyboard tab controller: editing is refused', () => {
  const BLOCKS: readonly KeymapEditBlock[] = [
    'not_loaded',
    'rejected',
    'read_failed',
    'invalid_toml',
    'too_large',
  ];

  it.each(BLOCKS)(
    'does nothing on Change, Reset or Remove while the block is %s',
    async (block) => {
      const { state, controller } = harness({ block });
      state.keys = assignChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q').keys;
      controller.change('toggle-chat');
      await controller.reset('toggle-chat');
      await controller.remove('toggle-chat');
      expect(state.began).toHaveLength(0);
      expect(state.edits).toHaveLength(0);
      expect(get(controller).recording).toBeNull();
    }
  );

  it('does the same three things when nothing blocks (the control for the refusals above)', async () => {
    const { state, controller } = harness({ block: null });
    state.keys = assignChord(state.keys, 'toggle-chat', 'Ctrl+Shift+q').keys;
    controller.change('toggle-chat');
    expect(state.began).toHaveLength(1);
    const reset = controller.reset('toggle-chat');
    const remove = controller.remove('toggle-plans');
    expect(state.edits).toHaveLength(2);
    state.settle.forEach((resolve) => resolve(true));
    await Promise.all([reset, remove]);
  });

  it('reads the block when a press happens, so a file that fails to load later still stops editing', () => {
    const { state, controller } = harness({ block: null });
    state.block = 'invalid_toml';
    controller.change('toggle-chat');
    expect(state.began).toHaveLength(0);
  });
});

describe('the Keyboard tab controller: leaving the tab', () => {
  it('cancels a recording that is under way and stops showing the row as recording', () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    controller.dispose();
    expect(state.cancels).toBe(1);
    expect(get(controller).recording).toBeNull();
  });

  it('does not cancel anything when no recording was under way', () => {
    const { state, controller } = harness();
    controller.dispose();
    expect(state.cancels).toBe(0);
  });
});

describe('the Keyboard tab controller: notices and stale recordings', () => {
  it('clears only the notice of the row that was pressed, keeping the others', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+r');
    controller.change('toggle-plans');
    lastResult(state)('Ctrl+r');
    await flush();
    expect(Object.keys(get(controller).notices).sort()).toEqual(['toggle-chat', 'toggle-plans']);

    controller.change('toggle-chat');

    expect(Object.keys(get(controller).notices)).toEqual(['toggle-plans']);
  });

  it('clears the notice of a row when Reset or Remove is pressed on it, even when they save nothing', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+r');
    await flush();
    expect(get(controller).notices['toggle-chat']).toBeDefined();

    await controller.reset('toggle-chat');
    expect(get(controller).notices['toggle-chat']).toBeUndefined();

    controller.change('toggle-chat');
    lastResult(state)('Ctrl+r');
    await flush();
    state.keys = { ...state.keys, 'toggle-chat': null };
    await controller.remove('toggle-chat');
    expect(get(controller).notices['toggle-chat']).toBeUndefined();
    expect(state.edits).toHaveLength(0);
  });

  it('keeps the other rows notices when Reset or Remove is pressed', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    lastResult(state)('Ctrl+r');
    controller.change('toggle-plans');
    lastResult(state)('Ctrl+r');
    await flush();

    await controller.reset('toggle-chat');
    state.keys = { ...state.keys, 'toggle-chat': null };
    await controller.remove('toggle-chat');

    expect(Object.keys(get(controller).notices)).toEqual(['toggle-plans']);
  });

  it('ignores a recording that reports after the tab was left and another row was pressed', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    const stale = lastResult(state);
    controller.dispose();
    controller.change('toggle-plans');

    stale('Ctrl+Shift+q');
    await flush();

    expect(state.edits).toHaveLength(0);
    expect(get(controller).recording).toBe('toggle-plans');
  });

  it('ignores a recording that reports after Reset ended it and another row was pressed', async () => {
    const { state, controller } = harness();
    controller.change('toggle-chat');
    const stale = lastResult(state);
    await controller.reset('toggle-settings');
    controller.change('toggle-plans');

    stale('Ctrl+Shift+q');
    await flush();

    expect(state.edits).toHaveLength(0);
    expect(get(controller).recording).toBe('toggle-plans');
  });
});
