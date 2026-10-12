import { describe, it, expect } from 'vitest';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';
import { decideKeyDown, textOfInputEvent, type KeyDownFacts } from './key-input';

interface Press {
  key: string;
  code?: string;
  keyCode: number;
  shift?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  meta?: boolean;
  altGraph?: boolean;
  isComposing?: boolean;
}

function keydown(press: Press): KeyDownFacts {
  const altGraph = press.altGraph ?? false;
  return {
    type: 'keydown',
    key: press.key,
    code: press.code ?? '',
    keyCode: press.keyCode,
    shiftKey: press.shift ?? false,
    ctrlKey: press.ctrl ?? false,
    altKey: press.alt ?? false,
    metaKey: press.meta ?? false,
    isComposing: press.isComposing ?? false,
    getModifierState: (name: string) => name === 'AltGraph' && altGraph,
  };
}

const NO_MODS = { shift: false, alt: false, ctrl: false };

function named(name: string, mods: Partial<typeof NO_MODS> = {}): unknown {
  return { kind: 'key', input: { key: { named: name }, mods: { ...NO_MODS, ...mods } } };
}

function char(value: string, mods: Partial<typeof NO_MODS> = {}): unknown {
  return { kind: 'key', input: { key: { char: value }, mods: { ...NO_MODS, ...mods } } };
}

const BROWSER = { kind: 'browser' };

const ALL_PLATFORMS: KeyboardPlatform[] = ['windows', 'mac', 'linux'];

describe('keys that print nothing become key messages', () => {
  const cases: [Press, string][] = [
    [{ key: 'Enter', keyCode: 13 }, 'enter'],
    [{ key: 'Tab', keyCode: 9 }, 'tab'],
    [{ key: 'Backspace', keyCode: 8 }, 'backspace'],
    [{ key: 'Escape', keyCode: 27 }, 'escape'],
    [{ key: 'ArrowUp', keyCode: 38 }, 'arrow_up'],
    [{ key: 'ArrowDown', keyCode: 40 }, 'arrow_down'],
    [{ key: 'ArrowLeft', keyCode: 37 }, 'arrow_left'],
    [{ key: 'ArrowRight', keyCode: 39 }, 'arrow_right'],
    [{ key: 'Home', keyCode: 36 }, 'home'],
    [{ key: 'End', keyCode: 35 }, 'end'],
    [{ key: 'PageUp', keyCode: 33 }, 'page_up'],
    [{ key: 'PageDown', keyCode: 34 }, 'page_down'],
    [{ key: 'Insert', keyCode: 45 }, 'insert'],
    [{ key: 'Delete', keyCode: 46 }, 'delete'],
    ...Array.from({ length: 12 }, (_, i): [Press, string] => [
      { key: `F${i + 1}`, keyCode: 112 + i },
      `f${i + 1}`,
    ]),
  ];

  it('sends each named key with no modifiers on every platform', () => {
    for (const platform of ALL_PLATFORMS) {
      for (const [press, name] of cases) {
        expect(decideKeyDown(keydown(press), platform), `${platform} ${press.key}`).toEqual(
          named(name)
        );
      }
    }
  });

  it('carries Shift, Alt and Ctrl held with a named key', () => {
    expect(decideKeyDown(keydown({ key: 'ArrowUp', keyCode: 38, ctrl: true }), 'linux')).toEqual(
      named('arrow_up', { ctrl: true })
    );
    expect(decideKeyDown(keydown({ key: 'Tab', keyCode: 9, shift: true }), 'windows')).toEqual(
      named('tab', { shift: true })
    );
    expect(
      decideKeyDown(keydown({ key: 'F3', keyCode: 114, shift: true, alt: true }), 'linux')
    ).toEqual(named('f3', { shift: true, alt: true }));
  });

  it('sends Shift+Enter as Enter with Shift, which the daemon writes as ESC CR', () => {
    for (const platform of ALL_PLATFORMS) {
      expect(decideKeyDown(keydown({ key: 'Enter', keyCode: 13, shift: true }), platform)).toEqual(
        named('enter', { shift: true })
      );
    }
  });

  it('sends Shift+PageUp as a key, which the daemon turns into a view page', () => {
    expect(decideKeyDown(keydown({ key: 'PageUp', keyCode: 33, shift: true }), 'windows')).toEqual(
      named('page_up', { shift: true })
    );
  });

  it('names the key from the physical key when the engine left it unidentified', () => {
    // WebKitGTK reports Shift+Tab with an unidentified key.
    expect(
      decideKeyDown(keydown({ key: 'Unidentified', code: 'Tab', keyCode: 9, shift: true }), 'linux')
    ).toEqual(named('tab', { shift: true }));
  });

  it('names the key from the physical key when the engine reports it as processed', () => {
    expect(decideKeyDown(keydown({ key: 'Process', code: 'Tab', keyCode: 9 }), 'linux')).toEqual(
      named('tab')
    );
  });

  it('leaves a function key the wire does not carry to the browser', () => {
    expect(decideKeyDown(keydown({ key: 'F13', keyCode: 124 }), 'linux')).toEqual(BROWSER);
  });
});

