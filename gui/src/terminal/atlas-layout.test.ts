import { describe, it, expect } from 'vitest';
import { AtlasLayout, PAGE_SIZE, glyphKey } from './atlas-layout';

describe('where glyphs go in the atlas', () => {
  it('fills a page left to right, then top to bottom', () => {
    const layout = new AtlasLayout(400, 300);
    expect(layout.place(400)).toEqual({ page: 0, x: 0, y: 0, restarted: false });
    expect(layout.place(400)).toEqual({ page: 0, x: 400, y: 0, restarted: false });
    expect(layout.place(400)).toEqual({ page: 0, x: 0, y: 300, restarted: false });
    expect(layout.place(600)).toEqual({ page: 0, x: 400, y: 300, restarted: false });
    expect(layout.place(100)).toEqual({ page: 0, x: 0, y: 600, restarted: false });
  });

  it('keeps a glyph that ends exactly at the page edge on the row', () => {
    const layout = new AtlasLayout(512, 100);
    layout.place(512);
    expect(layout.place(512)).toMatchObject({ x: 512, y: 0 });
    expect(layout.place(512)).toMatchObject({ x: 0, y: 100 });
  });

  it('starts another page when the rows run out, and numbers pages from 0', () => {
    const layout = new AtlasLayout(PAGE_SIZE, 512);
    expect(layout.place(PAGE_SIZE)).toMatchObject({ page: 0, y: 0 });
    expect(layout.place(PAGE_SIZE)).toMatchObject({ page: 0, y: 512 });
    expect(layout.place(PAGE_SIZE)).toEqual({ page: 1, x: 0, y: 0, restarted: false });
  });

  it('starts over after four pages, telling the caller earlier places are void', () => {
    const layout = new AtlasLayout(PAGE_SIZE, PAGE_SIZE);
    for (let page = 0; page < 4; page += 1) {
      expect(layout.place(PAGE_SIZE)).toEqual({ page, x: 0, y: 0, restarted: false });
    }
    expect(layout.place(PAGE_SIZE)).toEqual({ page: 0, x: 0, y: 0, restarted: true });
    expect(layout.place(PAGE_SIZE)).toEqual({ page: 1, x: 0, y: 0, restarted: false });
  });

  it('has no place for a glyph wider or taller than a page', () => {
    expect(new AtlasLayout(10, 10).place(PAGE_SIZE + 1)).toBeNull();
    expect(new AtlasLayout(10, PAGE_SIZE + 1).place(10)).toBeNull();
    expect(new AtlasLayout(10, PAGE_SIZE).place(PAGE_SIZE)).not.toBeNull();
  });

  it('starts over for another cell size', () => {
    const layout = new AtlasLayout(100, 100);
    layout.place(100);
    layout.place(100);
    layout.reset(50, 60);
    expect(layout.slotWidth(2)).toBe(100);
    expect(layout.place(50)).toEqual({ page: 0, x: 0, y: 0, restarted: false });
    expect(layout.place(50)).toEqual({ page: 0, x: 50, y: 0, restarted: false });
  });

  it('gives a slot as wide as its cells', () => {
    const layout = new AtlasLayout(9, 18);
    expect(layout.slotWidth(1)).toBe(9);
    expect(layout.slotWidth(2)).toBe(18);
  });
});

describe('the cache key of a glyph', () => {
  it('tells apart text, cells, font and color', () => {
    const keys = new Set([
      glyphKey('a', 1, '14px mono', 'white'),
      glyphKey('b', 1, '14px mono', 'white'),
      glyphKey('a', 2, '14px mono', 'white'),
      glyphKey('a', 1, 'bold 14px mono', 'white'),
      glyphKey('a', 1, '14px mono', 'red'),
    ]);
    expect(keys.size).toBe(5);
  });

  it('does not run one field into the next', () => {
    expect(glyphKey('1', 1, 'f', '#fff')).not.toBe(glyphKey('', 1, 'f', '#fff1'));
    expect(glyphKey('a', 1, 'f1', 'c')).not.toBe(glyphKey('a', 1, 'f', '1c'));
  });
});
