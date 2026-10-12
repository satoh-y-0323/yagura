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
  };
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
  vi.mocked(invokeAttachWindow).mockResolvedValue({ child_exited: false, exit_code: null });
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
  });

  it('marks the session failed with the reason when the connection is lost', () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onDisconnected({ window_id: WINDOW_ID, reason: 'pipe closed' });

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: 'pipe closed' });
  });

  it('marks the session superseded when another client takes the window over', () => {
    const { controller, store } = attachedController();

    controller.eventHandlers().onSuperseded({ window_id: WINDOW_ID, reason: 'taken over' });

    expect(get(store).phase).toBe('superseded');
  });
});

describe('SessionController.start', () => {
  it('enters connecting before the attach answers', async () => {
    const { controller, store } = attachedController();
    let phaseDuringAttach: string | null = null;
    vi.mocked(invokeAttachWindow).mockImplementation(() => {
      phaseDuringAttach = get(store).phase;
      return Promise.resolve({ child_exited: false, exit_code: null });
    });

    await controller.start();

    expect(phaseDuringAttach).toBe('connecting');
    expect(get(store).phase).toBe('attached');
  });

  it.each<[string, unknown, string]>([
    ['a string', 'no such window', 'no such window'],
    ['an Error', new Error('boom'), 'boom'],
    ['a plain object', { code: 5 }, '{"code":5}'],
    ['undefined', undefined, 'unexpected error'],
  ])('records the detail when the attach is rejected with %s', async (_label, thrown, detail) => {
    const { controller, store } = attachedController();
    vi.mocked(invokeAttachWindow).mockRejectedValue(thrown);

    await controller.start();

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: detail });
  });

  it('gives the daemon the palette once attached', async () => {
    const { controller } = attachedController();
    controller.setPalette(PALETTE);
    vi.mocked(invokeSetWindowPalette).mockClear();

    await controller.start();

    expect(invokeSetWindowPalette).toHaveBeenCalledWith(WINDOW_ID, PALETTE);
  });

  it('does not send a palette that was never set', async () => {
    const { controller } = attachedController();

    await controller.start();

    expect(invokeSetWindowPalette).not.toHaveBeenCalled();
  });

  it('does not send the palette when the attach found the child already exited', async () => {
    const { controller } = attachedController();
    controller.setPalette(PALETTE);
    vi.mocked(invokeSetWindowPalette).mockClear();
    vi.mocked(invokeAttachWindow).mockResolvedValue({ child_exited: true, exit_code: 0 });

    await controller.start();

    expect(invokeSetWindowPalette).not.toHaveBeenCalled();
  });

  it('tells the daemon the window is hidden when it attaches while hidden', async () => {
    const { controller } = attachedController();
    controller.setVisible(false);
    vi.mocked(invokeSetWindowVisible).mockClear();

    await controller.start();

    expect(invokeSetWindowVisible).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, false);
  });

  it('says nothing about visibility when it attaches shown, which the daemon assumes', async () => {
    const { controller } = attachedController();

    await controller.start();

    expect(invokeSetWindowVisible).not.toHaveBeenCalled();
  });

  it('remembers the size of the latest attach, so the size of an earlier one is reported again', async () => {
    const { controller } = attachedController();
    const view = sink();
    controller.setSink(view);
    await controller.start();
    vi.mocked(view.measure).mockReturnValue({ cols: 100, rows: 30 });
    await controller.start();

    controller.notifyResize({ cols: 80, rows: 24 });

    expect(invokeResize).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, 80, 24);
  });
});

describe('SessionController.retry', () => {
  it('attaches again', async () => {
    const { controller, store } = attachedController();
    vi.mocked(invokeAttachWindow).mockRejectedValueOnce('down');
    await controller.start();
    expect(get(store).phase).toBe('failed');

    await controller.retry();

    expect(get(store).phase).toBe('attached');
    expect(invokeAttachWindow).toHaveBeenCalledTimes(2);
  });

  it('never reconnects a superseded session', async () => {
    const { controller, store } = attachedController();
    controller.eventHandlers().onSuperseded({ window_id: WINDOW_ID, reason: 'taken over' });

    await controller.retry();

    expect(invokeAttachWindow).not.toHaveBeenCalled();
    expect(get(store).phase).toBe('superseded');
  });
});

