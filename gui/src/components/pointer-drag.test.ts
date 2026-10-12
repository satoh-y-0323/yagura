/**
 * The press-and-move drag action: a left press the caller accepts captures the
 * pointer, the moves of that one pointer arrive as offsets from the press, and
 * the drag ends once on release or cancel.
 */
import { describe, expect, it } from 'vitest';

import { pointerDrag } from './pointer-drag';

type DragNode = Parameters<typeof pointerDrag>[0];
type DragOptions = Parameters<typeof pointerDrag>[1];

/** An element stand-in: dispatches like the node, and records pointer capture. */
class FakeNode extends EventTarget {
  readonly captured: number[] = [];
  setPointerCapture(pointerId: number): void {
    this.captured.push(pointerId);
  }
  releasePointerCapture(_pointerId: number): void {}
  hasPointerCapture(pointerId: number): boolean {
    return this.captured.includes(pointerId);
  }
}

interface PointerFields {
  button?: number;
  pointerId?: number;
  clientX?: number;
  clientY?: number;
}

/** A pointer event carrying only the fields the drag reads. */
function pointer(type: string, fields: PointerFields = {}): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.assign(event, { button: 0, pointerId: 1, clientX: 0, clientY: 0, ...fields });
  return event;
}

interface Recorder<S> {
  starts: number;
  moves: { state: S; delta: { dx: number; dy: number } }[];
  ends: S[];
  options: DragOptions;
}

/** Hands a stand-in to the action whatever option type it declares. */
function asOptions(value: unknown): DragOptions {
  return value as DragOptions;
}

/** Hands a stand-in to the action whatever node type it declares. */
function asNode(value: unknown): DragNode {
  return value as DragNode;
}

/** Options whose callbacks record every call; `start` answers with `state`. */
function recorder<S>(state: S | null): Recorder<S> {
  const calls: Omit<Recorder<S>, 'options'> = { starts: 0, moves: [], ends: [] };
  const options = asOptions({
    start: (): S | null => {
      calls.starts += 1;
      return state;
    },
    move: (current: S, delta: { dx: number; dy: number }): void => {
      calls.moves.push({ state: current, delta: { dx: delta.dx, dy: delta.dy } });
    },
    end: (current: S): void => {
      calls.ends.push(current);
    },
  });
  return Object.assign(calls, { options });
}

function setup<S>(state: S | null): {
  node: FakeNode;
  record: Recorder<S>;
  action: ReturnType<typeof pointerDrag>;
} {
  const node = new FakeNode();
  const record = recorder(state);
  const action = pointerDrag(asNode(node), record.options);
  return { node, record, action };
}

