/**
 * What one frame of the canvas renderer draws, beyond the row plans of
 * `paint-plan.ts`: a cell's drawing, a row with the input method's text over
 * it, the cursor, and whether the canvas must be resized. Plain data in, plain
 * data out; the renderer carries each plan out on the canvas.
 */
import type { CellAttr } from '../ipc/session-boundary';
import { boxGlyphShapes, type GlyphShape } from './box-glyphs';
import type { CompositionLayout } from './composition';
import type { GridLine, TerminalGrid } from './frame-grid';
import type { GlyphStyle } from './glyph-atlas';
import {
  canvasSizeOf,
  cellMarks,
  cursorPaint,
  cursorTarget,
  lineWidthOf,
  planComposition,
  planRow,
  rowsToDraw,
  shownCursorShape,
  type CompositionDraw,
  type CursorPaint,
  type DeviceCell,
  type DirtyState,
  type GlyphDraw,
  type PixelRect,
  type RowPlan,
} from './paint-plan';
import type { CellPaint } from './terminal-colors';

/** Whether the browser reports another device pixel ratio than the one drawn with. */
export function hasScaleChanged(reported: number, drawnWith: number): boolean {
  return reported !== drawnWith;
}

/** The rows to draw again when the input method's layout goes from `previous` to `next`. */
export function compositionRows(
  previous: CompositionLayout | null,
  next: CompositionLayout | null
): number[] {
  return [previous, next].flatMap((layout) => (layout === null ? [] : [layout.row]));
}

/** The CSS font of text `size` CSS pixels high at `pixelRatio`, in device pixels. */
export function glyphFont(
  family: string,
  size: number,
  pixelRatio: number,
  flags: CellAttr['flags']
): string {
  const italic = flags.includes('italic') ? 'italic ' : '';
  const bold = flags.includes('bold') ? 'bold ' : '';
  return `${italic}${bold}${size * pixelRatio}px ${family}`;
}

/** What a cell is drawn with, besides the glyph itself. */
export interface CellEnv {
  cell: DeviceCell;
  pixelRatio: number;
  font: { family: string; size: number };
}

/** One glyph of a cell: a box or block shape drawn to the cell's size, or text from the atlas. */
export type GlyphOp =
  | { kind: 'shapes'; shapes: readonly GlyphShape[]; x: number; y: number; width: number }
  | { kind: 'atlas'; text: string; cells: 1 | 2; x: number; y: number; style: GlyphStyle };

/** One cell's drawing, in drawing order: the glyph, then the lines over it. */
export interface CellDraw {
  alpha: number;
  glyphs: GlyphOp[];
  lines: PixelRect[];
  color: string;
}

/** Plans the cell of `glyph` in row `row`. */
export function planCell(glyph: GlyphDraw, row: number, env: CellEnv): CellDraw {
  const { cell } = env;
  const x = glyph.col * cell.width;
  const y = row * cell.height;
  const marks = cellMarks(
    glyph.text,
    glyph.attr.flags,
    { x, y },
    cell,
    glyph.cells,
    lineWidthOf(env.pixelRatio),
    glyph.isDim
  );
  const glyphs: GlyphOp[] = [];
  if (marks.drawsGlyph) {
    const shapes = boxGlyphShapes(glyph.text);
    glyphs.push(
      shapes === null
        ? {
            kind: 'atlas',
            text: glyph.text,
            cells: glyph.room,
            x,
            y,
            style: {
              font: glyphFont(env.font.family, env.font.size, env.pixelRatio, glyph.attr.flags),
              color: glyph.color,
              baseline: cell.baseline,
            },
          }
        : { kind: 'shapes', shapes, x, y, width: cell.width * glyph.cells }
    );
  }
  const lines = [marks.underline, marks.strikeout].filter((line) => line !== null);
  return { alpha: marks.alpha, glyphs, lines, color: glyph.color };
}

/** A row to draw: its cells and the input method's text over them. */
export interface PaintRow {
  plan: RowPlan;
  compositions: CompositionDraw[];
}

/**
 * Plans row `row`. A row the grid has no line for is only cleared to the
 * background, with no input method's text over it.
 */
export function planPaintRow(
  line: GridLine | undefined,
  row: number,
  cols: number,
  cell: { width: number; height: number },
  paintFor: (attr: CellAttr) => CellPaint,
  composition: CompositionLayout | null,
  thickness: number
): PaintRow {
  if (line === undefined) {
    return {
      plan: {
        clear: { x: 0, y: row * cell.height, w: cols * cell.width, h: cell.height },
        backgrounds: [],
        glyphs: [],
      },
      compositions: [],
    };
  }
  return {
    plan: planRow(line, row, cols, cell, paintFor),
    compositions: planComposition(composition, row, cell, thickness),
  };
}

/** The cursor to draw: nothing, or its paint and the glyph shown inside a block. */
export interface CursorDraw {
  /** The row to draw again when the cursor moves; `null` when none was drawn. */
  drawnRow: number | null;
  paints: CursorPaint[];
  glyphs: GlyphDraw[];
  glyphRow: number;
}

/**
 * Plans the cursor. Inside a block the cell's glyph is drawn in `innerColor`,
 * except while the input method composes, when the cursor sits at the end of
 * its text and not over a cell of the grid.
 */
export function planCursor(
  grid: TerminalGrid,
  composition: CompositionLayout | null,
  isFocused: boolean,
  cell: DeviceCell,
  thickness: number,
  innerColor: string
): CursorDraw {
  const target = cursorTarget(grid, composition);
  if (target === null) {
    return { drawnRow: null, paints: [], glyphs: [], glyphRow: grid.cursor.row };
  }
  const shape = shownCursorShape(grid.cursor.shape, isFocused);
  const origin = { x: target.col * cell.width, y: target.row * cell.height };
  const paints = [cursorPaint(shape, origin, cell, target.cells, thickness)];
  const glyphs: GlyphDraw[] = [];
  if (shape === 'block' && target.cell !== undefined && composition === null) {
    glyphs.push({
      col: target.col,
      text: target.cell.text,
      cells: target.cells,
      room: target.cells,
      attr: target.cell.attr,
      color: innerColor,
      isDim: false,
    });
  }
  return { drawnRow: grid.cursor.row, paints, glyphs, glyphRow: target.row };
}

/** A new size for the canvas, in device pixels and in CSS pixels. */
export interface CanvasResize {
  width: number;
  height: number;
  cssWidth: number;
  cssHeight: number;
  cols: number;
  rows: number;
}

/** What a frame does before it draws rows: the resize if the grid changed size, and the rows. */
export interface FramePlan {
  resize: CanvasResize | null;
  rows: number[];
}

/** A resized canvas is blank, so every row is drawn again with it. */
export function planFrame(
  dirty: DirtyState,
  grid: TerminalGrid,
  cell: DeviceCell,
  pixelRatio: number,
  drawnSize: { cols: number; rows: number },
  drawnCursorRow: number | null
): FramePlan {
  const isResized = grid.cols !== drawnSize.cols || grid.rows !== drawnSize.rows;
  const size = canvasSizeOf(grid, cell);
  const resize: CanvasResize | null = isResized
    ? {
        ...size,
        cssWidth: size.width / pixelRatio,
        cssHeight: size.height / pixelRatio,
        cols: grid.cols,
        rows: grid.rows,
      }
    : null;
  const rows = rowsToDraw(
    { isAll: dirty.isAll || isResized, rows: dirty.rows },
    grid.rows,
    drawnCursorRow,
    grid.cursor.row
  );
  return { resize, rows };
}
