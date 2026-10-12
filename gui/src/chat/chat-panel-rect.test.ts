/**
 * The chat panel's rect: the rect the user chose is kept as is, and only the
 * rect shown for the current window is fit into it. The window is handed in;
 * nothing here measures the DOM.
 */
import { describe, expect, it } from 'vitest';

import {
  CHAT_PANEL_DEFAULT_SIZE,
  CHAT_PANEL_MIN_SIZE,
  chatPanelMaxSize,
  clampChatPanelRect,
  defaultChatPanelRect,
  readStoredChatPanelRect,
  type ChatPanelRect,
} from './chat-panel-rect';

type Viewport = { width: number; height: number };

const WIDE: Viewport = { width: 1920, height: 1080 };
const NARROW: Viewport = { width: 800, height: 600 };
/** Narrower and shorter than the minimum panel. */
const TINY: Viewport = { width: 300, height: 200 };

function expectInside(rect: ChatPanelRect, viewport: Viewport): void {
  expect(rect.x).toBeGreaterThanOrEqual(0);
  expect(rect.y).toBeGreaterThanOrEqual(0);
  expect(rect.x + rect.width).toBeLessThanOrEqual(viewport.width);
  expect(rect.y + rect.height).toBeLessThanOrEqual(viewport.height);
}

describe('the declared chat panel sizes', () => {
  it('has a positive minimum no larger than the default on both axes', () => {
    expect(CHAT_PANEL_MIN_SIZE.width).toBeGreaterThan(0);
    expect(CHAT_PANEL_MIN_SIZE.height).toBeGreaterThan(0);
    expect(CHAT_PANEL_MIN_SIZE.width).toBeLessThanOrEqual(CHAT_PANEL_DEFAULT_SIZE.width);
    expect(CHAT_PANEL_MIN_SIZE.height).toBeLessThanOrEqual(CHAT_PANEL_DEFAULT_SIZE.height);
  });
});

describe('clampChatPanelRect - the rect shown for a window', () => {
  it('returns a rect that already fits unchanged', () => {
    const rect = { x: 100, y: 80, width: 600, height: 500 };
    expect(clampChatPanelRect(rect, WIDE)).toEqual(rect);
  });

  it('raises a size below the minimum to the minimum when the window has room', () => {
    const shown = clampChatPanelRect({ x: 10, y: 10, width: 1, height: 1 }, WIDE);
    expect(shown.width).toBe(CHAT_PANEL_MIN_SIZE.width);
    expect(shown.height).toBe(CHAT_PANEL_MIN_SIZE.height);
  });

  it.each<[string, ChatPanelRect]>([
    ['past the right and bottom edges', { x: 1700, y: 900, width: 600, height: 500 }],
    ['past the left and top edges', { x: -400, y: -300, width: 600, height: 500 }],
    ['larger than the window', { x: 0, y: 0, width: 5000, height: 4000 }],
  ])('pulls a rect %s back inside the window', (_label, rect) => {
    expectInside(clampChatPanelRect(rect, NARROW), NARROW);
  });

  it('keeps the size of a rect that only sits outside, moving it instead', () => {
    const shown = clampChatPanelRect({ x: 1700, y: 900, width: 600, height: 500 }, WIDE);
    expect(shown.width).toBe(600);
    expect(shown.height).toBe(500);
    expectInside(shown, WIDE);
  });

  it('stays inside a window smaller than the minimum: the window wins', () => {
    const shown = clampChatPanelRect({ x: 50, y: 50, width: 600, height: 500 }, TINY);
    expectInside(shown, TINY);
    expect(shown.width).toBeGreaterThan(0);
    expect(shown.height).toBeGreaterThan(0);
  });
});

describe('clampChatPanelRect - a smaller window only changes what is shown', () => {
  it('gives back the chosen rect once the window is wide again', () => {
    const chosen = { x: 1200, y: 400, width: 640, height: 560 };
    const whileNarrow = clampChatPanelRect(chosen, NARROW);
    expect(whileNarrow).not.toEqual(chosen);
    expectInside(whileNarrow, NARROW);
    expect(clampChatPanelRect(chosen, WIDE)).toEqual(chosen);
  });
});

