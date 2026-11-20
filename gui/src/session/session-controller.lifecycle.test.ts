/**
 * The controller's decisions around the daemon: which command it sends in which state, how it
 * reacts to the window's events, and how a refused frame re-attaches the window.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('../ipc/session-client', () => ({
  invokeAttachWindow: vi.fn(),
  invokeSendWindowInput: vi.fn(),
  invokeClearWindowSelection: vi.fn(),
  invokeCopyWindowSelection: vi.fn(),
  invokeResize: vi.fn(),
  invokeSetWindowPalette: vi.fn(),
  invokeSetWindowVisible: vi.fn(),
}));

import {
  invokeAttachWindow,
  invokeClearWindowSelection,
  invokeCopyWindowSelection,
  invokeResize,
  invokeSendWindowInput,
  invokeSetWindowPalette,
  invokeSetWindowVisible,
  type WirePalette,
} from '../ipc/session-client';
import type { Frame } from '../ipc/session-boundary';
import { createWindowSessionStore, type SessionStore } from '../stores/session-store';
import { SessionController, type TerminalSink } from './session-controller';

const WINDOW_ID = 'w1-k3f9xa';

const PALETTE: WirePalette = {
  foreground: [1, 2, 3],
  background: [4, 5, 6],
  cursor: [7, 8, 9],
  ansi: Array.from({ length: 16 }, (_, index): [number, number, number] => [index, index, index]),
};

const ENTER = {
  type: 'key',
  key: { named: 'enter' },
  mods: { shift: false, alt: false, ctrl: false },
} as const;

function frame(overrides: Partial<Frame> = {}): Frame {
  return {
    seq: 1,
    cols: 4,
    rows: 2,
    reset: true,
    last: true,
    attrs: [{ fg: 'default', bg: 'default', flags: [] }],
    lines: [],
    cursor: { row: 0, col: 0, visible: true, shape: 'block' },
    view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
    overrides: [],
    ...overrides,
  };
}

/** A frame of another size that does not reset the view: the grid refuses it. */
const REFUSED = frame({ reset: false, cols: 9 });

function sink() {
  return {
    invalidate: vi.fn<TerminalSink['invalidate']>(),
    measure: vi.fn<TerminalSink['measure']>(() => ({ cols: 80, rows: 24 })),
    focus: vi.fn<TerminalSink['focus']>(),
    isVisible: vi.fn<TerminalSink['isVisible']>(() => true),
  };
}

/** A view that measures `size`, attached to `controller`. */
function measuring(controller: SessionController, size: { cols: number; rows: number }) {
  const view = sink();
  view.measure.mockReturnValue(size);
  controller.setSink(view);
  return view;
}

function attachedController(): { controller: SessionController; store: SessionStore } {
  const store = createWindowSessionStore();
  store.markAttached();
  return { controller: new SessionController(WINDOW_ID, store), store };
}

