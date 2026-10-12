/**
 * What `narrowFrame` keeps from every closed vocabulary of a frame part, and
 * which part of the frame its refusal names. Complements
 * `terminal/frame-boundary.test.ts`, which covers the shape of a frame.
 */
import { describe, it, expect } from 'vitest';

import { BoundaryError, narrowFrame, narrowSelectionText } from './session-boundary';

type Wire = Record<string, unknown>;

const CTX = 'next_frames.frame';

function frameWire(overrides: Wire = {}): Wire {
  return {
    seq: 1,
    cols: 4,
    rows: 2,
    reset: false,
    last: true,
    attrs: [{ fg: 'default', bg: 'default' }],
    lines: [{ row: 0, runs: [{ attr: 0, cells: ['a'] }] }],
    cursor: { row: 0, col: 0, visible: true, shape: 'block' },
    view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
    ...overrides,
  };
}

function withAttr(attr: Wire): Wire {
  return frameWire({ attrs: [attr] });
}

function withRun(run: Wire): Wire {
  return frameWire({ lines: [{ row: 0, runs: [run] }] });
}

function refusal(wire: Wire): string {
  try {
    narrowFrame(wire);
  } catch (error) {
    expect(error).toBeInstanceOf(BoundaryError);
    return (error as Error).message;
  }
  throw new Error('the frame was accepted');
}

describe('the closed vocabularies of a frame part', () => {
  it.each(['bold', 'dim', 'italic', 'underline', 'strikeout', 'inverse', 'hidden', 'selected'])(
    'keeps the cell flag %s',
    (flag) => {
      const frame = narrowFrame(withAttr({ flags: [flag] }));
      expect(frame.attrs[0]?.flags).toEqual([flag]);
    }
  );

  it.each(['block', 'underline', 'bar', 'hollow_block'])('keeps the cursor shape %s', (shape) => {
    const frame = narrowFrame(frameWire({ cursor: { row: 0, col: 0, visible: false, shape } }));
    expect(frame.cursor.shape).toBe(shape);
  });

  it.each(['foreground', 'background', 'cursor'])('keeps the named palette slot %s', (slot) => {
    const frame = narrowFrame(frameWire({ overrides: [{ slot, rgb: [1, 2, 3] }] }));
    expect(frame.overrides).toEqual([{ slot, rgb: [1, 2, 3] }]);
  });

  it('keeps an indexed palette slot and the extreme color components', () => {
    const frame = narrowFrame(
      frameWire({ overrides: [{ slot: { indexed: 255 }, rgb: [0, 255, 0] }] })
    );
    expect(frame.overrides).toEqual([{ slot: { indexed: 255 }, rgb: [0, 255, 0] }]);
  });

  it('reads a run without wide indices as having none', () => {
    expect(narrowFrame(withRun({ attr: 0, cells: ['a'] })).lines[0]?.runs[0]?.wide).toEqual([]);
  });

  it('reads an attribute without flags as having none, and a frame without overrides likewise', () => {
    const frame = narrowFrame(withAttr({ fg: { indexed: 3 } }));
    expect(frame.attrs[0]).toEqual({ fg: { indexed: 3 }, bg: 'default', flags: [] });
    expect(frame.overrides).toEqual([]);
  });
});

