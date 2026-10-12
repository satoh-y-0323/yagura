/**
 * The single keydown entry point for shortcuts: what it stops, what it lets
 * through to the terminal and the inputs, and how it records a new chord.
 */
import { describe, expect, it } from 'vitest';

import { defaultKeysFor } from './default-keymap';
import type { ActionId, KeyMap } from './key-binding';
import type { KeyboardPlatform } from './keyboard-platform';
import { bindingsOf, effectiveKeys } from './keymap-model';
import { createShortcutDispatcher, type KeydownLike } from './shortcut-dispatcher';

/** A keydown whose preventDefault / stopPropagation calls are counted. */
interface CountedKeydown {
  event: KeydownLike;
  prevented: () => number;
  stopped: () => number;
}

interface KeydownInit {
  key: string;
  code: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
  isComposing?: boolean;
  keyCode?: number;
  repeat?: boolean;
}

function keydown(init: KeydownInit): CountedKeydown {
  let prevented = 0;
  let stopped = 0;
  const event: KeydownLike = {
    key: init.key,
    code: init.code,
    ctrlKey: init.ctrlKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: init.shiftKey ?? false,
    metaKey: init.metaKey ?? false,
    isComposing: init.isComposing ?? false,
    keyCode: init.keyCode ?? 0,
    repeat: init.repeat ?? false,
    preventDefault: (): void => {
      prevented += 1;
    },
    stopPropagation: (): void => {
      stopped += 1;
    },
  };
  return { event, prevented: () => prevented, stopped: () => stopped };
}

const WINDOWS_BINDINGS: KeyMap = bindingsOf(
  effectiveKeys(defaultKeysFor('windows'), {}, 'windows').keys
);
const MAC_BINDINGS: KeyMap = bindingsOf(effectiveKeys(defaultKeysFor('mac'), {}, 'mac').keys);

const CTRL_SHIFT_W: KeydownInit = {
  key: 'W',
  code: 'KeyW',
  ctrlKey: true,
  shiftKey: true,
  keyCode: 87,
};

interface Harness {
  handle: (init: KeydownInit) => CountedKeydown;
  runs: ActionId[];
  dispatcher: ReturnType<typeof createShortcutDispatcher>;
  setBindings: (next: KeyMap) => void;
  setModal: (open: boolean) => void;
}

function harness(
  initial: KeyMap = WINDOWS_BINDINGS,
  platform: KeyboardPlatform = 'windows'
): Harness {
  let bindings = initial;
  let modal = false;
  const runs: ActionId[] = [];
  const dispatcher = createShortcutDispatcher({
    bindings: (): KeyMap => bindings,
    isModalOpen: (): boolean => modal,
    run: (action: ActionId): void => {
      runs.push(action);
    },
    platform,
  });
  return {
    handle: (init: KeydownInit): CountedKeydown => {
      const counted = keydown(init);
      dispatcher.handleKeydown(counted.event);
      return counted;
    },
    runs,
    dispatcher,
    setBindings: (next: KeyMap): void => {
      bindings = next;
    },
    setModal: (open: boolean): void => {
      modal = open;
    },
  };
}

function expectUntouched(counted: CountedKeydown): void {
  expect(counted.prevented()).toBe(0);
  expect(counted.stopped()).toBe(0);
}

describe('a matching keydown', () => {
  it('is stopped once in both ways and runs its action once', () => {
    const h = harness();
    const counted = h.handle(CTRL_SHIFT_W);
    expect(counted.prevented()).toBe(1);
    expect(counted.stopped()).toBe(1);
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });

  it('runs the action the binding names, not a fixed one', () => {
    const h = harness();
    h.handle({ key: 'Tab', code: 'Tab', ctrlKey: true, keyCode: 9 });
    h.handle({ key: 'Tab', code: 'Tab', ctrlKey: true, shiftKey: true, keyCode: 9 });
    h.handle({ key: ',', code: 'Comma', ctrlKey: true, keyCode: 188 });
    const expected: ActionId[] = ['focus-next-window', 'focus-previous-window', 'toggle-settings'];
    expect(h.runs).toEqual(expected);
  });

  it('matches the letter whether or not the layout reports it upper-cased', () => {
    const h = harness();
    h.handle({ ...CTRL_SHIFT_W, key: 'w' });
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });
});

