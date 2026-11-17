/**
 * Keyboard movement inside an open menu. Wrap-around at the ends and skipping
 * disabled entries follow the Modern Web Guidance `menu` guide ("Within a
 * menu": ArrowUp/ArrowDown move between enabled items, Home/End go to the
 * first/last enabled item, Escape closes and restores focus to the trigger,
 * Tab leaves the menu system).
 */
import { describe, expect, it } from 'vitest';

interface MenuEntry {
  role: string | null;
  disabled: boolean;
}

type MenuKeyResult = { type: 'focus'; index: number } | { type: 'close'; restoreFocus: boolean };
type HandleMenuKey = (key: string, entries: MenuEntry[], current: number) => MenuKeyResult | null;

async function loadHandleMenuKey(): Promise<HandleMenuKey> {
  const module = (await import('./menu-navigation')) as Record<string, unknown>;
  return module['handleMenuKey'] as HandleMenuKey;
}

const item = (disabled = false): MenuEntry => ({ role: 'menuitem', disabled });
const separator: MenuEntry = { role: 'separator', disabled: false };
// The room menu's explanatory sentence: present in the menu, never a menuitem.
const note: MenuEntry = { role: null, disabled: false };

// 0 item, 1 separator, 2 disabled item, 3 note, 4 item, 5 item
const MIDDLE_OBSTACLES: MenuEntry[] = [item(), separator, item(true), note, item(), item()];
// 0 note, 1 item, 2 separator, 3 item, 4 disabled item
const OBSTACLES_AT_BOTH_ENDS: MenuEntry[] = [note, item(), separator, item(), item(true)];

describe('menu keyboard: arrow keys move between enabled menu items only', () => {
  it.each([
    ['ArrowDown', 0, 4],
    ['ArrowDown', 4, 5],
    ['ArrowUp', 5, 4],
    ['ArrowUp', 4, 0],
    // -1: focus is on the menu itself, before any item has been focused.
    ['ArrowDown', -1, 0],
    ['ArrowUp', -1, 5],
  ])(
    '%s from entry %i lands on entry %i, skipping separators, disabled items and the note',
    async (key, from, to) => {
      const handle = await loadHandleMenuKey();

      expect(handle(key, MIDDLE_OBSTACLES, from)).toEqual({ type: 'focus', index: to });
    }
  );

  it('wraps from the last enabled item to the first, and from the first to the last', async () => {
    const handle = await loadHandleMenuKey();

    expect(handle('ArrowDown', MIDDLE_OBSTACLES, 5)).toEqual({ type: 'focus', index: 0 });
    expect(handle('ArrowUp', MIDDLE_OBSTACLES, 0)).toEqual({ type: 'focus', index: 5 });
  });
});

describe('menu keyboard: Home and End go to the first and last enabled item', () => {
  it.each([
    ['Home', 3, 1],
    ['End', 1, 3],
  ])(
    '%s from entry %i lands on entry %i even when the ends are not items',
    async (key, from, to) => {
      const handle = await loadHandleMenuKey();

      expect(handle(key, OBSTACLES_AT_BOTH_ENDS, from)).toEqual({ type: 'focus', index: to });
    }
  );

  it('lands on the very first and last entries when those are items', async () => {
    const handle = await loadHandleMenuKey();

    expect(handle('Home', MIDDLE_OBSTACLES, 4)).toEqual({ type: 'focus', index: 0 });
    expect(handle('End', MIDDLE_OBSTACLES, 0)).toEqual({ type: 'focus', index: 5 });
  });
});

describe('menu keyboard: leaving the menu', () => {
  it('Escape closes the menu and returns focus to the button that opened it', async () => {
    const handle = await loadHandleMenuKey();

    expect(handle('Escape', MIDDLE_OBSTACLES, 4)).toEqual({ type: 'close', restoreFocus: true });
  });

  it('Tab closes the menu and lets focus move on instead of pulling it back to the button', async () => {
    const handle = await loadHandleMenuKey();

    expect(handle('Tab', MIDDLE_OBSTACLES, 4)).toEqual({ type: 'close', restoreFocus: false });
  });
});
