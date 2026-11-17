/**
 * The show/hide timing of a tooltip, and the press-to-toggle behavior of the
 * toggletip chip, driven with fake timers (no real waiting).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

interface Controller {
  pointerEnterTrigger: () => void;
  pointerLeaveTrigger: () => void;
  pointerEnterTooltip: () => void;
  pointerLeaveTooltip: () => void;
  focusTrigger: (focusVisible: boolean) => void;
  escape: () => void;
  toggle: () => void;
  pressOutside: () => void;
}

interface Harness {
  controller: Controller;
  isVisible: () => boolean;
  shownCount: () => number;
}

type CreateController = (options: {
  mode?: 'hover' | 'toggle';
  onVisibleChange: (visible: boolean) => void;
}) => Controller;

async function makeController(mode: 'hover' | 'toggle'): Promise<Harness> {
  const module = (await import('./tooltip-controller')) as Record<string, unknown>;
  const create = module['createTooltipController'] as CreateController;
  let visible = false;
  let shown = 0;
  const controller = create({
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
  it('appears after the pointer has rested on the trigger for 500ms, and not before', async () => {
    const { controller, isVisible } = await makeController('hover');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(499);
    expect(isVisible()).toBe(false);

    vi.advanceTimersByTime(1);
    expect(isVisible()).toBe(true);
  });

  it('never appears when the pointer leaves the trigger before 500ms', async () => {
    const { controller, isVisible, shownCount } = await makeController('hover');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(499);
    controller.pointerLeaveTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
    expect(shownCount()).toBe(0);
  });

  it('appears at once, with no delay, for a :focus-visible focus', async () => {
    const { controller, isVisible } = await makeController('hover');

    controller.focusTrigger(true);

    expect(isVisible()).toBe(true);
  });

  it('does not appear for a focus that is not :focus-visible (a mouse press)', async () => {
    const { controller, isVisible } = await makeController('hover');

    controller.focusTrigger(false);
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
  });
});

describe('tooltip controller: when it goes away', () => {
  it.each([
    ['hover', (c: Controller) => c.pointerEnterTrigger(), 500],
    ['focus-visible', (c: Controller) => c.focusTrigger(true), 0],
  ])('Esc dismisses a tooltip shown by %s', async (_how, show, delayMs) => {
    const { controller, isVisible } = await makeController('hover');
    show(controller);
    vi.advanceTimersByTime(delayMs);
    expect(isVisible()).toBe(true);

    controller.escape();

    expect(isVisible()).toBe(false);
  });

  it('stays visible while the pointer moves from the trigger onto the tooltip itself (WCAG 1.4.13)', async () => {
    const { controller, isVisible } = await makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);

    controller.pointerLeaveTrigger();
    controller.pointerEnterTooltip();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(true);
  });

  it('goes away once the pointer has left the trigger and never reached the tooltip', async () => {
    const { controller, isVisible } = await makeController('hover');
    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(500);

    controller.pointerLeaveTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
  });

  it('goes away once the pointer has left both the trigger and the tooltip', async () => {
    const { controller, isVisible } = await makeController('hover');
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
  it('opens on a press and closes on the next press', async () => {
    const { controller, isVisible } = await makeController('toggle');

    controller.toggle();
    expect(isVisible()).toBe(true);

    controller.toggle();
    expect(isVisible()).toBe(false);
  });

  it('closes on Esc', async () => {
    const { controller, isVisible } = await makeController('toggle');
    controller.toggle();

    controller.escape();

    expect(isVisible()).toBe(false);
  });

  it('closes on a press outside the chip and its trigger', async () => {
    const { controller, isVisible } = await makeController('toggle');
    controller.toggle();

    controller.pressOutside();

    expect(isVisible()).toBe(false);
  });

  it('does not open from hover alone', async () => {
    const { controller, isVisible, shownCount } = await makeController('toggle');

    controller.pointerEnterTrigger();
    vi.advanceTimersByTime(FAR_FUTURE_MS);

    expect(isVisible()).toBe(false);
    expect(shownCount()).toBe(0);
  });
});
