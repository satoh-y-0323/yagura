/**
 * What the canvas renderer draws, decided as plain data: which rows, which
 * rectangles and glyphs in them, where the cursor and the composition sit.
 * The renderer only carries these decisions out on the canvas.
 */
import type { CellAttr, CursorShape } from '../ipc/session-boundary';
import type { GlyphShape } from './box-glyphs';
import type { CompositionLayout } from './composition';
import type { GridCell, GridLine, TerminalGrid } from './frame-grid';
import { glyphRoom } from './glyph-fit';
import type { CellPaint } from './terminal-colors';

/** A rectangle in device pixels. */
export interface PixelRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Cell size in device pixels and where the baseline sits in a cell. */
export interface DeviceCell {
  width: number;
  height: number;
  baseline: number;
}

const DEFAULT_LINE_HEIGHT = 1.2;

/** The device pixel ratio to draw with; a browser that reports none draws at 1. */
export function pixelRatioOf(reported: number): number {
  return reported || 1;
}

/** The thickness of a light line in device pixels. */
export function lineWidthOf(pixelRatio: number): number {
  return Math.max(1, Math.round(pixelRatio));
}

/** What measuring the text `W` in the window's font gives. */
export interface TextSample {
  width: number;
  /** The font's own ascent and descent; not finite where the engine reports none. */
  ascent: number;
  descent: number;
}

/**
 * The cell of a font of `fontSize` CSS pixels: as wide as `W`, as tall as the
 * font's box, or a default line height where the engine reports no box.
 */
export function deviceCellOf(sample: TextSample, fontSize: number, pixelRatio: number): DeviceCell {
  const deviceSize = fontSize * pixelRatio;
  const hasBox =
    Number.isFinite(sample.ascent) &&
    Number.isFinite(sample.descent) &&
    sample.ascent + sample.descent > 0;
  const height = Math.ceil(
    hasBox ? sample.ascent + sample.descent : deviceSize * DEFAULT_LINE_HEIGHT
  );
  return {
    width: Math.max(1, Math.floor(sample.width)),
    height: Math.max(1, height),
    baseline: Math.round(hasBox ? sample.ascent : deviceSize),
  };
}

/** The cell size in CSS pixels. */
export function cssCellSize(
  cell: DeviceCell,
  pixelRatio: number
): { cellWidth: number; cellHeight: number } {
  return { cellWidth: cell.width / pixelRatio, cellHeight: cell.height / pixelRatio };
}

/** The canvas size for a grid, never empty. */
export function canvasSizeOf(
  grid: { cols: number; rows: number },
  cell: DeviceCell
): { width: number; height: number } {
  return {
    width: Math.max(1, grid.cols * cell.width),
    height: Math.max(1, grid.rows * cell.height),
  };
}

/** What is out of date on the canvas. */
export interface DirtyState {
  isAll: boolean;
  rows: ReadonlySet<number>;
}

/**
 * The rows to draw: the dirty ones, plus the row the cursor was drawn in and
 * the row it is in now (it moves with no cell changing); only rows of the grid.
 */
export function rowsToDraw(
  dirty: DirtyState,
  gridRows: number,
  drawnCursorRow: number | null,
  cursorRow: number
): number[] {
  const rows = new Set<number>(
    dirty.isAll ? Array.from({ length: gridRows }, (_, row) => row) : dirty.rows
  );
  if (drawnCursorRow !== null) rows.add(drawnCursorRow);
  rows.add(cursorRow);
  return [...rows].filter((row) => row >= 0 && row < gridRows);
}

/** One glyph to draw in a row. */
export interface GlyphDraw {
  col: number;
  text: string;
  /** The cells the model gives it. */
  cells: 1 | 2;
  /** The cells it may draw over. */
  room: 1 | 2;
  attr: CellAttr;
  color: string;
  isDim: boolean;
}

/** A row's background and glyphs, in drawing order. */
export interface RowPlan {
  /** The whole row in the window's background. */
  clear: PixelRect;
  /** Cells with a background of their own. */
  backgrounds: { rect: PixelRect; color: string }[];
  glyphs: GlyphDraw[];
}

/**
 * Plans one row. A wide character's spacer cell draws nothing; the cell
 * before it covers both columns.
 */