describe('chatPanelMaxSize - the room from where the panel is shown to the far edges of the window', () => {
  const CANDIDATES: ReadonlyArray<ChatPanelRect> = [
    { x: 100, y: 80, width: 600, height: 500 },
    { x: 10, y: 10, width: 1, height: 1 },
    { x: 1700, y: 900, width: 600, height: 500 },
    { x: -400, y: -300, width: 600, height: 500 },
    { x: 0, y: 0, width: 5000, height: 4000 },
  ];
  const VIEWPORTS: ReadonlyArray<[Viewport]> = [[WIDE], [NARROW], [TINY]];

  it.each(VIEWPORTS)('is the window less the position of the rect shown, in %j', (viewport) => {
    for (const candidate of CANDIDATES) {
      const shown = clampChatPanelRect(candidate, viewport);
      expect(chatPanelMaxSize(shown, viewport), JSON.stringify(candidate)).toEqual({
        width: viewport.width - shown.x,
        height: viewport.height - shown.y,
      });
    }
  });

  it.each(VIEWPORTS)('bounds the size of the rect shown, in %j', (viewport) => {
    for (const candidate of CANDIDATES) {
      const shown = clampChatPanelRect(candidate, viewport);
      const max = chatPanelMaxSize(shown, viewport);
      expect(shown.width, JSON.stringify(candidate)).toBeLessThanOrEqual(max.width);
      expect(shown.height, JSON.stringify(candidate)).toBeLessThanOrEqual(max.height);
    }
  });

  it.each(VIEWPORTS)('is never below the minimum the window can show, in %j', (viewport) => {
    for (const candidate of CANDIDATES) {
      const max = chatPanelMaxSize(clampChatPanelRect(candidate, viewport), viewport);
      expect(max.width, JSON.stringify(candidate)).toBeGreaterThanOrEqual(
        Math.min(CHAT_PANEL_MIN_SIZE.width, viewport.width)
      );
      expect(max.height, JSON.stringify(candidate)).toBeGreaterThanOrEqual(
        Math.min(CHAT_PANEL_MIN_SIZE.height, viewport.height)
      );
    }
  });

  it('follows the position: moving the panel right and down leaves less room', () => {
    const near = chatPanelMaxSize({ x: 100, y: 80, width: 600, height: 500 }, WIDE);
    const far = chatPanelMaxSize({ x: 300, y: 200, width: 600, height: 500 }, WIDE);
    expect(far.width).toBe(near.width - 200);
    expect(far.height).toBe(near.height - 120);
  });
});

describe('defaultChatPanelRect - where the panel opens when nothing is stored', () => {
  it('opens at the default size, inside a window with room for it', () => {
    const rect = defaultChatPanelRect(WIDE);
    expect(rect.width).toBe(CHAT_PANEL_DEFAULT_SIZE.width);
    expect(rect.height).toBe(CHAT_PANEL_DEFAULT_SIZE.height);
    expectInside(rect, WIDE);
  });

  it('keeps to the right edge: a wider window moves it right by the difference', () => {
    const wider = { width: WIDE.width + 300, height: WIDE.height };
    expect(defaultChatPanelRect(wider).x - defaultChatPanelRect(WIDE).x).toBe(300);
    expect(defaultChatPanelRect(wider).y).toBe(defaultChatPanelRect(WIDE).y);
  });
});

describe('readStoredChatPanelRect - the stored rect is the chosen one, read without the window', () => {
  it('reads back a well-formed rect', () => {
    expect(readStoredChatPanelRect('{"x":100,"y":80,"width":600,"height":500}')).toEqual({
      x: 100,
      y: 80,
      width: 600,
      height: 500,
    });
  });

  it('keeps a rect larger than any window and a position past any edge as stored', () => {
    expect(readStoredChatPanelRect('{"x":5000,"y":3000,"width":4000,"height":3000}')).toEqual({
      x: 5000,
      y: 3000,
      width: 4000,
      height: 3000,
    });
  });

  it('raises a stored size below the minimum to the minimum, keeping the position', () => {
    expect(readStoredChatPanelRect('{"x":12,"y":34,"width":1,"height":2}')).toEqual({
      x: 12,
      y: 34,
      width: CHAT_PANEL_MIN_SIZE.width,
      height: CHAT_PANEL_MIN_SIZE.height,
    });
  });

  it.each([
    [null],
    [''],
    ['   '],
    ['not json'],
    ['{"x":1,'],
    ['null'],
    ['42'],
    ['[1,2,3,4]'],
    ['{}'],
    ['{"x":1,"y":2,"width":600}'],
    ['{"x":"1","y":2,"width":600,"height":500}'],
    ['{"x":1,"y":null,"width":600,"height":500}'],
    ['{"x":1,"y":2,"width":1e999,"height":500}'],
    ['{"x":-1e999,"y":2,"width":600,"height":500}'],
  ])('gives null for a missing, broken or non-finite value %j', (raw) => {
    let result: ChatPanelRect | null | undefined;
    expect(() => {
      result = readStoredChatPanelRect(raw);
    }).not.toThrow();
    expect(result).toBeNull();
  });
});

