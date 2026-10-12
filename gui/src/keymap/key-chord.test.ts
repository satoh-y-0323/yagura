import { describe, expect, it } from 'vitest';
import { chordOfEvent, formatChord, isImeKey, type KeyEventLike } from './key-chord';

interface Modifiers {
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
  meta?: boolean;
}

function press(key: string, code: string, modifiers: Modifiers = {}): KeyEventLike {
  return {
    key,
    code,
    ctrlKey: modifiers.ctrl ?? false,
    altKey: modifiers.alt ?? false,
    shiftKey: modifiers.shift ?? false,
    metaKey: modifiers.meta ?? false,
  };
}

describe('chordOfEvent: letters', () => {
  it('lowercases the letter and keeps Shift as a modifier', () => {
    const chord: string | null = chordOfEvent(press('A', 'KeyA', { ctrl: true, shift: true }));
    expect(chord).toBe('Ctrl+Shift+a');
  });

  it('ignores CapsLock: an uppercase key without Shift is the same chord as the lowercase one', () => {
    const capsLock: string | null = chordOfEvent(press('R', 'KeyR', { ctrl: true }));
    const plain: string | null = chordOfEvent(press('r', 'KeyR', { ctrl: true }));
    expect(capsLock).toBe('Ctrl+r');
    expect(plain).toBe('Ctrl+r');
  });

  it('uses the letter the layout typed even when the physical key differs', () => {
    // AZERTY puts "a" on the key that QWERTY calls KeyQ; the label the user reads wins.
    const chord: string | null = chordOfEvent(press('a', 'KeyQ', { ctrl: true, shift: true }));
    expect(chord).toBe('Ctrl+Shift+a');
  });

  it.each<[string, string, Modifiers, string]>([
    ['ф', 'KeyA', { ctrl: true, shift: true }, 'Ctrl+Shift+a'],
    ['Ф', 'KeyA', { ctrl: true, shift: true }, 'Ctrl+Shift+a'],
    ['å', 'KeyA', { alt: true, meta: true }, 'Alt+Meta+a'],
    ['Process', 'KeyW', { ctrl: true, shift: true }, 'Ctrl+Shift+w'],
    ['Dead', 'KeyE', { alt: true, meta: true }, 'Alt+Meta+e'],
  ])(
    'takes the letter from the physical key when the key %j is not an ASCII letter',
    (key, code, modifiers, expected) => {
      const chord: string | null = chordOfEvent(press(key, code, modifiers));
      expect(chord).toBe(expected);
    }
  );
});

describe('chordOfEvent: keys an input method reports as Process', () => {
  it.each<[string, Modifiers, string]>([
    ['Tab', { ctrl: true }, 'Ctrl+Tab'],
    ['Tab', { ctrl: true, shift: true }, 'Ctrl+Shift+Tab'],
    ['Enter', { ctrl: true, shift: true }, 'Ctrl+Shift+Enter'],
    ['Comma', { ctrl: true }, 'Ctrl+,'],
    ['Period', { meta: true }, 'Meta+.'],
    ['Comma', { meta: true }, 'Meta+,'],
  ])(
    'recovers %s from the physical key so default shortcuts work with the IME on',
    (code, modifiers, expected) => {
      const withImeOn: string | null = chordOfEvent(press('Process', code, modifiers));
      expect(withImeOn).toBe(expected);
    }
  );

  it('produces the same chord with the IME on as with it off', () => {
    const imeOff: string | null = chordOfEvent(press(',', 'Comma', { ctrl: true }));
    const imeOn: string | null = chordOfEvent(press('Process', 'Comma', { ctrl: true }));
    expect(imeOn).toBe(imeOff);
  });
});

describe('chordOfEvent: keys the engine gave no name (Process or Unidentified)', () => {
  it.each<[string, string, Modifiers, string]>([
    // WebKitGTK reports Ctrl+Shift+Tab as key=Unidentified code=Tab (GTK ISO_Left_Tab).
    ['Unidentified', 'Tab', { ctrl: true, shift: true }, 'Ctrl+Shift+Tab'],
    ['Unidentified', 'Tab', { ctrl: true }, 'Ctrl+Tab'],
    ['Unidentified', 'Enter', { ctrl: true }, 'Ctrl+Enter'],
    ['Unidentified', 'Comma', { ctrl: true }, 'Ctrl+,'],
    ['Unidentified', 'Period', { meta: true }, 'Meta+.'],
    ['Process', 'Tab', { ctrl: true, shift: true }, 'Ctrl+Shift+Tab'],
  ])('reads key=%s code=%s through the one table', (key, code, modifiers, expected) => {
    const chord: string | null = chordOfEvent(press(key, code, modifiers));
    expect(chord).toBe(expected);
  });

  it('reads Unidentified and Process alike for every code the table names', () => {
    for (const code of ['Tab', 'Enter', 'Comma', 'Period']) {
      const unidentified: string | null = chordOfEvent(press('Unidentified', code, { ctrl: true }));
      const process: string | null = chordOfEvent(press('Process', code, { ctrl: true }));
      expect(unidentified).toBe(process);
      expect(unidentified).not.toContain('Unidentified');
    }
  });

  it('reads a nameless key whose code the table does not know as it did before (no match)', () => {
    const unidentified: string | null = chordOfEvent(press('Unidentified', 'F13', { ctrl: true }));
    expect(unidentified).toBe('Ctrl+Unidentified');
    const process: string | null = chordOfEvent(press('Process', 'F13', { ctrl: true }));
    expect(process).toBe('Ctrl+Process');
  });
});

