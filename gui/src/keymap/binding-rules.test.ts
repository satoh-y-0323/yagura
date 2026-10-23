import { beforeEach, describe, expect, it, vi } from 'vitest';
import { validateBinding, type BindingRejection } from './binding-rules';
import type { KeyEventLike } from './key-chord';
import type { KeyboardPlatform } from './keyboard-platform';
import { isReloadKey } from './reload-keys';

type ReloadChordLookup = (chord: string, platform: KeyboardPlatform) => boolean;

// Lets a test grow or empty the reload table, to show the rule reads it rather than a copy.
const reloadTable = vi.hoisted(() => ({ added: new Set<string>(), emptied: false }));

vi.mock('./reload-keys', async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  const original = actual.isReloadChord as ReloadChordLookup | undefined;
  const isReloadChord: ReloadChordLookup = (chord, platform) => {
    if (original === undefined) throw new Error('reload-keys.ts exports no isReloadChord');
    if (reloadTable.emptied) return false;
    return reloadTable.added.has(`${platform} ${chord}`) || original(chord, platform);
  };
  return { ...actual, isReloadChord };
});

beforeEach(() => {
  reloadTable.added.clear();
  reloadTable.emptied = false;
});

const ALL_PLATFORMS: readonly KeyboardPlatform[] = ['windows', 'mac', 'linux'];
const WINDOWS_AND_LINUX: readonly KeyboardPlatform[] = ['windows', 'linux'];

function expectRejection(
  chord: string,
  platform: KeyboardPlatform,
  expected: BindingRejection | null
): void {
  const rejection: BindingRejection | null = validateBinding(chord, platform);
  expect(rejection, `${chord} on ${platform}`).toBe(expected);
}

describe('a malformed chord', () => {
  it.each(['Shift+Ctrl+a', 'Ctrl+A', 'Ctrl+', 'Hyper+a', 'Ctrl++', ''])(
    'rejects the malformed chord %j',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, 'invalid-chord');
      }
    }
  );

  it('checks the syntax before any other rule', () => {
    // Shift before Alt is out of order; the Alt rule must not answer for a chord that cannot exist.
    expectRejection('Shift+Alt+r', 'windows', 'invalid-chord');
    expectRejection('Alt+Shift+r', 'windows', 'alt-reaches-terminal');
  });
});

describe('a shortcut needs Ctrl or Meta', () => {
  it.each(['r', 'F5', 'Shift+r', 'Shift+F5', 'Shift+Tab'])(
    'rejects %s, which has neither Ctrl nor Meta nor Alt',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, 'needs-modifier');
      }
    }
  );

  it.each(['Alt+r', 'Alt+Shift+r', 'Alt+ArrowLeft', 'Alt+1'])(
    'rejects %s because Alt without Ctrl or Meta reaches the terminal',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, 'alt-reaches-terminal');
      }
    }
  );

  it('accepts Alt once Meta joins it', () => {
    for (const platform of ALL_PLATFORMS) {
      expectRejection('Alt+Meta+r', platform, null);
      expectRejection('Alt+Meta+ArrowLeft', platform, null);
    }
  });
});

describe('Ctrl+Alt with a character key types characters on some layouts', () => {
  it.each(['Ctrl+Alt+a', 'Ctrl+Alt+Shift+a', 'Ctrl+Alt+1', 'Ctrl+Alt+[', 'Ctrl+Alt+Shift+<'])(
    'rejects %s, including digits and symbols',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, 'types-characters');
      }
    }
  );

  it('rejects the + key under its name Plus just like any other character key', () => {
    for (const platform of ALL_PLATFORMS) {
      expectRejection('Ctrl+Alt+Plus', platform, 'types-characters');
    }
  });

  it('accepts Ctrl+Alt with a key that types nothing', () => {
    for (const platform of ALL_PLATFORMS) {
      expectRejection('Ctrl+Alt+Tab', platform, null);
    }
  });

  it('answers before the terminal rule, which would also reject Ctrl+Alt+r', () => {
    expectRejection('Ctrl+Alt+r', 'windows', 'types-characters');
  });
});

