/**
 * How a glyph whose picture is wider than the cells the window's model gives
 * it is drawn. The model's widths stay as they are, because the program in the
 * window places its cursor by them; only the drawing adapts.
 */

interface CellText {
  text: string;
  width: 0 | 1 | 2;
}

/**
 * The cells a glyph may draw over: a one-cell symbol (such as an emoji the
 * model counts as one cell) may spill into a blank cell on its right.
 */
export function glyphRoom(cell: CellText, next: CellText | undefined): 1 | 2 {
  if (cell.width === 2) return 2;
  const isAscii = [...cell.text].every((c) => (c.codePointAt(0) ?? 0) < 0x80);
  if (isAscii) return 1;
  const isNextBlank = next?.width === 1 && (next.text === ' ' || next.text === '');
  return isNextBlank ? 2 : 1;
}

/** The scale that fits a glyph `measured` wide into `room` (same units); never above 1. */
export function glyphScale(measured: number, room: number): number {
  if (!(measured > room)) return 1;
  return room / measured;
}
