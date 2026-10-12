/**
 * `appHidden` follows the main window being minimized and the document being hidden, and stops
 * following once nobody listens.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

const mainWindow = vi.hoisted(() => ({
  isMinimized: vi.fn<() => Promise<boolean>>(),
  onResized: vi.fn<(handler: () => void) => Promise<() => void>>(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => mainWindow,
}));

import { appHidden } from './app-visibility';

interface FakeDocument {
  visibilityState: 'visible' | 'hidden';
  listeners: Map<string, Set<() => void>>;
  addEventListener: (type: string, handler: () => void) => void;
  removeEventListener: (type: string, handler: () => void) => void;
  fire: (type: string) => void;
}

function fakeDocument(): FakeDocument {
  const listeners = new Map<string, Set<() => void>>();
  return {
    visibilityState: 'visible',
    listeners,
    addEventListener: (type, handler) => {
      const set = listeners.get(type) ?? new Set();
      set.add(handler);
      listeners.set(type, set);
    },
    removeEventListener: (type, handler) => {
      listeners.get(type)?.delete(handler);
    },
    fire: (type) => {
      for (const handler of listeners.get(type) ?? []) handler();
    },
  };
}

async function settle(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

let doc: FakeDocument;
let unlisten: ReturnType<typeof vi.fn<() => void>>;
let resized: () => void;

beforeEach(() => {
  doc = fakeDocument();
  vi.stubGlobal('document', doc);
  unlisten = vi.fn<() => void>();
  resized = () => undefined;
  mainWindow.isMinimized.mockReset().mockResolvedValue(false);
  mainWindow.onResized.mockReset().mockImplementation((handler) => {
    resized = handler;
    return Promise.resolve(unlisten);
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('appHidden', () => {
  it('is false while the app is shown, even before the minimized state is read', () => {
    const stop = appHidden.subscribe(() => undefined);

    expect(get(appHidden)).toBe(false);
    stop();
  });

  it('becomes true once the main window is found minimized', async () => {
    mainWindow.isMinimized.mockResolvedValue(true);
    const seen: boolean[] = [];

    const stop = appHidden.subscribe((hidden) => seen.push(hidden));
    await settle();

    expect(seen[seen.length - 1]).toBe(true);
    stop();
  });

  it('is true from the start when the document is already hidden', () => {
    doc.visibilityState = 'hidden';
    const seen: boolean[] = [];

    const stop = appHidden.subscribe((hidden) => seen.push(hidden));

    expect(seen[0]).toBe(true);
    stop();
  });

  it('follows the document being hidden and shown again', () => {
    const seen: boolean[] = [];
    const stop = appHidden.subscribe((hidden) => seen.push(hidden));

    doc.visibilityState = 'hidden';
    doc.fire('visibilitychange');
    expect(seen[seen.length - 1]).toBe(true);

    doc.visibilityState = 'visible';
    doc.fire('visibilitychange');
    expect(seen[seen.length - 1]).toBe(false);
    stop();
  });

  it('reads the minimized state again whenever the main window is resized', async () => {
    const seen: boolean[] = [];
    const stop = appHidden.subscribe((hidden) => seen.push(hidden));
    await settle();
    mainWindow.isMinimized.mockResolvedValue(true);

    resized();
    await settle();
    expect(seen[seen.length - 1]).toBe(true);

    mainWindow.isMinimized.mockResolvedValue(false);
    resized();
    await settle();
    expect(seen[seen.length - 1]).toBe(false);
    stop();
  });

  it('keeps its last answer when the minimized state cannot be read', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    mainWindow.isMinimized.mockRejectedValue(new Error('no window'));
    const stop = appHidden.subscribe(() => undefined);
    await settle();

    expect(get(appHidden)).toBe(false);
    expect(warn).toHaveBeenCalledTimes(1);
    stop();
  });

  it('stops listening to the document and the window when the last subscriber leaves', async () => {
    const stop = appHidden.subscribe(() => undefined);
    await settle();
    expect(doc.listeners.get('visibilitychange')?.size).toBe(1);

    stop();
    await settle();

    expect(doc.listeners.get('visibilitychange')?.size).toBe(0);
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('keeps a late minimized answer of an ended subscription from the next subscriber', async () => {
    let answer: (minimized: boolean) => void = () => undefined;
    mainWindow.isMinimized.mockReturnValueOnce(
      new Promise<boolean>((resolve) => {
        answer = resolve;
      })
    );
    appHidden.subscribe(() => undefined)();
    const seen: boolean[] = [];
    const stop = appHidden.subscribe((hidden) => seen.push(hidden));
    await settle();

    answer(true);
    await settle();

    expect(seen[seen.length - 1]).toBe(false);
    stop();
  });

  it('reads the document afresh for a subscriber that comes after an earlier one left', () => {
    const earlier = appHidden.subscribe(() => undefined);
    doc.visibilityState = 'hidden';
    doc.fire('visibilitychange');
    expect(get(appHidden)).toBe(true);
    earlier();
    doc.visibilityState = 'visible';
    const seen: boolean[] = [];

    const stop = appHidden.subscribe((hidden) => seen.push(hidden));

    expect(seen).toEqual([false]);
    stop();
  });

  it('only warns when the window size listener cannot be removed', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    mainWindow.onResized.mockReset().mockReturnValue(Promise.reject(new Error('gone')));
    const stop = appHidden.subscribe(() => undefined);

    stop();
    await settle();

    expect(warn).toHaveBeenCalledTimes(1);
  });
});
