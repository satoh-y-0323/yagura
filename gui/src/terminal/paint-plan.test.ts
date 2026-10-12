import { describe, it, expect } from 'vitest';
import type { CellAttr } from '../ipc/session-boundary';
import type { CompositionLayout } from './composition';
import { createGrid, type GridCell, type GridLine, type TerminalGrid } from './frame-grid';
import type { GlyphShape } from './box-glyphs';
import {
  canvasSizeOf,
  cellMarks,
  cssCellSize,
  cursorOrigin,
  cursorPaint,
  cursorTarget,
  deviceCellOf,
  lineWidthOf,
  pixelRatioOf,
  planComposition,
  planRow,
  rowsToDraw,
  shapeOps,
  shownCursorShape,
} from './paint-plan';
import type { CellPaint } from './terminal-colors';

const PLAIN: CellAttr = { fg: 'default', bg: 'default', flags: [] };
const CELL = { width: 10, height: 20 };

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
    cells: Array.from({ length: cols }, () => cell(' ')),
  }));
  grid.cursor = { row: 0, col: 0, visible: true, shape: 'block', ...cursor };
  return grid;
}

describe('the device pixel ratio', () => {
  it('draws at 1 where the browser reports none', () => {
    expect(pixelRatioOf(2)).toBe(2);
    expect(pixelRatioOf(1.5)).toBe(1.5);
    expect(pixelRatioOf(0)).toBe(1);
    expect(pixelRatioOf(Number.NaN)).toBe(1);
  });

  it('makes a light line a whole number of pixels, at least one', () => {
    expect(lineWidthOf(1)).toBe(1);
    expect(lineWidthOf(1.25)).toBe(1);
    expect(lineWidthOf(1.5)).toBe(2);
    expect(lineWidthOf(2)).toBe(2);
    expect(lineWidthOf(0.5)).toBe(1);
    expect(lineWidthOf(0.2)).toBe(1);
  });
});

describe('the cell a font gives', () => {
  it('is as wide as W rounded down and as tall as the font box rounded up', () => {
    expect(deviceCellOf({ width: 8.7, ascent: 14.2, descent: 3.4 }, 14, 1)).toEqual({
      width: 8,
      height: 18,
      baseline: 14,
    });
  });

  it('rounds the baseline to the nearest pixel', () => {
    expect(deviceCellOf({ width: 8, ascent: 14.6, descent: 3 }, 14, 1).baseline).toBe(15);
  });

  it('measures in device pixels', () => {
    expect(deviceCellOf({ width: 16, ascent: 28, descent: 8 }, 14, 2)).toEqual({
      width: 16,
      height: 36,
      baseline: 28,
    });
  });

  it('falls back to a 1.2 line height where the engine reports no font box', () => {
    const fallback = { width: 8, height: Math.ceil(14 * 2 * 1.2), baseline: 28 };
    expect(deviceCellOf({ width: 8, ascent: Number.NaN, descent: 3 }, 14, 2)).toEqual(fallback);
    expect(deviceCellOf({ width: 8, ascent: 3, descent: Number.POSITIVE_INFINITY }, 14, 2)).toEqual(
      fallback
    );
    expect(deviceCellOf({ width: 8, ascent: 0, descent: 0 }, 14, 2)).toEqual(fallback);
  });

  it('is never empty', () => {
    expect(deviceCellOf({ width: 0.4, ascent: 0.1, descent: 0.1 }, 14, 1)).toEqual({
      width: 1,
      height: 1,
      baseline: 0,
    });
  });

  it('converts to CSS pixels by the pixel ratio', () => {
    expect(cssCellSize({ width: 15, height: 30, baseline: 20 }, 1.5)).toEqual({
      cellWidth: 10,
      cellHeight: 20,
    });
  });

  it('sizes the canvas to whole cells, never empty', () => {
    expect(canvasSizeOf({ cols: 80, rows: 24 }, { width: 9, height: 18, baseline: 14 })).toEqual({
      width: 720,
      height: 432,
    });
    expect(canvasSizeOf({ cols: 0, rows: 0 }, { width: 9, height: 18, baseline: 14 })).toEqual({
      width: 1,
      height: 1,
    });
  });
});