/** Lets promise continuations and the rejection handlers queued behind them run. */
async function settle(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(invokeAttachWindow).mockResolvedValue(undefined);
  vi.mocked(invokeSendWindowInput).mockResolvedValue(undefined);
  vi.mocked(invokeClearWindowSelection).mockResolvedValue(undefined);
  vi.mocked(invokeResize).mockResolvedValue(undefined);
  vi.mocked(invokeSetWindowPalette).mockResolvedValue(undefined);
  vi.mocked(invokeSetWindowVisible).mockResolvedValue(undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('SessionController window events', () => {
  it('marks the session exited with the exit code when the child exits', () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onExit({ window_id: WINDOW_ID, exit_code: 7 });

    expect(get(store)).toMatchObject({ phase: 'exited', exitCode: 7 });

    const unknown = attachedController();
    unknown.controller.eventHandlers().onExit({ window_id: WINDOW_ID, exit_code: null });

    expect(get(unknown.store)).toMatchObject({ phase: 'exited', exitCode: null });
  });

  it('marks the session failed with the reason when the connection is lost', () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onLink({
      window_id: WINDOW_ID,
      state: 'lost',
      reason: 'reconnect_failed',
      epoch: 1,
    });

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: 'reconnect_failed' });
  });

  it('marks the session superseded when another client takes the window over', () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onLink({ window_id: WINDOW_ID, state: 'superseded', epoch: 1 });

    expect(get(store).phase).toBe('superseded');
  });

  it("follows the core's link reports in epoch order and resends nothing", () => {
    const { controller, store } = attachedController();
    controller.setSink(sink());
    controller.setPalette(PALETTE);
    controller.setVisible(false);
    vi.mocked(invokeResize).mockClear();
    vi.mocked(invokeSetWindowVisible).mockClear();
    vi.mocked(invokeSetWindowPalette).mockClear();
    const handlers = controller.eventHandlers();

    handlers.onLink({ window_id: WINDOW_ID, state: 'lost', reason: 'reconnect_failed', epoch: 5 });
    expect(get(store).phase).toBe('failed');
    handlers.onLink({ window_id: WINDOW_ID, state: 'attached', epoch: 5 });
    handlers.onLink({ window_id: WINDOW_ID, state: 'attached', epoch: 4 });
    expect(get(store).phase).toBe('failed');
    handlers.onLink({ window_id: WINDOW_ID, state: 'attached', epoch: 6 });
    expect(get(store).phase).toBe('attached');
    expect(invokeResize).not.toHaveBeenCalled();
    expect(invokeSetWindowVisible).not.toHaveBeenCalled();
    expect(invokeSetWindowPalette).not.toHaveBeenCalled();

    handlers.onLink({ window_id: WINDOW_ID, state: 'superseded', epoch: 7 });
    handlers.onLink({ window_id: WINDOW_ID, state: 'attached', epoch: 8 });
    expect(get(store).phase).toBe('superseded');

    const exited = attachedController();
    exited.controller.eventHandlers().onExit({ window_id: WINDOW_ID, exit_code: 0 });
    exited.controller.eventHandlers().onLink({ window_id: WINDOW_ID, state: 'attached', epoch: 9 });
    expect(get(exited.store).phase).toBe('exited');
  });
});

describe('SessionController.start', () => {
  it.each<[string, unknown]>([
    ['a string', 'no such window'],
    ['an Error', new Error('boom')],
    ['a plain object', { code: 5 }],
    ['undefined', undefined],
  ])(
    'only warns when the attach is rejected with %s, leaving the phase as it was',
    async (_label, thrown) => {
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
      const { controller, store } = attachedController();
      vi.mocked(invokeAttachWindow).mockRejectedValue(thrown);

      await controller.start();

      expect(get(store)).toMatchObject({ phase: 'attached', failureDetail: null });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]?.[0])).toContain(WINDOW_ID);
    }
  );

  it('remembers the size of the latest attach, so the size of an earlier one is reported again', async () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);
    await controller.start();
    vi.mocked(view.measure).mockReturnValue({ cols: 100, rows: 30 });
    await controller.start();

    vi.mocked(view.measure).mockReturnValue({ cols: 80, rows: 24 });
    controller.reportMeasuredSize();

    expect(invokeResize).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, 80, 24);
  });
});

describe('SessionController commands', () => {
  it('only warns when a forwarded input is rejected, leaving the session as it was', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const { controller, store } = attachedController();
    vi.mocked(invokeSendWindowInput).mockRejectedValue(new Error('pipe broke'));

    controller.sendInput(ENTER);
    await settle();

    expect(get(store).phase).toBe('attached');
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain(WINDOW_ID);
  });

  it('only warns when clearing the selection is rejected, leaving the session as it was', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const { controller, store } = attachedController();
    vi.mocked(invokeClearWindowSelection).mockRejectedValue('gone');

    controller.clearSelection();
    await settle();

    expect(get(store).phase).toBe('attached');
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain(WINDOW_ID);
  });

  it('returns the text the window has selected, or null when nothing is', async () => {
    const { controller } = attachedController();
    vi.mocked(invokeCopyWindowSelection)
      .mockResolvedValueOnce('picked')
      .mockResolvedValueOnce(null);

    expect(await controller.copySelection()).toBe('picked');
    expect(await controller.copySelection()).toBeNull();
    expect(invokeCopyWindowSelection).toHaveBeenCalledWith(WINDOW_ID);
  });
});