export function planRow(
  line: GridLine,
  row: number,
  cols: number,
  cell: { width: number; height: number },
  paintFor: (attr: CellAttr) => CellPaint
): RowPlan {
  const top = row * cell.height;
  const plan: RowPlan = {
    clear: { x: 0, y: top, w: cols * cell.width, h: cell.height },
    backgrounds: [],
    glyphs: [],
  };
  line.cells.forEach((gridCell, col) => {
    if (gridCell.width === 0) return;
    const paint = paintFor(gridCell.attr);
    if (paint.background !== null) {
      plan.backgrounds.push({
        rect: { x: col * cell.width, y: top, w: cell.width * gridCell.width, h: cell.height },
        color: paint.background,
      });
    }
    plan.glyphs.push({
      col,
      text: gridCell.text,
      cells: gridCell.width,
      room: glyphRoom(gridCell, line.cells[col + 1]),
      attr: gridCell.attr,
      color: paint.foreground,
      isDim: paint.isDim,
    });
  });
  return plan;
}

/** What one cell is drawn with besides its glyph. */
export interface CellMarks {
  /** Opacity of the cell's drawing. */
  alpha: number;
  /** A blank cell has no glyph to draw. */
  drawsGlyph: boolean;
  underline: PixelRect | null;
  strikeout: PixelRect | null;
}

/** The marks of the cell at (`x`, `y`) device pixels, `cells` wide. */
export function cellMarks(
  text: string,
  flags: CellAttr['flags'],
  origin: { x: number; y: number },
  cell: { width: number; height: number },
  cells: 1 | 2,
  thickness: number,
  isDim: boolean
): CellMarks {
  const w = cell.width * cells;
  return {
    alpha: isDim ? 0.5 : 1,
    drawsGlyph: text !== ' ' && text !== '',
    underline: flags.includes('underline')
      ? { x: origin.x, y: origin.y + cell.height - thickness, w, h: thickness }
      : null,
    strikeout: flags.includes('strikeout')
      ? { x: origin.x, y: origin.y + Math.floor(cell.height / 2), w, h: thickness }
      : null,
  };
}

/** One input-method character to draw over the grid. */
export interface CompositionDraw {
  text: string;
  cells: 1 | 2;
  background: PixelRect;
  underline: PixelRect;
}

/** The input method's text, over the cells it covers; nothing in other rows. */
export function planComposition(
  layout: CompositionLayout | null,
  row: number,
  cell: { width: number; height: number },
  thickness: number
): CompositionDraw[] {
  if (layout?.row !== row) return [];
  return layout.cells.map((composed) => {
    const x = composed.col * cell.width;
    const y = row * cell.height;
    const w = cell.width * composed.width;
    return {
      text: composed.text,
      cells: composed.width,
      background: { x, y, w, h: cell.height },
      underline: { x, y: y + cell.height - thickness, w, h: thickness },
    };
  });
}

/** The cell the cursor is drawn in. */
export interface CursorTarget {
  row: number;
  col: number;
  cells: 1 | 2;
  /** The cell under the cursor, whose glyph shows inside a block. */
  cell: GridCell | undefined;
}

/**
 * Where the cursor is drawn, or `null` when it is not: hidden, or scrolled
 * back from the live screen. The input method's cursor is always shown, at
 * the end of its text.
 */
export function cursorTarget(
  grid: TerminalGrid,
  composition: CompositionLayout | null
): CursorTarget | null {
  const isShown = (grid.cursor.visible && grid.view.display_offset === 0) || composition !== null;
  if (!isShown || grid.cols === 0) return null;
  const row = composition?.row ?? grid.cursor.row;
  const col = composition?.cursorCol ?? Math.min(grid.cursor.col, grid.cols - 1);
  const cell = grid.lines[row]?.cells[col];
  return { row, col, cells: cell?.width === 2 ? 2 : 1, cell };
}

/** How the cursor is painted. */
export type CursorPaint =
  { kind: 'fill'; rect: PixelRect } | { kind: 'outline'; rect: PixelRect; lineWidth: number };

/** The shape drawn: an unfocused window shows a hollow block whatever the program asked for. */
export function shownCursorShape(shape: CursorShape, isFocused: boolean): CursorShape {
  return isFocused ? shape : 'hollow_block';
}

/** The painting of a cursor of `shape` over the cell at `origin`. */
export function cursorPaint(
  shape: CursorShape,
  origin: { x: number; y: number },
  cell: { width: number; height: number },
  cells: 1 | 2,
  thickness: number
): CursorPaint {
  const { x, y } = origin;
  const w = cell.width * cells;
  switch (shape) {
    case 'block':
      return { kind: 'fill', rect: { x, y, w, h: cell.height } };
    case 'underline':
      return {
        kind: 'fill',
        rect: { x, y: y + cell.height - thickness * 2, w, h: thickness * 2 },
      };
    case 'bar':
      return { kind: 'fill', rect: { x, y, w: thickness * 2, h: cell.height } };
    case 'hollow_block':
      return {
        kind: 'outline',
        rect: {
          x: x + thickness / 2,
          y: y + thickness / 2,
          w: w - thickness,
          h: cell.height - thickness,
        },
        lineWidth: thickness,
      };
  }
}

