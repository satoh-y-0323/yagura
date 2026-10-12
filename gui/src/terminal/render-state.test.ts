import { describe, it, expect } from 'vitest';
import type { CompositionLayout } from './composition';
import type { DirtyRows } from './redraw-scheduler';
import { RenderState } from './render-state';

function composing(row: number): CompositionLayout {
  return { row, cells: [{ col: 0, text: 'あ', width: 2 }], cursorCol: 2 };
}

function stateWithLog(): { state: RenderState; invalidated: DirtyRows[] } {
  const invalidated: DirtyRows[] = [];
  return {
    state: new RenderState((rows) => {
      invalidated.push(rows);
    }),
    invalidated,
  };
}

describe('the focus of a window', () => {
  it('draws everything again when it changes, as the selection and cursor change color', () => {
    const { state, invalidated } = stateWithLog();
    state.setFocused(true);
    expect(state.isFocused).toBe(true);
    state.setFocused(false);
    expect(state.isFocused).toBe(false);
    expect(invalidated).toEqual(['all', 'all']);
  });

  it('draws nothing again when it is the focus already drawn', () => {
    const { state, invalidated } = stateWithLog();
    state.setFocused(false);
    state.setFocused(true);
    state.setFocused(true);
    expect(invalidated).toEqual(['all']);
  });
});

describe('the input method text', () => {
  it('draws the row it moves from and the row it moves to', () => {
    const { state, invalidated } = stateWithLog();
    state.setComposition(composing(1));
    state.setComposition(composing(3));
    expect(invalidated).toEqual([[1], [1, 3]]);
    expect(state.composition).toEqual(composing(3));
  });

  it('draws its row again when it ends', () => {
    const { state, invalidated } = stateWithLog();
    state.setComposition(composing(2));
    state.setComposition(null);
    expect(invalidated[1]).toEqual([2]);
    expect(state.composition).toBeNull();
  });
});
