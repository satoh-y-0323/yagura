/**
 * The keymap model: how a keymap file's overrides and the defaults combine,
 * how the Keyboard tab edits the result, and how it is written back.
 */
import { describe, expect, it } from 'vitest';

import { validateBinding } from './binding-rules';
import { defaultKeysFor } from './default-keymap';
import type { KeyboardPlatform } from './keyboard-platform';
import { ACTION_IDS, type ActionId, type KeyMap } from './key-binding';
import {
  assignChord,
  bindingsOf,
  effectiveKeys,
  overridesOf,
  removeAction,
  resetAction,
  type Keys,
} from './keymap-model';

const PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];

type Defaults = Readonly<Record<ActionId, string>>;

function defaultsOf(platform: KeyboardPlatform): Defaults {
  return defaultKeysFor(platform);
}

function untouched(platform: KeyboardPlatform): Keys {
  return effectiveKeys(defaultsOf(platform), {}, platform).keys;
}

function chordsOf(keys: Keys): string[] {
  return ACTION_IDS.flatMap((action) => {
    const chord = keys[action];
    return chord === null ? [] : [chord];
  });
}

function expectOneToOne(keys: Keys): void {
  const chords = chordsOf(keys);
  expect(new Set(chords).size, JSON.stringify(keys)).toBe(chords.length);
}

/** Every action keeps its default chord except the ones named in `changes`. */
function defaultsWith(
  platform: KeyboardPlatform,
  changes: Partial<Record<ActionId, string | null>>
) {
  const expected: Record<string, string | null> = { ...defaultsOf(platform) };
  for (const [action, chord] of Object.entries(changes)) {
    expected[action] = chord ?? null;
  }
  return expected;
}

describe('effectiveKeys without overrides', () => {
  it.each(PLATFORMS)('gives every action its default chord on %s', (platform) => {
    const result = effectiveKeys(defaultsOf(platform), {}, platform);
    expect({ ...result.keys }).toEqual({ ...defaultsOf(platform) });
    expect(result.discarded).toBe(0);
  });

  it('has a chord or null for exactly the twelve actions', () => {
    const { keys } = effectiveKeys(defaultsOf('windows'), { 'close-window': '' }, 'windows');
    expect(Object.keys(keys).sort()).toEqual([...ACTION_IDS].sort());
  });
});

describe('effectiveKeys: an override replaces the chord of its own action', () => {
  it('moves the action to the new chord and leaves the others alone', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'close-window': 'Ctrl+Shift+q' },
      'windows'
    );
    expect({ ...result.keys }).toEqual(defaultsWith('windows', { 'close-window': 'Ctrl+Shift+q' }));
    expect(result.discarded).toBe(0);
  });

  it('accepts an override that spells the default chord again', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'close-window': 'Ctrl+Shift+w' },
      'windows'
    );
    expect({ ...result.keys }).toEqual({ ...defaultsOf('windows') });
    expect(result.discarded).toBe(0);
  });

  it('applies to every platform with its own rules', () => {
    for (const platform of PLATFORMS) {
      const result = effectiveKeys(
        defaultsOf(platform),
        { 'toggle-chat': 'Ctrl+Shift+q' },
        platform
      );
      expect(result.keys['toggle-chat'], platform).toBe('Ctrl+Shift+q');
    }
  });
});

describe('effectiveKeys: an override takes the chord away from the action that had it by default', () => {
  it('leaves the previous holder without a chord', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+Shift+w' },
      'windows'
    );
    expect({ ...result.keys }).toEqual(
      defaultsWith('windows', { 'toggle-chat': 'Ctrl+Shift+w', 'close-window': null })
    );
    expect(result.discarded).toBe(0);
    expectOneToOne(result.keys);
  });

  it('keeps the previous holder when it is overridden itself', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+Shift+w', 'close-window': 'Ctrl+Shift+q' },
      'windows'
    );
    expect({ ...result.keys }).toEqual(
      defaultsWith('windows', { 'toggle-chat': 'Ctrl+Shift+w', 'close-window': 'Ctrl+Shift+q' })
    );
    expect(result.discarded).toBe(0);
  });

  it('lets two actions swap their default chords', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+Shift+p', 'toggle-plans': 'Ctrl+Shift+l' },
      'windows'
    );
    expect({ ...result.keys }).toEqual(
      defaultsWith('windows', { 'toggle-chat': 'Ctrl+Shift+p', 'toggle-plans': 'Ctrl+Shift+l' })
    );
    expect(result.discarded).toBe(0);
  });

  it('does not take a chord away for an override that was discarded', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+r', 'toggle-plans': 'Ctrl+Shift+w' },
      'windows'
    );
    // The chat override is refused, so chat keeps its default; plans still takes the close chord.
    expect({ ...result.keys }).toEqual(
      defaultsWith('windows', { 'toggle-plans': 'Ctrl+Shift+w', 'close-window': null })
    );
    expect(result.discarded).toBe(1);
  });
});