describe('characters', () => {
  it('leaves a character typed alone or with Shift to the input event', () => {
    for (const platform of ALL_PLATFORMS) {
      expect(decideKeyDown(keydown({ key: 'a', code: 'KeyA', keyCode: 65 }), platform)).toEqual(
        BROWSER
      );
      expect(
        decideKeyDown(keydown({ key: 'A', code: 'KeyA', keyCode: 65, shift: true }), platform)
      ).toEqual(BROWSER);
      expect(decideKeyDown(keydown({ key: ' ', code: 'Space', keyCode: 32 }), platform)).toEqual(
        BROWSER
      );
    }
  });

  it('sends Ctrl with a letter as the letter with Ctrl', () => {
    expect(
      decideKeyDown(keydown({ key: 'c', code: 'KeyC', keyCode: 67, ctrl: true }), 'windows')
    ).toEqual(char('c', { ctrl: true }));
    expect(
      decideKeyDown(keydown({ key: 'c', code: 'KeyC', keyCode: 67, ctrl: true }), 'mac')
    ).toEqual(char('c', { ctrl: true }));
  });

  it('reads the letter of Ctrl and Alt chords from the key code, as xterm.js does', () => {
    // A Cyrillic layout types `с` on the C key; the chord is still Ctrl+C.
    expect(
      decideKeyDown(keydown({ key: 'с', code: 'KeyC', keyCode: 67, ctrl: true }), 'linux')
    ).toEqual(char('c', { ctrl: true }));
    expect(
      decideKeyDown(keydown({ key: 'ф', code: 'KeyA', keyCode: 65, alt: true }), 'linux')
    ).toEqual(char('a', { alt: true }));
    expect(
      decideKeyDown(
        keydown({ key: 'Ф', code: 'KeyA', keyCode: 65, alt: true, shift: true }),
        'linux'
      )
    ).toEqual(char('A', { alt: true, shift: true }));
  });

  it('reads the letter at both ends of the alphabet from the key code', () => {
    expect(
      decideKeyDown(keydown({ key: 'я', code: 'KeyZ', keyCode: 90, ctrl: true }), 'linux')
    ).toEqual(char('z', { ctrl: true }));
    expect(
      decideKeyDown(keydown({ key: '!', code: 'Digit1', keyCode: 49, ctrl: true }), 'linux')
    ).toEqual(char('!', { ctrl: true }));
  });

  it('leaves a chord whose key is not a single character to the browser', () => {
    expect(decideKeyDown(keydown({ key: 'Dead', keyCode: 222, ctrl: true }), 'linux')).toEqual(
      BROWSER
    );
    expect(decideKeyDown(keydown({ key: 'F13', keyCode: 124, alt: true }), 'linux')).toEqual(
      BROWSER
    );
  });

  it('counts a character outside the BMP as one character', () => {
    expect(decideKeyDown(keydown({ key: '\u{1f600}', keyCode: 0, alt: true }), 'linux')).toEqual(
      char('\u{1f600}', { alt: true })
    );
  });

  it('sends Ctrl with a symbol as the symbol the layout typed', () => {
    expect(
      decideKeyDown(keydown({ key: '[', code: 'BracketLeft', keyCode: 219, ctrl: true }), 'linux')
    ).toEqual(char('[', { ctrl: true }));
    expect(
      decideKeyDown(keydown({ key: ' ', code: 'Space', keyCode: 32, ctrl: true }), 'linux')
    ).toEqual(char(' ', { ctrl: true }));
    expect(
      decideKeyDown(
        keydown({ key: '_', code: 'Minus', keyCode: 189, ctrl: true, shift: true }),
        'linux'
      )
    ).toEqual(char('_', { ctrl: true, shift: true }));
  });

  it('sends Alt with a character on Windows and Linux', () => {
    for (const platform of ['windows', 'linux'] as const) {
      expect(
        decideKeyDown(keydown({ key: 'b', code: 'KeyB', keyCode: 66, alt: true }), platform)
      ).toEqual(char('b', { alt: true }));
      expect(
        decideKeyDown(keydown({ key: '1', code: 'Digit1', keyCode: 49, alt: true }), platform)
      ).toEqual(char('1', { alt: true }));
    }
  });

  it('sends Ctrl+Alt with a letter on Linux', () => {
    expect(
      decideKeyDown(
        keydown({ key: 'a', code: 'KeyA', keyCode: 65, ctrl: true, alt: true }),
        'linux'
      )
    ).toEqual(char('a', { ctrl: true, alt: true }));
  });
});