/** The cursor's cell origin in CSS pixels from the canvas's top-left. */
export function cursorOrigin(
  grid: Pick<TerminalGrid, 'cols' | 'cursor'>,
  cell: DeviceCell,
  pixelRatio: number
): { x: number; y: number } {
  const col = Math.min(grid.cursor.col, Math.max(grid.cols - 1, 0));
  return {
    x: (col * cell.width) / pixelRatio,
    y: (grid.cursor.row * cell.height) / pixelRatio,
  };
}

/** One drawing step of a box or block glyph, in device pixels from the cell's top-left. */
export type ShapeOp =
  | { kind: 'fill'; rect: PixelRect; alpha: number | null }
  | { kind: 'stroke'; lineWidth: number; from: [number, number]; to: [number, number] }
  | {
      kind: 'curve';
      lineWidth: number;
      from: [number, number];
      control: [number, number];
      to: [number, number];
    };

/**
 * Turns glyph shapes into drawing steps for a cell `width` by `height`
 * device pixels with light lines `light` thick. Axis-aligned lines become
 * filled rectangles at whole pixels; an end inside the cell reaches half a
 * stroke past its point so that lines meeting there leave no notch.
 */
export function shapeOps(
  shapes: readonly GlyphShape[],
  width: number,
  height: number,
  light: number
): ShapeOp[] {
  const at = (fraction: number, offset: number, size: number): number =>
    Math.round(fraction * size) + offset * light;
  const ops: ShapeOp[] = [];
  for (const shape of shapes) {
    switch (shape.kind) {
      case 'rect': {
        const left = Math.round(shape.x * width);
        const top = Math.round(shape.y * height);
        ops.push({
          kind: 'fill',
          rect: {
            x: left,
            y: top,
            w: Math.round((shape.x + shape.w) * width) - left,
            h: Math.round((shape.y + shape.h) * height) - top,
          },
          alpha: null,
        });
        break;
      }
      case 'line':
      case 'offset_line': {
        const stroke = shape.kind === 'line' && shape.weight === 'heavy' ? light * 2 : light;
        const [dx1, dy1, dx2, dy2] =
          shape.kind === 'line' ? [0, 0, 0, 0] : [shape.dx1, shape.dy1, shape.dx2, shape.dy2];
        const x1 = at(shape.x1, dx1, width);
        const y1 = at(shape.y1, dy1, height);
        const x2 = at(shape.x2, dx2, width);
        const y2 = at(shape.y2, dy2, height);
        if (y1 === y2 || x1 === x2) {
          ops.push({
            kind: 'fill',
            rect: axisLine(x1, y1, x2, y2, stroke, width, height),
            alpha: null,
          });
        } else {
          ops.push({ kind: 'stroke', lineWidth: stroke, from: [x1, y1], to: [x2, y2] });
        }
        break;
      }
      case 'curve':
        ops.push({
          kind: 'curve',
          lineWidth: light,
          from: [shape.x1 * width, shape.y1 * height],
          control: [shape.cx * width, shape.cy * height],
          to: [shape.x2 * width, shape.y2 * height],
        });
        break;
      case 'shade':
        ops.push({ kind: 'fill', rect: { x: 0, y: 0, w: width, h: height }, alpha: shape.density });
        break;
    }
  }
  return ops;
}

function axisLine(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  stroke: number,
  width: number,
  height: number
): PixelRect {
  const half = Math.floor(stroke / 2);
  const isInside = (value: number, size: number): boolean => value > 0 && value < size;
  if (y1 === y2) {
    const [from, to] = x1 <= x2 ? [x1, x2] : [x2, x1];
    const start = isInside(from, width) ? from - half : from;
    const end = isInside(to, width) ? to - half + stroke : to;
    return { x: start, y: y1 - half, w: end - start, h: stroke };
  }
  const [from, to] = y1 <= y2 ? [y1, y2] : [y2, y1];
  const start = isInside(from, height) ? from - half : from;
  const end = isInside(to, height) ? to - half + stroke : to;
  return { x: x1 - half, y: start, w: stroke, h: end - start };
}
