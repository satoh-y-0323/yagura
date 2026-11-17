/**
 * A modal that stays open while its subject changes (the approval queue
 * moving to the next request) must put focus back on the safe control, or an
 * Enter aimed at the new request lands on whatever the last one left focused.
 * The module is loaded inside each test so a missing module fails the tests
 * one by one rather than the whole file.
 */
import { describe, it, expect, vi } from 'vitest';

async function load() {
  return await import('./modal-refocus');
}

describe('shouldRefocus', () => {
  it.each([
    ['r1', 'r2', true, true],
    ['r1', 'r1', true, false],
    ['r1', 'r2', false, false],
    ['r1', null, true, false],
    [null, null, true, false],
    [null, 'r1', true, true],
  ] as const)('previous=%s next=%s open=%s gives %s', async (previous, next, isOpen, expected) => {
    const { shouldRefocus } = await load();

    expect(shouldRefocus(previous, next, isOpen)).toBe(expected);
  });
});

describe('focusAutofocusDelegate', () => {
  /** A root that answers only `querySelector`: autofocus selectors find `delegate`, anything else finds `other`. */
  function fakeRoot(hasDelegate: boolean) {
    const delegate = { focus: vi.fn() };
    const other = { focus: vi.fn() };
    const queried: string[] = [];
    const root = {
      querySelector(selector: string) {
        queried.push(selector);
        if (selector.includes('autofocus')) {
          return hasDelegate ? delegate : null;
        }
        return other;
      },
    };
    return { root, delegate, other, queried };
  }

  it('focuses the autofocus element exactly once', async () => {
    const { focusAutofocusDelegate } = await load();
    const { root, delegate, other } = fakeRoot(true);

    focusAutofocusDelegate(root);

    expect(delegate.focus).toHaveBeenCalledTimes(1);
    expect(other.focus).not.toHaveBeenCalled();
  });

  it('moves focus nowhere when no autofocus element is found', async () => {
    const { focusAutofocusDelegate } = await load();
    const { root, delegate, other } = fakeRoot(false);

    focusAutofocusDelegate(root);

    expect(delegate.focus).not.toHaveBeenCalled();
    expect(other.focus).not.toHaveBeenCalled();
  });
});
