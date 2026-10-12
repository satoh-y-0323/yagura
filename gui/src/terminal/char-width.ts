import widthTable from './char-widths.generated.json';

/**
 * How many cells a character takes in a window: 0, 1 or 2. The table is
 * written by the daemon's tests from the width table its terminal uses
 * (`crates/yagura-daemon/tests/gui_generated_tables.rs`), so the input
 * method's composition, which the GUI lays out itself, is as wide as the
 * cells the daemon gives the committed text.
 */

type Ranges = readonly (readonly number[])[];

function contains(ranges: Ranges, code: number): boolean {
  let low = 0;
  let high = ranges.length - 1;
  while (low <= high) {
    const middle = (low + high) >> 1;
    const range = ranges[middle];
    const start = range?.[0];
    const end = range?.[1];
    if (start === undefined || end === undefined) return false;
    if (code < start) {
      high = middle - 1;
    } else if (code > end) {
      low = middle + 1;
    } else {
      return true;
    }
  }
  return false;
}

/** The cells code point `code` takes. */
export function characterWidth(code: number): 0 | 1 | 2 {
  if (contains(widthTable.zero, code)) return 0;
  return contains(widthTable.wide, code) ? 2 : 1;
}
