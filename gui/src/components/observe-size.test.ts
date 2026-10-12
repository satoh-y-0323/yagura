/**
 * The size read-back action: it is the only writer of the element's width and
 * height. It writes the size it is handed on creation and on update, passes a
 * change the browser reports on as the element's border box, and afterwards
 * writes the size it holds back over whatever the native corner left behind.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { observeSize } from './observe-size';

type Size = { width: number; height: number };
type ObservedNode = Parameters<typeof observeSize>[0];
type ObserveOptions = Parameters<typeof observeSize>[1];

/** A ResizeObserver stand-in that records what it watches and fires on demand. */
class FakeResizeObserver {
  static instances: FakeResizeObserver[] = [];
  readonly observed = new Set<unknown>();
  constructor(private readonly callback: (entries: unknown[], observer: unknown) => void) {
    FakeResizeObserver.instances.push(this);
  }
  observe(target: unknown): void {
    this.observed.add(target);
  }
  unobserve(target: unknown): void {
    this.observed.delete(target);
  }
  disconnect(): void {
    this.observed.clear();
  }
  /** Reports a change for every watched target, as the browser would. */
  fire(contentRect: Size): void {
    const entries = [...this.observed].map((target) => ({ target, contentRect }));
    if (entries.length > 0) {
      this.callback(entries, this);
    }
  }
}

/** Hands a stand-in to the action whatever node type it declares. */
function asObservedNode(value: unknown): ObservedNode {
  return value as ObservedNode;
}

/** Hands `{ size, onResize }` to the action whatever options type it declares. */
function asOptions(value: { size: Size; onResize: (size: Size) => void }): ObserveOptions {
  return value;
}

interface FakeNode {
  node: ObservedNode;
  style: { width: string; height: string };
  /** Changes the border box the node reports, as the native corner does. */
  resize: (next: Size) => void;
}

/** A node whose border box can be changed between reports, with an inline style to write to. */
function fakeNode(initial: Size): FakeNode {
  let box = initial;
  const style = { width: '', height: '' };
  const raw = {
    style,
    getBoundingClientRect: () => ({
      width: box.width,
      height: box.height,
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      right: box.width,
      bottom: box.height,
    }),
  };
  return {
    node: asObservedNode(raw),
    style,
    resize: (next: Size): void => {
      box = next;
    },
  };
}

/** The native corner: writes the element's inline size and changes its border box. */
function dragCorner(target: FakeNode, next: Size): void {
  target.style.width = `${next.width}px`;
  target.style.height = `${next.height}px`;
  target.resize(next);
}

function watching(node: ObservedNode): FakeResizeObserver[] {
  return FakeResizeObserver.instances.filter((observer) => observer.observed.has(node));
}

function fireAll(contentRect: Size): void {
  for (const observer of FakeResizeObserver.instances) {
    observer.fire(contentRect);
  }
}

