/**
 * Glyphs drawn once and copied from then on: each distinct text, style and
 * color is rasterized into a slot of a page, and the renderer copies the slot
 * to the cell. {@link AtlasLayout} decides where each slot goes; a
 * {@link PageSource} makes the pages and draws into them.
 */

import { AtlasLayout, glyphKey } from './atlas-layout';

/** Where a cached glyph is. */
export interface GlyphSlot<Page> {
  page: Page;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** What a glyph is rasterized with, in device pixels. */
export interface GlyphStyle {
  font: string;
  color: string;
  baseline: number;
}

/** The surface the atlas draws on, a browser canvas or a fake one under test. */
export interface PageSource<Page> {
  /** A new blank page; `null` when no canvas can be made. */
  createPage: () => Page | null;
  /** Draws `text` in the rectangle `slot` of `page`, shrunk to fit its width. */
  rasterize: (page: Page, slot: GlyphRect, text: string, style: GlyphStyle) => void;
}

/** A slot's rectangle on its page. */
export interface GlyphRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export class GlyphAtlas<Page> {
  private readonly slots = new Map<string, GlyphSlot<Page>>();
  private readonly layout: AtlasLayout;
  private pages: Page[] = [];

  constructor(
    private readonly source: PageSource<Page>,
    private cellWidth: number,
    private cellHeight: number
  ) {
    this.layout = new AtlasLayout(cellWidth, cellHeight);
  }

  /** Starts over for another cell size (font or device pixel ratio changed). */
  reset(cellWidth: number, cellHeight: number): void {
    this.cellWidth = cellWidth;
    this.cellHeight = cellHeight;
    this.layout.reset(cellWidth, cellHeight);
    this.slots.clear();
    this.pages = [];
  }

  /**
   * The slot holding `text` in `style`, `cells` wide, a glyph wider than that
   * shrunk to fit; `null` when no canvas can be made.
   */
  glyph(text: string, cells: 1 | 2, style: GlyphStyle): GlyphSlot<Page> | null {
    const key = glyphKey(text, cells, style.font, style.color);
    const cached = this.slots.get(key);
    if (cached !== undefined) return cached;
    const width = this.layout.slotWidth(cells);
    const placement = this.layout.place(width);
    if (placement === null) return null;
    if (placement.restarted) {
      this.slots.clear();
      this.pages = [];
    }
    const page = this.pages[placement.page] ?? this.source.createPage();
    if (page === null) {
      this.reset(this.cellWidth, this.cellHeight);
      return null;
    }
    this.pages[placement.page] = page;
    const slot = { page, x: placement.x, y: placement.y, width, height: this.cellHeight };
    this.source.rasterize(page, slot, text, style);
    this.slots.set(key, slot);
    return slot;
  }
}
