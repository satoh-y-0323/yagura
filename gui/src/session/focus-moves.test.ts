/**
 * Where keyboard focus goes when it crosses the sidebar boundary, and when a
 * requested move has to wait for the open modal dialogs to finish.
 */
import { describe, it, expect } from 'vitest';

import {
  createPendingFocus,
  panelReturnTarget,
  restoreFocusMove,
  sidebarToggleMove,
  type FocusMove,
  type PanelOrigin,
  type PanelReturn,
} from './focus-moves';

const CURRENT: { windowId: string } = { windowId: 'w1-k3f9xa' };
const OTHER: { windowId: string } = { windowId: 'w2-p0d8qz' };

const allShown = (): boolean => true;
const noneShown = (): boolean => false;

describe('sidebarToggleMove', () => {
  it('goes to the current window terminal when focus is inside the sidebar', () => {
    expect(sidebarToggleMove(true, CURRENT, allShown)).toEqual({
      kind: 'terminal',
      windowId: CURRENT.windowId,
    });
    expect(sidebarToggleMove(true, OTHER, noneShown)).toEqual({
      kind: 'terminal',
      windowId: OTHER.windowId,
    });
  });

  it('does nothing from inside the sidebar when there is no current window', () => {
    expect(sidebarToggleMove(true, null, allShown)).toEqual({ kind: 'none' });
  });

  it('goes to the current window row when focus is outside the sidebar and the row is rendered', () => {
    const asked: string[] = [];
    const move: FocusMove = sidebarToggleMove(false, CURRENT, (windowId: string) => {
      asked.push(windowId);
      return true;
    });

    expect(move).toEqual({ kind: 'sidebar-row', windowId: CURRENT.windowId });
    expect(asked).toEqual([CURRENT.windowId]);
  });

  it('falls back to the start of the sidebar when the current row is not rendered', () => {
    expect(sidebarToggleMove(false, CURRENT, noneShown)).toEqual({ kind: 'sidebar-start' });
  });

  it('goes to the start of the sidebar when there is no current window', () => {
    expect(sidebarToggleMove(false, null, allShown)).toEqual({ kind: 'sidebar-start' });
  });

  it('answers the same for the same input however the calls are interleaved', () => {
    const outward = (): FocusMove => sidebarToggleMove(false, CURRENT, allShown);
    const inward = (): FocusMove => sidebarToggleMove(true, CURRENT, allShown);

    const first = outward();
    const back = inward();
    const again = outward();
    const backAgain = inward();
    const third = outward();

    expect(first).toEqual({ kind: 'sidebar-row', windowId: CURRENT.windowId });
    expect(again).toEqual(first);
    expect(third).toEqual(first);
    expect(back).toEqual({ kind: 'terminal', windowId: CURRENT.windowId });
    expect(backAgain).toEqual(back);
  });

  it('follows a change of the current window instead of a remembered one', () => {
    expect(sidebarToggleMove(false, CURRENT, allShown)).toEqual({
      kind: 'sidebar-row',
      windowId: CURRENT.windowId,
    });
    expect(sidebarToggleMove(false, OTHER, allShown)).toEqual({
      kind: 'sidebar-row',
      windowId: OTHER.windowId,
    });
    expect(sidebarToggleMove(true, OTHER, allShown)).toEqual({
      kind: 'terminal',
      windowId: OTHER.windowId,
    });
  });
});

describe('restoreFocusMove', () => {
  it('returns to the current window terminal', () => {
    expect(restoreFocusMove(CURRENT)).toEqual({ kind: 'terminal', windowId: CURRENT.windowId });
    expect(restoreFocusMove(OTHER)).toEqual({ kind: 'terminal', windowId: OTHER.windowId });
  });

  it('falls back to the start of the sidebar when there is no current window', () => {
    expect(restoreFocusMove(null)).toEqual({ kind: 'sidebar-start' });
  });
});

/** A settle step the test releases by hand, so "after the view rendered" is observable. */
function manualSettle(): { settle: () => Promise<void>; release: () => void } {
  const resolvers: Array<() => void> = [];
  return {
    settle: () =>
      new Promise<void>((resolve) => {
        resolvers.push(resolve);
      }),
    release: () => {
      for (const resolve of resolvers.splice(0)) {
        resolve();
      }
    },
  };
}