describe('a keydown that matches nothing', () => {
  it.each<[string, KeydownInit]>([
    ['a plain letter', { key: 'a', code: 'KeyA', keyCode: 65 }],
    ['Escape inside a window', { key: 'Escape', code: 'Escape', keyCode: 27 }],
    [
      'Shift+Enter (converted for the terminal, not a shortcut)',
      { key: 'Enter', code: 'Enter', shiftKey: true, keyCode: 13 },
    ],
    [
      'Ctrl+V (pasted by the terminal, not a shortcut)',
      { key: 'v', code: 'KeyV', ctrlKey: true, keyCode: 86 },
    ],
    ['a bound chord with an extra modifier', { ...CTRL_SHIFT_W, altKey: true }],
    ['a modifier key alone', { key: 'Control', code: 'ControlLeft', ctrlKey: true, keyCode: 17 }],
  ])('flows on untouched: %s', (_name, init) => {
    const h = harness();
    expectUntouched(h.handle(init));
    expect(h.runs).toEqual([]);
  });

  it('lets Ctrl+R reach the CLI as history search, with only the page reload cancelled', () => {
    const h = harness();
    const counted = h.handle({ key: 'r', code: 'KeyR', ctrlKey: true });
    expect(counted.prevented()).toBe(1);
    expect(counted.stopped()).toBe(0);
    expect(h.runs).toEqual([]);
  });
});

describe('input method composition', () => {
  it('leaves a keydown that is part of a composition alone, even a bound chord with Ctrl', () => {
    const h = harness();
    expectUntouched(h.handle({ ...CTRL_SHIFT_W, isComposing: true }));
    expect(h.runs).toEqual([]);

    const control = h.handle(CTRL_SHIFT_W);
    expect(control.prevented()).toBe(1);
  });

  it('treats keyCode 229 without Ctrl or Meta as composing, even when the key would match', () => {
    const shiftTab: KeyMap = new Map<string, ActionId>([['Shift+Tab', 'focus-previous-window']]);
    const h = harness(shiftTab);
    expectUntouched(h.handle({ key: 'Process', code: 'Tab', shiftKey: true, keyCode: 229 }));
    expect(h.runs).toEqual([]);

    const control = h.handle({ key: 'Tab', code: 'Tab', shiftKey: true, keyCode: 9 });
    expect(control.prevented()).toBe(1);
    const expected: ActionId[] = ['focus-previous-window'];
    expect(h.runs).toEqual(expected);
  });

  it('matches keyCode 229 with Ctrl by the physical key, so the default Ctrl+Shift+A works with the input method on', () => {
    const h = harness();
    const counted = h.handle({
      key: 'Process',
      code: 'KeyA',
      ctrlKey: true,
      shiftKey: true,
      keyCode: 229,
    });
    expect(counted.prevented()).toBe(1);
    expect(counted.stopped()).toBe(1);
    const expected: ActionId[] = ['focus-next-attention'];
    expect(h.runs).toEqual(expected);
  });

  it('matches keyCode 229 with Meta by the physical key', () => {
    const h = harness(MAC_BINDINGS, 'mac');
    const counted = h.handle({
      key: 'Process',
      code: 'KeyA',
      shiftKey: true,
      metaKey: true,
      keyCode: 229,
    });
    expect(counted.prevented()).toBe(1);
    const expected: ActionId[] = ['focus-next-attention'];
    expect(h.runs).toEqual(expected);
  });
});

describe('keys the input method owns (measured on Windows and macOS, 2026-09-29)', () => {
  const ESCAPE_AND_ENTER: KeyMap = new Map<string, ActionId>([
    ['Escape', 'close-window'],
    ['Enter', 'toggle-settings'],
  ]);

  it.each<[string, KeydownInit]>([
    ['Process 229 true', { key: 'Process', code: 'Escape', keyCode: 229, isComposing: true }],
    ['Escape 229 true', { key: 'Escape', code: 'Escape', keyCode: 229, isComposing: true }],
    ['Escape 229 false', { key: 'Escape', code: 'Escape', keyCode: 229 }],
    ['Enter 229 false', { key: 'Enter', code: 'Enter', keyCode: 229 }],
  ])('leaves %s alone even when the chord is bound', (_label, init) => {
    const h = harness(ESCAPE_AND_ENTER);
    expectUntouched(h.handle(init));
    expect(h.runs).toEqual([]);
  });

  it('still runs the same bound keys when the user presses them (Escape 27 false, Enter 13 false)', () => {
    const h = harness(ESCAPE_AND_ENTER);
    h.handle({ key: 'Escape', code: 'Escape', keyCode: 27 });
    h.handle({ key: 'Enter', code: 'Enter', keyCode: 13 });
    const expected: ActionId[] = ['close-window', 'toggle-settings'];
    expect(h.runs).toEqual(expected);
  });

  it('cancels a reload key even when it is an IME key, and still lets it reach the CLI', () => {
    const h = harness();
    const composing = h.handle({ key: 'r', code: 'KeyR', ctrlKey: true, isComposing: true });
    expect(composing.prevented()).toBe(1);
    expect(composing.stopped()).toBe(0);
    const f5 = h.handle({ key: 'F5', code: 'F5', keyCode: 229, isComposing: true });
    expect(f5.prevented()).toBe(1);
    expect(f5.stopped()).toBe(0);
    const mac = harness(MAC_BINDINGS, 'mac');
    const macReload = mac.handle({ key: 'r', code: 'KeyR', metaKey: true, isComposing: true });
    expect(macReload.prevented()).toBe(1);
    expect(macReload.stopped()).toBe(0);
  });

  it('does not record an IME key, and records the next key the user presses', () => {
    const h = harness();
    const results: Array<string | null> = [];
    h.dispatcher.beginRecording((chord) => {
      results.push(chord);
    });
    expectUntouched(h.handle({ key: 'Escape', code: 'Escape', keyCode: 229 }));
    expectUntouched(h.handle({ key: 'Enter', code: 'Enter', keyCode: 229 }));
    expect(results).toEqual([]);
    expect(h.dispatcher.isRecording()).toBe(true);

    h.handle({ key: 'Escape', code: 'Escape', keyCode: 27 });
    expect(results).toEqual([null]);
  });
});

