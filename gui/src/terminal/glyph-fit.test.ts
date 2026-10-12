import { describe, it, expect } from 'vitest';
import { glyphRoom, glyphScale } from './glyph-fit';

const narrow = (text: string) => ({ text, width: 1 as const });
const wide = (text: string) => ({ text, width: 2 as const });

describe('the room a glyph is drawn in', () => {
  it('lets a one-cell symbol spill into a blank cell on its right', () => {
    expect(glyphRoom(narrow('⚠️'), narrow(' '))).toBe(2);
    expect(glyphRoom(narrow('⚠️'), narrow(''))).toBe(2);
  });

  it('keeps a one-cell symbol in its cell when text follows it', () => {
    expect(glyphRoom(narrow('⚠️'), narrow('x'))).toBe(1);
  });

  it('keeps a one-cell symbol in its cell at the end of the line', () => {
    expect(glyphRoom(narrow('⚠️'), undefined)).toBe(1);
  });

  it('keeps a one-cell symbol in its cell when a wide character follows', () => {
    expect(glyphRoom(narrow('⚠️'), wide('日'))).toBe(1);
  });

  it('never widens ASCII, which a monospace font draws within one cell', () => {
    expect(glyphRoom(narrow('W'), narrow(' '))).toBe(1);
  });

  it('keeps a wide character in its two cells', () => {
    expect(glyphRoom(wide('\u{1f600}'), narrow(' '))).toBe(2);
    expect(glyphRoom(wide('日'), narrow('x'))).toBe(2);
    expect(glyphRoom(wide('日'), undefined)).toBe(2);
  });

  it('widens a symbol that only starts with an ASCII character', () => {
    expect(glyphRoom(narrow('1\uFE0F\u20E3'), narrow(' '))).toBe(2);
  });

  it('counts only code points below U+0080 as ASCII', () => {
    expect(glyphRoom(narrow('\u007F'), narrow(' '))).toBe(1);
    expect(glyphRoom(narrow('\u0080'), narrow(' '))).toBe(2);
  });
});

describe('the scale a glyph is drawn at', () => {
  it('draws a glyph that fits at its own size', () => {
    expect(glyphScale(9, 10)).toBe(1);
    expect(glyphScale(10, 10)).toBe(1);
  });

  it('shrinks a glyph wider than its room to the room', () => {
    expect(glyphScale(20, 10)).toBe(0.5);
  });

  it('draws a glyph with no measured width at its own size', () => {
    expect(glyphScale(0, 10)).toBe(1);
  });
});
