import type { ActionReturn } from 'svelte/action';

/** Why a floating element asks to be closed without a key press. */
export type OutsideDismissReason = 'outside' | 'resize';

export interface DismissOnOutsideOptions {
  /** A press inside this element (the button that opens the node) is not outside. */
  keepOpenWithin: HTMLElement | undefined;
  onDismiss: (reason: OutsideDismissReason) => void;
}

/**
 * Svelte action for a floating element shown only while it is open: asks to
 * be closed on a press outside it and on a window resize, which would leave it
 * away from what it was opened next to. Keys stay with the element itself.
 */
export function dismissOnOutside(
  node: HTMLElement,
  options: DismissOnOutsideOptions
): ActionReturn<DismissOnOutsideOptions> {
  let current = options;

  function handlePointerDown(event: PointerEvent): void {
    const target = event.target as Node;
    if (current.keepOpenWithin?.contains(target) === true) {
      return;
    }
    // No preventDefault: the outside press is usually the start of another
    // gesture (a title-bar drag), which must still run.
    if (!node.contains(target)) {
      current.onDismiss('outside');
    }
  }

  function handleResize(): void {
    current.onDismiss('resize');
  }

  window.addEventListener('pointerdown', handlePointerDown);
  window.addEventListener('resize', handleResize);
  return {
    update(next: DismissOnOutsideOptions): void {
      current = next;
    },
    destroy(): void {
      window.removeEventListener('pointerdown', handlePointerDown);
      window.removeEventListener('resize', handleResize);
    },
  };
}
