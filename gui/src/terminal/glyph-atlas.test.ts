import { describe, it, expect } from 'vitest';
import { PAGE_SIZE } from './atlas-layout';
import { GlyphAtlas, type GlyphRect, type GlyphStyle, type PageSource } from './glyph-atlas';

interface FakePage {
  id: number;
}

interface Drawn {
  page: FakePage;
  slot: GlyphRect;
  text: string;
  style: GlyphStyle;
}

function fakeSource(): {
  source: PageSource<FakePage>;
  pages: FakePage[];
  drawn: Drawn[];
  control: { canCreate: boolean };
} {
  const pages: FakePage[] = [];
  const drawn: Drawn[] = [];
  const control = { canCreate: true };
  return {
    pages,
    drawn,
    control,
    source: {
      createPage: () => {
        if (!control.canCreate) return null;
        const page = { id: pages.length };
        pages.push(page);
        return page;
      },
      rasterize: (page, slot, text, style) => {
        drawn.push({ page, slot, text, style });
      },
    },
  };
}

const STYLE: GlyphStyle = { font: '14px Mono', color: '#fff', baseline: 12 };

describe('the glyph atlas', () => {
  it('rasterizes a glyph into a slot the size of its cells', () => {
    const { source, drawn } = fakeSource();
    const atlas = new GlyphAtlas(source, 10, 20);
    const slot = atlas.glyph('a', 2, STYLE);
    expect(slot).toMatchObject({ x: 0, y: 0, width: 20, height: 20 });
    expect(drawn).toHaveLength(1);
    expect(drawn[0]).toMatchObject({ text: 'a', slot: { x: 0, y: 0, width: 20, height: 20 } });
    expect(drawn[0]?.page).toBe(slot?.page);
  });

  it('gives the cached slot back without rasterizing again', () => {
    const { source, drawn } = fakeSource();
    const atlas = new GlyphAtlas(source, 10, 20);
    const first = atlas.glyph('a', 1, STYLE);
    const second = atlas.glyph('a', 1, STYLE);
    expect(second).toBe(first);
    expect(drawn).toHaveLength(1);
  });

  it('keeps a glyph apart per text, width, font and color', () => {
    const { source, drawn } = fakeSource();
    const atlas = new GlyphAtlas(source, 10, 20);
    atlas.glyph('a', 1, STYLE);
    atlas.glyph('b', 1, STYLE);
    atlas.glyph('a', 2, STYLE);
    atlas.glyph('a', 1, { ...STYLE, font: 'bold 14px Mono' });
    atlas.glyph('a', 1, { ...STYLE, color: '#000' });
    expect(drawn).toHaveLength(5);
  });

  it('places the next glyph after the one before on the same page', () => {
    const { source, pages } = fakeSource();
    const atlas = new GlyphAtlas(source, 10, 20);
    const first = atlas.glyph('a', 1, STYLE);
    const second = atlas.glyph('b', 1, STYLE);
    expect(second).toMatchObject({ x: 10, y: 0 });
    expect(second?.page).toBe(first?.page);
    expect(pages).toHaveLength(1);
  });

  it('draws again after a reset for another cell size', () => {
    const { source, drawn, pages } = fakeSource();
    const atlas = new GlyphAtlas(source, 10, 20);
    atlas.glyph('a', 1, STYLE);
    atlas.reset(12, 24);
    const slot = atlas.glyph('a', 1, STYLE);
    expect(slot).toMatchObject({ x: 0, y: 0, width: 12, height: 24 });
    expect(drawn).toHaveLength(2);
    expect(pages).toHaveLength(2);
  });

  it('has no slot for a glyph no page can hold', () => {
    const { source, drawn } = fakeSource();
    const atlas = new GlyphAtlas(source, PAGE_SIZE, 20);
    expect(atlas.glyph('a', 2, STYLE)).toBeNull();
    expect(drawn).toEqual([]);
  });

  it('has no slot where no page can be made, and starts over', () => {
    const { source, control, drawn } = fakeSource();
    control.canCreate = false;
    const atlas = new GlyphAtlas(source, 10, 20);
    expect(atlas.glyph('a', 1, STYLE)).toBeNull();
    control.canCreate = true;
    expect(atlas.glyph('a', 1, STYLE)).toMatchObject({ x: 0, y: 0 });
    expect(drawn).toHaveLength(1);
  });

  it('forgets its glyphs when a page cannot be made, and draws them again', () => {
    const { source, drawn, control } = fakeSource();
    // One glyph per page: the second needs a page the source cannot make.
    const atlas = new GlyphAtlas(source, PAGE_SIZE, PAGE_SIZE);
    expect(atlas.glyph('a', 1, STYLE)).not.toBeNull();
    control.canCreate = false;
    expect(atlas.glyph('b', 1, STYLE)).toBeNull();
    control.canCreate = true;
    expect(atlas.glyph('a', 1, STYLE)).not.toBeNull();
    expect(drawn.map((d) => d.text)).toEqual(['a', 'a']);
  });

  it('forgets every glyph when the pages are full, and draws them again', () => {
    const { source, drawn, pages } = fakeSource();
    // One glyph fills a page, and there are four pages.
    const atlas = new GlyphAtlas(source, PAGE_SIZE, PAGE_SIZE);
    const first = atlas.glyph('a', 1, STYLE);
    for (const text of ['b', 'c', 'd']) atlas.glyph(text, 1, STYLE);
    expect(pages).toHaveLength(4);
    const fifth = atlas.glyph('e', 1, STYLE);
    expect(fifth).toMatchObject({ x: 0, y: 0 });
    expect(pages).toHaveLength(5);
    const again = atlas.glyph('a', 1, STYLE);
    expect(again).not.toBe(first);
    expect(drawn.map((d) => d.text)).toEqual(['a', 'b', 'c', 'd', 'e', 'a']);
  });
});
