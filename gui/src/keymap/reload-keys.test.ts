/**
 * The keys that would reload the GUI: which presses they are, and how the
 * window-level dispatcher cancels them without keeping them from the terminal.
 */
import { describe, expect, it } from 'vitest';

import type { ActionId, KeyMap } from './key-binding';
import type { KeyEventLike } from './key-chord';
import type { KeyboardPlatform } from './keyboard-platform';
import { isReloadKey } from './reload-keys';
import { createShortcutDispatcher, type KeydownLike } from './shortcut-dispatcher';

interface PressInit {
  key: string;
  code: string;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  metaKey?: boolean;
}

function press(init: PressInit): KeyEventLike {
  return {
    key: init.key,
    code: init.code,
    ctrlKey: init.ctrlKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: init.shiftKey ?? false,
    metaKey: init.metaKey ?? false,
  };
}

const F5: PressInit = { key: 'F5', code: 'F5' };
const SHIFT_F5: PressInit = { ...F5, shiftKey: true };
const CTRL_F5: PressInit = { ...F5, ctrlKey: true };
const CTRL_R: PressInit = { key: 'r', code: 'KeyR', ctrlKey: true };
const CTRL_SHIFT_R: PressInit = { key: 'R', code: 'KeyR', ctrlKey: true, shiftKey: true };
const CMD_R: PressInit = { key: 'r', code: 'KeyR', metaKey: true };
const CMD_SHIFT_R: PressInit = { key: 'R', code: 'KeyR', metaKey: true, shiftKey: true };

const NON_MAC: KeyboardPlatform[] = ['windows', 'linux'];

describe('isReloadKey on Windows and Linux', () => {
  it.each(NON_MAC)('treats F5, Shift+F5, Ctrl+F5, Ctrl+R and Ctrl+Shift+R as reload on %s', (p) => {
    for (const init of [F5, SHIFT_F5, CTRL_F5, CTRL_R, CTRL_SHIFT_R]) {
      expect(isReloadKey(press(init), p), JSON.stringify(init)).toBe(true);
    }
  });

  it.each(NON_MAC)(
    'finds Ctrl+R however the layout or input method reports the letter on %s',
    (p) => {
      const capsLock = press({ key: 'R', code: 'KeyR', ctrlKey: true });
      const inputMethod = press({ key: 'Process', code: 'KeyR', ctrlKey: true });
      const cyrillic = press({ key: 'к', code: 'KeyR', ctrlKey: true });
      expect(isReloadKey(capsLock, p)).toBe(true);
      expect(isReloadKey(inputMethod, p)).toBe(true);
      expect(isReloadKey(cyrillic, p)).toBe(true);
    }
  );

  it.each<[string, PressInit]>([
    ['a plain r', { key: 'r', code: 'KeyR' }],
    ['Shift+R', { key: 'R', code: 'KeyR', shiftKey: true }],
    ['Alt+R', { key: 'r', code: 'KeyR', altKey: true }],
    ['Ctrl+Alt+R', { ...CTRL_R, altKey: true }],
    ['Ctrl+Alt+Shift+R', { ...CTRL_SHIFT_R, altKey: true }],
    ['Meta+R (the OS Run dialog, not the page)', CMD_R],
    ['F6', { key: 'F6', code: 'F6' }],
    ['Alt+F5', { ...F5, altKey: true }],
    ['Ctrl+T', { key: 't', code: 'KeyT', ctrlKey: true }],
    ['Control alone', { key: 'Control', code: 'ControlLeft', ctrlKey: true }],
  ])('does not treat %s as reload', (_name, init) => {
    for (const p of NON_MAC) {
      expect(isReloadKey(press(init), p), p).toBe(false);
    }
  });
});

describe('isReloadKey on macOS', () => {
  it('treats Cmd+R and Cmd+Shift+R as reload', () => {
    expect(isReloadKey(press(CMD_R), 'mac')).toBe(true);
    expect(isReloadKey(press(CMD_SHIFT_R), 'mac')).toBe(true);
  });

  it.each<[string, PressInit]>([
    ['Ctrl+R (reaches the CLI as history search)', CTRL_R],
    ['Ctrl+Shift+R', CTRL_SHIFT_R],
    ['Cmd+Option+R', { ...CMD_R, altKey: true }],
    ['a plain r', { key: 'r', code: 'KeyR' }],
    ['Cmd+T', { key: 't', code: 'KeyT', metaKey: true }],
  ])('does not treat %s as reload', (_name, init) => {
    expect(isReloadKey(press(init), 'mac')).toBe(false);
  });
});

/** A keydown whose preventDefault / stopPropagation calls are counted. */
interface CountedKeydown {
  prevented: () => number;
  stopped: () => number;
}

interface KeydownInit extends PressInit {
  isComposing?: boolean;
  keyCode?: number;
  repeat?: boolean;
}

