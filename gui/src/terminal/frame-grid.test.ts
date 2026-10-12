import { describe, it, expect } from 'vitest';
import sampleSource from '../../../crates/yagura-protocol/tests/fixtures/frame-sample.json?raw';
import { narrowFrame, type Frame } from '../ipc/session-boundary';
import { applyFrame, createGrid, hasSelection, type TerminalGrid } from './frame-grid';

interface SampleCell {
  row: number;
  col: number;
  text: string;
  width: number;
  fg: unknown;
  bg: unknown;
  flags: unknown;
}

interface SampleStep {
  name: string;
  messages: Record<string, unknown>[];
  expected: {
    cols: number;
    rows: number;
    screen_text: string[];
    wrapped_rows: number[];
    cursor: unknown;
    view: unknown;
    overrides: unknown;
    cells: SampleCell[];
  };
}

const SAMPLE = JSON.parse(sampleSource) as { steps: SampleStep[] };

// The Tauri core returns a frame as its own fields, without the daemon
// message's `type` tag.
function payloadOf(message: Record<string, unknown>): unknown {
  const fields: Record<string, unknown> = { ...message };
  delete fields['type'];
  return fields;
}

function lineAt(grid: TerminalGrid, row: number): TerminalGrid['lines'][number] {
  const found = grid.lines[row];
  if (found === undefined) throw new Error(`no row ${row}`);
  return found;
}

function cellAt(
  grid: TerminalGrid,
  row: number,
  col: number
): TerminalGrid['lines'][number]['cells'][number] {
  const found = lineAt(grid, row).cells[col];
  if (found === undefined) throw new Error(`no cell (${row},${col})`);
  return found;
}

function rowText(grid: TerminalGrid, row: number): string {
  return lineAt(grid, row)
    .cells.filter((cell) => cell.width !== 0)
    .map((cell) => cell.text)
    .join('')
    .replace(/ +$/, '');
}

const BLANK_VIEW = {
  display_offset: 0,
  history_rows: 0,
  mouse_reporting: false,
  alt_screen: false,
};
const CURSOR = { row: 0, col: 0, visible: true, shape: 'block' as const };
const PLAIN = { fg: 'default' as const, bg: 'default' as const, flags: [] };

function frame(fields: Partial<Frame>): Frame {
  return {
    seq: 1,
    cols: 4,
    rows: 2,
    reset: true,
    last: true,
    attrs: [PLAIN],
    lines: [],
    cursor: CURSOR,
    view: BLANK_VIEW,
    overrides: [],
    ...fields,
  };
}

function line(row: number, cells: string[], wide: number[] = []): Frame['lines'][number] {
  return { row, wrapped: false, runs: [{ attr: 0, cells, wide }] };
}

describe('applying the frame sample shared with the Rust reference applier', () => {
  it('leaves the grid every step of frame-sample.json expects', () => {
    const grid = createGrid();
    for (const step of SAMPLE.steps) {
      for (const message of step.messages) {
        const part = narrowFrame(payloadOf(message));
        const result = applyFrame(grid, part);
        expect(result.ok, step.name).toBe(true);
      }
      const expected = step.expected;
      expect(grid.cols, step.name).toBe(expected.cols);
      expect(grid.rows, step.name).toBe(expected.rows);
      expect(grid.lines.length, step.name).toBe(expected.rows);
      const screen = Array.from({ length: grid.rows }, (_, row) => rowText(grid, row));
      expect(screen, step.name).toEqual(expected.screen_text);
      const wrapped = grid.lines.flatMap((l, row) => (l.wrapped ? [row] : []));
      expect(wrapped, step.name).toEqual(expected.wrapped_rows);
      expect(grid.cursor, step.name).toEqual(expected.cursor);
      expect(grid.view, step.name).toEqual(expected.view);
      expect(grid.overrides, step.name).toEqual(expected.overrides);
      for (const cell of expected.cells) {
        const at = `${step.name}: cell (${cell.row},${cell.col})`;
        const got = cellAt(grid, cell.row, cell.col);
        expect(got.text, at).toBe(cell.text);
        expect(got.width, at).toBe(cell.width);
        expect(got.attr.fg, at).toEqual(cell.fg);
        expect(got.attr.bg, at).toEqual(cell.bg);
        expect(got.attr.flags, at).toEqual(cell.flags);
      }
    }
  });

  it('acknowledges a frame only on its last part, with that frame number', () => {
    const grid = createGrid();
    const acks: (number | null)[] = [];
    for (const step of SAMPLE.steps) {
      for (const message of step.messages) {
        const result = applyFrame(grid, narrowFrame(payloadOf(message)));
        acks.push(result.ok ? result.ack : null);
      }
    }
    // Step 1 is one frame in two parts, then a diff frame, then a reset frame.
    expect(acks).toEqual([null, 1, 2, 3]);
  });
});

