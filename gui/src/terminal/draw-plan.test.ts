import { describe, it, expect } from 'vitest';
import type { CellAttr } from '../ipc/session-boundary';
import type { CompositionLayout } from './composition';
import {
  compositionRows,
  glyphFont,
  hasScaleChanged,
  planCell,
  planCursor,
  planFrame,
  planPaintRow,
  type CellEnv,
} from './draw-plan';
import { createGrid, type GridCell, type GridLine, type TerminalGrid } from './frame-grid';
import type { GlyphDraw } from './paint-plan';
import type { CellPaint } from './terminal-colors';

const PLAIN: CellAttr = { fg: 'default', bg: 'default', flags: [] };
const CELL = { width: 10, height: 20, baseline: 15 };
const ENV: CellEnv = { cell: CELL, pixelRatio: 2, font: { family: 'Mono', size: 14 } };

function cell(text: string, width: 0 | 1 | 2 = 1, attr: CellAttr = PLAIN): GridCell {
  return { text, width, attr };
}

function gridWith(
  cols: number,
  rows: number,
  cursor: Partial<TerminalGrid['cursor']> = {}
): TerminalGrid {
  const grid = createGrid();
  grid.cols = cols;
  grid.rows = rows;
  grid.lines = Array.from({ length: rows }, () => ({
    wrapped: false,
    cells: Array.from({ length: cols }, () => cell('a')),
  }));
  grid.cursor = { row: 0, col: 0, visible: true, shape: 'block', ...cursor };
  return grid;
}

function glyph(text: string, overrides: Partial<GlyphDraw> = {}): GlyphDraw {
  return {
    col: 3,
    text,
    cells: 1,
    room: 1,
    attr: PLAIN,
    color: '#abcdef',
    isDim: false,
    ...overrides,
  };
}

function composing(row: number): CompositionLayout {
  return { row, cells: [{ col: 0, text: 'あ', width: 2 }], cursorCol: 2 };
}

describe('the device pixel ratio changing', () => {
  it('is a ratio other than the one drawn with', () => {
    expect(hasScaleChanged(2, 2)).toBe(false);
    expect(hasScaleChanged(1.5, 2)).toBe(true);
  });
});

describe('the rows an input method layout leaves out of date', () => {
  it('are the row the text was in and the row it is in', () => {
    expect(compositionRows(composing(1), composing(3))).toEqual([1, 3]);
  });

  it('is only the new row when the text starts', () => {
    expect(compositionRows(null, composing(3))).toEqual([3]);
  });

  it('is only the old row when the text ends', () => {
    expect(compositionRows(composing(1), null)).toEqual([1]);
  });

  it('is no row when there was and is no text', () => {
    expect(compositionRows(null, null)).toEqual([]);
  });
});

describe('the font of a glyph', () => {
  it('is in device pixels', () => {
    expect(glyphFont('Mono', 14, 2, [])).toBe('28px Mono');
  });

  it('puts italic before bold', () => {
    expect(glyphFont('Mono', 14, 1, ['bold'])).toBe('bold 14px Mono');
    expect(glyphFont('Mono', 14, 1, ['italic'])).toBe('italic 14px Mono');
    expect(glyphFont('Mono', 14, 1, ['bold', 'italic'])).toBe('italic bold 14px Mono');
  });

  it('ignores flags that are not about the font', () => {
    expect(glyphFont('Mono', 14, 1, ['underline', 'strikeout'])).toBe('14px Mono');
  });
});

describe('the drawing of a cell', () => {
  it('has the atlas draw text at the cell, in the glyph color and the flagged font', () => {
    const draw = planCell(
      glyph('x', { attr: { ...PLAIN, flags: ['bold'] }, cells: 2, room: 1 }),
      4,
      ENV
    );
    expect(draw).toEqual({
      alpha: 1,
      color: '#abcdef',
      lines: [],
      glyphs: [
        {
          kind: 'atlas',
          text: 'x',
          cells: 1,
          x: 30,
          y: 80,
          style: { font: 'bold 28px Mono', color: '#abcdef', baseline: 15 },
        },
      ],
    });
  });

  it('draws a box character to the width of the cells it covers, not from the atlas', () => {
    const draw = planCell(glyph('─', { cells: 2, room: 2 }), 1, ENV);
    expect(draw.glyphs).toHaveLength(1);
    expect(draw.glyphs[0]).toMatchObject({ kind: 'shapes', x: 30, y: 20, width: 20 });
  });

  it('draws no glyph for a blank cell', () => {
    expect(planCell(glyph(' '), 0, ENV).glyphs).toEqual([]);
    expect(planCell(glyph(''), 0, ENV).glyphs).toEqual([]);
  });

  it('draws the underline and the strikeout in the glyph color, over a blank cell too', () => {
    const draw = planCell(
      glyph(' ', { attr: { ...PLAIN, flags: ['underline', 'strikeout'] } }),
      0,
      ENV
    );
    expect(draw.glyphs).toEqual([]);
    expect(draw.lines).toEqual([
      { x: 30, y: 18, w: 10, h: 2 },
      { x: 30, y: 10, w: 10, h: 2 },
    ]);
  });

  it('draws a dim cell at half opacity', () => {
    expect(planCell(glyph('x', { isDim: true }), 0, ENV).alpha).toBe(0.5);
  });
});