/** The fields isImeKey reads, with the values a real keydown carried. */
function imeEvent(
  keyCode: number,
  isComposing: boolean,
  modifiers: Modifiers = {}
): Parameters<typeof isImeKey>[0] {
  return {
    isComposing,
    keyCode,
    ctrlKey: modifiers.ctrl ?? false,
    metaKey: modifiers.meta ?? false,
  };
}

describe('isImeKey: the keys the input method still owns (measured 2026-09-29)', () => {
  it.each<[string, number, boolean, boolean]>([
    // Windows (WebView2, MS-IME)
    ['Process 229 true (Esc cancelling a composition, Enter confirming)', 229, true, true],
    // macOS (WKWebView, Kotoeri)
    ['Escape 229 true (Esc closing the candidates)', 229, true, true],
    ['Escape 229 false (Esc cancelling the composition)', 229, false, true],
    ['Enter 229 false (Enter confirming the composition)', 229, false, true],
    // Not an IME key on either OS
    ['Escape 27 false (Esc with no composition)', 27, false, false],
    ['Enter 13 false (Enter with no composition)', 13, false, false],
  ])('%s', (_label, keyCode, isComposing, expected) => {
    expect(isImeKey(imeEvent(keyCode, isComposing))).toBe(expected);
  });

  it('takes isComposing alone as an IME key whatever the key code', () => {
    expect(isImeKey(imeEvent(27, true))).toBe(true);
    expect(isImeKey(imeEvent(0, true))).toBe(true);
  });

  it('reads keyCode 229 with Ctrl or Meta as the user own key (the input method does not own chords)', () => {
    expect(isImeKey(imeEvent(229, false, { ctrl: true }))).toBe(false);
    expect(isImeKey(imeEvent(229, false, { meta: true }))).toBe(false);
  });

  it('does not treat keyCode 228 or 230 as the input method', () => {
    expect(isImeKey(imeEvent(228, false))).toBe(false);
    expect(isImeKey(imeEvent(230, false))).toBe(false);
  });
});

describe('chordOfEvent: other keys', () => {
  it('names the space bar Space', () => {
    const chord: string | null = chordOfEvent(press(' ', 'Space', { ctrl: true }));
    expect(chord).toBe('Ctrl+Space');
  });

  it('keeps the typed symbol and Shift, since the unshifted symbol needs a layout table', () => {
    const chord: string | null = chordOfEvent(press('<', 'Comma', { ctrl: true, shift: true }));
    expect(chord).toBe('Ctrl+Shift+<');
  });

  it('keeps an unshifted symbol and a digit as typed', () => {
    const comma: string | null = chordOfEvent(press(',', 'Comma', { ctrl: true }));
    const digit: string | null = chordOfEvent(press('1', 'Digit1', { ctrl: true, alt: true }));
    expect(comma).toBe('Ctrl+,');
    expect(digit).toBe('Ctrl+Alt+1');
  });

  it.each<[string, Modifiers, string]>([
    ['Equal', { ctrl: true, shift: true }, 'Ctrl+Shift+Plus'],
    ['NumpadAdd', { ctrl: true }, 'Ctrl+Plus'],
  ])(
    'names the + key Plus so it cannot be confused with the separator (%s)',
    (code, modifiers, expected) => {
      const chord: string | null = chordOfEvent(press('+', code, modifiers));
      expect(chord).toBe(expected);
    }
  );

  it('keeps named keys as they are', () => {
    const tab: string | null = chordOfEvent(press('Tab', 'Tab', { ctrl: true }));
    const arrow: string | null = chordOfEvent(press('ArrowLeft', 'ArrowLeft', { alt: true }));
    const f5: string | null = chordOfEvent(press('F5', 'F5'));
    expect(tab).toBe('Ctrl+Tab');
    expect(arrow).toBe('Alt+ArrowLeft');
    expect(f5).toBe('F5');
  });

  it('returns a chord without modifiers so the caller can say a modifier is missing', () => {
    const chord: string | null = chordOfEvent(press('r', 'KeyR'));
    expect(chord).toBe('r');
  });

  it('orders modifiers Ctrl, Alt, Shift, Meta', () => {
    const chord: string | null = chordOfEvent(
      press('x', 'KeyX', { ctrl: true, alt: true, shift: true, meta: true })
    );
    expect(chord).toBe('Ctrl+Alt+Shift+Meta+x');
  });

  it.each<[string, string, Modifiers]>([
    ['Control', 'ControlLeft', { ctrl: true }],
    ['Shift', 'ShiftLeft', { shift: true }],
    ['Alt', 'AltLeft', { alt: true }],
    ['Meta', 'MetaLeft', { meta: true }],
    ['AltGraph', 'AltRight', { ctrl: true, alt: true }],
  ])('returns null while only the modifier %s is down', (key, code, modifiers) => {
    const chord: string | null = chordOfEvent(press(key, code, modifiers));
    expect(chord).toBeNull();
  });
});