describe('clampChatPanelRect - each field is judged on its own', () => {
  const base: ChatPanelRect = { x: 100, y: 80, width: 600, height: 500 };

  it('returns the very rect it was given when it already fits', () => {
    expect(clampChatPanelRect(base, WIDE)).toBe(base);
  });

  it.each<[string, ChatPanelRect, ChatPanelRect]>([
    ['past the right edge', { ...base, x: 1700 }, { ...base, x: 1320 }],
    ['past the left edge', { ...base, x: -5 }, { ...base, x: 0 }],
    ['past the bottom edge', { ...base, y: 900 }, { ...base, y: 580 }],
    ['past the top edge', { ...base, y: -5 }, { ...base, y: 0 }],
    ['too narrow', { ...base, width: 100 }, { ...base, width: 420 }],
    ['too short', { ...base, height: 100 }, { ...base, height: 320 }],
    ['wider than the window', { ...base, x: 0, width: 5000 }, { ...base, x: 0, width: 1920 }],
    ['taller than the window', { ...base, y: 0, height: 5000 }, { ...base, y: 0, height: 1080 }],
  ])('changes only what is wrong with a rect %s', (_label, candidate, expected) => {
    expect(clampChatPanelRect(candidate, WIDE)).toEqual(expected);
  });

  it('shows a window smaller than the minimum whole, at its origin', () => {
    expect(clampChatPanelRect({ x: 50, y: 50, width: 600, height: 500 }, TINY)).toEqual({
      x: 0,
      y: 0,
      width: 300,
      height: 200,
    });
  });
});

describe('defaultChatPanelRect', () => {
  it('opens at the default size, against the right edge and below the top', () => {
    expect(defaultChatPanelRect(WIDE)).toEqual({ x: 1336, y: 72, width: 560, height: 520 });
  });
});

describe('readStoredChatPanelRect - what is not trusted', () => {
  const good = { x: 10, y: 20, width: 500, height: 400 };

  it('reads a well-formed rect as it was stored', () => {
    expect(readStoredChatPanelRect(JSON.stringify(good))).toEqual(good);
  });

  it('raises a size below the minimum and keeps a size above it', () => {
    expect(readStoredChatPanelRect(JSON.stringify({ ...good, width: 1, height: 1 }))).toEqual({
      ...good,
      width: CHAT_PANEL_MIN_SIZE.width,
      height: CHAT_PANEL_MIN_SIZE.height,
    });
    expect(readStoredChatPanelRect(JSON.stringify({ ...good, width: 900, height: 700 }))).toEqual({
      ...good,
      width: 900,
      height: 700,
    });
  });

  it.each<[string, string | null]>([
    ['nothing stored', null],
    ['text that is not JSON', '{not json'],
    ['JSON null', 'null'],
    ['a JSON number', '42'],
    ['a JSON string', '"rect"'],
    ['a JSON array', '[10,20,500,400]'],
    ['a missing x', JSON.stringify({ y: 20, width: 500, height: 400 })],
    ['a missing y', JSON.stringify({ x: 10, width: 500, height: 400 })],
    ['a missing width', JSON.stringify({ x: 10, y: 20, height: 400 })],
    ['a missing height', JSON.stringify({ x: 10, y: 20, width: 500 })],
    ['a string x', JSON.stringify({ ...good, x: '10' })],
    ['a non-finite x', '{"x":1e999,"y":20,"width":500,"height":400}'],
    ['a non-finite height', '{"x":10,"y":20,"width":500,"height":1e999}'],
  ])('gives null for %s', (_label, raw) => {
    expect(readStoredChatPanelRect(raw)).toBeNull();
  });
});
