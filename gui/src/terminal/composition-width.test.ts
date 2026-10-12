import { describe, expect, it } from 'vitest';
import { layoutComposition } from './composition';

// How many cells the daemon's window gives each character is decided by its
// `unicode-width` table (Unicode 17 here); the expected advances below are what
// that table says, summed one character at a time because the window's model
// does not join characters into clusters.
const COLS = 200;

function advanceOf(text: string): number {
  return layoutComposition({ row: 0, col: 0 }, text, COLS).cursorCol;
}

describe('the cursor advances by the daemon width of each character', () => {
  it.each([
    ['narrow letters', 'ab', 2],
    ['kana', 'かな', 4],
    ['fullwidth letter', 'Ａ', 2],
    ['a combining mark takes no cell of its own', 'e\u0301', 1],
    ['a variation selector takes no cell', '\u2764\ufe0f', 1],
    ['a lone variation selector', '\ufe0f', 0],
    ['CJK extension B', '\u{20000}', 2],
    ['an emoji of Unicode 15', '\u{1fae8}', 2],
    ['an emoji of Unicode 12', '\u{1f971}', 2],
    [
      'a family of emoji joined by zero width joiners counts each character',
      '👨\u200d👩\u200d👧',
      6,
    ],
    ['an emoji with a skin tone counts both characters', '\u{1f9d1}\u{1f3fd}', 4],
    ['regional indicators are one cell each', '\u{1f1ef}\u{1f1f5}', 2],
    ['an emoji the daemon keeps narrow', '\u{1f321}', 1],
    ['a symbol Unicode 16 made wide', '\u2630', 2],
    ['a radical the daemon keeps narrow', '\u2e9a', 1],
    ['an enclosed ideograph the daemon keeps narrow', '\u{1f252}', 1],
  ])('%s', (_name, text, cells) => {
    expect(advanceOf(text)).toBe(cells);
  });

  it('starts from the cursor column', () => {
    expect(layoutComposition({ row: 4, col: 10 }, 'かa', COLS)).toMatchObject({
      row: 4,
      cursorCol: 13,
    });
  });
});

describe('the cursor stays inside the row', () => {
  it('stops at the last column when the text runs past the end of the row', () => {
    expect(layoutComposition({ row: 0, col: 78 }, 'かな', 80).cursorCol).toBe(79);
  });

  it('stops at the last column when the cursor is already there', () => {
    expect(layoutComposition({ row: 0, col: 79 }, 'a', 80).cursorCol).toBe(79);
  });

  it('is not moved when the text ends inside the row', () => {
    expect(layoutComposition({ row: 0, col: 10 }, 'abc', 80).cursorCol).toBe(13);
  });
});