describe('SessionController commands', () => {
  it('marks the session failed when a forwarded input is rejected', async () => {
    const { controller, store } = attachedController();
    vi.mocked(invokeSendWindowInput).mockRejectedValue(new Error('pipe broke'));

    controller.sendInput(ENTER);
    await settle();

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: 'pipe broke' });
  });

  it('marks the session failed when clearing the selection is rejected', async () => {
    const { controller, store } = attachedController();
    vi.mocked(invokeClearWindowSelection).mockRejectedValue('gone');

    controller.clearSelection();
    await settle();

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: 'gone' });
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

  it('keeps the palette for the attach when the session is not interactive yet', async () => {
    const store = createWindowSessionStore();
    const controller = new SessionController(WINDOW_ID, store);

    controller.setPalette(PALETTE);
    expect(invokeSetWindowPalette).not.toHaveBeenCalled();
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

  it('gives the palette again with the first frame of a new view', () => {
    const { controller } = attachedController();
    controller.setPalette(PALETTE);
    vi.mocked(invokeSetWindowPalette).mockClear();

    controller.eventHandlers().onFrame(frame());

    expect(invokeSetWindowPalette).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, PALETTE);
  });

  it('does not give the palette again for a frame that continues the view', () => {
    const { controller } = attachedController();
    controller.eventHandlers().onFrame(frame());
    controller.setPalette(PALETTE);
    vi.mocked(invokeSetWindowPalette).mockClear();

    controller.eventHandlers().onFrame(frame({ seq: 2, reset: false }));

    expect(invokeSetWindowPalette).not.toHaveBeenCalled();
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

  it('keeps the visibility for the attach while the session is not interactive', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());

    controller.setVisible(false);

    expect(invokeSetWindowVisible).not.toHaveBeenCalled();
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

describe('SessionController.notifyResize', () => {
  it('reports a new size to the PTY', () => {
    const { controller } = attachedController();

    controller.notifyResize({ cols: 100, rows: 30 });

    expect(invokeResize).toHaveBeenCalledExactlyOnceWith(WINDOW_ID, 100, 30);
  });

  it('does not report while the session is not interactive', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());

    controller.notifyResize({ cols: 100, rows: 30 });

    expect(invokeResize).not.toHaveBeenCalled();
  });

  it('does not report a size a window cannot take', () => {
    const { controller } = attachedController();

    controller.notifyResize({ cols: 1, rows: 30 });
    controller.notifyResize({ cols: 100, rows: 0 });

    expect(invokeResize).not.toHaveBeenCalled();
  });

  it('does not report the same size twice, but reports when either dimension changes', () => {
    const { controller } = attachedController();

    controller.notifyResize({ cols: 100, rows: 30 });
    controller.notifyResize({ cols: 100, rows: 30 });
    controller.notifyResize({ cols: 101, rows: 30 });
    controller.notifyResize({ cols: 101, rows: 31 });

    expect(vi.mocked(invokeResize).mock.calls).toEqual([
      [WINDOW_ID, 100, 30],
      [WINDOW_ID, 101, 30],
      [WINDOW_ID, 101, 31],
    ]);
  });

  it('does not report the size the attach was made at', async () => {
    const store = createWindowSessionStore();
    const controller = new SessionController(WINDOW_ID, store);
    controller.setSink(sink());
    await controller.start();

    controller.notifyResize({ cols: 80, rows: 24 });

    expect(invokeResize).not.toHaveBeenCalled();
  });

  it('marks the session failed when the PTY refuses the size', async () => {
    const { controller, store } = attachedController();
    vi.mocked(invokeResize).mockRejectedValue('refused');

    controller.notifyResize({ cols: 100, rows: 30 });
    await settle();

    expect(get(store)).toMatchObject({ phase: 'failed', failureDetail: 'refused' });
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
          resolve({ child_exited: false, exit_code: null });
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
    controller.eventHandlers().onSuperseded({ window_id: WINDOW_ID, reason: 'taken over' });

    controller.eventHandlers().onFrame(REFUSED);
    await settle();

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
