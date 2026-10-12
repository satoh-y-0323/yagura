/**
 * A hidden window's container can measure 0px, and FitAddon's proposeDimensions()
 * then returns a valid-looking 2x1 candidate; applying it to terminal.resize()
 * permanently mangles scrollback line wrapping, so every call site gates on this.
 */

import { describe, it, expect } from 'vitest';
import { canApplyTerminalSize, MIN_REPORTABLE_COLS, MIN_REPORTABLE_ROWS } from './size-report';

describe('canApplyTerminalSize', () => {
  it('rejects a hidden window even with an ample candidate size', () => {
    expect(canApplyTerminalSize(false, { cols: 80, rows: 24 })).toBe(false);
  });

  it("rejects FitAddon's documented 2x1 minimum for a collapsed container", () => {
    expect(canApplyTerminalSize(true, { cols: 2, rows: 1 })).toBe(false);
  });

  it('rejects a candidate with cols below the frozen floor even when rows are sufficient', () => {
    expect(
      canApplyTerminalSize(true, { cols: MIN_REPORTABLE_COLS - 1, rows: MIN_REPORTABLE_ROWS })
    ).toBe(false);
  });

  it('rejects a candidate with rows below the frozen floor even when cols are sufficient', () => {
    expect(
      canApplyTerminalSize(true, { cols: MIN_REPORTABLE_COLS, rows: MIN_REPORTABLE_ROWS - 1 })
    ).toBe(false);
  });

  it('accepts a visible window whose candidate exactly meets the frozen floor', () => {
    expect(
      canApplyTerminalSize(true, { cols: MIN_REPORTABLE_COLS, rows: MIN_REPORTABLE_ROWS })
    ).toBe(true);
  });

  it('accepts a visible window sized like the minimum float window (320x180px, ~40 cols)', () => {
    expect(canApplyTerminalSize(true, { cols: 40, rows: 10 })).toBe(true);
  });

  it('rejects when no candidate size is available, even for a visible window (fail-safe)', () => {
    expect(canApplyTerminalSize(true, undefined)).toBe(false);
  });

  it('rejects when no candidate size is available for a hidden window', () => {
    expect(canApplyTerminalSize(false, undefined)).toBe(false);
  });
});
