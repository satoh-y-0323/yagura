import { describe, it, expect } from 'vitest';
import { BoundaryError, narrowFrame } from '../ipc/session-boundary';

// A frame part as `next_frames` returns it: the frame's fields, with the fields
// serde skips when empty left out.
function payload(): Record<string, unknown> {
  return {
    seq: 7,
    cols: 4,
    rows: 2,
    reset: true,
    last: true,
    attrs: [
      { fg: 'default', bg: 'default' },
      { fg: { indexed: 1 }, bg: { rgb: [1, 2, 3] }, flags: ['bold'] },
    ],
    lines: [
      { row: 1, runs: [{ attr: 1, cells: ['全', 'a'], wide: [0] }], wrapped: true },
      { row: 0, runs: [] },
    ],
    cursor: { row: 1, col: 3, visible: true, shape: 'hollow_block' },
    view: { display_offset: 0, history_rows: 12, mouse_reporting: false, alt_screen: false },
  };
}

function rejects(mutate: (value: Record<string, unknown>) => void): void {
  const value = payload();
  mutate(value);
  expect(() => narrowFrame(value)).toThrow(BoundaryError);
}

function field(value: Record<string, unknown>, key: string): Record<string, unknown> {
  return value[key] as Record<string, unknown>;
}

function firstOf(value: Record<string, unknown>, key: string): Record<string, unknown> {
  return (value[key] as Record<string, unknown>[])[0] as Record<string, unknown>;
}

function firstRun(value: Record<string, unknown>): Record<string, unknown> {
  return firstOf(firstOf(value, 'lines'), 'runs');
}

describe('reading a frame payload at the IPC boundary', () => {
  it('reads a well-formed payload and fills the fields serde leaves out', () => {
    expect(narrowFrame(payload())).toEqual({
      seq: 7,
      cols: 4,
      rows: 2,
      reset: true,
      last: true,
      attrs: [
        { fg: 'default', bg: 'default', flags: [] },
        { fg: { indexed: 1 }, bg: { rgb: [1, 2, 3] }, flags: ['bold'] },
      ],
      lines: [
        { row: 1, wrapped: true, runs: [{ attr: 1, cells: ['全', 'a'], wide: [0] }] },
        { row: 0, wrapped: false, runs: [] },
      ],
      cursor: { row: 1, col: 3, visible: true, shape: 'hollow_block' },
      view: { display_offset: 0, history_rows: 12, mouse_reporting: false, alt_screen: false },
      overrides: [],
    });
  });

  it('defaults an attribute without colors to the theme defaults', () => {
    const value = payload();
    value['attrs'] = [{}];
    value['lines'] = [];
    expect(narrowFrame(value).attrs).toEqual([{ fg: 'default', bg: 'default', flags: [] }]);
  });

  it('reads the color overrides a program set', () => {
    const value = payload();
    value['overrides'] = [
      { slot: 'background', rgb: [1, 2, 3] },
      { slot: { indexed: 255 }, rgb: [0, 0, 255] },
      { slot: 'foreground', rgb: [9, 9, 9] },
      { slot: 'cursor', rgb: [8, 8, 8] },
    ];
    expect(narrowFrame(value).overrides).toEqual(value['overrides']);
  });

  it('accepts a cursor one past the last column, where a character waits to wrap', () => {
    const value = payload();
    field(value, 'cursor')['col'] = 4;
    expect(narrowFrame(value).cursor.col).toBe(4);
  });

  it('rejects a value that is not an object', () => {
    expect(() => narrowFrame(null)).toThrow(BoundaryError);
    expect(() => narrowFrame([payload()])).toThrow(BoundaryError);
    expect(() => narrowFrame('frame')).toThrow(BoundaryError);
  });

  it('rejects a payload missing a required field', () => {
    for (const key of [
      'seq',
      'cols',
      'rows',
      'reset',
      'last',
      'attrs',
      'lines',
      'cursor',
      'view',
    ]) {
      rejects((value) => {
        delete value[key];
      });
    }
  });

  it('rejects a field it does not own', () => {
    const value = payload();
    const cursor = value['cursor'];
    delete value['cursor'];
    const inherited = Object.assign(Object.create({ cursor }) as Record<string, unknown>, value);
    expect(() => narrowFrame(inherited)).toThrow(BoundaryError);
  });

  it('rejects fields of the wrong type', () => {
    rejects((value) => (value['seq'] = '7'));
    rejects((value) => (value['reset'] = 1));
    rejects((value) => (value['last'] = 'true'));
    rejects((value) => (value['attrs'] = {}));
    rejects((value) => (value['lines'] = {}));
    rejects((value) => (field(value, 'cursor')['visible'] = 'yes'));
    rejects((value) => (field(value, 'view')['alt_screen'] = 0));
    rejects((value) => (firstOf(value, 'lines')['wrapped'] = 'no'));
    rejects((value) => (firstRun(value)['cells'] = ['a', 1]));
    rejects((value) => (firstRun(value)['wide'] = ['0']));
  });

  it('rejects a frame number that is negative or not an integer', () => {
    rejects((value) => (value['seq'] = -1));
    rejects((value) => (value['seq'] = 1.5));
  });

  it('rejects a size outside what a window can have', () => {
    rejects((value) => (value['cols'] = 0));
    rejects((value) => (value['rows'] = 0));
    rejects((value) => (value['cols'] = 1001));
    rejects((value) => (value['rows'] = 1001));
  });

  it('rejects a line naming a row outside the frame', () => {
    rejects((value) => (firstOf(value, 'lines')['row'] = 2));
    rejects((value) => (firstOf(value, 'lines')['row'] = -1));
  });

  it('rejects a cursor outside the frame', () => {
    rejects((value) => (field(value, 'cursor')['row'] = 2));
    rejects((value) => (field(value, 'cursor')['col'] = 5));
    rejects((value) => (field(value, 'cursor')['row'] = -1));
  });

  it('rejects a cursor shape it does not know', () => {
    rejects((value) => (field(value, 'cursor')['shape'] = 'blinking_block'));
  });

  it('rejects a run attribute index or wide index that is negative', () => {
    rejects((value) => (firstRun(value)['attr'] = -1));
    rejects((value) => (firstRun(value)['wide'] = [-1]));
  });

  it('rejects a color it does not know', () => {
    rejects((value) => (firstOf(value, 'attrs')['fg'] = 'red'));
    rejects((value) => (firstOf(value, 'attrs')['fg'] = { indexed: 256 }));
    rejects((value) => (firstOf(value, 'attrs')['bg'] = { rgb: [1, 2] }));
    rejects((value) => (firstOf(value, 'attrs')['bg'] = { rgb: [1, 2, 256] }));
    rejects((value) => (firstOf(value, 'attrs')['bg'] = { rgb: [1, 2, -1] }));
  });

  it('rejects a cell flag it does not know', () => {
    rejects((value) => (firstOf(value, 'attrs')['flags'] = ['blink']));
  });

  it('rejects a view count that is negative', () => {
    rejects((value) => (field(value, 'view')['display_offset'] = -1));
    rejects((value) => (field(value, 'view')['history_rows'] = -1));
  });

  it('rejects an override of an unknown slot or color', () => {
    rejects((value) => (value['overrides'] = [{ slot: 'selection', rgb: [1, 2, 3] }]));
    rejects((value) => (value['overrides'] = [{ slot: { indexed: 256 }, rgb: [1, 2, 3] }]));
    rejects((value) => (value['overrides'] = [{ slot: 'background', rgb: [1, 2, 300] }]));
    rejects((value) => (value['overrides'] = {}));
  });
});