describe('the rules of the frame wire', () => {
  it('starts with no rows and no open frame', () => {
    const grid = createGrid();
    expect(grid.rows).toBe(0);
    expect(grid.cols).toBe(0);
    expect(grid.lines).toEqual([]);
    expect(grid.openSeq).toBeNull();
    expect(grid.openSize).toEqual({ cols: 0, rows: 0 });
    expect(grid.cursor).toEqual({ row: 0, col: 0, visible: false, shape: 'block' });
    expect(grid.view).toEqual(BLANK_VIEW);
    expect(grid.overrides).toEqual([]);
  });

  it('places a wide cell and its spacer, and blanks the columns after the last run', () => {
    const grid = createGrid();
    expect(applyFrame(grid, frame({ lines: [line(0, ['全', 'a'], [0])] })).ok).toBe(true);
    expect(lineAt(grid, 0).cells.map((c) => [c.text, c.width])).toEqual([
      ['全', 2],
      ['', 0],
      ['a', 1],
      [' ', 1],
    ]);
    expect(cellAt(grid, 0, 3).attr).toEqual(PLAIN);
  });

  it('keeps the rows a diff frame does not name', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ lines: [line(0, ['a']), line(1, ['b'])] }));
    const result = applyFrame(grid, frame({ seq: 2, reset: false, lines: [line(1, ['c'])] }));
    expect(result).toEqual({ ok: true, ack: 2 });
    expect([rowText(grid, 0), rowText(grid, 1)]).toEqual(['a', 'c']);
  });

  it('blanks the rows a reset frame of the same size does not name', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ lines: [line(0, ['a']), line(1, ['b'])] }));
    applyFrame(grid, frame({ seq: 2, lines: [line(1, ['c'])] }));
    expect([rowText(grid, 0), rowText(grid, 1)]).toEqual(['', 'c']);
  });

  it('does not acknowledge a part that is not the last', () => {
    const grid = createGrid();
    expect(applyFrame(grid, frame({ last: false, lines: [line(0, ['a'])] }))).toEqual({
      ok: true,
      ack: null,
    });
    expect(applyFrame(grid, frame({ reset: false, lines: [line(1, ['b'])] }))).toEqual({
      ok: true,
      ack: 1,
    });
  });

  it('refuses a first frame that is not a reset', () => {
    expect(applyFrame(createGrid(), frame({ reset: false }))).toEqual({
      ok: false,
      error: 'size_changed_without_reset',
    });
  });

  it('refuses a diff frame of another size', () => {
    const grid = createGrid();
    applyFrame(grid, frame({}));
    expect(applyFrame(grid, frame({ seq: 2, reset: false, cols: 5 }))).toEqual({
      ok: false,
      error: 'size_changed_without_reset',
    });
  });

  it('refuses a diff frame that changes only the rows, or a first diff frame of no size', () => {
    const grid = createGrid();
    applyFrame(grid, frame({}));
    expect(applyFrame(grid, frame({ seq: 2, reset: false, rows: 3 }))).toEqual({
      ok: false,
      error: 'size_changed_without_reset',
    });
    expect(applyFrame(createGrid(), frame({ reset: false, cols: 0, rows: 0 }))).toEqual({
      ok: false,
      error: 'size_changed_without_reset',
    });
  });

  it('refuses a part of another frame while one is open', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ last: false }));
    expect(applyFrame(grid, frame({ seq: 2, reset: false }))).toEqual({
      ok: false,
      error: 'interleaved_frames',
    });
  });

  it('refuses a reset on a later part', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ last: false }));
    expect(applyFrame(grid, frame({ reset: true }))).toEqual({
      ok: false,
      error: 'reset_inside_frame',
    });
  });

  it('refuses a later part that changes the size', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ last: false }));
    expect(applyFrame(grid, frame({ reset: false, rows: 3 }))).toEqual({
      ok: false,
      error: 'size_changed_inside_frame',
    });
  });

  it('refuses a later part that changes only the columns', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ last: false }));
    expect(applyFrame(grid, frame({ reset: false, cols: 5 }))).toEqual({
      ok: false,
      error: 'size_changed_inside_frame',
    });
  });

  it('refuses a row past the grid', () => {
    expect(applyFrame(createGrid(), frame({ lines: [line(2, ['a'])] }))).toEqual({
      ok: false,
      error: 'row_out_of_range',
    });
  });

  it('refuses a run naming an attribute the part does not carry', () => {
    const lines = [{ row: 0, wrapped: false, runs: [{ attr: 1, cells: ['a'], wide: [] }] }];
    expect(applyFrame(createGrid(), frame({ lines }))).toEqual({
      ok: false,
      error: 'attr_out_of_range',
    });
  });

  it('refuses a wide index outside the run or out of order', () => {
    expect(applyFrame(createGrid(), frame({ lines: [line(0, ['a'], [1])] }))).toEqual({
      ok: false,
      error: 'wide_index_invalid',
    });
    expect(applyFrame(createGrid(), frame({ lines: [line(0, ['a', 'b'], [1, 0])] }))).toEqual({
      ok: false,
      error: 'wide_index_invalid',
    });
    expect(applyFrame(createGrid(), frame({ lines: [line(0, ['a', 'b'], [1, 1])] }))).toEqual({
      ok: false,
      error: 'wide_index_invalid',
    });
  });

  it('refuses a row whose cells are wider than the grid', () => {
    expect(
      applyFrame(createGrid(), frame({ lines: [line(0, ['a', 'b', 'c', '全'], [3])] }))
    ).toEqual({
      ok: false,
      error: 'row_overflow',
    });
  });

  it('accepts a row whose cells fill the grid exactly', () => {
    const grid = createGrid();
    expect(applyFrame(grid, frame({ lines: [line(0, ['a', 'b', 'c', 'd'])] })).ok).toBe(true);
    expect(rowText(grid, 0)).toBe('abcd');
  });

  it('refuses the same row twice in one part', () => {
    expect(applyFrame(createGrid(), frame({ lines: [line(0, ['a']), line(0, ['b'])] }))).toEqual({
      ok: false,
      error: 'duplicate_row',
    });
  });
});

describe('the selection in the grid', () => {
  it('is there when any cell is selected', () => {
    const grid = createGrid();
    applyFrame(grid, frame({ lines: [line(0, ['a']), line(1, ['b'])] }));
    expect(hasSelection(grid)).toBe(false);
    applyFrame(
      grid,
      frame({
        seq: 2,
        reset: false,
        attrs: [PLAIN, { ...PLAIN, flags: ['bold', 'selected'] }],
        lines: [{ row: 1, wrapped: false, runs: [{ attr: 1, cells: ['b'], wide: [] }] }],
      })
    );
    expect(hasSelection(grid)).toBe(true);
  });

  it('is not there in an empty grid', () => {
    expect(hasSelection(createGrid())).toBe(false);
  });
});