describe('the rows to draw', () => {
  it('draws the dirty rows and the cursor rows, only those of the grid', () => {
    expect(rowsToDraw({ isAll: false, rows: new Set([2, 5]) }, 10, 7, 3)).toEqual([2, 5, 7, 3]);
  });

  it('draws every row when all are dirty', () => {
    expect(rowsToDraw({ isAll: true, rows: new Set() }, 3, null, 1)).toEqual([0, 1, 2]);
  });

  it('draws a row once and leaves out rows the grid does not have', () => {
    expect(rowsToDraw({ isAll: false, rows: new Set([1, 1, 9, -1]) }, 4, 1, 1)).toEqual([1]);
    expect(rowsToDraw({ isAll: false, rows: new Set([3]) }, 4, null, 4)).toEqual([3]);
    expect(rowsToDraw({ isAll: false, rows: new Set() }, 4, null, 0)).toEqual([0]);
    expect(rowsToDraw({ isAll: false, rows: new Set([4]) }, 4, null, 0)).toEqual([0]);
  });

  it('draws the row the cursor left, which no cell change names', () => {
    expect(rowsToDraw({ isAll: false, rows: new Set() }, 24, 5, 6)).toEqual([5, 6]);
  });

  it('draws nothing in a grid with no rows', () => {
    expect(rowsToDraw({ isAll: true, rows: new Set() }, 0, null, 0)).toEqual([]);
  });
});

describe('a row’s plan', () => {
  const paintFor = (attr: CellAttr): CellPaint => ({
    foreground: attr.fg === 'default' ? 'white' : 'red',
    background: attr.bg === 'default' ? null : 'blue',
    isDim: attr.flags.includes('dim'),
  });

  it('clears the whole row, then fills cells with a background, then draws glyphs', () => {
    const marked: CellAttr = { fg: { indexed: 1 }, bg: { indexed: 4 }, flags: ['dim'] };
    const line: GridLine = { wrapped: false, cells: [cell('a'), cell('b', 1, marked), cell(' ')] };
    const plan = planRow(line, 3, 3, CELL, paintFor);
    expect(plan.clear).toEqual({ x: 0, y: 60, w: 30, h: 20 });
    expect(plan.backgrounds).toEqual([{ rect: { x: 10, y: 60, w: 10, h: 20 }, color: 'blue' }]);
    expect(plan.glyphs.map(({ col, text, color, isDim }) => [col, text, color, isDim])).toEqual([
      [0, 'a', 'white', false],
      [1, 'b', 'red', true],
      [2, ' ', 'white', false],
    ]);
  });

  it('covers both columns of a wide character and draws nothing for its spacer', () => {
    const marked: CellAttr = { fg: 'default', bg: { indexed: 4 }, flags: [] };
    const line: GridLine = {
      wrapped: false,
      cells: [cell('全', 2, marked), cell('', 0, marked), cell('x')],
    };
    const plan = planRow(line, 0, 3, CELL, paintFor);
    expect(plan.backgrounds).toEqual([{ rect: { x: 0, y: 0, w: 20, h: 20 }, color: 'blue' }]);
    expect(plan.glyphs.map(({ col, text, cells }) => [col, text, cells])).toEqual([
      [0, '全', 2],
      [2, 'x', 1],
    ]);
  });

  it('lets a one-cell symbol spill into a blank cell on its right', () => {
    const line: GridLine = {
      wrapped: false,
      cells: [cell('⚠️'), cell(' '), cell('⚠️'), cell('x')],
    };
    const plan = planRow(line, 0, 4, CELL, paintFor);
    expect(plan.glyphs.map(({ room }) => room)).toEqual([2, 1, 1, 1]);
  });

  it('keeps the attribute each glyph is drawn with', () => {
    const bold: CellAttr = { fg: 'default', bg: 'default', flags: ['bold'] };
    const line: GridLine = { wrapped: false, cells: [cell('a', 1, bold)] };
    expect(planRow(line, 0, 1, CELL, paintFor).glyphs[0]?.attr).toBe(bold);
  });
});

