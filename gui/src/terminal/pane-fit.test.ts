import { describe, it, expect } from 'vitest';
import { cellsThatFit, inputPlacement, type PaneBox } from './pane-fit';

const BOX: PaneBox = {
  isVisible: true,
  clientWidth: 820,
  clientHeight: 440,
  paddingLeft: '10px',
  paddingRight: '10px',
  paddingTop: '8px',
  paddingBottom: '12px',
};
const CELL = { cellWidth: 10, cellHeight: 20 };

describe('the cells that fit in a pane', () => {
  it('counts the whole cells inside the padding', () => {
    expect(cellsThatFit(BOX, CELL)).toEqual({ cols: 80, rows: 21 });
  });

  it('leaves out a partial cell', () => {
    expect(cellsThatFit({ ...BOX, clientWidth: 829, clientHeight: 459 }, CELL)).toEqual({
      cols: 80,
      rows: 21,
    });
    expect(cellsThatFit({ ...BOX, clientWidth: 819, clientHeight: 439 }, CELL)).toEqual({
      cols: 79,
      rows: 20,
    });
  });

  it('reads each side of the padding', () => {
    expect(cellsThatFit({ ...BOX, paddingLeft: '110px' }, CELL)?.cols).toBe(70);
    expect(cellsThatFit({ ...BOX, paddingRight: '110px' }, CELL)?.cols).toBe(70);
    expect(cellsThatFit({ ...BOX, paddingTop: '108px' }, CELL)?.rows).toBe(16);
    expect(cellsThatFit({ ...BOX, paddingBottom: '112px' }, CELL)?.rows).toBe(16);
  });

  it('has no size for a pane with no room', () => {
    expect(cellsThatFit({ ...BOX, clientWidth: 0, clientHeight: 0 }, CELL)).toBeNull();
    expect(cellsThatFit({ ...BOX, clientWidth: 20 }, CELL)).toBeNull();
    expect(cellsThatFit({ ...BOX, clientHeight: 20 }, CELL)).toBeNull();
  });

  it('has no size for a pane that is not shown, whatever room its box holds', () => {
    expect(cellsThatFit({ ...BOX, isVisible: false }, CELL)).toBeNull();
  });

  it('has no size where a padding did not parse', () => {
    expect(cellsThatFit({ ...BOX, paddingLeft: '' }, CELL)).toBeNull();
    expect(cellsThatFit({ ...BOX, paddingTop: 'auto' }, CELL)).toBeNull();
  });
});

describe('where the input textarea sits', () => {
  it('is on the cursor cell, the size of a cell', () => {
    expect(
      inputPlacement(
        { x: 40, y: 60 },
        { cellWidth: 9.5, cellHeight: 18, paddingLeft: 5, paddingTop: 7 }
      )
    ).toEqual({ left: '45px', top: '67px', width: '9.5px', height: '18px' });
  });
});