/** Lets microtasks, a frame and a macrotask pass, whichever the action waits for. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 30));
}

beforeEach(() => {
  FakeResizeObserver.instances = [];
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  vi.stubGlobal('requestAnimationFrame', (callback: (time: number) => void) =>
    setTimeout(() => {
      callback(0);
    }, 0)
  );
  vi.stubGlobal('cancelAnimationFrame', (handle: ReturnType<typeof setTimeout>) => {
    clearTimeout(handle);
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('observeSize', () => {
  it('watches the node it is given', () => {
    const { node } = fakeNode({ width: 10, height: 20 });
    observeSize(node, asOptions({ size: { width: 10, height: 20 }, onResize: () => {} }));
    expect(watching(node)).toHaveLength(1);
  });

  it('passes the border box on each change, not the content box the observer reports', () => {
    const { node, resize } = fakeNode({ width: 10, height: 20 });
    const received: Size[] = [];
    observeSize(
      node,
      asOptions({
        size: { width: 10, height: 20 },
        onResize: (size: Size) => {
          received.push(size);
        },
      })
    );

    resize({ width: 300.5, height: 200 });
    fireAll({ width: 1, height: 2 });
    resize({ width: 480, height: 320.25 });
    fireAll({ width: 3, height: 4 });

    expect(received).toEqual([
      { width: 300.5, height: 200 },
      { width: 480, height: 320.25 },
    ]);
  });

  it('sends later changes only to the callback given by update', () => {
    const { node, resize } = fakeNode({ width: 10, height: 20 });
    const first: Size[] = [];
    const second: Size[] = [];
    const action = observeSize(
      node,
      asOptions({
        size: { width: 10, height: 20 },
        onResize: (size: Size) => {
          first.push(size);
        },
      })
    );

    resize({ width: 100, height: 50 });
    fireAll({ width: 0, height: 0 });
    action.update?.(
      asOptions({
        size: { width: 10, height: 20 },
        onResize: (size: Size) => {
          second.push(size);
        },
      })
    );
    resize({ width: 200, height: 75 });
    fireAll({ width: 0, height: 0 });

    expect(first).toEqual([{ width: 100, height: 50 }]);
    expect(second).toEqual([{ width: 200, height: 75 }]);
  });

  it('stops watching the node on destroy', () => {
    const { node, resize } = fakeNode({ width: 10, height: 20 });
    const received: Size[] = [];
    const action = observeSize(
      node,
      asOptions({
        size: { width: 10, height: 20 },
        onResize: (size: Size) => {
          received.push(size);
        },
      })
    );

    action.destroy?.();
    resize({ width: 640, height: 480 });
    fireAll({ width: 0, height: 0 });

    expect(watching(node)).toHaveLength(0);
    expect(received).toEqual([]);
  });
});

describe('observeSize - the only writer of the element size', () => {
  it('writes the size it is handed to the element when created', () => {
    const target = fakeNode({ width: 0, height: 0 });
    observeSize(target.node, asOptions({ size: { width: 640, height: 520 }, onResize: () => {} }));
    expect(target.style).toEqual({ width: '640px', height: '520px' });
  });

  it('writes the new size to the element on update', () => {
    const target = fakeNode({ width: 640, height: 520 });
    const onResize = (): void => {};
    const action = observeSize(
      target.node,
      asOptions({ size: { width: 640, height: 520 }, onResize })
    );

    action.update?.(asOptions({ size: { width: 700.5, height: 480 }, onResize }));

    expect(target.style).toEqual({ width: '700.5px', height: '480px' });
  });

  // The shown size did not change (a drag past the limit rounds to the same
  // size again), so nothing hands the action a new size; the size the corner
  // wrote must still be replaced by the size shown.
  it('writes the size it holds back over a drag the parent answers without a new size', async () => {
    const target = fakeNode({ width: 640, height: 520 });
    const received: Size[] = [];
    observeSize(
      target.node,
      asOptions({
        size: { width: 640, height: 520 },
        onResize: (size: Size) => {
          received.push(size);
        },
      })
    );

    dragCorner(target, { width: 9000, height: 7000 });
    fireAll({ width: 0, height: 0 });
    await settle();

    expect(received).toEqual([{ width: 9000, height: 7000 }]);
    expect(target.style).toEqual({ width: '640px', height: '520px' });
  });

  it.each<[string, (apply: () => void) => void]>([
    ['from inside the callback', (apply) => apply()],
    ['in a microtask after it, as a component update does', (apply) => queueMicrotask(apply)],
  ])(
    'ends at the size the parent hands back %s, not at the size it held before the drag',
    async (_label, schedule) => {
      const target = fakeNode({ width: 640, height: 520 });
      const fitted = { width: 1200, height: 800 };
      const parent: { action?: ReturnType<typeof observeSize> } = {};
      const onResize = (): void => {
        schedule(() => {
          parent.action?.update?.(asOptions({ size: fitted, onResize }));
        });
      };
      parent.action = observeSize(
        target.node,
        asOptions({ size: { width: 640, height: 520 }, onResize })
      );

      dragCorner(target, { width: 9000, height: 7000 });
      fireAll({ width: 0, height: 0 });
      await settle();

      expect(target.style).toEqual({ width: '1200px', height: '800px' });
    }
  );
});