describe('the marks of a cell', () => {
  const origin = { x: 30, y: 40 };

  it('draws a blank cell without a glyph', () => {
    expect(cellMarks('a', [], origin, CELL, 1, 1, false).drawsGlyph).toBe(true);
    expect(cellMarks(' ', [], origin, CELL, 1, 1, false).drawsGlyph).toBe(false);
    expect(cellMarks('', [], origin, CELL, 1, 1, false).drawsGlyph).toBe(false);
  });

  it('draws a dim cell at half opacity', () => {
    expect(cellMarks('a', [], origin, CELL, 1, 1, true).alpha).toBe(0.5);
    expect(cellMarks('a', [], origin, CELL, 1, 1, false).alpha).toBe(1);
  });

  it('puts the underline on the cell’s bottom edge across its cells', () => {
    expect(cellMarks('a', ['underline'], origin, CELL, 1, 2, false).underline).toEqual({
      x: 30,
      y: 58,
      w: 10,
      h: 2,
    });
    expect(cellMarks('全', ['underline'], origin, CELL, 2, 1, false).underline).toEqual({
      x: 30,
      y: 59,
      w: 20,
      h: 1,
    });
  });

  it('puts the strikeout through the middle of the cell', () => {
    expect(cellMarks('a', ['strikeout'], origin, CELL, 1, 1, false).strikeout).toEqual({
      x: 30,
      y: 50,
      w: 10,
      h: 1,
    });
    expect(
      cellMarks('a', ['strikeout'], origin, { width: 10, height: 21 }, 1, 1, false).strikeout
    ).toEqual({ x: 30, y: 50, w: 10, h: 1 });
  });

  it('marks only what the flags say', () => {
    const plain = cellMarks('a', ['bold', 'italic'], origin, CELL, 1, 1, false);
    expect(plain.underline).toBeNull();
    expect(plain.strikeout).toBeNull();
    const both = cellMarks('a', ['underline', 'strikeout'], origin, CELL, 1, 1, false);
    expect(both.underline).not.toBeNull();
    expect(both.strikeout).not.toBeNull();
  });
});

describe('the input method’s text', () => {
  const layout: CompositionLayout = {
    row: 2,
    cells: [
      { col: 4, text: 'か', width: 2 },
      { col: 6, text: 'a', width: 1 },
    ],
    cursorCol: 7,
  };

  it('covers the cells it takes and underlines them', () => {
    expect(planComposition(layout, 2, CELL, 2)).toEqual([
      {
        text: 'か',
        cells: 2,
        background: { x: 40, y: 40, w: 20, h: 20 },
        underline: { x: 40, y: 58, w: 20, h: 2 },
      },
      {
        text: 'a',
        cells: 1,
        background: { x: 60, y: 40, w: 10, h: 20 },
        underline: { x: 60, y: 58, w: 10, h: 2 },
      },
    ]);
  });

  it('draws nothing in another row, or with no composition', () => {
    expect(planComposition(layout, 1, CELL, 1)).toEqual([]);
    expect(planComposition(null, 2, CELL, 1)).toEqual([]);
  });
});

describe('where the cursor is drawn', () => {
  it('is in its own cell, one cell wide, or two over a wide character', () => {
    const grid = gridWith(5, 2, { row: 1, col: 2 });
    const target = cursorTarget(grid, null);
    expect(target).toMatchObject({ row: 1, col: 2, cells: 1 });
    const wideLine = grid.lines[1];
    if (wideLine === undefined) throw new Error('no row');
    wideLine.cells[2] = cell('全', 2);
    expect(cursorTarget(grid, null)).toMatchObject({ cells: 2, cell: { text: '全' } });
  });

  it('stays on the last column while a character waits to wrap', () => {
    expect(cursorTarget(gridWith(5, 2, { col: 5 }), null)).toMatchObject({ col: 4 });
  });

  it('is not drawn when hidden or scrolled back from the live screen', () => {
    expect(cursorTarget(gridWith(5, 2, { visible: false }), null)).toBeNull();
    const scrolled = gridWith(5, 2);
    scrolled.view = { ...scrolled.view, display_offset: 3 };
    expect(cursorTarget(scrolled, null)).toBeNull();
  });

  it('is not drawn in a grid with no columns', () => {
    expect(cursorTarget(gridWith(0, 0), null)).toBeNull();
  });

  it('is the input method’s cursor while it composes, even if the cursor is hidden', () => {
    const grid = gridWith(10, 4, { visible: false, row: 0, col: 0 });
    const layout: CompositionLayout = { row: 2, cells: [], cursorCol: 6 };
    expect(cursorTarget(grid, layout)).toMatchObject({ row: 2, col: 6 });
    grid.cursor = { ...grid.cursor, visible: true };
    grid.view = { ...grid.view, display_offset: 2 };
    expect(cursorTarget(grid, layout)).toMatchObject({ row: 2, col: 6 });
  });

  it('is still found when the grid has fewer rows than the cursor row names', () => {
    const target = cursorTarget(gridWith(5, 2, { row: 7, col: 1 }), null);
    expect(target).toMatchObject({ row: 7, col: 1, cells: 1, cell: undefined });
    const beyondColumns = gridWith(5, 2, { row: 0, col: 1 });
    const only = beyondColumns.lines[0];
    if (only === undefined) throw new Error('no row');
    only.cells.length = 0;
    expect(cursorTarget(beyondColumns, null)).toMatchObject({ cells: 1, cell: undefined });
  });

  it('shows a hollow block in a window that has no focus', () => {
    expect(shownCursorShape('bar', true)).toBe('bar');
    expect(shownCursorShape('bar', false)).toBe('hollow_block');
    expect(shownCursorShape('block', false)).toBe('hollow_block');
  });

  it('paints each shape inside its cell', () => {
    const origin = { x: 30, y: 40 };
    expect(cursorPaint('block', origin, CELL, 1, 1)).toEqual({
      kind: 'fill',
      rect: { x: 30, y: 40, w: 10, h: 20 },
    });
    expect(cursorPaint('block', origin, CELL, 2, 1)).toEqual({
      kind: 'fill',
      rect: { x: 30, y: 40, w: 20, h: 20 },
    });
    expect(cursorPaint('underline', origin, CELL, 1, 2)).toEqual({
      kind: 'fill',
      rect: { x: 30, y: 56, w: 10, h: 4 },
    });
    expect(cursorPaint('bar', origin, CELL, 2, 2)).toEqual({
      kind: 'fill',
      rect: { x: 30, y: 40, w: 4, h: 20 },
    });
    expect(cursorPaint('hollow_block', origin, CELL, 2, 2)).toEqual({
      kind: 'outline',
      rect: { x: 31, y: 41, w: 18, h: 18 },
      lineWidth: 2,
    });
  });

  it('puts the input method’s window on the cursor’s cell, in CSS pixels', () => {
    const device = { width: 20, height: 40, baseline: 30 };
    expect(cursorOrigin(gridWith(10, 5, { row: 3, col: 4 }), device, 2)).toEqual({
      x: 40,
      y: 60,
    });
    expect(cursorOrigin(gridWith(10, 5, { row: 0, col: 10 }), device, 2)).toEqual({
      x: 90,
      y: 0,
    });
    expect(cursorOrigin(gridWith(0, 0, { row: 0, col: 0 }), device, 2)).toEqual({ x: 0, y: 0 });
  });
});

