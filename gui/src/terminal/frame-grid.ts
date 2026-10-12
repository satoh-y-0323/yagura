/**
 * The grid a window's renderer draws: the frames the daemon sends, applied
 * without interpretation. Every cell, width, attribute, cursor and flag is
 * copied from the frame. A frame that breaks a rule of the wire is refused,
 * never repaired; the rules are those of the Rust reference applier
 * (`Grid::apply`, `yagura-test-support/src/frame.rs`).
 */
import type {
  CellAttr,
  ColorOverride,
  CursorState,
  Frame,
  FrameLine,
  ViewState,
} from '../ipc/session-boundary';

/** One cell. A wide character's second column is a spacer of width 0. */
export interface GridCell {
  /** The base character and its combining characters; empty for a spacer. */
  text: string;
  width: 0 | 1 | 2;
  attr: CellAttr;
}

/** One row: exactly `cols` cells. */
export interface GridLine {
  /** The line goes on in the next row. */
  wrapped: boolean;
  cells: GridCell[];
}

/** The state a renderer keeps between frames. */
export interface TerminalGrid {
  cols: number;
  rows: number;
  /** The rows, top to bottom. */
  lines: GridLine[];
  /** The cursor of the last applied part. */
  cursor: CursorState;
  /** The view state of the last applied part. */
  view: ViewState;
  /** The color overrides of the last applied part. */
  overrides: ColorOverride[];
  /** The frame whose last part has not arrived yet. */
  openSeq: number | null;
  /** The size the open frame's first part set. */
  openSize: { cols: number; rows: number };
}

/** A rule of the frame wire a part broke. */
export type FrameApplyErrorKind =
  | 'interleaved_frames'
  | 'reset_inside_frame'
  | 'size_changed_without_reset'
  | 'size_changed_inside_frame'
  | 'row_out_of_range'
  | 'attr_out_of_range'
  | 'wide_index_invalid'
  | 'row_overflow'
  | 'duplicate_row';

/**
 * The outcome of applying one part: `ack` is the frame number to acknowledge
 * when the part completed its frame, `null` otherwise. After an error the
 * grid is in no particular state.
 */
export type FrameApplyResult =
  { ok: true; ack: number | null } | { ok: false; error: FrameApplyErrorKind };

const DEFAULT_ATTR: CellAttr = { fg: 'default', bg: 'default', flags: [] };

function blankCell(): GridCell {
  return { text: ' ', width: 1, attr: DEFAULT_ATTR };
}

function blankLine(cols: number): GridLine {
  return { wrapped: false, cells: Array.from({ length: cols }, blankCell) };
}

/** An empty grid, which accepts only a `reset` frame. */
export function createGrid(): TerminalGrid {
  return {
    cols: 0,
    rows: 0,
    lines: [],
    cursor: { row: 0, col: 0, visible: false, shape: 'block' },
    view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
    overrides: [],
    openSeq: null,
    openSize: { cols: 0, rows: 0 },
  };
}

/** Whether any cell of the grid is selected. */
export function hasSelection(grid: Pick<TerminalGrid, 'lines'>): boolean {
  return grid.lines.some((line) => line.cells.some((cell) => cell.attr.flags.includes('selected')));
}

/** Applies one part of a frame to `grid` in place. */
export function applyFrame(grid: TerminalGrid, frame: Frame): FrameApplyResult {
  if (grid.openSeq !== null) {
    if (grid.openSeq !== frame.seq) return refuse('interleaved_frames');
    if (frame.reset) return refuse('reset_inside_frame');
    if (frame.cols !== grid.openSize.cols || frame.rows !== grid.openSize.rows) {
      return refuse('size_changed_inside_frame');
    }
  } else {
    if (frame.reset) {
      grid.cols = frame.cols;
      grid.rows = frame.rows;
      grid.lines = Array.from({ length: frame.rows }, () => blankLine(frame.cols));
    } else if (frame.cols !== grid.cols || frame.rows !== grid.rows || grid.rows === 0) {
      return refuse('size_changed_without_reset');
    }
    grid.openSize = { cols: frame.cols, rows: frame.rows };
  }

  const seen = new Set<number>();
  for (const line of frame.lines) {
    if (seen.has(line.row)) return refuse('duplicate_row');
    seen.add(line.row);
    const built = buildLine(grid, frame, line);
    if (typeof built === 'string') return refuse(built);
    grid.lines[line.row] = built;
  }

  grid.cursor = frame.cursor;
  grid.view = frame.view;
  grid.overrides = frame.overrides;

  if (frame.last) {
    grid.openSeq = null;
    return { ok: true, ack: frame.seq };
  }
  grid.openSeq = frame.seq;
  return { ok: true, ack: null };
}

function refuse(error: FrameApplyErrorKind): FrameApplyResult {
  return { ok: false, error };
}

function buildLine(
  grid: TerminalGrid,
  frame: Frame,
  line: FrameLine
): GridLine | FrameApplyErrorKind {
  if (line.row >= grid.rows) return 'row_out_of_range';
  const cells: GridCell[] = [];
  for (const run of line.runs) {
    const attr = frame.attrs[run.attr];
    if (attr === undefined) return 'attr_out_of_range';
    let lastWide = -1;
    for (const index of run.wide) {
      if (index >= run.cells.length || index <= lastWide) return 'wide_index_invalid';
      lastWide = index;
    }
    const wide = new Set(run.wide);
    run.cells.forEach((text, index) => {
      if (wide.has(index)) {
        cells.push({ text, width: 2, attr }, { text: '', width: 0, attr });
      } else {
        cells.push({ text, width: 1, attr });
      }
    });
  }
  if (cells.length > grid.cols) return 'row_overflow';
  while (cells.length < grid.cols) cells.push(blankCell());
  return { wrapped: line.wrapped, cells };
}