describe('formatChord', () => {
  it.each<[string, string[]]>([
    ['Ctrl+Shift+a', ['Ctrl', 'Shift', 'A']],
    ['Ctrl+Meta+f', ['Ctrl', 'Win', 'F']],
    ['Ctrl+Alt+Shift+Meta+k', ['Ctrl', 'Alt', 'Shift', 'Win', 'K']],
    ['Ctrl+,', ['Ctrl', ',']],
    ['Ctrl+Plus', ['Ctrl', 'Plus']],
    ['Ctrl+Shift+Tab', ['Ctrl', 'Shift', 'Tab']],
  ])('shows %s on Windows as %j', (chord, expected) => {
    const parts: string[] = formatChord(chord, 'windows');
    expect(parts).toEqual(expected);
  });

  it('shows Meta as Win on Linux too', () => {
    const parts: string[] = formatChord('Ctrl+Meta+f', 'linux');
    expect(parts).toEqual(['Ctrl', 'Win', 'F']);
  });

  it.each<[string, string[]]>([
    ['Shift+Meta+a', ['Cmd', 'Shift', 'A']],
    ['Meta+,', ['Cmd', ',']],
    ['Ctrl+Meta+f', ['Cmd', 'Ctrl', 'F']],
    ['Alt+Meta+h', ['Cmd', 'Option', 'H']],
    ['Ctrl+Alt+Shift+Meta+k', ['Cmd', 'Ctrl', 'Option', 'Shift', 'K']],
    ['Ctrl+Tab', ['Ctrl', 'Tab']],
  ])('shows %s on macOS as %j', (chord, expected) => {
    const parts: string[] = formatChord(chord, 'mac');
    expect(parts).toEqual(expected);
  });
});

describe('chordOfEvent: keys that name no chord, and the physical key of a letter', () => {
  it.each(['Control', 'Shift', 'Alt', 'Meta', 'AltGraph', 'OS'])(
    'gives no chord while only the %s key is down',
    (key) => {
      expect(chordOfEvent(press(key, 'Left', { ctrl: true }))).toBeNull();
    }
  );

  it('falls back to the physical key when the layout typed something that is not an ASCII letter', () => {
    expect(chordOfEvent(press('é', 'KeyE', { ctrl: true }))).toBe('Ctrl+e');
  });

  it('reads only a code that is exactly Key plus one capital letter as a physical letter', () => {
    expect(chordOfEvent(press('é', 'KeyEx', { ctrl: true }))).toBe('Ctrl+é');
    expect(chordOfEvent(press('é', 'NumpadKeyE', { ctrl: true }))).toBe('Ctrl+é');
    expect(chordOfEvent(press('é', 'Key', { ctrl: true }))).toBe('Ctrl+é');
  });
});

describe('formatChord: keys with a long or empty name', () => {
  it('upper-cases a single letter key and leaves a named key as it is', () => {
    expect(formatChord('Ctrl+Shift+a', 'windows')).toEqual(['Ctrl', 'Shift', 'A']);
    expect(formatChord('Ctrl+ArrowLeft', 'windows')).toEqual(['Ctrl', 'ArrowLeft']);
  });

  it('shows a chord that is only a key as that key', () => {
    expect(formatChord('F5', 'windows')).toEqual(['F5']);
    expect(formatChord('F5', 'mac')).toEqual(['F5']);
  });
});