describe('the steps that draw a glyph', () => {
  const line = (
    x1: number,
    y1: number,
    x2: number,
    y2: number,
    weight: 'light' | 'heavy' = 'light'
  ): GlyphShape => ({ kind: 'line', x1, y1, x2, y2, weight });

  it('draws a line the same from whichever end it starts', () => {
    expect(shapeOps([line(1, 0.5, 0, 0.5)], 10, 10, 1)).toEqual(
      shapeOps([line(0, 0.5, 1, 0.5)], 10, 10, 1)
    );
    expect(shapeOps([line(0.5, 1, 0.5, 0)], 10, 10, 1)).toEqual(
      shapeOps([line(0.5, 0, 0.5, 1)], 10, 10, 1)
    );
    expect(shapeOps([line(0.5, 1, 0.5, 0.5)], 10, 10, 2)).toEqual(
      shapeOps([line(0.5, 0.5, 0.5, 1)], 10, 10, 2)
    );
  });

  it('reaches half a stroke past an end inside the cell so lines that meet leave no notch', () => {
    const heavy: GlyphShape = line(0, 0.5, 0.5, 0.5, 'heavy');
    expect(shapeOps([heavy], 10, 10, 1)).toEqual([
      { kind: 'fill', rect: { x: 0, y: 4, w: 6, h: 2 }, alpha: null },
    ]);
    const vertical: GlyphShape = line(0.5, 0.5, 0.5, 1, 'heavy');
    expect(shapeOps([vertical], 10, 10, 1)).toEqual([
      { kind: 'fill', rect: { x: 4, y: 4, w: 2, h: 6 }, alpha: null },
    ]);
    const upward: GlyphShape = line(0.5, 0, 0.5, 0.5, 'heavy');
    expect(shapeOps([upward], 10, 10, 1)).toEqual([
      { kind: 'fill', rect: { x: 4, y: 0, w: 2, h: 6 }, alpha: null },
    ]);
  });

  it('draws a diagonal as a stroke as thick as the line is', () => {
    expect(shapeOps([line(0, 0, 1, 1)], 10, 10, 3)).toEqual([
      { kind: 'stroke', lineWidth: 3, from: [0, 0], to: [10, 10] },
    ]);
    const heavyDiagonal: GlyphShape = line(0, 0, 1, 1, 'heavy');
    expect(shapeOps([heavyDiagonal], 10, 10, 3)).toEqual([
      { kind: 'stroke', lineWidth: 6, from: [0, 0], to: [10, 10] },
    ]);
  });
});
