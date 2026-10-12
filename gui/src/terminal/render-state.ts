/**
 * What the renderer remembers between frames that changes how it draws, and
 * which rows that makes out of date: the focus (the selection and the cursor
 * change color with it) and the input method's text.
 */
import { compositionRows } from './draw-plan';
import type { CompositionLayout } from './composition';
import type { DirtyRows } from './redraw-scheduler';

export class RenderState {
  private focused = false;
  private composed: CompositionLayout | null = null;

  /** `invalidate` receives the rows a change leaves out of date. */
  constructor(private readonly invalidate: (rows: DirtyRows) => void) {}

  get isFocused(): boolean {
    return this.focused;
  }

  get composition(): CompositionLayout | null {
    return this.composed;
  }

  /** A focus that is the one already drawn leaves nothing out of date. */
  setFocused(isFocused: boolean): void {
    if (this.focused === isFocused) return;
    this.focused = isFocused;
    this.invalidate('all');
  }

  /** The row the text was in and the row it is in are drawn again. */
  setComposition(layout: CompositionLayout | null): void {
    const rows = compositionRows(this.composed, layout);
    this.composed = layout;
    this.invalidate(rows);
  }
}
