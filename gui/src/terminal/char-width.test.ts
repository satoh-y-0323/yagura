import { describe, it, expect } from 'vitest';
import widthTable from './char-widths.generated.json';
import { characterWidth } from './char-width';

describe('how many cells a character takes', () => {
  it('gives a narrow character one cell and a wide one two', () => {
    expect(characterWidth(0x61)).toBe(1);
    expect(characterWidth(0x6f22)).toBe(2);
    expect(characterWidth(0x10ffff)).toBe(1);
  });

  it('gives a combining mark no cell', () => {
    expect(characterWidth(0x0301)).toBe(0);
  });

  it('answers every range of the table from its first to its last code point', () => {
    for (const [start, end] of widthTable.zero) {
      expect(characterWidth(start as number)).toBe(0);
      expect(characterWidth(end as number)).toBe(0);
    }
    for (const [start, end] of widthTable.wide) {
      expect(characterWidth(start as number)).toBe(2);
      expect(characterWidth(end as number)).toBe(2);
    }
  });
});