describe('a frame refusal names the part that was wrong', () => {
  it.each<[string, Wire, RegExp]>([
    [
      'a window size of zero',
      frameWire({ cols: 0 }),
      /next_frames\.frame: 0x2 is not a window size/,
    ],
    ['attrs not an array', frameWire({ attrs: 5 }), /next_frames\.frame\.attrs: expected an array/],
    ['an attr not an object', frameWire({ attrs: [1] }), /\.attrs\[0\]: expected an object/],
    ['lines not an array', frameWire({ lines: 5 }), /next_frames\.frame\.lines: expected an array/],
    ['a line not an object', frameWire({ lines: [1] }), /\.lines\[0\]: expected an object/],
    ['a cursor not an object', frameWire({ cursor: 5 }), /\.cursor: expected an object/],
    ['a view not an object', frameWire({ view: 5 }), /\.view: expected an object/],
    ['overrides not an array', frameWire({ overrides: 5 }), /\.overrides: expected an array/],
    [
      'an override not an object',
      frameWire({ overrides: [1] }),
      /\.overrides\[0\]: expected an object/,
    ],
    ['flags not an array', withAttr({ flags: 5 }), /\.attrs\[0\]\.flags: expected an array/],
    [
      'an unknown flag',
      withAttr({ flags: ['sparkle'] }),
      /\.attrs\[0\]\.flags: unrecognized flag "sparkle"/,
    ],
    ['an fg not an object', withAttr({ fg: 5 }), /\.attrs\[0\]\.fg: expected an object/],
    ['a bg not an object', withAttr({ bg: 5 }), /\.attrs\[0\]\.bg: expected an object/],
    ['an rgb not an array', withAttr({ bg: { rgb: 5 } }), /\.bg\.rgb: expected an array/],
    [
      'an rgb of two components',
      withAttr({ bg: { rgb: [1, 2] } }),
      /\.bg\.rgb: expected three components/,
    ],
    [
      'a string color component',
      withAttr({ bg: { rgb: [1, '2', 3] } }),
      /\.bg\.rgb: expected a color component/,
    ],
    [
      'a fractional color component',
      withAttr({ bg: { rgb: [1, 1.5, 3] } }),
      /\.bg\.rgb: expected a color component/,
    ],
    [
      'a negative color component',
      withAttr({ bg: { rgb: [1, -1, 3] } }),
      /\.bg\.rgb: expected a color component/,
    ],
    ['a component past 255', withAttr({ bg: { rgb: [1, 256, 3] } }), /\.bg\.rgb: 256 is past 255/],
    [
      'an indexed color past 255',
      withAttr({ fg: { indexed: 256 } }),
      /\.fg\.indexed: 256 is past 255/,
    ],
    [
      'a line outside the rows',
      frameWire({ lines: [{ row: 2, runs: [] }] }),
      /\.lines\[0\]\.row: 2 is outside 2 rows/,
    ],
    [
      'runs not an array',
      frameWire({ lines: [{ row: 0, runs: 5 }] }),
      /\.lines\[0\]\.runs: expected/,
    ],
    [
      'a run not an object',
      frameWire({ lines: [{ row: 0, runs: [1] }] }),
      /\.runs\[0\]: expected an object/,
    ],
    [
      'wide not an array',
      withRun({ attr: 0, cells: [], wide: 5 }),
      /\.runs\[0\]\.wide: expected an array/,
    ],
    [
      'a negative wide index',
      withRun({ attr: 0, cells: [], wide: [-1] }),
      /\.wide: expected non-negative integers/,
    ],
    [
      'a fractional wide index',
      withRun({ attr: 0, cells: [], wide: [0.5] }),
      /\.wide: expected non-negative integers/,
    ],
    [
      'a string wide index',
      withRun({ attr: 0, cells: [], wide: ['0'] }),
      /\.wide: expected non-negative integers/,
    ],
    [
      'a cursor outside the frame',
      frameWire({ cursor: { row: 2, col: 0, visible: true, shape: 'block' } }),
      /\.cursor: \(2, 0\) is outside 4x2/,
    ],
    [
      'an unknown cursor shape',
      frameWire({ cursor: { row: 0, col: 0, visible: true, shape: 'star' } }),
      /\.cursor\.shape: unrecognized shape "star"/,
    ],
    [
      'an unknown named slot',
      frameWire({ overrides: [{ slot: 'sky', rgb: [1, 2, 3] }] }),
      /\.overrides\[0\]\.slot: unrecognized slot "sky"/,
    ],
    [
      'a slot that is neither a name nor an object',
      frameWire({ overrides: [{ slot: 5, rgb: [1, 2, 3] }] }),
      /\.overrides\[0\]\.slot: expected an object/,
    ],
    [
      'an override rgb of the wrong length',
      frameWire({ overrides: [{ slot: 'cursor', rgb: [1] }] }),
      /\.overrides\[0\]\.rgb: expected three components/,
    ],
    [
      'an indexed slot past 255',
      frameWire({ overrides: [{ slot: { indexed: 300 }, rgb: [1, 2, 3] }] }),
      /\.overrides\[0\]\.slot\.indexed: 300 is past 255/,
    ],
  ])('%s', (_name, wire, pattern) => {
    expect(refusal(wire)).toMatch(pattern);
  });

  it('names the frame when the value is not an object', () => {
    expect(() => narrowFrame(null)).toThrow(`${CTX}: expected an object`);
  });
});

describe('narrowSelectionText', () => {
  it('keeps the selected text and an empty selection as they are', () => {
    expect(narrowSelectionText('abc')).toBe('abc');
    expect(narrowSelectionText('')).toBe('');
  });

  it('reads no selection as null', () => {
    expect(narrowSelectionText(null)).toBeNull();
  });

  it.each([undefined, 3, {}, ['a']])('refuses %j', (value) => {
    expect(() => narrowSelectionText(value)).toThrow(
      'copy_window_selection: expected a string or null'
    );
  });
});