describe('SessionController.setPalette', () => {
  it('gives the palette to the daemon while the session is interactive', () => {
    const { controller } = attachedController();

    controller.setPalette(PALETTE);

    expect(invokeSetWindowPalette).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, PALETTE);
  });

  it('gives the palette to the core while the session is not interactive yet', async () => {
    const store = createWindowSessionStore();
    const controller = new SessionController(WINDOW_ID, store);

    controller.setPalette(PALETTE);
    expect(invokeSetWindowPalette).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, PALETTE);
    await controller.start();

    expect(invokeSetWindowPalette).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, PALETTE);
  });

  it('only warns when the daemon refuses the palette, leaving the session alone', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const { controller, store } = attachedController();
    vi.mocked(invokeSetWindowPalette).mockRejectedValue(new Error('refused'));

    controller.setPalette(PALETTE);
    await settle();

    expect(get(store).phase).toBe('attached');
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain(WINDOW_ID);
  });
});

describe('SessionController.setVisible', () => {
  it('tells the daemon when the window goes off screen and on again', () => {
    const { controller } = attachedController();

    controller.setVisible(false);
    controller.setVisible(true);

    expect(vi.mocked(invokeSetWindowVisible).mock.calls).toEqual([
      [WINDOW_ID, false],
      [WINDOW_ID, true],
    ]);
  });

  it('says nothing when the visibility did not change', () => {
    const { controller } = attachedController();

    controller.setVisible(true);
    controller.setVisible(false);
    controller.setVisible(false);

    expect(invokeSetWindowVisible).toHaveBeenCalledTimes(1);
  });

  it('tells the core the visibility while the session is not interactive', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());

    controller.setVisible(false);

    expect(invokeSetWindowVisible).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, false);
  });

  it('only warns when the daemon refuses the visibility, leaving the session alone', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const { controller, store } = attachedController();
    vi.mocked(invokeSetWindowVisible).mockRejectedValue(new Error('refused'));

    controller.setVisible(false);
    await settle();

    expect(get(store).phase).toBe('attached');
    expect(warn).toHaveBeenCalledTimes(1);
  });
});

describe('SessionController.reportMeasuredSize', () => {
  it('reports a new size to the PTY', () => {
    const { controller } = attachedController();
    measuring(controller, { cols: 100, rows: 30 });

    controller.reportMeasuredSize();

    expect(invokeResize).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, 100, 30);
  });

  it('reports the measured size while the session is not interactive', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    measuring(controller, { cols: 100, rows: 30 });

    controller.reportMeasuredSize();

    expect(invokeResize).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, 100, 30);
  });

  it('does not report a size a window cannot take', () => {
    const { controller } = attachedController();
    const view = measuring(controller, { cols: 1, rows: 30 });

    controller.reportMeasuredSize();
    view.measure.mockReturnValue({ cols: 100, rows: 0 });
    controller.reportMeasuredSize();
    // Below the smallest size a pane reports, and a size measured while hidden.
    view.measure.mockReturnValue({ cols: 9, rows: 3 });
    controller.reportMeasuredSize();
    view.measure.mockReturnValue({ cols: 100, rows: 30 });
    view.isVisible.mockReturnValue(false);
    controller.reportMeasuredSize();

    expect(invokeResize).not.toHaveBeenCalled();
  });

  it('only warns when the PTY refuses the size, leaving the session as it was', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const { controller, store } = attachedController();
    measuring(controller, { cols: 100, rows: 30 });
    vi.mocked(invokeResize).mockRejectedValue('refused');

    controller.reportMeasuredSize();
    await settle();

    expect(get(store).phase).toBe('attached');
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain(WINDOW_ID);
  });
});

describe('SessionController.dispose', () => {
  it('lets go of the view, so later frames draw nothing', () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);

    controller.dispose();
    controller.eventHandlers().onFrame(frame());

    expect(view.invalidate).not.toHaveBeenCalled();
  });

  it('lets go of the view, so a later attach is made without a size', async () => {
    const { controller } = attachedController();
    controller.setSink(sink());

    controller.dispose();
    await controller.start();

    expect(vi.mocked(invokeAttachWindow).mock.calls[0]?.[1]).toBeNull();
  });
});

