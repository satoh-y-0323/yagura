import { describe, it, expect } from 'vitest';
import type { KeyDownFacts } from './key-input';
import { keyDownEffect, pasteInput, textInput, wheelInput } from './window-input';

const NO_MODS = { shift: false, alt: false, ctrl: false };

function keydown(overrides: Partial<KeyDownFacts>): KeyDownFacts {
  return {
    type: 'keydown',
    key: '',
    code: '',
    keyCode: 0,
    shiftKey: false,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    isComposing: false,
    getModifierState: () => false,
    ...overrides,
  };
}

describe('the text a window receives', () => {
  it('is the text typed or committed', () => {
    expect(textInput('é')).toEqual({ type: 'text', text: 'é' });
  });

  it('is nothing when none was typed', () => {
    expect(textInput(null)).toBeNull();
  });
});

describe('a paste', () => {
  it('sends the text pasted', () => {
    expect(pasteInput('a\nb')).toEqual({ type: 'paste', text: 'a\nb' });
  });

  it('sends nothing for an empty paste', () => {
    expect(pasteInput('')).toBeNull();
  });

  it('sends a paste that is only blank', () => {
    expect(pasteInput(' ')).toEqual({ type: 'paste', text: ' ' });
  });
});

describe('the wheel', () => {
  const cell = { col: 4, row: 2 };

  it('scrolls one line up or down at the cell under the pointer', () => {
    expect(wheelInput({ deltaY: -120 }, () => cell, NO_MODS)).toEqual({
      type: 'wheel',
      delta: -1,
      cell,
      mods: NO_MODS,
    });
    expect(wheelInput({ deltaY: 3 }, () => cell, { ...NO_MODS, ctrl: true })).toEqual({
      type: 'wheel',
      delta: 1,
      cell,
      mods: { ...NO_MODS, ctrl: true },
    });
  });

  it('sends nothing for a turn with no direction, and does not look the cell up', () => {
    let lookups = 0;
    const input = wheelInput(
      { deltaY: 0 },
      () => {
        lookups += 1;
        return cell;
      },
      NO_MODS
    );
    expect(input).toBeNull();
    expect(lookups).toBe(0);
  });
});

describe('a keydown', () => {
  it('sends a key to the window and keeps the browser from handling it', () => {
    expect(keyDownEffect(keydown({ key: 'Enter', keyCode: 13 }), 'linux')).toEqual({
      preventsDefault: true,
      clipboard: null,
      input: { type: 'key', key: { named: 'enter' }, mods: NO_MODS },
    });
  });

  it('asks for the clipboard action of a chord and sends the window nothing', () => {
    const chord = keydown({ key: 'C', code: 'KeyC', keyCode: 67, ctrlKey: true, shiftKey: true });
    expect(keyDownEffect(chord, 'linux')).toEqual({
      preventsDefault: true,
      clipboard: 'copy',
      input: null,
    });
  });

  it('leaves a key the browser types to the browser', () => {
    expect(keyDownEffect(keydown({ key: 'a', code: 'KeyA', keyCode: 65 }), 'linux')).toEqual({
      preventsDefault: false,
      clipboard: null,
      input: null,
    });
  });
});
