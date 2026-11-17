/**
 * When a tooltip (or a toggletip) is visible. Pure timing and state: the caller
 * wires DOM events to the methods and draws what `onVisibleChange` reports.
 */

/** How long the pointer must rest on the trigger before a tooltip appears. */
export const TOOLTIP_SHOW_DELAY_MS = 500;
/** Grace period for the pointer to travel from the trigger onto the tooltip (WCAG 1.4.13). */
export const TOOLTIP_HIDE_DELAY_MS = 150;

// Function-valued properties, not methods, so a caller can pass one as an event listener.
export interface TooltipController {
  pointerEnterTrigger: () => void;
  pointerLeaveTrigger: () => void;
  pointerEnterTooltip: () => void;
  pointerLeaveTooltip: () => void;
  /** A focus on the trigger; only a `:focus-visible` focus shows the tooltip. */
  focusTrigger: (focusVisible: boolean) => void;
  blurTrigger: () => void;
  /** A press on the trigger itself: its action is under way, so the tooltip gets out of the way. */
  pressTrigger: () => void;
  escape: () => void;
  /** Opens a closed toggletip and closes an open one. */
  toggle: () => void;
  pressOutside: () => void;
  /** Cancels pending timers. */
  dispose: () => void;
}

export interface TooltipControllerOptions {
  /** `hover` shows on pointer rest and focus; `toggle` shows only on `toggle()`. */
  mode?: 'hover' | 'toggle';
  onVisibleChange: (visible: boolean) => void;
}

export function createTooltipController(options: TooltipControllerOptions): TooltipController {
  const mode = options.mode ?? 'hover';
  let visible = false;
  let overTrigger = false;
  let overTooltip = false;
  let focused = false;
  let showTimer: ReturnType<typeof setTimeout> | null = null;
  let hideTimer: ReturnType<typeof setTimeout> | null = null;

  function clearTimers(): void {
    if (showTimer !== null) {
      clearTimeout(showTimer);
      showTimer = null;
    }
    if (hideTimer !== null) {
      clearTimeout(hideTimer);
      hideTimer = null;
    }
  }

  function setVisible(next: boolean): void {
    if (next === visible) {
      return;
    }
    visible = next;
    options.onVisibleChange(next);
  }

  function hideNow(): void {
    clearTimers();
    setVisible(false);
  }

  /** Hides after a grace period unless the pointer or focus is still on either element. */
  function scheduleHideIfIdle(): void {
    if (!visible || overTrigger || overTooltip || focused) {
      return;
    }
    if (hideTimer !== null) {
      clearTimeout(hideTimer);
    }
    hideTimer = setTimeout(() => {
      hideTimer = null;
      if (!overTrigger && !overTooltip && !focused) {
        setVisible(false);
      }
    }, TOOLTIP_HIDE_DELAY_MS);
  }

  function cancelHide(): void {
    if (hideTimer !== null) {
      clearTimeout(hideTimer);
      hideTimer = null;
    }
  }

  const hoverOnly = mode === 'hover';

  return {
    pointerEnterTrigger(): void {
      if (!hoverOnly) {
        return;
      }
      overTrigger = true;
      cancelHide();
      if (!visible && showTimer === null) {
        showTimer = setTimeout(() => {
          showTimer = null;
          setVisible(true);
        }, TOOLTIP_SHOW_DELAY_MS);
      }
    },
    pointerLeaveTrigger(): void {
      if (!hoverOnly) {
        return;
      }
      overTrigger = false;
      if (showTimer !== null) {
        clearTimeout(showTimer);
        showTimer = null;
      }
      scheduleHideIfIdle();
    },
    pointerEnterTooltip(): void {
      if (!hoverOnly) {
        return;
      }
      overTooltip = true;
      cancelHide();
    },
    pointerLeaveTooltip(): void {
      if (!hoverOnly) {
        return;
      }
      overTooltip = false;
      scheduleHideIfIdle();
    },
    focusTrigger(focusVisible: boolean): void {
      if (!hoverOnly || !focusVisible) {
        return;
      }
      focused = true;
      cancelHide();
      if (showTimer !== null) {
        clearTimeout(showTimer);
        showTimer = null;
      }
      setVisible(true);
    },
    blurTrigger(): void {
      focused = false;
      if (hoverOnly && !overTrigger && !overTooltip) {
        hideNow();
      }
    },
    pressTrigger(): void {
      if (hoverOnly) {
        hideNow();
      }
    },
    escape(): void {
      hideNow();
    },
    toggle(): void {
      if (mode !== 'toggle') {
        return;
      }
      setVisible(!visible);
    },
    pressOutside(): void {
      hideNow();
    },
    dispose(): void {
      clearTimers();
    },
  };
}