describe('a key the engine gave no name (Linux WebKitGTK)', () => {
  it('runs focus-previous-window for Ctrl+Shift+Tab reported as Unidentified with code Tab', () => {
    const h = harness(WINDOWS_BINDINGS, 'linux');
    const counted = h.handle({
      key: 'Unidentified',
      code: 'Tab',
      ctrlKey: true,
      shiftKey: true,
      keyCode: 9,
    });
    expect(counted.prevented()).toBe(1);
    expect(counted.stopped()).toBe(1);
    const expected: ActionId[] = ['focus-previous-window'];
    expect(h.runs).toEqual(expected);
  });

  it('records Ctrl+Shift+Tab reported as Unidentified', () => {
    const h = harness(WINDOWS_BINDINGS, 'linux');
    const results: Array<string | null> = [];
    h.dispatcher.beginRecording((chord) => {
      results.push(chord);
    });
    h.handle({ key: 'Unidentified', code: 'Tab', ctrlKey: true, shiftKey: true, keyCode: 9 });
    expect(results).toEqual(['Ctrl+Shift+Tab']);
  });
});

describe('a key held down', () => {
  it('stops the repeated keydown of a bound chord without running the action again', () => {
    const h = harness();
    const first = h.handle(CTRL_SHIFT_W);
    const repeated = h.handle({ ...CTRL_SHIFT_W, repeat: true });
    expect(first.prevented()).toBe(1);
    expect(repeated.prevented()).toBe(1);
    expect(repeated.stopped()).toBe(1);
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });

  it('leaves the repeated keydown of an unbound key alone', () => {
    const h = harness();
    expectUntouched(h.handle({ key: 'a', code: 'KeyA', keyCode: 65, repeat: true }));
    expect(h.runs).toEqual([]);
  });
});

describe('while a modal dialog is open', () => {
  it('leaves even a bound chord to the dialog, and asks again on every keydown', () => {
    const h = harness();
    h.setModal(true);
    expectUntouched(h.handle(CTRL_SHIFT_W));
    expect(h.runs).toEqual([]);

    h.setModal(false);
    const afterClose = h.handle(CTRL_SHIFT_W);
    expect(afterClose.prevented()).toBe(1);
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });
});

describe('bindings take effect immediately', () => {
  it('reads the bindings on every keydown: the new chord works and the old one no longer does', () => {
    const h = harness();
    h.handle(CTRL_SHIFT_W);

    h.setBindings(new Map<string, ActionId>([['Ctrl+Shift+j', 'close-window']]));
    expectUntouched(h.handle(CTRL_SHIFT_W));
    const moved = h.handle({ key: 'J', code: 'KeyJ', ctrlKey: true, shiftKey: true, keyCode: 74 });
    expect(moved.prevented()).toBe(1);

    const expected: ActionId[] = ['close-window', 'close-window'];
    expect(h.runs).toEqual(expected);
  });
});