describe('the third level shift is left to the browser', () => {
  it('leaves Windows AltGr to the browser', () => {
    expect(
      decideKeyDown(
        keydown({ key: '@', code: 'KeyQ', keyCode: 81, ctrl: true, alt: true, altGraph: true }),
        'windows'
      )
    ).toEqual(BROWSER);
    expect(
      decideKeyDown(
        keydown({ key: '{', code: 'Digit7', keyCode: 55, ctrl: true, alt: true, altGraph: true }),
        'windows'
      )
    ).toEqual(BROWSER);
  });

  it('treats AltGr as the third level only on Windows', () => {
    const altGr = { key: '@', code: 'KeyQ', keyCode: 81, ctrl: true, altGraph: true };
    expect(decideKeyDown(keydown(altGr), 'windows')).toEqual(BROWSER);
    expect(decideKeyDown(keydown(altGr), 'linux')).toEqual(char('q', { ctrl: true }));
  });

  it('draws the third level line at key code 47 on Windows', () => {
    const chord = { ctrl: true, alt: true };
    expect(decideKeyDown(keydown({ key: '/', keyCode: 47, ...chord }), 'windows')).toEqual(
      char('/', chord)
    );
    expect(decideKeyDown(keydown({ key: '0', keyCode: 48, ...chord }), 'windows')).toEqual(BROWSER);
    expect(decideKeyDown(keydown({ key: 'x', keyCode: 0, ...chord }), 'windows')).toEqual(BROWSER);
  });

  it('leaves Windows Ctrl+Alt with a printing key to the browser, as AltGr', () => {
    expect(
      decideKeyDown(
        keydown({ key: 'a', code: 'KeyA', keyCode: 65, ctrl: true, alt: true }),
        'windows'
      )
    ).toEqual(BROWSER);
  });

  it('still sends Windows Ctrl+Alt with a key below the printing range', () => {
    expect(
      decideKeyDown(keydown({ key: 'ArrowUp', keyCode: 38, ctrl: true, alt: true }), 'windows')
    ).toEqual(named('arrow_up', { ctrl: true, alt: true }));
  });

  it('leaves macOS Option alone with a printing key to the browser', () => {
    expect(
      decideKeyDown(keydown({ key: 'å', code: 'KeyA', keyCode: 65, alt: true }), 'mac')
    ).toEqual(BROWSER);
    expect(
      decideKeyDown(keydown({ key: 'Dead', code: 'KeyE', keyCode: 69, alt: true }), 'mac')
    ).toEqual(BROWSER);
  });

  it('still sends macOS Option with a named key below the printing range', () => {
    expect(decideKeyDown(keydown({ key: 'ArrowLeft', keyCode: 37, alt: true }), 'mac')).toEqual(
      named('arrow_left', { alt: true })
    );
  });

  it('sends macOS Ctrl+Option with a letter, which is not the third level', () => {
    expect(
      decideKeyDown(keydown({ key: 'a', code: 'KeyA', keyCode: 65, ctrl: true, alt: true }), 'mac')
    ).toEqual(char('a', { ctrl: true, alt: true }));
  });
});

