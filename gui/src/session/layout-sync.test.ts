/**
 * The real `createWindowRegistry()` stands in for a mock: it already produces the
 * `WindowDisplay` shapes this module serializes, and it drives the same maximize /
 * restoreBounds bookkeeping the UI does.
 */
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest';

import { startLayoutSync } from './layout-sync';
import { createWindowRegistry, type WindowRegistry } from '../stores/window-registry';
import type { WindowInfo } from '../ipc/session-client';

const DEBOUNCE_MS = 300;

type MockSend = Mock<(windowId: string, layoutJson: string) => void>;

function openWindowInfo(windowId: string): WindowInfo {
  return {
    window_id: windowId,
    name: windowId,
    vendor: null,
    cwd: '/x',
    detected: { status: 'running' },
    exit_code: null,
  };
}

function parsedLayoutOf(send: MockSend, callIndex: number): Record<string, unknown> {
  const call = send.mock.calls[callIndex] as [string, string];
  return JSON.parse(call[1]) as Record<string, unknown>;
}

describe('startLayoutSync', () => {
  let registry: WindowRegistry;
  let send: MockSend;

  beforeEach(() => {
    vi.useFakeTimers();
    registry = createWindowRegistry();
    send = vi.fn<(windowId: string, layoutJson: string) => void>();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('returns a function (the unsubscribe/disposer)', () => {
    const stop = startLayoutSync(registry, send);
    expect(typeof stop).toBe('function');
    stop();
  });

  it('never sends on a window it has only observed once (the initial baseline)', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);

    vi.advanceTimersByTime(10_000);

    expect(send).not.toHaveBeenCalled();
  });

  it('does not send when a later observation serializes identically (unrelated store update)', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);

    // A store emission that touches no layout-relevant field.
    registry.setStatus('w1', { status: 'idle' }, null);
    vi.advanceTimersByTime(10_000);

    expect(send).not.toHaveBeenCalled();
  });

  it('sends after a 300ms trailing debounce once a window layout actually differs from its baseline', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);

    registry.setWindowPosition('w1', { x: 10, y: 20 });

    vi.advanceTimersByTime(DEBOUNCE_MS - 1);
    expect(send).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0]?.[0]).toBe('w1');
  });

  it('coalesces rapid successive changes into a single trailing send', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);

    registry.setWindowPosition('w1', { x: 1, y: 1 });
    vi.advanceTimersByTime(100);
    registry.setWindowPosition('w1', { x: 2, y: 2 });
    vi.advanceTimersByTime(100);
    registry.setWindowPosition('w1', { x: 3, y: 3 });
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(send).toHaveBeenCalledTimes(1);
    const layout = parsedLayoutOf(send, 0);
    expect(layout['pos']).toEqual({ x: 3, y: 3 });
  });

  it('sends only for the window that actually changed, not an untouched sibling', () => {
    registry.openWindow(openWindowInfo('w1'));
    registry.openWindow(openWindowInfo('w2'));
    startLayoutSync(registry, send);

    registry.setWindowPosition('w1', { x: 7, y: 7 });
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0]?.[0]).toBe('w1');
  });

  it('serializes pos/size from restoreBounds (not the applied fullscreen rect) while maximized, with max: true', () => {
    registry.openWindow(openWindowInfo('w1'));
    registry.setWindowPosition('w1', { x: 5, y: 5 });
    registry.setWindowSize('w1', { width: 100, height: 100 });
    startLayoutSync(registry, send);

    // Maximizing captures restoreBounds; the position/size calls after it are the
    // UI applying the fullscreen rect, as `App.svelte` does.
    registry.setWindowMaximized('w1', true);
    registry.setWindowPosition('w1', { x: 0, y: 0 });
    registry.setWindowSize('w1', { width: 1920, height: 1080 });
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(send).toHaveBeenCalledTimes(1);
    const layout = parsedLayoutOf(send, 0);
    expect(layout['pos']).toEqual({ x: 5, y: 5 });
    expect(layout['size']).toEqual({ width: 100, height: 100 });
    expect(layout['max']).toBe(true);
  });

  it('falls back to the current pos/size while maximized when restoreBounds is null (captured before position/size were ever set)', () => {
    registry.openWindow(openWindowInfo('w1'));
    // position/size are still null, so `captureRect` (`window-registry.ts`) yields
    // a null restoreBounds here.
    registry.setWindowMaximized('w1', true);
    startLayoutSync(registry, send);

    registry.setWindowPosition('w1', { x: 0, y: 0 });
    registry.setWindowSize('w1', { width: 1920, height: 1080 });
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(send).toHaveBeenCalledTimes(1);
    const layout = parsedLayoutOf(send, 0);
    expect(layout['pos']).toEqual({ x: 0, y: 0 });
    expect(layout['size']).toEqual({ width: 1920, height: 1080 });
    expect(layout['max']).toBe(true);
  });

  it('stops sending once unsubscribed', () => {
    registry.openWindow(openWindowInfo('w1'));
    const stop = startLayoutSync(registry, send);
    stop();

    registry.setWindowPosition('w1', { x: 10, y: 20 });
    vi.advanceTimersByTime(DEBOUNCE_MS);

    expect(send).not.toHaveBeenCalled();
  });

  it('discards a pending debounced send when unsubscribed before it fires', () => {
    registry.openWindow(openWindowInfo('w1'));
    const stop = startLayoutSync(registry, send);

    registry.setWindowPosition('w1', { x: 10, y: 20 });
    vi.advanceTimersByTime(DEBOUNCE_MS - 10);
    stop();
    vi.advanceTimersByTime(10_000);

    expect(send).not.toHaveBeenCalled();
  });
});

