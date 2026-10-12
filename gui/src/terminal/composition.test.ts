import { describe, it, expect } from 'vitest';
import { layoutComposition } from './composition';

describe('the text being composed is laid out in the cursor row', () => {
  it('lays out nothing and keeps the cursor where it is for no text', () => {
    expect(layoutComposition({ row: 3, col: 5 }, '', 80)).toEqual({
      row: 3,
      cells: [],
      cursorCol: 5,
    });
  });

  it('lays out narrow characters one cell each from the cursor', () => {
    expect(layoutComposition({ row: 0, col: 2 }, 'ab', 80)).toEqual({
      row: 0,
      cells: [
        { col: 2, text: 'a', width: 1 },
        { col: 3, text: 'b', width: 1 },
      ],
      cursorCol: 4,
    });
  });

  it('lays out wide characters two cells each and puts the cursor after them', () => {
    expect(layoutComposition({ row: 1, col: 4 }, 'かな', 80)).toEqual({
      row: 1,
      cells: [
        { col: 4, text: 'か', width: 2 },
        { col: 6, text: 'な', width: 2 },
      ],
      cursorCol: 8,
    });
  });

  it('mixes narrow and wide characters', () => {
    expect(layoutComposition({ row: 2, col: 0 }, 'a漢b', 80)).toEqual({
      row: 2,
      cells: [
        { col: 0, text: 'a', width: 1 },
        { col: 1, text: '漢', width: 2 },
        { col: 3, text: 'b', width: 1 },
      ],
      cursorCol: 4,
    });
  });

  it('keeps a combining mark in the cell of its base character', () => {
    expect(layoutComposition({ row: 0, col: 0 }, 'éx', 80)).toEqual({
      row: 0,
      cells: [
        { col: 0, text: 'é', width: 1 },
        { col: 1, text: 'x', width: 1 },
      ],
      cursorCol: 2,
    });
  });

  it('lays out nothing past the end of the row', () => {
    expect(layoutComposition({ row: 0, col: 3 }, 'abcd', 5)).toEqual({
      row: 0,
      cells: [
        { col: 3, text: 'a', width: 1 },
        { col: 4, text: 'b', width: 1 },
      ],
      cursorCol: 4,
    });
  });

  it('treats fullwidth forms as wide', () => {
    expect(layoutComposition({ row: 0, col: 0 }, 'Ａ', 80)).toEqual({
      row: 0,
      cells: [{ col: 0, text: 'Ａ', width: 2 }],
      cursorCol: 2,
    });
  });
});
