/**
 * Draws a window's grid on one canvas: cells as the daemon sent them, never
 * reinterpreted. Cells are a whole number of device pixels, glyphs come from
 * an atlas, box drawing and block elements are drawn to the cell's size, and
 * only the rows that changed are drawn, once per animation frame. What to draw
 * is decided in `paint-plan.ts`; this class carries it out on the canvas.
 */
import type { TerminalTheme } from '../components/terminal-theme';
import type { GlyphShape } from './box-glyphs';
import { canvasPages, type CanvasPage } from './canvas-pages';
import type { CompositionLayout } from './composition';
import {
  glyphFont,
  hasScaleChanged,
  planCell,
  planCursor,
  planFrame,
  planPaintRow,
  type CellEnv,
  type GlyphOp,
} from './draw-plan';
import type { TerminalGrid } from './frame-grid';
import { GlyphAtlas, type GlyphSlot } from './glyph-atlas';
import type { CellMetrics } from './pointer-input';
import {
  cssCellSize,
  cursorOrigin,
  deviceCellOf,
  lineWidthOf,
  pixelRatioOf,
  shapeOps,
  type DeviceCell,
  type DirtyState,
  type GlyphDraw,
  type PixelRect,
} from './paint-plan';
import { RedrawScheduler, type DirtyRows } from './redraw-scheduler';
import { RenderState } from './render-state';
import { PaintCache, resolvePalette, type ResolvedPalette } from './terminal-colors';

export type { DirtyRows };

/** The font cells are drawn in, in CSS pixels. */
export interface TerminalFont {
  family: string;
  size: number;
}

