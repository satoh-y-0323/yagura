import type { ActionReturn } from 'svelte/action';

export interface PointerDragDelta {
  dx: number;
  dy: number;
}

export interface PointerDragOptions<S> {
  /** Returns the state the drag carries, or null to leave the press alone. */
  start: (event: PointerEvent) => S | null;
  move: (state: S, delta: PointerDragDelta, event: PointerEvent) => void;
  end?: (state: S) => void;
}

interface ActiveDrag<S> {
  pointerId: number;
  originX: number;
  originY: number;
  state: S;
}

/**
 * Turns a left press on the node into a captured drag of that one pointer:
 * moves arrive as offsets from the press, and the drag ends once on release
 * or cancel.
 */
export function pointerDrag<S>(
  node: HTMLElement,
  options: PointerDragOptions<S>
): ActionReturn<PointerDragOptions<S>> {
  let current = options;
  let active: ActiveDrag<S> | null = null;

  const onDown = (event: PointerEvent): void => {
    if (event.button !== 0) {
      return;
    }
    const state = current.start(event);
    if (state === null) {
      return;
    }
    node.setPointerCapture(event.pointerId);
    active = {
      pointerId: event.pointerId,
      originX: event.clientX,
      originY: event.clientY,
      state,
    };
    event.preventDefault();
  };

  const onMove = (event: PointerEvent): void => {
    if (active === null || event.pointerId !== active.pointerId) {
      return;
    }
    current.move(
      active.state,
      { dx: event.clientX - active.originX, dy: event.clientY - active.originY },
      event
    );
  };

  const onEnd = (event: PointerEvent): void => {
    if (active === null || event.pointerId !== active.pointerId) {
      return;
    }
    const { state } = active;
    active = null;
    current.end?.(state);
  };

  node.addEventListener('pointerdown', onDown);
  node.addEventListener('pointermove', onMove);
  node.addEventListener('pointerup', onEnd);
  node.addEventListener('pointercancel', onEnd);

  return {
    update(next: PointerDragOptions<S>): void {
      current = next;
    },
    destroy(): void {
      node.removeEventListener('pointerdown', onDown);
      node.removeEventListener('pointermove', onMove);
      node.removeEventListener('pointerup', onEnd);
      node.removeEventListener('pointercancel', onEnd);
    },
  };
}
