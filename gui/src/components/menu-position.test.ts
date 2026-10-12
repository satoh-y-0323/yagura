/**
 * `placeMenu` computes the window menu's `position: fixed` coordinates from
 * viewport-relative numbers only, with no DOM reads: below-left of the anchor
 * by default, wrapping at the viewport's right and bottom edges, and clamped
 * into the viewport when no fully-on-screen placement exists.
 */
import { describe, it, expect } from 'vitest';

import { placeMenu, type Rect, type Size } from './menu-position';

const VIEWPORT: Size = { width: 1000, height: 800 };
const MENU: Size = { width: 150, height: 200 };

describe('placeMenu: default placement below-left of the anchor, when it fits', () => {
  it('aligns left to the anchor and places top just below the anchor', () => {
    const anchor: Rect = { left: 20, top: 20, width: 100, height: 30 };
    expect(placeMenu(anchor, MENU, VIEWPORT)).toEqual({ left: 20, top: 50 });
  });
});

describe('placeMenu: wraps at the right edge of the viewport', () => {
  it("aligns the menu's right edge to the anchor's right edge when the default placement overflows right", () => {
    const anchor: Rect = { left: 900, top: 20, width: 50, height: 30 };
    const result = placeMenu(anchor, MENU, VIEWPORT);
    expect(result).toEqual({ left: 800, top: 50 });
    expect(result.left + MENU.width).toBeLessThanOrEqual(VIEWPORT.width);
  });
});

describe('placeMenu: wraps at the bottom edge of the viewport', () => {
  it('opens above the anchor when the default placement overflows the bottom', () => {
    const anchor: Rect = { left: 20, top: 750, width: 50, height: 30 };
    const result = placeMenu(anchor, MENU, VIEWPORT);
    expect(result).toEqual({ left: 20, top: 550 });
    expect(result.top).toBeGreaterThanOrEqual(0);
  });
});

describe('placeMenu: wraps at both the right and bottom edges at once', () => {
  it('combines the right-edge and bottom-edge wraps independently', () => {
    const anchor: Rect = { left: 900, top: 750, width: 50, height: 30 };
    const result = placeMenu(anchor, MENU, VIEWPORT);
    expect(result).toEqual({ left: 800, top: 550 });
  });
});

describe('placeMenu: clamps into the viewport when there is no fully-on-screen solution', () => {
  it('keeps the result within [0, viewport - menu] when the anchor sits off-screen (negative coordinates)', () => {
    const anchor: Rect = { left: -50, top: -50, width: 10, height: 10 };
    const smallViewport: Size = { width: 300, height: 300 };
    const result = placeMenu(anchor, MENU, smallViewport);
    expect(result.left).toBeGreaterThanOrEqual(0);
    expect(result.left).toBeLessThanOrEqual(smallViewport.width - MENU.width);
    expect(result.top).toBeGreaterThanOrEqual(0);
    expect(result.top).toBeLessThanOrEqual(smallViewport.height - MENU.height);
  });

  it('never returns a negative coordinate even when the menu is larger than the viewport', () => {
    const anchor: Rect = { left: 500, top: 500, width: 10, height: 10 };
    const tinyViewport: Size = { width: 100, height: 100 };
    const bigMenu: Size = { width: 400, height: 400 };
    const result = placeMenu(anchor, bigMenu, tinyViewport);
    expect(result.left).toBeGreaterThanOrEqual(0);
    expect(result.top).toBeGreaterThanOrEqual(0);
  });

  it('keeps the anchor far outside a normal-sized viewport fully clamped on both axes', () => {
    const anchor: Rect = { left: 5000, top: -5000, width: 10, height: 10 };
    const result = placeMenu(anchor, MENU, VIEWPORT);
    expect(result.left).toBeGreaterThanOrEqual(0);
    expect(result.left).toBeLessThanOrEqual(VIEWPORT.width - MENU.width);
    expect(result.top).toBeGreaterThanOrEqual(0);
    expect(result.top).toBeLessThanOrEqual(VIEWPORT.height - MENU.height);
  });
});

describe('placeMenu: a menu that fits exactly stays where it is', () => {
  it('keeps the default left when the menu ends exactly at the right edge', () => {
    const anchor: Rect = { left: 850, top: 20, width: 50, height: 30 };

    expect(placeMenu(anchor, MENU, VIEWPORT)).toEqual({ left: 850, top: 50 });
  });

  it('keeps the default top when the menu ends exactly at the bottom edge', () => {
    const anchor: Rect = { left: 20, top: 570, width: 50, height: 30 };

    expect(placeMenu(anchor, MENU, VIEWPORT)).toEqual({ left: 20, top: 600 });
  });

  it('wraps one pixel past the edge', () => {
    expect(placeMenu({ left: 851, top: 20, width: 50, height: 30 }, MENU, VIEWPORT).left).toBe(751);
    expect(placeMenu({ left: 20, top: 571, width: 50, height: 30 }, MENU, VIEWPORT).top).toBe(371);
  });

  it('puts a menu exactly as wide as the viewport at the origin', () => {
    const anchor: Rect = { left: 300, top: 20, width: 50, height: 30 };

    expect(placeMenu(anchor, { width: 1000, height: 200 }, VIEWPORT).left).toBe(0);
  });

  it('puts a menu larger than the viewport at the origin instead of at a negative coordinate', () => {
    const anchor: Rect = { left: 300, top: 300, width: 50, height: 30 };

    expect(placeMenu(anchor, { width: 1200, height: 900 }, VIEWPORT)).toEqual({ left: 0, top: 0 });
  });
});