describe('what never becomes a key message', () => {
  it('leaves every Meta chord to the browser', () => {
    for (const platform of ALL_PLATFORMS) {
      expect(
        decideKeyDown(keydown({ key: 'k', code: 'KeyK', keyCode: 75, meta: true }), platform)
      ).toEqual(BROWSER);
      expect(decideKeyDown(keydown({ key: 'ArrowUp', keyCode: 38, meta: true }), platform)).toEqual(
        BROWSER
      );
      expect(
        decideKeyDown(
          keydown({ key: 'k', code: 'KeyK', keyCode: 75, meta: true, ctrl: true }),
          platform
        )
      ).toEqual(BROWSER);
    }
  });

  it('leaves a key the input method owns to the input method', () => {
    for (const platform of ALL_PLATFORMS) {
      expect(
        decideKeyDown(keydown({ key: 'Enter', keyCode: 13, isComposing: true }), platform)
      ).toEqual(BROWSER);
      expect(decideKeyDown(keydown({ key: 'Process', keyCode: 229 }), platform)).toEqual(BROWSER);
      expect(decideKeyDown(keydown({ key: 'Escape', keyCode: 229 }), platform)).toEqual(BROWSER);
      expect(
        decideKeyDown(keydown({ key: 'ArrowLeft', keyCode: 37, isComposing: true }), platform)
      ).toEqual(BROWSER);
    }
  });

  it('leaves a modifier pressed alone to the browser', () => {
    for (const key of ['Shift', 'Control', 'Alt', 'Meta', 'AltGraph']) {
      expect(decideKeyDown(keydown({ key, keyCode: 16 }), 'linux'), key).toEqual(BROWSER);
    }
  });

  it('leaves a dead key to the browser', () => {
    expect(decideKeyDown(keydown({ key: 'Dead', code: 'Quote', keyCode: 222 }), 'linux')).toEqual(
      BROWSER
    );
  });

  it('ignores an event that is not a keydown', () => {
    const up = { ...keydown({ key: 'Enter', keyCode: 13 }), type: 'keyup' };
    expect(decideKeyDown(up, 'linux')).toEqual(BROWSER);
  });
});

describe('the clipboard chords and Windows Ctrl+V', () => {
  it('leaves Windows Ctrl+V to the browser so its own paste event fires', () => {
    expect(
      decideKeyDown(keydown({ key: 'v', code: 'KeyV', keyCode: 86, ctrl: true }), 'windows')
    ).toEqual(BROWSER);
  });

  it('sends Ctrl+V on Linux as a key, the literal-next convention', () => {
    expect(
      decideKeyDown(keydown({ key: 'v', code: 'KeyV', keyCode: 86, ctrl: true }), 'linux')
    ).toEqual(char('v', { ctrl: true }));
  });

  it('turns Ctrl+Shift+C and Ctrl+Shift+V into copy and paste on Windows and Linux', () => {
    for (const platform of ['windows', 'linux'] as const) {
      expect(
        decideKeyDown(
          keydown({ key: 'C', code: 'KeyC', keyCode: 67, ctrl: true, shift: true }),
          platform
        )
      ).toEqual({ kind: 'clipboard', action: 'copy' });
      expect(
        decideKeyDown(
          keydown({ key: 'V', code: 'KeyV', keyCode: 86, ctrl: true, shift: true }),
          platform
        )
      ).toEqual({ kind: 'clipboard', action: 'paste' });
    }
  });

  it('leaves macOS Cmd+C and Cmd+V to the menu copy and paste events', () => {
    expect(
      decideKeyDown(keydown({ key: 'c', code: 'KeyC', keyCode: 67, meta: true }), 'mac')
    ).toEqual(BROWSER);
    expect(
      decideKeyDown(keydown({ key: 'v', code: 'KeyV', keyCode: 86, meta: true }), 'mac')
    ).toEqual(BROWSER);
  });

  it('does not treat a clipboard chord under the input method as one', () => {
    expect(
      decideKeyDown(
        keydown({
          key: 'V',
          code: 'KeyV',
          keyCode: 86,
          ctrl: true,
          shift: true,
          isComposing: true,
        }),
        'linux'
      )
    ).toEqual(BROWSER);
  });
});

describe('text from the input event', () => {
  it('sends text the browser inserted', () => {
    expect(textOfInputEvent({ inputType: 'insertText', data: 'a', isComposing: false })).toBe('a');
    expect(textOfInputEvent({ inputType: 'insertText', data: 'å', isComposing: false })).toBe('å');
  });

  it('sends nothing for text still being composed', () => {
    expect(
      textOfInputEvent({ inputType: 'insertCompositionText', data: 'か', isComposing: true })
    ).toBeNull();
    expect(textOfInputEvent({ inputType: 'insertText', data: 'か', isComposing: true })).toBeNull();
  });

  it('sends nothing for an input that carries no text or is not typing', () => {
    expect(
      textOfInputEvent({ inputType: 'insertText', data: null, isComposing: false })
    ).toBeNull();
    expect(textOfInputEvent({ inputType: 'insertText', data: '', isComposing: false })).toBeNull();
    expect(
      textOfInputEvent({ inputType: 'deleteContentBackward', data: null, isComposing: false })
    ).toBeNull();
    expect(
      textOfInputEvent({ inputType: 'insertFromPaste', data: 'x', isComposing: false })
    ).toBeNull();
  });
});