describe('effectiveKeys: an empty chord means no chord', () => {
  it('leaves the action without a chord and does not count it', () => {
    const result = effectiveKeys(defaultsOf('windows'), { 'close-window': '' }, 'windows');
    expect({ ...result.keys }).toEqual(defaultsWith('windows', { 'close-window': null }));
    expect(result.discarded).toBe(0);
  });

  it('does not hand the freed default chord to anyone', () => {
    const { keys } = effectiveKeys(defaultsOf('windows'), { 'close-window': '' }, 'windows');
    expect(bindingsOf(keys).has('Ctrl+Shift+w')).toBe(false);
  });

  it('is told apart from a refused chord, which is counted', () => {
    const empty = effectiveKeys(defaultsOf('windows'), { 'toggle-plans': '' }, 'windows');
    const refused = effectiveKeys(defaultsOf('windows'), { 'toggle-plans': 'Ctrl+r' }, 'windows');
    expect(empty.discarded).toBe(0);
    expect(refused.discarded).toBe(1);
    expect(empty.keys['toggle-plans']).toBeNull();
    expect(refused.keys['toggle-plans']).toBe('Ctrl+Shift+p');
  });
});

describe('effectiveKeys: an override that cannot be used is discarded and counted once', () => {
  const REFUSED: readonly [string, Record<string, string>][] = [
    ['an unknown action', { 'restart-daemon': 'Ctrl+Shift+q' }],
    ['a syntax error', { 'toggle-chat': 'Shift+Ctrl+q' }],
    ['a chord without a modifier', { 'toggle-chat': 'q' }],
    ['an Alt-only chord', { 'toggle-chat': 'Alt+q' }],
    ['a Ctrl+Alt chord on a letter', { 'toggle-chat': 'Ctrl+Alt+q' }],
    ['a chord the terminal needs', { 'toggle-chat': 'Ctrl+r' }],
  ];

  it.each(REFUSED)('discards %s and keeps the defaults', (_reason, overrides) => {
    const result = effectiveKeys(defaultsOf('windows'), overrides, 'windows');
    expect({ ...result.keys }).toEqual({ ...defaultsOf('windows') });
    expect(result.discarded).toBe(1);
  });

  it('counts every refused entry and keeps the good one among them', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      {
        'focus-next-window': 'Ctrl+r',
        'close-window': 'Ctrl+Shift+q',
        'toggle-chat': 'Shift+Ctrl+l',
        'unknown-action': 'Ctrl+Shift+x',
      },
      'windows'
    );
    expect(result.keys['close-window']).toBe('Ctrl+Shift+q');
    expect(result.keys['focus-next-window']).toBe('Ctrl+Tab');
    expect(result.keys['toggle-chat']).toBe('Ctrl+Shift+l');
    expect(result.discarded).toBe(3);
  });

  it('applies the rules of the platform it is asked for', () => {
    const clipboard = { 'toggle-chat': 'Ctrl+Shift+c' };
    const menu = { 'toggle-chat': 'Meta+w' };
    expect(effectiveKeys(defaultsOf('windows'), clipboard, 'windows').discarded).toBe(1);
    expect(effectiveKeys(defaultsOf('mac'), clipboard, 'mac').discarded).toBe(0);
    expect(effectiveKeys(defaultsOf('mac'), menu, 'mac').discarded).toBe(1);
    expect(effectiveKeys(defaultsOf('windows'), menu, 'windows').discarded).toBe(0);
  });

  it('refuses each rule through validateBinding, not a private copy', () => {
    for (const chord of ['Ctrl+r', 'Ctrl+Alt+q', 'Alt+q', 'q', 'Ctrl+Shift+v']) {
      const refusal = validateBinding(chord, 'windows');
      expect(refusal, chord).not.toBeNull();
      const result = effectiveKeys(defaultsOf('windows'), { 'toggle-chat': chord }, 'windows');
      expect(result.discarded, chord).toBe(1);
    }
  });

  it('never lets a value outside the chord syntax through', () => {
    const payloads: string[] = [
      String.fromCharCode(0x03),
      String.fromCharCode(0x1b) + '[A',
      'rm -rf /',
      'cmd.exe /c calc',
      ' Ctrl+Shift+q',
      'Ctrl+Shift+q\n',
      '__proto__',
    ];
    for (const payload of payloads) {
      const result = effectiveKeys(defaultsOf('windows'), { 'toggle-chat': payload }, 'windows');
      expect({ ...result.keys }, JSON.stringify(payload)).toEqual({ ...defaultsOf('windows') });
      expect(result.discarded, JSON.stringify(payload)).toBe(1);
    }
  });

  it('never lets an action name outside the vocabulary through', () => {
    const names: string[] = [
      '__proto__',
      'constructor',
      'toString',
      ' close-window ',
      'Close-Window',
    ];
    // fromEntries defines own properties, so `__proto__` really is a key here.
    const overrides: Record<string, string> = Object.fromEntries(
      names.map((name) => [name, 'Ctrl+Shift+q'])
    );
    const result = effectiveKeys(defaultsOf('windows'), overrides, 'windows');
    expect({ ...result.keys }).toEqual({ ...defaultsOf('windows') });
    expect(result.discarded).toBe(names.length);
  });
});