describe('keys the host keeps for itself', () => {
  it.each(['Ctrl+Shift+c', 'Ctrl+Shift+v'])(
    'keeps %s for copy and paste on Windows and Linux',
    (chord) => {
      for (const platform of WINDOWS_AND_LINUX) {
        expectRejection(chord, platform, 'reserved-for-clipboard');
      }
    }
  );

  it('leaves the same chord free on macOS, where copy and paste use Cmd', () => {
    expectRejection('Ctrl+Shift+c', 'mac', null);
    expectRejection('Ctrl+Shift+v', 'mac', null);
  });

  it.each<[string, string]>([
    ['Meta+q', 'Meta+n'],
    ['Meta+w', 'Meta+n'],
    ['Meta+h', 'Meta+n'],
    ['Meta+m', 'Meta+n'],
    ['Meta+z', 'Meta+n'],
    ['Meta+x', 'Meta+n'],
    ['Meta+c', 'Meta+n'],
    ['Meta+v', 'Meta+n'],
    ['Meta+a', 'Meta+n'],
    ['Shift+Meta+z', 'Shift+Meta+a'],
    ['Alt+Meta+h', 'Alt+Meta+j'],
    ['Ctrl+Meta+f', 'Ctrl+Meta+g'],
  ])('keeps %s for the macOS app menu while %s stays free', (reserved, free) => {
    expectRejection(reserved, 'mac', 'reserved-by-app-menu');
    expectRejection(free, 'mac', null);
  });

  it('names the app menu, not the terminal, for Cmd+A on macOS', () => {
    // xterm would also select all on Cmd+A; the user needs to hear about the menu first.
    expectRejection('Meta+a', 'mac', 'reserved-by-app-menu');
  });
});

describe('a shortcut must not take input the terminal can tell apart', () => {
  it.each(['Ctrl+r', 'Ctrl+c', 'Ctrl+Space', 'Ctrl+[', 'Ctrl+ArrowLeft', 'Ctrl+Backspace'])(
    'rejects %s, which sends something different from the key without Ctrl',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, 'reaches-terminal');
      }
    }
  );

  it.each(['Ctrl+Tab', 'Ctrl+Shift+Tab', 'Ctrl+Shift+Enter', 'Ctrl+,'])(
    'accepts %s, which the terminal receives the same as without Ctrl or not at all',
    (chord) => {
      for (const platform of ALL_PLATFORMS) {
        expectRejection(chord, platform, null);
      }
    }
  );

  it.each(['Meta+.', 'Meta+n', 'Meta+,', 'Shift+Meta+w', 'Shift+Meta+Enter'])(
    'accepts %s on macOS',
    (chord) => {
      expectRejection(chord, 'mac', null);
    }
  );

  it('accepts Ctrl+Shift with every letter except c, v and r on Windows and Linux', () => {
    for (const platform of WINDOWS_AND_LINUX) {
      for (let code = 'a'.charCodeAt(0); code <= 'z'.charCodeAt(0); code++) {
        const letter = String.fromCharCode(code);
        const expected: BindingRejection | null =
          letter === 'c' || letter === 'v'
            ? 'reserved-for-clipboard'
            : letter === 'r'
              ? 'reserved-for-reload'
              : null;
        expectRejection(`Ctrl+Shift+${letter}`, platform, expected);
      }
    }
  });
});