interface Harness {
  handle: (init: KeydownInit) => CountedKeydown;
  runs: ActionId[];
  dispatcher: ReturnType<typeof createShortcutDispatcher>;
  setModal: (open: boolean) => void;
}

function harness(platform: KeyboardPlatform, bindings: KeyMap = new Map()): Harness {
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
      let prevented = 0;
      let stopped = 0;
      const event: KeydownLike = {
        ...press(init),
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
      dispatcher.handleKeydown(event);
      return { prevented: () => prevented, stopped: () => stopped };
    },
    runs,
    dispatcher,
    setModal: (open: boolean): void => {
      modal = open;
    },
  };
}

// Only the browser's default is cancelled; the key still travels to xterm,
// which ignores defaultPrevented, so Ctrl+R stays the shell's history search.
function expectCancelledButDelivered(counted: CountedKeydown, label: string): void {
  expect(counted.prevented(), label).toBe(1);
  expect(counted.stopped(), label).toBe(0);
}

function expectUntouched(counted: CountedKeydown, label: string): void {
  expect(counted.prevented(), label).toBe(0);
  expect(counted.stopped(), label).toBe(0);
}

describe('the dispatcher and a reload key', () => {
  it.each(NON_MAC)('cancels the default of every reload key on %s without stopping it', (p) => {
    const h = harness(p);
    for (const init of [F5, SHIFT_F5, CTRL_F5, CTRL_R, CTRL_SHIFT_R]) {
      expectCancelledButDelivered(h.handle(init), JSON.stringify(init));
    }
    expect(h.runs).toEqual([]);
  });

  it('cancels Cmd+R and Cmd+Shift+R on macOS without stopping them', () => {
    const h = harness('mac');
    expectCancelledButDelivered(h.handle(CMD_R), 'Cmd+R');
    expectCancelledButDelivered(h.handle(CMD_SHIFT_R), 'Cmd+Shift+R');
  });

  it('leaves keys that only look like reload untouched', () => {
    const windows = harness('windows');
    expectUntouched(windows.handle({ ...CTRL_R, altKey: true }), 'Ctrl+Alt+R');
    expectUntouched(windows.handle({ key: 'r', code: 'KeyR', altKey: true }), 'Alt+R');
    expectUntouched(windows.handle({ key: 'F6', code: 'F6' }), 'F6');
    expectUntouched(windows.handle(CMD_R), 'Meta+R on Windows');

    const mac = harness('mac');
    expectUntouched(mac.handle(CTRL_R), 'Ctrl+R on macOS');
  });

  it('judges the reload key before the bindings, so even a bound reload chord only cancels', () => {
    const bound: KeyMap = new Map<string, ActionId>([
      ['Ctrl+r', 'close-window'],
      ['F5', 'toggle-settings'],
    ]);
    const h = harness('windows', bound);
    expectCancelledButDelivered(h.handle(CTRL_R), 'Ctrl+R');
    expectCancelledButDelivered(h.handle(F5), 'F5');
    expect(h.runs).toEqual([]);
  });

  it('cancels a reload key while the input method is composing', () => {
    const h = harness('windows');
    expectCancelledButDelivered(
      h.handle({ ...F5, isComposing: true, keyCode: 229 }),
      'F5 while composing'
    );
    expectCancelledButDelivered(
      h.handle({ key: 'Process', code: 'KeyR', ctrlKey: true, isComposing: true, keyCode: 229 }),
      'Ctrl+R while composing'
    );
  });

  it('cancels a reload key while a modal dialog is open', () => {
    const h = harness('windows');
    h.setModal(true);
    expectCancelledButDelivered(h.handle(F5), 'F5 under a modal');
    expectCancelledButDelivered(h.handle(CTRL_R), 'Ctrl+R under a modal');
    expect(h.runs).toEqual([]);
  });

  it('cancels a held-down reload key on every repeat', () => {
    const h = harness('windows');
    expectCancelledButDelivered(h.handle({ ...F5, repeat: true }), 'F5 repeat');
    expectCancelledButDelivered(h.handle({ ...CTRL_R, repeat: true }), 'Ctrl+R repeat');
  });

  it('cancels a reload key while recording, even when composing or when a dialog opens', () => {
    const plain = harness('windows');
    plain.dispatcher.beginRecording(() => undefined);
    expect(plain.handle(F5).prevented()).toBeGreaterThanOrEqual(1);

    const composing = harness('windows');
    composing.dispatcher.beginRecording(() => undefined);
    expect(
      composing.handle({ ...F5, isComposing: true, keyCode: 229 }).prevented()
    ).toBeGreaterThanOrEqual(1);

    const dialog = harness('windows');
    dialog.dispatcher.beginRecording(() => undefined);
    dialog.setModal(true);
    expect(dialog.handle(CTRL_R).prevented()).toBeGreaterThanOrEqual(1);
  });
});
