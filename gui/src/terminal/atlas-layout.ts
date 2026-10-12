/**
 * Where the glyph atlas puts the next glyph: pages fill left to right, top
 * to bottom, and when the page limit is reached the layout starts over.
 */

/** The width and height of one page, in device pixels. */
export const PAGE_SIZE = 1024;
const MAX_PAGES = 4;

/** Where a glyph goes. */
export interface Placement {
  /** The page, counted from 0; one past the last page when it must be created. */
  page: number;
  x: number;
  y: number;
  /** The layout started over: every earlier placement is void. */
  restarted: boolean;
}

export class AtlasLayout {
  private pageCount = 0;
  private cursorX = 0;
  private cursorY = 0;

  constructor(
    private cellWidth: number,
    private cellHeight: number
  ) {}

  /** Starts over for another cell size. */
  reset(cellWidth: number, cellHeight: number): void {
    this.cellWidth = cellWidth;
    this.cellHeight = cellHeight;
    this.pageCount = 0;
    this.cursorX = 0;
    this.cursorY = 0;
  }

  /** The cell width times `cells`, the width of a glyph slot. */
  slotWidth(cells: 1 | 2): number {
    return this.cellWidth * cells;
  }

  /** The place for a slot `width` wide, or `null` when no page can hold it. */
  place(width: number): Placement | null {
    if (width > PAGE_SIZE || this.cellHeight > PAGE_SIZE) return null;
    if (this.cursorX + width > PAGE_SIZE) {
      this.cursorX = 0;
      this.cursorY += this.cellHeight;
    }
    let restarted = false;
    if (this.pageCount === 0 || this.cursorY + this.cellHeight > PAGE_SIZE) {
      if (this.pageCount >= MAX_PAGES) {
        this.reset(this.cellWidth, this.cellHeight);
        restarted = true;
      }
      this.pageCount += 1;
      this.cursorX = 0;
      this.cursorY = 0;
    }
    const x = this.cursorX;
    this.cursorX += width;
    return { page: this.pageCount - 1, x, y: this.cursorY, restarted };
  }
}

/** The cache key of a glyph: the same text drawn in another style is another glyph. */
export function glyphKey(text: string, cells: 1 | 2, font: string, color: string): string {
  return `${font}\u0000${color}\u0000${cells}\u0000${text}`;
}
