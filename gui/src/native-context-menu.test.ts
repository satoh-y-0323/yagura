import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { keepsNativeContextMenu, suppressNativeContextMenu } from './native-context-menu';

describe('keepsNativeContextMenu', () => {
  it('keeps the menu on a field the user can type into', () => {
    expect(keepsNativeContextMenu({ tagName: 'INPUT', type: 'text' })).toBe(true);
    expect(keepsNativeContextMenu({ tagName: 'INPUT' })).toBe(true);
    expect(keepsNativeContextMenu({ tagName: 'input', type: 'search' })).toBe(true);
    expect(keepsNativeContextMenu({ tagName: 'TEXTAREA' })).toBe(true);
    expect(keepsNativeContextMenu({ tagName: 'DIV', isContentEditable: true })).toBe(true);
  });

  it('stops it on anything else', () => {
    expect(keepsNativeContextMenu(null)).toBe(false);
    expect(keepsNativeContextMenu({ tagName: 'DIV' })).toBe(false);
    expect(keepsNativeContextMenu({ tagName: 'BUTTON' })).toBe(false);
    expect(keepsNativeContextMenu({ tagName: 'INPUT', type: 'checkbox' })).toBe(false);
  });

  it('stops it on a field that cannot be typed into right now', () => {
    expect(keepsNativeContextMenu({ tagName: 'INPUT', type: 'text', readOnly: true })).toBe(false);
    expect(keepsNativeContextMenu({ tagName: 'TEXTAREA', disabled: true })).toBe(false);
  });
});

describe('keepsNativeContextMenu: every text input type', () => {
  it.each(['text', 'search', 'url', 'tel', 'email', 'password', 'number', 'SEARCH'])(
    'keeps the menu on an input of type %s',
    (type) => {
      expect(keepsNativeContextMenu({ tagName: 'INPUT', type })).toBe(true);
    }
  );

  it.each(['checkbox', 'radio', 'range', 'button', 'color', 'file', 'date'])(
    'stops it on an input of type %s',
    (type) => {
      expect(keepsNativeContextMenu({ tagName: 'INPUT', type })).toBe(false);
    }
  );

  it('stops it on a read-only editable region', () => {
    expect(keepsNativeContextMenu({ tagName: 'DIV', isContentEditable: false })).toBe(false);
  });
});

describe('suppressNativeContextMenu', () => {
  class FakeElement {
    constructor(
      readonly tagName: string,
      readonly type?: string
    ) {}
  }

  beforeEach(() => {
    vi.stubGlobal('HTMLElement', FakeElement);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** A right-click on `target`, and how many times its default was prevented. */
  function rightClick(target: unknown, defaultPrevented = false) {
    let prevented = 0;
    const event = {
      target,
      defaultPrevented,
      preventDefault: () => {
        prevented += 1;
      },
    } as unknown as MouseEvent;
    return { event, prevented: () => prevented };
  }

  it('stops the menu over something that is not a field', () => {
    const click = rightClick(new FakeElement('DIV'));

    suppressNativeContextMenu(click.event);

    expect(click.prevented()).toBe(1);
  });

  it('stops the menu when the target is not an element at all', () => {
    const click = rightClick({ nodeName: '#document' });

    suppressNativeContextMenu(click.event);

    expect(click.prevented()).toBe(1);
  });

  it('keeps the menu over a field the user can type into', () => {
    const click = rightClick(new FakeElement('INPUT', 'text'));

    suppressNativeContextMenu(click.event);

    expect(click.prevented()).toBe(0);
  });

  it('leaves a right-click alone that a handler already took', () => {
    const click = rightClick(new FakeElement('DIV'), true);

    suppressNativeContextMenu(click.event);

    expect(click.prevented()).toBe(0);
  });
});
