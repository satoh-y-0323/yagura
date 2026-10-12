/**
 * When Escape closes a side panel: only an unmodified, finished, unhandled
 * Escape, so a CLI in a window and the chat mention list keep their own Escape.
 */
import { describe, it, expect } from 'vitest';

import {
  bodyEscapePanel,
  closeOnEscape,
  isPanelCloseKey,
  type BodyEscapeContext,
  type PanelKeyLike,
} from './panel-focus';

/** What a keydown carries for the Escape rule: `keyCode` is how the input method's keys are told apart. */
type KeyFields = PanelKeyLike & { keyCode: number };

const ESCAPE: KeyFields = {
  key: 'Escape',
  keyCode: 27,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  isComposing: false,
  defaultPrevented: false,
};

describe('isPanelCloseKey', () => {
  it('accepts a plain Escape', () => {
    expect(isPanelCloseKey(ESCAPE)).toBe(true);
  });

  it.each<[string, Partial<KeyFields>]>([
    ['Ctrl+Escape', { ctrlKey: true }],
    ['Alt+Escape', { altKey: true }],
    ['Shift+Escape', { shiftKey: true }],
    ['Meta+Escape', { metaKey: true }],
    ['an Escape that ends an IME composition', { isComposing: true }],
    // macOS: the Escape that cancels a composition has isComposing already false.
    ['an Escape the IME still owns (keyCode 229, isComposing false)', { keyCode: 229 }],
    [
      'an Escape closing the IME candidates (keyCode 229, isComposing true)',
      { keyCode: 229, isComposing: true },
    ],
    ['an Escape the mention list already handled', { defaultPrevented: true }],
    ['Enter', { key: 'Enter' }],
    ['a letter', { key: 'a' }],
    ['a lower-case escape name', { key: 'escape' }],
  ])('rejects %s', (_label: string, override: Partial<KeyFields>) => {
    expect(isPanelCloseKey({ ...ESCAPE, ...override })).toBe(false);
  });
});

/** Records which listeners the action registers, and with which options. */
class RecordingTarget extends EventTarget {
  readonly added: Array<{ type: string; capture: boolean }> = [];

  override addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject | null,
    options?: boolean | AddEventListenerOptions
  ): void {
    const capture = typeof options === 'boolean' ? options : (options?.capture ?? false);
    this.added.push({ type, capture });
    super.addEventListener(type, listener, options);
  }
}

/** A keydown carrying the fields the action reads; `defaultPrevented` comes from a real preventDefault. */
function keydown(fields: Partial<KeyFields>): Event {
  const event = new Event('keydown', { cancelable: true });
  for (const [name, value] of Object.entries({ ...ESCAPE, ...fields })) {
    if (name === 'defaultPrevented') {
      continue;
    }
    Object.defineProperty(event, name, { value });
  }
  return event;
}

describe('closeOnEscape', () => {
  it('calls onClose once for an Escape and never for other keys', () => {
    const target = new EventTarget();
    let closed = 0;
    closeOnEscape(target, () => {
      closed += 1;
    });

    target.dispatchEvent(keydown({ key: 'Enter' }));
    target.dispatchEvent(keydown({ key: 'a' }));
    expect(closed).toBe(0);

    target.dispatchEvent(keydown({ key: 'Escape' }));
    expect(closed).toBe(1);
  });

  it('ignores Shift+Escape, an Escape during composition and one already handled', () => {
    const target = new EventTarget();
    let closed = 0;
    const handled = (event: Event): void => {
      event.preventDefault();
    };
    closeOnEscape(target, () => {
      closed += 1;
    });

    target.dispatchEvent(keydown({ shiftKey: true }));
    target.dispatchEvent(keydown({ isComposing: true }));
    target.dispatchEvent(keydown({ keyCode: 229 }));
    expect(closed).toBe(0);

    const mention = new EventTarget();
    let mentionClosed = 0;
    mention.addEventListener('keydown', handled);
    closeOnEscape(mention, () => {
      mentionClosed += 1;
    });
    mention.dispatchEvent(keydown({}));
    expect(mentionClosed).toBe(0);

    target.dispatchEvent(keydown({}));
    expect(closed).toBe(1);
  });

  it('listens for keydown in the bubble phase, so a handler nearer the key runs first', () => {
    const target = new RecordingTarget();
    closeOnEscape(target, () => {});

    expect(target.added.some((entry) => entry.type === 'keydown')).toBe(true);
    expect(target.added.every((entry) => entry.type !== 'keydown' || !entry.capture)).toBe(true);
  });

  it('calls the function given to update instead of the first one', () => {
    const target = new EventTarget();
    const calls: string[] = [];
    const action = closeOnEscape(target, () => {
      calls.push('first');
    });

    action.update(() => {
      calls.push('second');
    });
    target.dispatchEvent(keydown({}));

    expect(calls).toEqual(['second']);
  });

  it('stops calling onClose after destroy', () => {
    const target = new EventTarget();
    let closed = 0;
    const action = closeOnEscape(target, () => {
      closed += 1;
    });

    target.dispatchEvent(keydown({}));
    expect(closed).toBe(1);

    action.destroy();
    target.dispatchEvent(keydown({}));
    expect(closed).toBe(1);
  });
});

describe('bodyEscapePanel: which panel an Escape closes while focus is on the body', () => {
  type Panel = 'chat' | 'plans' | 'settings';

  /** Focus fell to the body after last being in the Settings panel, which is still open. */
  const ON_BODY: BodyEscapeContext<Panel> = {
    focusOnBody: true,
    isModalOpen: false,
    lastFocusedPanel: 'settings',
    isOpen: (panel: Panel) => panel === 'settings' || panel === 'chat',
  };

  it('closes the panel that last held focus when every condition holds', () => {
    expect(bodyEscapePanel(ESCAPE, ON_BODY)).toBe('settings');
    expect(bodyEscapePanel(ESCAPE, { ...ON_BODY, lastFocusedPanel: 'chat' })).toBe('chat');
  });

  it.each<[string, Partial<KeyFields>]>([
    ['Ctrl+Escape', { ctrlKey: true }],
    ['Alt+Escape', { altKey: true }],
    ['Shift+Escape', { shiftKey: true }],
    ['Meta+Escape', { metaKey: true }],
    ['an Escape that ends an IME composition', { isComposing: true }],
    ['an Escape the IME still owns (keyCode 229, isComposing false)', { keyCode: 229 }],
    ['an Escape a handler already claimed', { defaultPrevented: true }],
    ['a key other than Escape', { key: 'Enter' }],
  ])('closes nothing for %s', (_label: string, override: Partial<KeyFields>) => {
    expect(bodyEscapePanel({ ...ESCAPE, ...override }, ON_BODY)).toBeNull();
  });

  it.each<[string, Partial<BodyEscapeContext<Panel>>]>([
    ['focus is not on the body (a window, the sidebar or a panel has it)', { focusOnBody: false }],
    ['a modal dialog is open', { isModalOpen: true }],
    ['no panel has held focus', { lastFocusedPanel: null }],
    ['the panel that last held focus has closed', { isOpen: (panel: Panel) => panel === 'chat' }],
  ])('closes nothing when %s', (_label: string, override: Partial<BodyEscapeContext<Panel>>) => {
    expect(bodyEscapePanel(ESCAPE, { ...ON_BODY, ...override })).toBeNull();
  });
});