describe('keys that reload the GUI cannot be bound', () => {
  it.each<[string, KeyboardPlatform, BindingRejection]>([
    ['Ctrl+Shift+r', 'windows', 'reserved-for-reload'],
    ['Ctrl+Shift+r', 'linux', 'reserved-for-reload'],
    ['Meta+r', 'mac', 'reserved-for-reload'],
    ['Shift+Meta+r', 'mac', 'reserved-for-reload'],
  ])('rejects %s on %s, which no earlier rule catches', (chord, platform, expected) => {
    expectRejection(chord, platform, expected);
  });

  it.each<[string, BindingRejection]>([
    ['F5', 'needs-modifier'],
    ['Shift+F5', 'needs-modifier'],
    ['Ctrl+F5', 'reaches-terminal'],
    ['Ctrl+r', 'reaches-terminal'],
  ])('keeps the earlier reason for %s on Windows and Linux', (chord, expected) => {
    for (const platform of WINDOWS_AND_LINUX) {
      expectRejection(chord, platform, expected);
    }
  });

  it('leaves Ctrl+Shift+r free on macOS, where it does not reload', () => {
    expectRejection('Ctrl+Shift+r', 'mac', null);
  });

  const DOMAIN_KEYS: readonly string[] = [
    ...Array.from({ length: 26 }, (_, i) => String.fromCharCode('a'.charCodeAt(0) + i)),
    ...Array.from({ length: 10 }, (_, i) => String(i)),
    ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
    'Tab',
    'Enter',
  ];

  function chordsOfDomain(): string[] {
    const chords: string[] = [];
    for (let mask = 0; mask < 16; mask++) {
      for (const key of DOMAIN_KEYS) {
        const names: string[] = [];
        if ((mask & 8) !== 0) names.push('Ctrl');
        if ((mask & 4) !== 0) names.push('Alt');
        if ((mask & 2) !== 0) names.push('Shift');
        if ((mask & 1) !== 0) names.push('Meta');
        chords.push([...names, key].join('+'));
      }
    }
    return chords;
  }

  /** The key press a chord stands for, shaped as the browser reports it. */
  function pressOf(chord: string): KeyEventLike {
    const names = chord.split('+');
    const key = names.pop() ?? '';
    const shiftKey = names.includes('Shift');
    const letter = /^[a-z]$/.test(key);
    const digit = /^[0-9]$/.test(key);
    return {
      key: letter && shiftKey ? key.toUpperCase() : key,
      code: letter ? `Key${key.toUpperCase()}` : digit ? `Digit${key}` : key,
      ctrlKey: names.includes('Ctrl'),
      altKey: names.includes('Alt'),
      shiftKey,
      metaKey: names.includes('Meta'),
    };
  }

  it.each(ALL_PLATFORMS)(
    'rejects exactly the keys the dispatcher treats as reloads, after every earlier rule, on %s',
    (platform) => {
      const mismatches: string[] = [];
      for (const chord of chordsOfDomain()) {
        reloadTable.emptied = true;
        const earlier: BindingRejection | null = validateBinding(chord, platform);
        reloadTable.emptied = false;
        const expected: BindingRejection | null = isReloadKey(pressOf(chord), platform)
          ? (earlier ?? 'reserved-for-reload')
          : earlier;
        const actual: BindingRejection | null = validateBinding(chord, platform);
        if (actual !== expected) mismatches.push(`${chord}: ${actual} != ${expected}`);
      }
      expect(mismatches).toEqual([]);
    }
  );

  it('rejects a chord the reload table gains, and only on the platform it gains it', () => {
    expectRejection('Ctrl+Shift+j', 'windows', null);
    reloadTable.added.add('windows Ctrl+Shift+j');
    expectRejection('Ctrl+Shift+j', 'windows', 'reserved-for-reload');
    expectRejection('Ctrl+Shift+j', 'linux', null);
  });

  it('answers a chord the table gains with an earlier reason when one applies', () => {
    reloadTable.added.add('windows Ctrl+Shift+c');
    reloadTable.added.add('mac Meta+w');
    expectRejection('Ctrl+Shift+c', 'windows', 'reserved-for-clipboard');
    expectRejection('Meta+w', 'mac', 'reserved-by-app-menu');
  });

  it('accepts a reload chord again once the table no longer holds it', () => {
    reloadTable.emptied = true;
    expectRejection('Ctrl+Shift+r', 'windows', null);
    expectRejection('Meta+r', 'mac', null);
  });
});

describe('Ctrl+Alt with the keys that are named rather than typed', () => {
  it.each(['Plus', 'Space', 'a', '1', ','])('rejects Ctrl+Alt+%s as typing a character', (key) => {
    expectRejection(`Ctrl+Alt+${key}`, 'windows', 'types-characters');
  });

  it.each(['Tab', 'F5', 'ArrowLeft'])(
    'does not reject Ctrl+Alt+%s as typing a character',
    (key) => {
      expect(validateBinding(`Ctrl+Alt+${key}`, 'windows')).not.toBe('types-characters');
    }
  );
});