describe('effectiveKeys: two overrides on one chord', () => {
  it('keeps the action that comes first in the action order, whatever the object order', () => {
    // plans comes after chat in the action order; the object lists it first.
    const overrides: Record<string, string> = {
      'toggle-plans': 'Ctrl+Shift+q',
      'toggle-chat': 'Ctrl+Shift+q',
    };
    const result = effectiveKeys(defaultsOf('windows'), overrides, 'windows');
    expect(result.keys['toggle-chat']).toBe('Ctrl+Shift+q');
    expect(result.keys['toggle-plans']).toBe('Ctrl+Shift+p');
    expect(result.discarded).toBe(1);
    expectOneToOne(result.keys);
  });

  it('gives the same answer when the object order is reversed', () => {
    const forward = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+Shift+q', 'toggle-plans': 'Ctrl+Shift+q' },
      'windows'
    );
    const backward = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-plans': 'Ctrl+Shift+q', 'toggle-chat': 'Ctrl+Shift+q' },
      'windows'
    );
    expect({ ...backward.keys }).toEqual({ ...forward.keys });
    expect(backward.discarded).toBe(forward.discarded);
  });

  it('counts each of two refused overrides even when they name the same chord', () => {
    const result = effectiveKeys(
      defaultsOf('windows'),
      { 'toggle-chat': 'Ctrl+r', 'toggle-plans': 'Ctrl+r' },
      'windows'
    );
    expect(result.discarded).toBe(2);
    expect({ ...result.keys }).toEqual({ ...defaultsOf('windows') });
  });
});

describe('effectiveKeys: an override is independent of the table it was written against', () => {
  const NEXT_VERSION: Defaults = {
    ...defaultsOf('windows'),
    'close-window': 'Ctrl+Shift+x',
    'toggle-chat': 'Ctrl+Shift+y',
  };

  it('keeps the chord of an overridden action when the defaults change', () => {
    const overrides: Record<string, string> = { 'close-window': 'Ctrl+Shift+q' };
    const before = effectiveKeys(defaultsOf('windows'), overrides, 'windows');
    const after = effectiveKeys(NEXT_VERSION, overrides, 'windows');
    expect(before.keys['close-window']).toBe('Ctrl+Shift+q');
    expect(after.keys['close-window']).toBe('Ctrl+Shift+q');
  });

  it('follows the new default for an action that has no override', () => {
    const after = effectiveKeys(NEXT_VERSION, { 'close-window': 'Ctrl+Shift+q' }, 'windows');
    expect(after.keys['toggle-chat']).toBe('Ctrl+Shift+y');
  });

  it('keeps an action without a chord when the defaults change', () => {
    const after = effectiveKeys(NEXT_VERSION, { 'close-window': '' }, 'windows');
    expect(after.keys['close-window']).toBeNull();
  });
});