describe('the drawing of a row', () => {
  const paintFor = (): CellPaint => ({ foreground: '#fff', background: null, isDim: false });
  const line: GridLine = { wrapped: false, cells: [cell('a'), cell('b')] };

  it('plans the cells of a row the grid has a line for', () => {
    const { plan } = planPaintRow(line, 2, 2, CELL, paintFor, null, 1);
    expect(plan.clear).toEqual({ x: 0, y: 40, w: 20, h: 20 });
    expect(plan.glyphs.map((g) => g.text)).toEqual(['a', 'b']);
  });

  it('only clears a row the grid has no line for, whatever the input method holds', () => {
    const row = planPaintRow(undefined, 2, 2, CELL, paintFor, composing(2), 1);
    expect(row.plan).toEqual({
      clear: { x: 0, y: 40, w: 20, h: 20 },
      backgrounds: [],
      glyphs: [],
    });
    expect(row.compositions).toEqual([]);
  });

  it('draws the input method text over its own row only', () => {
    expect(planPaintRow(line, 1, 2, CELL, paintFor, composing(1), 1).compositions).toHaveLength(1);
    expect(planPaintRow(line, 0, 2, CELL, paintFor, composing(1), 1).compositions).toEqual([]);
  });
});

describe('the drawing of the cursor', () => {
  const draw = (
    grid: TerminalGrid,
    composition: CompositionLayout | null = null,
    isFocused = true
  ): ReturnType<typeof planCursor> => planCursor(grid, composition, isFocused, CELL, 1, '#101010');

  it('shows the glyph under a block cursor in the given color', () => {
    const grid = gridWith(4, 2, { row: 1, col: 2 });
    const cursor = draw(grid);
    expect(cursor.paints).toEqual([{ kind: 'fill', rect: { x: 20, y: 20, w: 10, h: 20 } }]);
    expect(cursor.glyphs).toEqual([
      { col: 2, text: 'a', cells: 1, room: 1, attr: PLAIN, color: '#101010', isDim: false },
    ]);
    expect(cursor.glyphRow).toBe(1);
    expect(cursor.drawnRow).toBe(1);
  });

  it('fits the glyph inside a wide cell to the cursor', () => {
    const grid = gridWith(4, 1, { col: 1 });
    grid.lines = [{ wrapped: false, cells: [cell('a'), cell('あ', 2), cell('', 0), cell('b')] }];
    const cursor = draw(grid);
    expect(cursor.paints).toEqual([{ kind: 'fill', rect: { x: 10, y: 0, w: 20, h: 20 } }]);
    expect(cursor.glyphs).toMatchObject([{ text: 'あ', cells: 2, room: 2 }]);
  });

  it('shows no glyph inside the other shapes', () => {
    expect(draw(gridWith(4, 1, { shape: 'bar' })).glyphs).toEqual([]);
    expect(draw(gridWith(4, 1, { shape: 'underline' })).glyphs).toEqual([]);
  });

  it('is an outline with no glyph in a window that is not focused', () => {
    const cursor = draw(gridWith(4, 1), null, false);
    expect(cursor.paints).toMatchObject([{ kind: 'outline' }]);
    expect(cursor.glyphs).toEqual([]);
    expect(cursor.drawnRow).toBe(0);
  });

  it('shows no glyph while the input method composes: the cursor is at the end of its text', () => {
    const cursor = draw(gridWith(4, 3, { row: 0 }), composing(2));
    expect(cursor.paints).toHaveLength(1);
    expect(cursor.glyphs).toEqual([]);
    expect(cursor.glyphRow).toBe(2);
    expect(cursor.drawnRow).toBe(0);
  });

  it('shows no glyph where the grid has no cell under the cursor', () => {
    const grid = gridWith(4, 2, { row: 1 });
    grid.lines = [grid.lines[0] as GridLine];
    const cursor = draw(grid);
    expect(cursor.paints).toHaveLength(1);
    expect(cursor.glyphs).toEqual([]);
  });

  it('draws nothing for a hidden cursor and remembers no row', () => {
    const cursor = draw(gridWith(4, 2, { row: 1, visible: false }));
    expect(cursor).toMatchObject({ paints: [], glyphs: [], drawnRow: null });
  });
});

describe('the work of a frame', () => {
  const dirty = { isAll: false, rows: new Set<number>([2]) };

  it('resizes the canvas, in device and CSS pixels, when the grid changed size', () => {
    const frame = planFrame(dirty, gridWith(4, 3), CELL, 2, { cols: 0, rows: 0 }, null);
    expect(frame.resize).toEqual({
      width: 40,
      height: 60,
      cssWidth: 20,
      cssHeight: 30,
      cols: 4,
      rows: 3,
    });
  });

  it('resizes the canvas for a change of columns alone, or of rows alone', () => {
    expect(
      planFrame(dirty, gridWith(4, 3), CELL, 1, { cols: 3, rows: 3 }, null).resize
    ).not.toBeNull();
    expect(
      planFrame(dirty, gridWith(4, 3), CELL, 1, { cols: 4, rows: 2 }, null).resize
    ).not.toBeNull();
  });

  it('draws every row after a resize, as the canvas is blank', () => {
    const frame = planFrame(dirty, gridWith(4, 3), CELL, 1, { cols: 4, rows: 2 }, null);
    expect(frame.rows).toEqual([0, 1, 2]);
  });

  it('leaves the canvas and the other rows alone when the size is the one drawn', () => {
    const frame = planFrame(dirty, gridWith(4, 3, { row: 0 }), CELL, 1, { cols: 4, rows: 3 }, null);
    expect(frame.resize).toBeNull();
    expect([...frame.rows].sort((a, b) => a - b)).toEqual([0, 2]);
  });

  it('draws the row the cursor was drawn in again', () => {
    const frame = planFrame(dirty, gridWith(4, 3, { row: 0 }), CELL, 1, { cols: 4, rows: 3 }, 1);
    expect([...frame.rows].sort((a, b) => a - b)).toEqual([0, 1, 2]);
  });
});
