/**
 * The `use:tooltip` action and the one tooltip the application draws. Triggers
 * publish into `activeTooltip`; `Tooltip.svelte` renders it, in the host that
 * `overlay-host` picks for the trigger. A tooltip only repeats what the trigger
 * already exposes by name or description, never carries information alone.
 * With `details` it is a hover card: the heading `text` and the lines below it.
 */
import { writable } from 'svelte/store';

import { isImeKey } from '../keymap/key-chord';
import { isKeyboardFocus, sharedInputModality } from './input-modality';
import { createTooltipController } from './tooltip-controller';

export interface TooltipOptions {
  text: string;
  /** Lines drawn under `text`; empty or absent for a plain tooltip. */
  details?: readonly string[];
  /** `toggle` is the press-to-open chip (a toggletip); `hover` is the default. */
  mode?: 'hover' | 'toggle';
  /** The trigger already exposes `text` as its name or description, so assistive tech skips the tooltip. */
  announcedElsewhere?: boolean;
}

/** What the tooltip component needs to draw the visible tooltip. */
export interface ActiveTooltip {
  owner: object;
  origin: HTMLElement;
  text: string;
  details: readonly string[];
  mode: 'hover' | 'toggle';
  announcedElsewhere: boolean;
  holdOpen: () => void;
  release: () => void;
}

export const activeTooltip = writable<ActiveTooltip | null>(null);

/** Class of the tooltip element; presses on it do not count as outside a toggletip. */
export const TOOLTIP_CLASS = 'tooltip';

export function tooltip(node: HTMLElement, initial: TooltipOptions | undefined) {
  let options = initial;
  let isVisible = false;
  const owner = {};
  // Started here, not on the first focus, so that the key that moves it is seen.
  sharedInputModality();

  const controller = createTooltipController({
    mode: initial?.mode ?? 'hover',
    onVisibleChange: (visible) => {
      isVisible = visible;
      if (visible) {
        publish();
        // eslint-disable-next-line no-restricted-syntax -- observes Escape only while shown; it dispatches no shortcut
        window.addEventListener('keydown', handleKeydown, true);
        window.addEventListener('pointerdown', handleWindowPointerDown, true);
      } else {
        retract();
      }
    },
  });

  function publish(): void {
    if (options === undefined || options.text === '') {
      // Nothing to say: a text that was cleared while shown must not linger.
      activeTooltip.update((current) => (current?.owner === owner ? null : current));
      return;
    }
    activeTooltip.set({
      owner,
      origin: node,
      text: options.text,
      details: options.details ?? [],
      mode: options.mode ?? 'hover',
      announcedElsewhere: options.announcedElsewhere ?? false,
      holdOpen: controller.pointerEnterTooltip,
      release: controller.pointerLeaveTooltip,
    });
  }

  function retract(): void {
    window.removeEventListener('keydown', handleKeydown, true);
    window.removeEventListener('pointerdown', handleWindowPointerDown, true);
    activeTooltip.update((current) => (current?.owner === owner ? null : current));
  }

  // Observes without consuming: an Escape meant for the page still reaches it.
  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && !isImeKey(event)) {
      controller.escape();
    }
  }

  function handleWindowPointerDown(event: PointerEvent): void {
    const target = event.target;
    if (
      target instanceof Element &&
      (node.contains(target) || target.closest(`.${TOOLTIP_CLASS}`) !== null)
    ) {
      return;
    }
    controller.pressOutside();
  }

  const isMouseLike = (event: PointerEvent): boolean => event.pointerType !== 'touch';

  function handlePointerEnter(event: PointerEvent): void {
    if (isMouseLike(event)) {
      controller.pointerEnterTrigger();
    }
  }

  function handlePointerLeave(event: PointerEvent): void {
    if (isMouseLike(event)) {
      controller.pointerLeaveTrigger();
    }
  }

  function handleFocus(): void {
    controller.focusTrigger(isKeyboardFocus(node));
  }

  function handleClick(): void {
    controller.toggle();
  }

  node.addEventListener('pointerenter', handlePointerEnter);
  node.addEventListener('pointerleave', handlePointerLeave);
  node.addEventListener('pointerdown', controller.pressTrigger);
  node.addEventListener('focus', handleFocus);
  node.addEventListener('blur', controller.blurTrigger);
  node.addEventListener('click', handleClick);

  return {
    update(next: TooltipOptions | undefined): void {
      options = next;
      controller.setMode(next?.mode ?? 'hover');
      if (isVisible) {
        publish();
      }
    },
    destroy(): void {
      controller.dispose();
      retract();
      node.removeEventListener('pointerenter', handlePointerEnter);
      node.removeEventListener('pointerleave', handlePointerLeave);
      node.removeEventListener('pointerdown', controller.pressTrigger);
      node.removeEventListener('focus', handleFocus);
      node.removeEventListener('blur', controller.blurTrigger);
      node.removeEventListener('click', handleClick);
    },
  };
}