describe('effectiveKeys: purity', () => {
  it('leaves the overrides and the defaults it was given unchanged', () => {
    const overrides: Record<string, string> = Object.freeze({
      'toggle-chat': 'Ctrl+Shift+w',
      'toggle-plans': '',
    });
    const defaults = Object.freeze({ ...defaultsOf('windows') });
    const first = effectiveKeys(defaults, overrides, 'windows');
    const second = effectiveKeys(defaults, overrides, 'windows');
    expect({ ...second.keys }).toEqual({ ...first.keys });
    expect({ ...overrides }).toEqual({ 'toggle-chat': 'Ctrl+Shift+w', 'toggle-plans': '' });
    expect({ ...defaults }).toEqual({ ...defaultsOf('windows') });
  });
});

describe('assignChord', () => {
  it.each(PLATFORMS)('gives a free chord to the action on %s', (platform) => {
    const start = untouched(platform);
    const result = assignChord(start, 'toggle-chat', 'Ctrl+Shift+q');
    expect({ ...result.keys }).toEqual({ ...start, 'toggle-chat': 'Ctrl+Shift+q' });
    expect(result.movedFrom).toBeNull();
  });

  it('moves a chord from the action that held it and reports which', () => {
    const start = untouched('windows');
    const result = assignChord(start, 'toggle-chat', 'Ctrl+Shift+w');
    expect(result.keys['toggle-chat']).toBe('Ctrl+Shift+w');
    expect(result.keys['close-window']).toBeNull();
    expect(result.movedFrom).toBe('close-window');
    expect({ ...result.keys }).toEqual({
      ...start,
      'toggle-chat': 'Ctrl+Shift+w',
      'close-window': null,
    });
  });

  it('does not report a move when the action already had the chord', () => {
    const start = untouched('windows');
    const result = assignChord(start, 'close-window', 'Ctrl+Shift+w');
    expect({ ...result.keys }).toEqual({ ...start });
    expect(result.movedFrom).toBeNull();
  });

  it('frees the chord the action had before', () => {
    const start = untouched('windows');
    const result = assignChord(start, 'toggle-chat', 'Ctrl+Shift+q');
    expect(bindingsOf(result.keys).has('Ctrl+Shift+l')).toBe(false);
    expect(bindingsOf(result.keys).get('Ctrl+Shift+q')).toBe('toggle-chat');
  });

  it('gives a chord to an action that had none', () => {
    const start = removeAction(untouched('windows'), 'toggle-chat');
    const result = assignChord(start, 'toggle-chat', 'Ctrl+Shift+q');
    expect(result.keys['toggle-chat']).toBe('Ctrl+Shift+q');
    expect(result.movedFrom).toBeNull();
  });

  it('leaves the keys it was given unchanged', () => {
    const start: Keys = Object.freeze({ ...untouched('windows') });
    assignChord(start, 'toggle-chat', 'Ctrl+Shift+w');
    expect({ ...start }).toEqual({ ...untouched('windows') });
  });

  it('keeps the chords one to one', () => {
    let keys = untouched('windows');
    for (const [action, chord] of [
      ['toggle-chat', 'Ctrl+Shift+w'],
      ['toggle-plans', 'Ctrl+Shift+w'],
      ['close-window', 'Ctrl+Shift+p'],
    ] as const) {
      keys = assignChord(keys, action, chord).keys;
      expectOneToOne(keys);
    }
  });
});

