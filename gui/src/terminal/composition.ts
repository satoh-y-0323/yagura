/**
 * Lays out the text an input method is composing in the cursor row, for the
 * renderer to draw underlined; the window receives it only once committed.
 */
import { characterWidth } from './char-width';

/** One character of the composition at its column, with the marks that follow it. */
export interface CompositionCell {
  col: number;
  text: string;
  width: 1 | 2;
}

/** The composition's cells and the column the cursor is drawn after them. */
export interface CompositionLayout {
  row: number;
  cells: CompositionCell[];
  cursorCol: number;
}

/**
 * Places `text` from the cursor in a row of `cols` columns. Each character
 * takes the cells the daemon gives it; a character that takes none joins the
 * cell before it. The cursor and the cells stay inside the row.
 */
export function layoutComposition(
  cursor: { row: number; col: number },
  text: string,
  cols: number
): CompositionLayout {
  const cells: CompositionCell[] = [];
  let col = cursor.col;
  for (const char of text) {
    const width = characterWidth(char.codePointAt(0) ?? 0);
    if (width === 0) {
      const last = cells[cells.length - 1];
      if (last !== undefined) last.text += char;
      continue;
    }
    if (col < cols) cells.push({ col, text: char, width });
    col += width;
  }
  return { row: cursor.row, cells, cursorCol: Math.min(col, Math.max(cols - 1, 0)) };
}