describe('startLayoutSync: windows that close and come back', () => {
  let registry: WindowRegistry;
  let send: MockSend;

  beforeEach(() => {
    vi.useFakeTimers();
    registry = createWindowRegistry();
    send = vi.fn<(windowId: string, layoutJson: string) => void>();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('drops a pending send when its window closes before the debounce ends', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);
    registry.setWindowPosition('w1', { x: 10, y: 20 });

    registry.closeWindow('w1');
    vi.advanceTimersByTime(10_000);

    expect(send).not.toHaveBeenCalled();
  });

  it('treats a window that reopens under the same id as new, so its first layout is only a baseline', () => {
    registry.openWindow(openWindowInfo('w1'));
    startLayoutSync(registry, send);
    registry.closeWindow('w1');

    registry.openWindow(openWindowInfo('w1'));
    registry.setWindowPosition('w1', { x: 10, y: 20 });
    vi.advanceTimersByTime(10_000);

    expect(send).toHaveBeenCalledTimes(1);
    expect(parsedLayoutOf(send, 0)['pos']).toEqual({ x: 10, y: 20 });
  });

  it('does not send for a window that reopens with a layout unlike its old one', () => {
    registry.openWindow(openWindowInfo('w1'));
    registry.setWindowPosition('w1', { x: 5, y: 5 });
    startLayoutSync(registry, send);
    registry.closeWindow('w1');

    registry.openWindow(openWindowInfo('w1'));
    vi.advanceTimersByTime(10_000);

    expect(send).not.toHaveBeenCalled();
  });

  it('reports the tab position of each window', () => {
    registry.openWindow(openWindowInfo('w1'));
    registry.openWindow(openWindowInfo('w2'));
    startLayoutSync(registry, send);

    registry.moveTab('w2', 0);
    vi.advanceTimersByTime(DEBOUNCE_MS);

    const sent = send.mock.calls.map(([id, json]) => [
      id,
      (JSON.parse(json) as { tab: number }).tab,
    ]);
    expect(sent).toEqual([
      ['w1', 1],
      ['w2', 0],
    ]);
  });

  it('sends nothing more for a window after the stop function ran, even if it changes again', () => {
    registry.openWindow(openWindowInfo('w1'));
    const stop = startLayoutSync(registry, send);
    registry.setWindowPosition('w1', { x: 1, y: 1 });
    vi.advanceTimersByTime(DEBOUNCE_MS);
    expect(send).toHaveBeenCalledTimes(1);

    stop();
    registry.setWindowPosition('w1', { x: 2, y: 2 });
    vi.advanceTimersByTime(10_000);

    expect(send).toHaveBeenCalledTimes(1);
  });
});