export class CanvasRenderer {
  private readonly context: CanvasRenderingContext2D;
  private dpr = 1;
  private cell: DeviceCell = { width: 1, height: 1, baseline: 1 };
  private readonly atlas = new GlyphAtlas<CanvasPage>(canvasPages, 1, 1);
  private readonly scheduler: RedrawScheduler;
  private readonly state: RenderState;
  private palette: ResolvedPalette;
  private theme: TerminalTheme;
  /** The row the cursor was last drawn in, which needs drawing again when it moves. */
  private drawnCursorRow: number | null = null;
  private drawnSize = { cols: 0, rows: 0 };

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly gridOf: () => TerminalGrid,
    private font: TerminalFont,
    theme: TerminalTheme
  ) {
    const context = canvas.getContext('2d', { alpha: false });
    if (context === null) {
      throw new Error('the terminal canvas has no 2D context');
    }
    this.context = context;
    this.theme = theme;
    this.palette = resolvePalette(theme, []);
    this.scheduler = new RedrawScheduler(
      {
        request: (callback) => requestAnimationFrame(callback),
        cancel: (id) => {
          cancelAnimationFrame(id);
        },
      },
      (dirty) => {
        this.draw(dirty);
      }
    );
    this.state = new RenderState((rows) => {
      this.scheduler.invalidate(rows);
    });
    this.measure();
  }

  /** The cell size and the canvas's offset, in CSS pixels. */
  metrics(): CellMetrics {
    return {
      ...cssCellSize(this.cell, this.dpr),
      paddingLeft: this.canvas.offsetLeft,
      paddingTop: this.canvas.offsetTop,
    };
  }

  /** Measures the cell again if the device pixel ratio changed; whether it did. */
  remeasureIfScaled(): boolean {
    if (!hasScaleChanged(window.devicePixelRatio, this.dpr)) return false;
    this.measure();
    this.invalidate('all');
    return true;
  }

  setTheme(theme: TerminalTheme): void {
    this.theme = theme;
    this.invalidate('all');
  }

  setVisible(isVisible: boolean): void {
    this.scheduler.setVisible(isVisible);
  }

  setFocused(isFocused: boolean): void {
    this.state.setFocused(isFocused);
  }

  setComposition(layout: CompositionLayout | null): void {
    this.state.setComposition(layout);
  }

  /** Schedules `rows` to be drawn on the next animation frame. */
  invalidate(rows: DirtyRows): void {
    this.scheduler.invalidate(rows);
  }

  /**
   * Where the cursor's cell is, in CSS pixels from the canvas's top-left. The
   * input method's text starts there, so its candidate window stays put.
   */
  cursorPosition(): { x: number; y: number } {
    return cursorOrigin(this.gridOf(), this.cell, this.dpr);
  }

  dispose(): void {
    this.scheduler.dispose();
  }

  private measure(): void {
    this.dpr = pixelRatioOf(window.devicePixelRatio);
    const context = this.context;
    context.font = this.fontOf([]);
    const sample = context.measureText('W');
    this.cell = deviceCellOf(
      {
        width: sample.width,
        ascent: sample.fontBoundingBoxAscent,
        descent: sample.fontBoundingBoxDescent,
      },
      this.font.size,
      this.dpr
    );
    this.atlas.reset(this.cell.width, this.cell.height);
    this.drawnSize = { cols: 0, rows: 0 };
  }

  private fontOf(flags: GlyphDraw['attr']['flags']): string {
    return glyphFont(this.font.family, this.font.size, this.dpr, flags);
  }

  private cellEnv(): CellEnv {
    return { cell: this.cell, pixelRatio: this.dpr, font: this.font };
  }

  private draw(dirty: DirtyState): void {
    const grid = this.gridOf();
    this.palette = resolvePalette(this.theme, grid.overrides);
    const frame = planFrame(dirty, grid, this.cell, this.dpr, this.drawnSize, this.drawnCursorRow);
    if (frame.resize !== null) {
      this.canvas.width = frame.resize.width;
      this.canvas.height = frame.resize.height;
      this.canvas.style.width = `${frame.resize.cssWidth}px`;
      this.canvas.style.height = `${frame.resize.cssHeight}px`;
      this.drawnSize = { cols: frame.resize.cols, rows: frame.resize.rows };
    }
    const paints = new PaintCache(this.palette, this.state.isFocused);
    for (const row of frame.rows) this.drawRow(grid, row, paints);
    this.drawnCursorRow = this.drawCursor(grid);
  }

  private fillRect(rect: PixelRect, color: string): void {
    this.context.fillStyle = color;
    this.context.fillRect(rect.x, rect.y, rect.w, rect.h);
  }

  private drawRow(grid: TerminalGrid, row: number, paints: PaintCache): void {
    const { plan, compositions } = planPaintRow(
      grid.lines[row],
      row,
      grid.cols,
      this.cell,
      (attr) => paints.of(attr),
      this.state.composition,
      lineWidthOf(this.dpr)
    );
    this.fillRect(plan.clear, this.palette.background);
    for (const { rect, color } of plan.backgrounds) this.fillRect(rect, color);
    for (const glyph of plan.glyphs) this.drawCell(glyph, row);
    // The input method's text: in the theme's colors, underlined, over the cells it covers.
    for (const composed of compositions) {
      this.fillRect(composed.background, this.palette.background);
      const slot = this.atlas.glyph(composed.text, composed.cells, {
        font: this.fontOf([]),
        color: this.palette.foreground,
        baseline: this.cell.baseline,
      });
      if (slot !== null) this.copySlot(slot, composed.background.x, composed.background.y);
      this.fillRect(composed.underline, this.palette.foreground);
    }
  }

  private drawCell(glyph: GlyphDraw, row: number): void {
    const draw = planCell(glyph, row, this.cellEnv());
    this.context.globalAlpha = draw.alpha;
    for (const op of draw.glyphs) this.drawGlyph(op, draw.color);
    for (const line of draw.lines) this.fillRect(line, draw.color);
    this.context.globalAlpha = 1;
  }

  private drawGlyph(op: GlyphOp, color: string): void {
    if (op.kind === 'shapes') {
      this.drawShapes(op.shapes, op.x, op.y, op.width, color);
      return;
    }
    const slot = this.atlas.glyph(op.text, op.cells, op.style);
    if (slot !== null) this.copySlot(slot, op.x, op.y);
  }

  private copySlot(slot: GlyphSlot<CanvasPage>, x: number, y: number): void {
    this.context.drawImage(
      slot.page.canvas,
      slot.x,
      slot.y,
      slot.width,
      slot.height,
      x,
      y,
      slot.width,
      slot.height
    );
  }

  /** Draws the cursor; the row to draw again when it moves, `null` if none was drawn. */
  private drawCursor(grid: TerminalGrid): number | null {
    const cursor = planCursor(
      grid,
      this.state.composition,
      this.state.isFocused,
      this.cell,
      lineWidthOf(this.dpr),
      this.palette.background
    );
    const context = this.context;
    context.fillStyle = this.palette.cursor;
    context.strokeStyle = this.palette.cursor;
    for (const paint of cursor.paints) {
      if (paint.kind === 'outline') {
        context.lineWidth = paint.lineWidth;
        context.strokeRect(paint.rect.x, paint.rect.y, paint.rect.w, paint.rect.h);
      } else {
        context.fillRect(paint.rect.x, paint.rect.y, paint.rect.w, paint.rect.h);
      }
    }
    for (const glyph of cursor.glyphs) this.drawCell(glyph, cursor.glyphRow);
    return cursor.drawnRow;
  }

  private drawShapes(
    shapes: readonly GlyphShape[],
    x: number,
    y: number,
    width: number,
    color: string
  ): void {
    const context = this.context;
    context.save();
    context.beginPath();
    context.rect(x, y, width, this.cell.height);
    context.clip();
    context.fillStyle = color;
    context.strokeStyle = color;
    for (const op of shapeOps(shapes, width, this.cell.height, lineWidthOf(this.dpr))) {
      switch (op.kind) {
        case 'fill':
          if (op.alpha !== null) context.globalAlpha = op.alpha;
          context.fillRect(x + op.rect.x, y + op.rect.y, op.rect.w, op.rect.h);
          if (op.alpha !== null) context.globalAlpha = 1;
          break;
        case 'stroke':
          context.lineWidth = op.lineWidth;
          context.beginPath();
          context.moveTo(x + op.from[0], y + op.from[1]);
          context.lineTo(x + op.to[0], y + op.to[1]);
          context.stroke();
          break;
        case 'curve':
          context.lineWidth = op.lineWidth;
          context.beginPath();
          context.moveTo(x + op.from[0], y + op.from[1]);
          context.quadraticCurveTo(
            x + op.control[0],
            y + op.control[1],
            x + op.to[0],
            y + op.to[1]
          );
          context.stroke();
          break;
      }
    }
    context.restore();
  }
}