describe('resetAction', () => {
  it('restores the default chord of a changed action', () => {
    const defaults = defaultsOf('windows');
    const changed = assignChord(untouched('windows'), 'toggle-chat', 'Ctrl+Shift+q').keys;
    const result = resetAction(changed, defaults, 'toggle-chat');
    expect({ ...result.keys }).toEqual({ ...defaults });
    expect(result.movedFrom).toBeNull();
  });

  it('takes the default chord back from the action that took it and reports which', () => {
    const defaults = defaultsOf('windows');
    const stolen = assignChord(untouched('windows'), 'toggle-chat', 'Ctrl+Shift+w').keys;
    const result = resetAction(stolen, defaults, 'close-window');
    expect(result.keys['close-window']).toBe('Ctrl+Shift+w');
    expect(result.keys['toggle-chat']).toBeNull();
    expect(result.movedFrom).toBe('toggle-chat');
    expectOneToOne(result.keys);
  });

  it('gives an unassigned action its default chord back', () => {
    const defaults = defaultsOf('windows');
    const removed = removeAction(untouched('windows'), 'close-window');
    const result = resetAction(removed, defaults, 'close-window');
    expect({ ...result.keys }).toEqual({ ...defaults });
    expect(result.movedFrom).toBeNull();
  });

  it('changes nothing for an action already at its default', () => {
    const defaults = defaultsOf('windows');
    const start = untouched('windows');
    const result = resetAction(start, defaults, 'close-window');
    expect({ ...result.keys }).toEqual({ ...start });
    expect(result.movedFrom).toBeNull();
  });

  it('leaves the other actions as they were', () => {
    const defaults = defaultsOf('mac');
    const changed = assignChord(untouched('mac'), 'toggle-chat', 'Shift+Meta+q').keys;
    const result = resetAction(changed, defaults, 'toggle-plans');
    expect({ ...result.keys }).toEqual({ ...changed });
  });
});

describe('removeAction', () => {
  it('leaves the action without a chord and the others alone', () => {
    const start = untouched('windows');
    const result = removeAction(start, 'toggle-chat');
    expect({ ...result }).toEqual({ ...start, 'toggle-chat': null });
  });

  it('is idempotent', () => {
    const once = removeAction(untouched('windows'), 'toggle-chat');
    expect({ ...removeAction(once, 'toggle-chat') }).toEqual({ ...once });
  });

  it('leaves the keys it was given unchanged', () => {
    const start: Keys = Object.freeze({ ...untouched('windows') });
    removeAction(start, 'toggle-chat');
    expect({ ...start }).toEqual({ ...untouched('windows') });
  });
});

describe('overridesOf', () => {
  it('is empty when nothing differs from the defaults', () => {
    for (const platform of PLATFORMS) {
      expect(overridesOf(defaultsOf(platform), untouched(platform)), platform).toEqual({});
    }
  });

  it('holds only the actions whose chord differs from the default', () => {
    const start = untouched('windows');
    const keys = assignChord(start, 'toggle-chat', 'Ctrl+Shift+q').keys;
    expect(overridesOf(defaultsOf('windows'), keys)).toEqual({ 'toggle-chat': 'Ctrl+Shift+q' });
  });

  it('writes an empty string for an action without a chord', () => {
    const keys = removeAction(untouched('windows'), 'toggle-plans');
    expect(overridesOf(defaultsOf('windows'), keys)).toEqual({ 'toggle-plans': '' });
  });

  it('records both sides of a move', () => {
    const keys = assignChord(untouched('windows'), 'toggle-chat', 'Ctrl+Shift+w').keys;
    expect(overridesOf(defaultsOf('windows'), keys)).toEqual({
      'toggle-chat': 'Ctrl+Shift+w',
      'close-window': '',
    });
  });

  it('drops an action once it is reset', () => {
    const defaults = defaultsOf('windows');
    const changed = assignChord(untouched('windows'), 'toggle-chat', 'Ctrl+Shift+q').keys;
    const reset = resetAction(changed, defaults, 'toggle-chat').keys;
    expect(overridesOf(defaults, reset)).toEqual({});
  });

  it('reads the table it is given, not the defaults of the moment', () => {
    const older: Defaults = { ...defaultsOf('windows'), 'close-window': 'Ctrl+Shift+x' };
    const keys = untouched('windows');
    expect(overridesOf(older, keys)).toEqual({ 'close-window': 'Ctrl+Shift+w' });
  });
});