/** Lets every continuation that is already runnable finish. */
function drain(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function harness(initialModals = 0) {
  const state = { modals: initialModals };
  const ran: FocusMove[] = [];
  const gate = manualSettle();
  const pending = createPendingFocus({
    isModalOpen: (): boolean => state.modals > 0,
    settle: gate.settle,
    run: (move: FocusMove): void => {
      ran.push(move);
    },
  });
  return { state, ran, gate, pending };
}

const TO_B: FocusMove = { kind: 'terminal', windowId: 'wB' };
const TO_C: FocusMove = { kind: 'terminal', windowId: 'wC' };

describe('createPendingFocus without an open modal', () => {
  it('runs the requested move once, and only after the view has settled', async () => {
    const { ran, gate, pending } = harness();

    pending.request(TO_B);
    await drain();
    expect(ran).toEqual([]);

    gate.release();
    await drain();
    expect(ran).toEqual([TO_B]);
  });

  it('does not run the same move again on a later flush', async () => {
    const { ran, gate, pending } = harness();

    pending.request(TO_B);
    gate.release();
    await drain();
    pending.flush();
    gate.release();
    await drain();

    expect(ran).toEqual([TO_B]);
  });
});

describe('createPendingFocus with a modal open', () => {
  it('holds the move while the modal stays open, however often it is flushed', async () => {
    const { state, ran, gate, pending } = harness(1);

    pending.request(TO_B);
    gate.release();
    await drain();
    pending.flush();
    gate.release();
    await drain();

    expect(state.modals).toBe(1);
    expect(ran).toEqual([]);
  });

  it('runs the held move once when the last modal has closed, after the view has settled', async () => {
    const { state, ran, gate, pending } = harness(1);
    pending.request(TO_B);
    gate.release();
    await drain();

    state.modals = 0;
    pending.flush();
    await drain();
    expect(ran).toEqual([]);

    gate.release();
    await drain();
    expect(ran).toEqual([TO_B]);
  });

  it('keeps holding until every open modal has closed, not just the first', async () => {
    const { state, ran, gate, pending } = harness(2);
    pending.request(TO_B);
    gate.release();
    await drain();

    state.modals = 1;
    pending.flush();
    gate.release();
    await drain();
    expect(ran).toEqual([]);

    state.modals = 0;
    pending.flush();
    gate.release();
    await drain();
    expect(ran).toEqual([TO_B]);
  });

  it('reads the modal state after the view has settled, so a dialog that opens meanwhile still holds it', async () => {
    const { state, ran, gate, pending } = harness(1);
    pending.request(TO_B);
    gate.release();
    await drain();

    state.modals = 0;
    pending.flush();
    await drain();
    state.modals = 1;
    gate.release();
    await drain();
    expect(ran).toEqual([]);

    state.modals = 0;
    pending.flush();
    gate.release();
    await drain();
    expect(ran).toEqual([TO_B]);
  });

  it('replaces a held move with the newest request instead of keeping the first or running both', async () => {
    const { state, ran, gate, pending } = harness(1);

    pending.request(TO_B);
    pending.request(TO_C);
    gate.release();
    await drain();
    expect(ran).toEqual([]);

    state.modals = 0;
    pending.flush();
    gate.release();
    await drain();
    expect(ran).toEqual([TO_C]);
  });

  it('runs a request made after the modals closed at once, and never resurrects the older held move', async () => {
    const { state, ran, gate, pending } = harness(1);
    pending.request(TO_B);
    gate.release();
    await drain();

    state.modals = 0;
    pending.request(TO_C);
    gate.release();
    await drain();
    expect(ran).toEqual([TO_C]);

    pending.flush();
    gate.release();
    await drain();
    expect(ran).toEqual([TO_C]);
  });

  it('runs nothing when a modal closes and no move is held', async () => {
    const { state, ran, gate, pending } = harness(1);

    state.modals = 0;
    pending.flush();
    gate.release();
    await drain();

    expect(ran).toEqual([]);
  });
});

describe('createPendingFocus against the browser restoring focus after a modal closes', () => {
  it('ends on the requested window when an approval dialog opens and closes right after the first modal', async () => {
    const modals = { open: 1 };
    let focused = 'wA';
    const restoreTarget = 'wA';
    const gate = manualSettle();
    const pending = createPendingFocus({
      isModalOpen: (): boolean => modals.open > 0,
      settle: gate.settle,
      run: (move: FocusMove): void => {
        if (move.kind === 'terminal') {
          focused = move.windowId;
        }
      },
    });
    const closeModal = (): void => {
      modals.open -= 1;
      if (modals.open === 0) {
        focused = restoreTarget;
      }
    };

    // The add-window dialog is still open when the new window's move is requested.
    pending.request({ kind: 'terminal', windowId: 'wB' });
    gate.release();
    await drain();

    // It closes and the browser hands focus back to the window that had it.
    closeModal();
    pending.flush();
    // The approval dialog opens before the view has settled.
    modals.open += 1;
    gate.release();
    await drain();
    expect(focused).toBe('wA');

    // The approval dialog closes; focus goes back to A again, then the held move runs.
    closeModal();
    pending.flush();
    gate.release();
    await drain();

    expect(focused).toBe('wB');
  });
});

describe('panelReturnTarget: where focus goes when a panel closes', () => {
  type Panel = 'chat' | 'plans' | 'settings';
  interface Element {
    id: string;
  }

  const insidePlans: Element = { id: 'plans-row' };
  const insideChat: Element = { id: 'chat-input' };

  /**
   * The decision for closing `closing`, with the panels in `open` still open,
   * the elements in `connected` still in the DOM, and `recentPanels` the open
   * panels in the order they last held focus (most recent first).
   */
  function decide(options: {
    origin: PanelOrigin<Panel, Element>;
    closing: Panel;
    open: Panel[];
    connected?: Element[];
    recentPanels: Panel[];
    current?: { windowId: string } | null;
  }): PanelReturn<Panel, Element> {
    const connected = options.connected ?? [insidePlans, insideChat];
    return panelReturnTarget<Panel, Element>({
      origin: options.origin,
      closing: options.closing,
      isOpen: (panel: Panel) => options.open.includes(panel),
      isConnected: (element: Element) => connected.includes(element),
      recentPanels: options.recentPanels,
      current: options.current === undefined ? CURRENT : options.current,
    });
  }

  describe('the panel was opened from inside another panel', () => {
    it('goes back to that element while its panel is still open and the element is in the DOM', () => {
      expect(
        decide({
          origin: { kind: 'panel', panel: 'plans', element: insidePlans },
          closing: 'settings',
          open: ['plans', 'settings', 'chat'],
          recentPanels: ['settings', 'chat', 'plans'],
        })
      ).toEqual({ kind: 'element', element: insidePlans });
    });

    it('goes back to the element even when there is no current window', () => {
      expect(
        decide({
          origin: { kind: 'panel', panel: 'chat', element: insideChat },
          closing: 'plans',
          open: ['chat'],
          recentPanels: [],
          current: null,
        })
      ).toEqual({ kind: 'element', element: insideChat });
    });

    it('does not go back to an element of a panel that has closed since; the most recent other open panel takes over', () => {
      expect(
        decide({
          origin: { kind: 'panel', panel: 'plans', element: insidePlans },
          closing: 'settings',
          open: ['settings', 'chat'],
          recentPanels: ['settings', 'chat'],
        })
      ).toEqual({ kind: 'panel', panel: 'chat' });
    });

    it('does not go back to an element that has left the DOM', () => {
      expect(
        decide({
          origin: { kind: 'panel', panel: 'plans', element: insidePlans },
          closing: 'settings',
          open: ['plans', 'settings'],
          connected: [insideChat],
          recentPanels: ['settings'],
        })
      ).toEqual({ kind: 'move', move: restoreFocusMove(CURRENT) });
    });

    it('treats an origin inside the closing panel itself as not another panel', () => {
      expect(
        decide({
          origin: { kind: 'panel', panel: 'plans', element: insidePlans },
          closing: 'plans',
          open: ['plans', 'chat'],
          recentPanels: ['plans', 'chat'],
        })
      ).toEqual({ kind: 'panel', panel: 'chat' });
    });
  });

  describe('the panel was opened from a place yagura carries focus to (a terminal or a sidebar row)', () => {
    it.each<[string, { windowId: string } | null]>([
      ['with a current window', CURRENT],
      ['without a current window', null],
    ])(
      'goes back through restoreFocusMove %s, even when another panel is open',
      (_label, current) => {
        expect(
          decide({
            origin: { kind: 'carried' },
            closing: 'plans',
            open: ['plans', 'chat'],
            recentPanels: ['plans', 'chat'],
            current,
          })
        ).toEqual({ kind: 'move', move: restoreFocusMove(current) });
      }
    );
  });

  describe('the panel was opened from anywhere else (a rail button, the body)', () => {
    it('goes to the first panel of the focus order other than the closing one', () => {
      expect(
        decide({
          origin: { kind: 'other' },
          closing: 'plans',
          open: ['plans', 'chat', 'settings'],
          recentPanels: ['plans', 'chat', 'settings'],
        })
      ).toEqual({ kind: 'panel', panel: 'chat' });
      expect(
        decide({
          origin: { kind: 'other' },
          closing: 'chat',
          open: ['chat', 'settings', 'plans'],
          recentPanels: ['settings', 'chat', 'plans'],
        })
      ).toEqual({ kind: 'panel', panel: 'settings' });
    });

    it('goes back to the lower panel when two panels were stacked from the rail and the upper one closes', () => {
      // Chat was opened from the rail and focused; the rail click that opened
      // Plans moved focus out of Chat, which leaves Chat in the focus order.
      // Plans then took focus, so it heads the order.
      expect(
        decide({
          origin: { kind: 'other' },
          closing: 'plans',
          open: ['chat', 'plans'],
          recentPanels: ['plans', 'chat'],
        })
      ).toEqual({ kind: 'panel', panel: 'chat' });
    });

    it.each<[string, Panel[]]>([
      ['the focus order is empty', []],
      ['the focus order holds only the closing panel', ['plans']],
    ])('falls back to restoreFocusMove when %s', (_label, recentPanels) => {
      const open: Panel[] = ['plans', 'chat'];
      expect(decide({ origin: { kind: 'other' }, closing: 'plans', open, recentPanels })).toEqual({
        kind: 'move',
        move: restoreFocusMove(CURRENT),
      });
      expect(
        decide({ origin: { kind: 'other' }, closing: 'plans', open, recentPanels, current: null })
      ).toEqual({ kind: 'move', move: restoreFocusMove(null) });
    });

    it('follows the current window of the moment instead of a remembered one', () => {
      expect(
        decide({
          origin: { kind: 'other' },
          closing: 'plans',
          open: ['plans'],
          recentPanels: ['plans'],
          current: OTHER,
        })
      ).toEqual({ kind: 'move', move: restoreFocusMove(OTHER) });
    });
  });
});