describe('pointerDrag', () => {
  it('captures the pointer and claims the press when start returns a state', () => {
    const { node, record } = setup({ id: 'drag' });
    const down = pointer('pointerdown', { pointerId: 7, clientX: 100, clientY: 50 });

    node.dispatchEvent(down);

    expect(record.starts).toBe(1);
    expect(node.captured).toEqual([7]);
    expect(down.defaultPrevented).toBe(true);
  });

  it.each([
    ['the middle button', 1],
    ['the right button', 2],
    ['the back button', 3],
  ])('ignores a press with %s', (_label, button) => {
    const { node, record } = setup({ id: 'drag' });
    const down = pointer('pointerdown', { button, pointerId: 3 });

    node.dispatchEvent(down);
    node.dispatchEvent(pointer('pointermove', { pointerId: 3, clientX: 40, clientY: 40 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 3 }));

    expect(record.starts).toBe(0);
    expect(node.captured).toEqual([]);
    expect(down.defaultPrevented).toBe(false);
    expect(record.moves).toEqual([]);
    expect(record.ends).toEqual([]);
  });

  it('neither captures nor claims the press when start returns null', () => {
    const { node, record } = setup<{ id: string }>(null);
    const down = pointer('pointerdown', { pointerId: 2 });

    node.dispatchEvent(down);
    node.dispatchEvent(pointer('pointermove', { pointerId: 2, clientX: 40, clientY: 40 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 2 }));

    expect(record.starts).toBe(1);
    expect(node.captured).toEqual([]);
    expect(down.defaultPrevented).toBe(false);
    expect(record.moves).toEqual([]);
    expect(record.ends).toEqual([]);
  });

  it('passes each move as the offset from the press, with the state start returned', () => {
    const state = { id: 'drag' };
    const { node, record } = setup(state);

    node.dispatchEvent(pointer('pointerdown', { pointerId: 4, clientX: 100, clientY: 50 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 4, clientX: 130, clientY: 40 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 4, clientX: 95, clientY: 72 }));

    expect(record.moves).toEqual([
      { state, delta: { dx: 30, dy: -10 } },
      { state, delta: { dx: -5, dy: 22 } },
    ]);
    expect(record.moves[0]?.state).toBe(state);
  });

  it('ignores moves and releases of another pointer', () => {
    const state = { id: 'drag' };
    const { node, record } = setup(state);

    node.dispatchEvent(pointer('pointerdown', { pointerId: 5, clientX: 10, clientY: 10 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 6, clientX: 90, clientY: 90 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 6 }));
    node.dispatchEvent(pointer('pointercancel', { pointerId: 6 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 5, clientX: 12, clientY: 13 }));

    expect(record.moves).toEqual([{ state, delta: { dx: 2, dy: 3 } }]);
    expect(record.ends).toEqual([]);
  });

  it.each(['pointerup', 'pointercancel'])('ends once on %s and delivers no later move', (type) => {
    const state = { id: 'drag' };
    const { node, record } = setup(state);

    node.dispatchEvent(pointer('pointerdown', { pointerId: 8, clientX: 0, clientY: 0 }));
    node.dispatchEvent(pointer(type, { pointerId: 8, clientX: 5, clientY: 5 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 8, clientX: 20, clientY: 20 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 8 }));
    node.dispatchEvent(pointer('pointercancel', { pointerId: 8 }));

    expect(record.ends).toHaveLength(1);
    expect(record.ends[0]).toBe(state);
    expect(record.moves).toEqual([]);
  });

  it('keeps the drag in progress across update and hands it to the new callbacks', () => {
    const state = { id: 'first' };
    const { node, record, action } = setup(state);
    const next = recorder({ id: 'second' });

    node.dispatchEvent(pointer('pointerdown', { pointerId: 9, clientX: 100, clientY: 100 }));
    action.update?.(next.options);
    node.dispatchEvent(pointer('pointermove', { pointerId: 9, clientX: 110, clientY: 90 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 9 }));

    expect(next.starts).toBe(0);
    expect(next.moves).toEqual([{ state, delta: { dx: 10, dy: -10 } }]);
    expect(next.moves[0]?.state).toBe(state);
    expect(next.ends).toHaveLength(1);
    expect(next.ends[0]).toBe(state);
    expect(record.moves).toEqual([]);
    expect(record.ends).toEqual([]);
  });

  it('starts the next drag with the callbacks given by update', () => {
    const { node, record, action } = setup({ id: 'first' });
    const second = { id: 'second' };
    const next = recorder(second);

    action.update?.(next.options);
    node.dispatchEvent(pointer('pointerdown', { pointerId: 10, clientX: 0, clientY: 0 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 10, clientX: 1, clientY: 2 }));

    expect(record.starts).toBe(0);
    expect(next.starts).toBe(1);
    expect(next.moves).toEqual([{ state: second, delta: { dx: 1, dy: 2 } }]);
  });

  it('stops listening on destroy, before and during a drag', () => {
    const during = setup({ id: 'during' });
    during.node.dispatchEvent(pointer('pointerdown', { pointerId: 11, clientX: 0, clientY: 0 }));
    during.action.destroy?.();
    during.node.dispatchEvent(pointer('pointermove', { pointerId: 11, clientX: 5, clientY: 5 }));
    during.node.dispatchEvent(pointer('pointerup', { pointerId: 11 }));
    during.node.dispatchEvent(pointer('pointercancel', { pointerId: 11 }));
    expect(during.record.moves).toEqual([]);
    expect(during.record.ends).toEqual([]);

    const before = setup({ id: 'before' });
    before.action.destroy?.();
    const down = pointer('pointerdown', { pointerId: 12 });
    before.node.dispatchEvent(down);
    expect(before.record.starts).toBe(0);
    expect(before.node.captured).toEqual([]);
    expect(down.defaultPrevented).toBe(false);
  });
});

describe('pointerDrag: how a drag ends', () => {
  it.each(['pointerup', 'pointercancel'])('ends once on %s and ignores moves after it', (type) => {
    const { node, record } = setup({ id: 'drag' });
    node.dispatchEvent(pointer('pointerdown', { pointerId: 4 }));

    node.dispatchEvent(pointer(type, { pointerId: 4 }));
    node.dispatchEvent(pointer('pointermove', { pointerId: 4, clientX: 9, clientY: 9 }));
    node.dispatchEvent(pointer(type, { pointerId: 4 }));

    expect(record.ends).toEqual([{ id: 'drag' }]);
    expect(record.moves).toEqual([]);
  });

  it('ends quietly when the callbacks have no end', () => {
    const node = new FakeNode();
    const moves: unknown[] = [];
    pointerDrag(
      asNode(node),
      asOptions({ start: () => ({ id: 'drag' }), move: (state: unknown) => moves.push(state) })
    );
    node.dispatchEvent(pointer('pointerdown', { pointerId: 4 }));

    expect(() => node.dispatchEvent(pointer('pointerup', { pointerId: 4 }))).not.toThrow();
    node.dispatchEvent(pointer('pointermove', { pointerId: 4, clientX: 9, clientY: 9 }));
    expect(moves).toEqual([]);
  });

  it('moves nothing before a press', () => {
    const { node, record } = setup({ id: 'drag' });

    node.dispatchEvent(pointer('pointermove', { pointerId: 1, clientX: 9, clientY: 9 }));
    node.dispatchEvent(pointer('pointerup', { pointerId: 1 }));

    expect(record.moves).toEqual([]);
    expect(record.ends).toEqual([]);
  });
});