describe('SessionController frame handling', () => {
  it('redraws the cursor row and every row a continuing frame carries', () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);
    controller
      .eventHandlers()
      .onFrame(frame({ cursor: { row: 1, col: 0, visible: true, shape: 'block' } }));
    view.invalidate.mockClear();

    controller.eventHandlers().onFrame(
      frame({
        seq: 2,
        reset: false,
        lines: [{ row: 0, wrapped: false, runs: [] }],
        cursor: { row: 0, col: 0, visible: true, shape: 'block' },
      })
    );

    expect(view.invalidate).toHaveBeenCalledExactlyOnceWith([1, 0]);
  });

  it('redraws everything for the first frame of a view', () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);

    controller.eventHandlers().onFrame(frame());

    expect(view.invalidate).toHaveBeenCalledExactlyOnceWith('all');
  });

  it('applies a reset frame that arrives while an earlier frame is still open', () => {
    const { controller } = attachedController();
    controller.eventHandlers().onFrame(frame({ seq: 1, last: false }));

    const ack = controller.eventHandlers().onFrame(frame({ seq: 2, cols: 6 }));

    expect(ack).toBe(2);
    expect(controller.currentGrid().cols).toBe(6);
  });

  it('answers nothing for a part that does not complete a frame', () => {
    const { controller } = attachedController();

    expect(controller.eventHandlers().onFrame(frame({ last: false }))).toBeNull();
  });
});

describe('SessionController after a refused frame', () => {
  beforeEach(() => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
  });

  it('drops the grid, redraws, logs it and acknowledges nothing', () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);
    controller.eventHandlers().onFrame(frame());
    view.invalidate.mockClear();

    const ack = controller.eventHandlers().onFrame(REFUSED);

    expect(ack).toBeNull();
    expect(controller.currentGrid().rows).toBe(0);
    expect(view.invalidate).toHaveBeenCalledExactlyOnceWith('all');
    expect(console.error).toHaveBeenCalledTimes(1);
    expect(String(vi.mocked(console.error).mock.calls[0]?.[0])).toContain(WINDOW_ID);
  });

  it('attaches the window again', async () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onFrame(REFUSED);
    await settle();

    expect(invokeAttachWindow).toHaveBeenCalledTimes(1);
    expect(get(store).phase).toBe('attached');
  });

  it('attaches only once while a recovery is already under way', async () => {
    const { controller } = attachedController();
    let release: () => void = () => undefined;
    vi.mocked(invokeAttachWindow).mockReturnValue(
      new Promise((resolve) => {
        release = () => {
          resolve(undefined);
        };
      })
    );

    controller.eventHandlers().onFrame(REFUSED);
    controller.eventHandlers().onFrame(REFUSED);
    release();
    await settle();

    expect(invokeAttachWindow).toHaveBeenCalledTimes(1);
  });

  it('attaches again for a refusal that comes after the first recovery finished', async () => {
    const { controller } = attachedController();

    controller.eventHandlers().onFrame(REFUSED);
    await settle();
    controller.eventHandlers().onFrame(REFUSED);
    await settle();

    expect(invokeAttachWindow).toHaveBeenCalledTimes(2);
  });

  it('does not reattach a superseded window', async () => {
    const { controller } = attachedController();
    controller.eventHandlers().onLink({ window_id: WINDOW_ID, state: 'superseded', epoch: 1 });

    controller.eventHandlers().onFrame(REFUSED);
    await settle();
    await controller.start();

    expect(invokeAttachWindow).not.toHaveBeenCalled();
  });

  it('starts a reset frame over on a fresh grid when the old view left a frame open', () => {
    const { controller } = attachedController();
    controller.eventHandlers().onFrame(frame({ seq: 1, last: false }));

    const ack = controller.eventHandlers().onFrame(frame({ seq: 1 }));

    expect(ack).toBe(1);
    expect(console.error).not.toHaveBeenCalled();
  });

  it('refuses a reset frame that is itself broken even on a fresh grid', () => {
    const { controller } = attachedController();
    const broken = frame({ lines: [{ row: 5, wrapped: false, runs: [] }] });

    const ack = controller.eventHandlers().onFrame(broken);

    expect(ack).toBeNull();
    expect(console.error).toHaveBeenCalledTimes(1);
  });
});