function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

const EXTRA_CHORDS: readonly string[] = [
  'Ctrl+Shift+q',
  'Ctrl+Shift+x',
  'Ctrl+Shift+y',
  'Ctrl+Shift+u',
  'Ctrl+Shift+j',
  'Ctrl+Shift+1',
  'Ctrl+.',
  'Ctrl+/',
  'Meta+j',
  'Shift+Meta+j',
  'Ctrl+Meta+j',
];

describe('what the Keyboard tab saves brings back the same keys', () => {
  it.each(PLATFORMS)('after any run of edits on %s', (platform) => {
    const defaults = defaultsOf(platform);
    const pool = [...Object.values(defaults), ...EXTRA_CHORDS].filter(
      (chord) => validateBinding(chord, platform) === null
    );
    expect(pool.length).toBeGreaterThanOrEqual(15);

    for (let seed = 1; seed <= 60; seed += 1) {
      const next = seeded(seed * 7919 + platform.length);
      let keys = untouched(platform);
      for (let step = 0; step < 16; step += 1) {
        const action = ACTION_IDS[Math.floor(next() * ACTION_IDS.length)] as ActionId;
        const pick = next();
        if (pick < 0.5) {
          const chord = pool[Math.floor(next() * pool.length)] as string;
          keys = assignChord(keys, action, chord).keys;
        } else if (pick < 0.8) {
          keys = resetAction(keys, defaults, action).keys;
        } else {
          keys = removeAction(keys, action);
        }
        expectOneToOne(keys);
        const back = effectiveKeys(defaults, overridesOf(defaults, keys), platform);
        expect({ ...back.keys }, `seed ${seed} step ${step}`).toEqual({ ...keys });
        expect(back.discarded, `seed ${seed} step ${step}`).toBe(0);
      }
    }
  });

  it('for a fixed run that moves, empties and restores chords', () => {
    const defaults = defaultsOf('windows');
    let keys = untouched('windows');
    keys = assignChord(keys, 'toggle-chat', 'Ctrl+Shift+w').keys;
    keys = assignChord(keys, 'toggle-plans', 'Ctrl+Shift+l').keys;
    keys = removeAction(keys, 'interrupt-window');
    keys = resetAction(keys, defaults, 'close-window').keys;
    const saved = overridesOf(defaults, keys);
    expect(saved).toEqual({
      'toggle-chat': '',
      'toggle-plans': 'Ctrl+Shift+l',
      'interrupt-window': '',
    });
    const back = effectiveKeys(defaults, saved, 'windows');
    expect({ ...back.keys }).toEqual({ ...keys });
    expect(back.discarded).toBe(0);
  });
});

describe('bindingsOf', () => {
  it('maps each chord to its action, one to one', () => {
    const keys = untouched('windows');
    const map: KeyMap = bindingsOf(keys);
    expect(map.size).toBe(ACTION_IDS.length);
    for (const action of ACTION_IDS) {
      expect(map.get(defaultsOf('windows')[action]), action).toBe(action);
    }
  });

  it('leaves out an action without a chord', () => {
    const keys = removeAction(untouched('windows'), 'close-window');
    const map: KeyMap = bindingsOf(keys);
    expect(map.size).toBe(ACTION_IDS.length - 1);
    expect([...map.values()]).not.toContain('close-window');
    expect(map.has('Ctrl+Shift+w')).toBe(false);
  });

  it('follows a move: the chord now belongs to the new action and nothing else', () => {
    const keys = assignChord(untouched('windows'), 'toggle-chat', 'Ctrl+Shift+w').keys;
    const map: KeyMap = bindingsOf(keys);
    expect(map.get('Ctrl+Shift+w')).toBe('toggle-chat');
    expect([...map.values()]).not.toContain('close-window');
    expect(map.has('Ctrl+Shift+l')).toBe(false);
  });

  it('is empty when no action has a chord', () => {
    let keys = untouched('windows');
    for (const action of ACTION_IDS) {
      keys = removeAction(keys, action);
    }
    expect(bindingsOf(keys).size).toBe(0);
  });
});
