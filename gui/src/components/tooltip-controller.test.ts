/**
 * The show/hide timing of a tooltip, and the press-to-toggle behavior of the
 * toggletip chip, driven with fake timers (no real waiting).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createTooltipController, type TooltipController } from './tooltip-controller';

interface Harness {
  controller: TooltipController;
  isVisible: () => boolean;
  shownCount: () => number;
}

function makeController(mode: 'hover' | 'toggle'): Harness {
  let visible = false;
  let shown = 0;
  const controller = createTooltipController({
    mode,
    onVisibleChange: (next) => {
      if (next && !visible) {
        shown += 1;
      }
      visible = next;
    },
  });
  return { controller, isVisible: () => visible, shownCount: () => shown };
}

const FAR_FUTURE_MS = 10_000;

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('tooltip controller: when it appears', () => {
  it('appears after the pointer has rested on the trigger for 500ms, and not before', () => {
    const { controller, isVisible } = makeController('hover');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(499);
    expect(isVisible()).toBe(false);

    vi.advanceTimersByTime(1);
    expect(isVisible()).toBe(true);
  });

  it('never appears when the pointer leaves the trigger before 500ms', () => {
    const { controller, isVisible, shownCount } = makeController('hover');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(499);
    controller.pointerLeaveTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
    expect(shownCount()).toBe(0);
  });

  it('appears at once, with no delay, for a :focus-visible focus', () => {
    const { controller, isVisible } = makeController('hover');

    controller.focusTrigger(true);

    expect(isVisible()).toBe(true);
  });

  it('does not appear for a focus that is not :focus-visible (a mouse press)', () => {
    const { controller, isVisible } = makeController('hover');

    controller.focusTrigger(false);
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
  });
});

describe('tooltip controller: when it goes away', () => {
  it.each([
    ['hover', (c: TooltipController) => c.pointerEnterTrigger(), 500],
    ['focus-visible', (c: TooltipController) => c.focusTrigger(true), 0],
  ])('Esc dismisses a tooltip shown by %s', (_how, show, delayMs) => {
    const { controller, isVisible } = makeController('hover');
    show(controller);
    vi.advanceTimersByTime(delayMs);
    expect(isVisible()).toBe(true);

    controller.escape();

    expect(isVisible()).toBe(false);
  });

  it('goes away when focus leaves the trigger', () => {
    const { controller, isVisible } = makeController('hover');
    controller.focusTrigger(true);
    expect(isVisible()).toBe(true);

    controller.blurTrigger();

    expect(isVisible()).toBe(false);
  });

  it('gets out of the way when the trigger is pressed, so it does not cover what the press opens', () => {
    const { controller, isVisible } = makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);
    expect(isVisible()).toBe(true);

    controller.pressTrigger();

    expect(isVisible()).toBe(false);
  });

  it('does not appear after dispose, however long the clock runs', () => {
    const { controller, isVisible, shownCount } = makeController('hover');
    controller.pointerEnterTrigger();

    controller.dispose();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
    expect(shownCount()).toBe(0);
  });

  it('stays visible while the pointer moves from the trigger onto the tooltip itself (WCAG 1.4.13)', () => {
    const { controller, isVisible } = makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);

    controller.pointerLeaveTrigger();
    controller.pointerEnterTooltip();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(true);
  });

  it('goes away once the pointer has left the trigger and never reached the tooltip', () => {
    const { controller, isVisible } = makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);

    controller.pointerLeaveTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
  });

  it('goes away once the pointer has left both the trigger and the tooltip', () => {
    const { controller, isVisible } = makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);
    controller.pointerLeaveTrigger();
    controller.pointerEnterTooltip();

    controller.pointerLeaveTooltip();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
  });
});

describe('toggletip controller: the ⓘ chip in the window header', () => {
  it('opens on a press and closes on the next press', () => {
    const { controller, isVisible } = makeController('toggle');

    controller.toggle();
    expect(isVisible()).toBe(true);

    controller.toggle();
    expect(isVisible()).toBe(false);
  });

  it('closes on Esc', () => {
    const { controller, isVisible } = makeController('toggle');
    controller.toggle();

    controller.escape();

    expect(isVisible()).toBe(false);
  });

  it('closes on a press outside the chip and its trigger', () => {
    const { controller, isVisible } = makeController('toggle');
    controller.toggle();

    controller.pressOutside();

    expect(isVisible()).toBe(false);
  });

  it('does not open from hover alone', () => {
    const { controller, isVisible, shownCount } = makeController('toggle');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
    expect(shownCount()).toBe(0);
  });
});

describe('tooltip controller: switching mode after creation', () => {
  it('hides what is shown and then behaves as a toggletip', () => {
    const { controller, isVisible } = makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);
    expect(isVisible()).toBe(true);

    controller.setMode('toggle');
    expect(isVisible()).toBe(false);

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);
    expect(isVisible()).toBe(false);

    controller.toggle();
    expect(isVisible()).toBe(true);
  });
});