describe('recording a chord', () => {
  function recorder(h: Harness): (string | null)[] {
    const results: (string | null)[] = [];
    h.dispatcher.beginRecording((chord: string | null): void => {
      results.push(chord);
    });
    return results;
  }

  it('reports whether it is recording', () => {
    const h = harness();
    expect(h.dispatcher.isRecording()).toBe(false);
    recorder(h);
    expect(h.dispatcher.isRecording()).toBe(true);
  });

  it('stops every keydown, keeps waiting through modifier keys, and reports the first full chord once', () => {
    const h = harness();
    const results = recorder(h);

    const modifier = h.handle({ key: 'Control', code: 'ControlLeft', ctrlKey: true, keyCode: 17 });
    const shift = h.handle({
      key: 'Shift',
      code: 'ShiftLeft',
      ctrlKey: true,
      shiftKey: true,
      keyCode: 16,
    });
    expect(modifier.prevented()).toBe(1);
    expect(modifier.stopped()).toBe(1);
    expect(shift.prevented()).toBe(1);
    expect(results).toEqual([]);
    expect(h.dispatcher.isRecording()).toBe(true);

    const chord = h.handle({ key: 'J', code: 'KeyJ', ctrlKey: true, shiftKey: true, keyCode: 74 });
    expect(chord.prevented()).toBe(1);
    expect(chord.stopped()).toBe(1);
    expect(results).toEqual(['Ctrl+Shift+j']);
    expect(h.dispatcher.isRecording()).toBe(false);
    expect(h.runs).toEqual([]);
  });

  it('records a chord that is already bound instead of running its action', () => {
    const h = harness();
    const results = recorder(h);
    const counted = h.handle(CTRL_SHIFT_W);
    expect(counted.prevented()).toBe(1);
    expect(results).toEqual(['Ctrl+Shift+w']);
    expect(h.runs).toEqual([]);
  });

  it('stops keys that would otherwise reach the terminal, such as a plain letter', () => {
    const h = harness();
    const results = recorder(h);
    const counted = h.handle({ key: 'a', code: 'KeyA', keyCode: 65 });
    expect(counted.prevented()).toBe(1);
    expect(counted.stopped()).toBe(1);
    expect(results).toEqual(['a']);
  });

  it('cancels on a plain Escape, but records Escape with a modifier', () => {
    const h = harness();
    const cancelled = recorder(h);
    const escape = h.handle({ key: 'Escape', code: 'Escape', keyCode: 27 });
    expect(escape.prevented()).toBe(1);
    expect(cancelled).toEqual([null]);
    expect(h.dispatcher.isRecording()).toBe(false);

    const recorded = recorder(h);
    h.handle({ key: 'Escape', code: 'Escape', shiftKey: true, keyCode: 27 });
    expect(recorded).toEqual(['Shift+Escape']);
  });

  it('does not record a key that is part of a composition', () => {
    const h = harness();
    const results = recorder(h);
    h.handle({ key: 'a', code: 'KeyA', keyCode: 229 });
    h.handle({ ...CTRL_SHIFT_W, isComposing: true });
    expect(results).toEqual([]);
    expect(h.dispatcher.isRecording()).toBe(true);

    h.handle({ key: 'J', code: 'KeyJ', ctrlKey: true, shiftKey: true, keyCode: 74 });
    expect(results).toEqual(['Ctrl+Shift+j']);
  });

  it('goes back to running shortcuts once the chord is recorded', () => {
    const h = harness();
    const results = recorder(h);
    h.handle({ key: 'J', code: 'KeyJ', ctrlKey: true, shiftKey: true, keyCode: 74 });
    h.handle(CTRL_SHIFT_W);
    expect(results).toEqual(['Ctrl+Shift+j']);
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });

  it('ends on cancelRecording without reporting, and shortcuts run again', () => {
    const h = harness();
    const results = recorder(h);
    h.dispatcher.cancelRecording();
    expect(h.dispatcher.isRecording()).toBe(false);

    const counted = h.handle(CTRL_SHIFT_W);
    expect(counted.prevented()).toBe(1);
    expect(results).toEqual([]);
    const expected: ActionId[] = ['close-window'];
    expect(h.runs).toEqual(expected);
  });
});

describe('recording while a dialog opens', () => {
  it('ends the recording without a chord, once, when a key comes with a modal dialog open', () => {
    const h = harness();
    const results: (string | null)[] = [];
    h.dispatcher.beginRecording((chord) => {
      results.push(chord);
    });
    h.setModal(true);

    h.handle({ key: 'k', code: 'KeyK', ctrlKey: true, keyCode: 75 });

    expect(results).toEqual([null]);
    expect(h.dispatcher.isRecording()).toBe(false);
    expect(h.runs).toEqual([]);
  });

  it('leaves the next key to the shortcuts once the dialog ended the recording', () => {
    const h = harness();
    h.dispatcher.beginRecording(() => undefined);
    h.setModal(true);
    h.handle({ key: 'k', code: 'KeyK', ctrlKey: true, keyCode: 75 });
    h.setModal(false);

    h.handle(CTRL_SHIFT_W);

    expect(h.runs).toHaveLength(1);
  });

  it('stops a bare modifier key while recording and keeps waiting', () => {
    const h = harness();
    const results: (string | null)[] = [];
    h.dispatcher.beginRecording((chord) => {
      results.push(chord);
    });

    h.handle({ key: 'Shift', code: 'ShiftLeft', shiftKey: true, keyCode: 16 });

    expect(results).toEqual([]);
    expect(h.dispatcher.isRecording()).toBe(true);
  });
});
