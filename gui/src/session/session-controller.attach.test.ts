/**
 * Starting a session: the terminal is measured first, the attach asks the daemon for
 * that size, and the frames the daemon sends are what the terminal draws.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('../ipc/session-client', () => ({
  invokeSendWindowInput: vi.fn().mockResolvedValue(undefined),
  invokeAttachWindow: vi.fn(),
  invokeSetWindowPalette: vi.fn().mockResolvedValue(undefined),
  invokeSetWindowVisible: vi.fn().mockResolvedValue(undefined),
  invokeResize: vi.fn().mockResolvedValue(undefined),
}));

import { invokeAttachWindow } from '../ipc/session-client';
import type { Frame } from '../ipc/session-boundary';
import { createWindowSessionStore } from '../stores/session-store';
import { SessionController, type TerminalSink, type TerminalSize } from './session-controller';

const WINDOW_ID = 'w1-k3f9xa';

const SCREEN: Frame = {
  seq: 1,
  cols: 4,
  rows: 2,
  reset: true,
  last: true,
  attrs: [{ fg: 'default', bg: 'default', flags: ['bold'] }],
  lines: [{ row: 0, wrapped: false, runs: [{ attr: 0, cells: ['s', 'c'], wide: [] }] }],
  cursor: { row: 0, col: 2, visible: true, shape: 'block' },
  view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
  overrides: [],
};

function sinkMeasuring(size: TerminalSize | null, isVisible = true) {
  const sink = {
    invalidate: vi.fn<TerminalSink['invalidate']>(),
    measure: vi.fn<TerminalSink['measure']>(() => size),
    focus: vi.fn<TerminalSink['focus']>(),
    isVisible: vi.fn<TerminalSink['isVisible']>(() => isVisible),
  };
  return sink;
}

function attachResolves(): void {
  vi.mocked(invokeAttachWindow).mockResolvedValue(undefined);
}

/** The arguments of the first attach after the window id. */
function attachedArgs(): unknown[] {
  const call = vi.mocked(invokeAttachWindow).mock.calls[0] as unknown[] | undefined;
  expect(call).toBeDefined();
  expect(call?.[0]).toBe(WINDOW_ID);
  return call?.slice(1) ?? [];
}

/** The size argument the attach was made with, `null` when it named none. */
function attachedSize(): unknown {
  return attachedArgs()[0] ?? null;
}

function rowText(controller: SessionController, row: number): string {
  return (controller.currentGrid().lines[row]?.cells ?? [])
    .map((cell) => cell.text)
    .join('')
    .trimEnd();
}

describe('SessionController.start', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('measures the terminal first and attaches at that size', async () => {
    attachResolves();
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    const sink = sinkMeasuring({ cols: 132, rows: 43 });
    controller.setSink(sink);

    await controller.start();

    expect(attachedSize()).toEqual({ cols: 132, rows: 43 });
    expect(sink.measure.mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(invokeAttachWindow).mock.invocationCallOrder[0] ?? 0
    );
  });

  it('draws the frame the daemon sends and acknowledges it', async () => {
    attachResolves();
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    const sink = sinkMeasuring({ cols: 132, rows: 43 });
    controller.setSink(sink);

    await controller.start();
    const ack = controller.eventHandlers().onFrame(SCREEN);

    expect(rowText(controller, 0)).toBe('sc');
    expect(sink.invalidate).toHaveBeenCalledWith('all');
    expect(ack).toBe(1);
  });

  it('applies and acknowledges a frame that arrives while no terminal is connected', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());

    const ack = controller.eventHandlers().onFrame(SCREEN);

    expect(rowText(controller, 0)).toBe('sc');
    expect(ack).toBe(1);
  });

  it('attaches with no size when the terminal cannot be measured', async () => {
    attachResolves();
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    controller.setSink(sinkMeasuring(null, false));
    controller.setVisible(false);

    await controller.start();

    expect(attachedSize()).toBeNull();
  });

  it('attaches with no size when the measured size is not one a window can take', async () => {
    attachResolves();
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    controller.setSink(sinkMeasuring({ cols: 0, rows: 24 }));

    await controller.start();

    expect(attachedSize()).toBeNull();

    // Below the smallest size a pane reports, and a pane that is hidden.
    for (const sink of [
      sinkMeasuring({ cols: 9, rows: 3 }),
      sinkMeasuring({ cols: 132, rows: 43 }, false),
    ]) {
      vi.mocked(invokeAttachWindow).mockClear();
      const another = new SessionController(WINDOW_ID, createWindowSessionStore());
      another.setSink(sink);

      await another.start();

      expect(attachedSize()).toBeNull();
    }
  });
});
