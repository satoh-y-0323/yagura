/**
 * `folder-picker.ts` is the sole place in the GUI that touches the Tauri
 * dialog or webview APIs. Vitest runs without a DOM or a Tauri runtime, so
 * both APIs are mocked here and `pickFolder` / `subscribeFolderDrop` are
 * exercised through those mocks -- calling the real exported functions
 * rather than reading the module's source text.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

import type { RawDropEvent, DropSignal } from './folder-drop-model';

const { openMock, onDragDropEventMock, getCurrentWebviewMock } = vi.hoisted(() => ({
  openMock: vi.fn(),
  onDragDropEventMock: vi.fn(),
  getCurrentWebviewMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: openMock,
}));

vi.mock('@tauri-apps/api/webview', () => ({
  getCurrentWebview: getCurrentWebviewMock,
}));

import { pickFolder, subscribeFolderDrop } from './folder-picker';

type DragDropCallback = (event: { payload: RawDropEvent }) => void;

beforeEach(() => {
  openMock.mockReset();
  onDragDropEventMock.mockReset();
  getCurrentWebviewMock.mockReset();
  getCurrentWebviewMock.mockReturnValue({ onDragDropEvent: onDragDropEventMock });
});

/** Runs `subscribeFolderDrop`, capturing the callback it registered. */
async function subscribeAndCapture(
  handler: (signal: DropSignal) => void
): Promise<DragDropCallback> {
  let captured: DragDropCallback | null = null;
  onDragDropEventMock.mockImplementationOnce((callback: DragDropCallback) => {
    captured = callback;
    return Promise.resolve(vi.fn());
  });
  await subscribeFolderDrop(handler);
  if (captured === null) {
    throw new Error('onDragDropEvent was not called');
  }
  return captured;
}

describe('pickFolder(): Promise<string | null>', () => {
  it('opens the native directory picker through @tauri-apps/plugin-dialog', async () => {
    openMock.mockResolvedValueOnce('/chosen');

    await pickFolder();

    expect(openMock).toHaveBeenCalledTimes(1);
  });

  it('requests directory mode, the only mode this dialog needs', async () => {
    openMock.mockResolvedValueOnce('/chosen');

    await pickFolder();

    expect(openMock).toHaveBeenCalledWith({ directory: true });
  });

  it('resolves to the path the dialog returned, unchanged', async () => {
    openMock.mockResolvedValueOnce('/chosen');

    await expect(pickFolder()).resolves.toBe('/chosen');
  });

  it('resolves to null when the user cancels, unchanged', async () => {
    openMock.mockResolvedValueOnce(null);

    await expect(pickFolder()).resolves.toBeNull();
  });
});

describe('subscribeFolderDrop(handler): Promise<UnlistenFn>', () => {
  it('registers through the current webview obtained from @tauri-apps/api/webview', async () => {
    onDragDropEventMock.mockResolvedValueOnce(vi.fn());

    await subscribeFolderDrop(vi.fn());

    expect(getCurrentWebviewMock).toHaveBeenCalledTimes(1);
  });

  it('registers through onDragDropEvent, the raw Tauri drag-drop hook', async () => {
    onDragDropEventMock.mockResolvedValueOnce(vi.fn());

    await subscribeFolderDrop(vi.fn());

    expect(onDragDropEventMock).toHaveBeenCalledTimes(1);
  });

  it('resolves to the unlisten function onDragDropEvent returned', async () => {
    const unlisten = vi.fn();
    onDragDropEventMock.mockResolvedValueOnce(unlisten);

    await expect(subscribeFolderDrop(vi.fn())).resolves.toBe(unlisten);
  });

  const FORWARDING_CASES: ReadonlyArray<readonly [string, RawDropEvent, DropSignal]> = [
    [
      'an enter carrying a path normalizes to over',
      { type: 'enter', paths: ['/a'] },
      { kind: 'over' },
    ],
    ['an enter carrying no path normalizes to null', { type: 'enter', paths: [] }, null],
    ['an over event normalizes to over', { type: 'over' }, { kind: 'over' }],
    ['a leave event normalizes to leave', { type: 'leave' }, { kind: 'leave' }],
    [
      'a drop carrying a path normalizes to drop',
      { type: 'drop', paths: ['/a'] },
      { kind: 'drop', path: '/a' },
    ],
    ['a drop carrying no path normalizes to null', { type: 'drop', paths: [] }, null],
  ];

  it.each(FORWARDING_CASES)(
    'forwards the normalized signal to the caller-supplied handler: %s',
    async (_label, rawEvent, expected) => {
      const handler = vi.fn();
      const callback = await subscribeAndCapture(handler);

      callback({ payload: rawEvent });

      expect(handler).toHaveBeenCalledWith(expected);
    }
  );
});
