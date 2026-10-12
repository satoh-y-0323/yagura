/**
 * Where keyboard focus goes when it crosses the sidebar boundary, and how a
 * requested move waits for the open modal dialogs to finish. Decisions only:
 * the caller performs the moves.
 */

/** A place keyboard focus can be sent to. */
export type FocusMove =
  | { kind: 'terminal'; windowId: string }
  | { kind: 'sidebar-row'; windowId: string }
  | { kind: 'sidebar-start' }
  | { kind: 'none' };

/**
 * The move for the sidebar round trip. It keeps no state: the answer depends
 * only on whether focus is inside the sidebar now and on the current window.
 *
 * @param isRowShown whether the sidebar renders a row for the given window id
 * (asked only about the current window).
 */
export function sidebarToggleMove(
  focusInSidebar: boolean,
  current: { windowId: string } | null,
  isRowShown: (windowId: string) => boolean
): FocusMove {
  if (focusInSidebar) {
    return current === null ? { kind: 'none' } : { kind: 'terminal', windowId: current.windowId };
  }
  if (current !== null && isRowShown(current.windowId)) {
    return { kind: 'sidebar-row', windowId: current.windowId };
  }
  return { kind: 'sidebar-start' };
}

/** Where focus returns to after a panel or window operation: the current window, else the sidebar. */
export function restoreFocusMove(current: { windowId: string } | null): FocusMove {
  return current === null
    ? { kind: 'sidebar-start' }
    : { kind: 'terminal', windowId: current.windowId };
}

/** Where focus was when a panel opened. */
export type PanelOrigin<P, E> =
  /** An element inside another panel. */
  | { kind: 'panel'; panel: P; element: E }
  /** A window's terminal or a sidebar row: places yagura carries focus to itself. */
  | { kind: 'carried' }
  /** Anywhere else, such as a rail button or the body. */
  | { kind: 'other' };

/** Where focus goes when a panel closes. */
export type PanelReturn<P, E> =
  { kind: 'element'; element: E } | { kind: 'panel'; panel: P } | { kind: 'move'; move: FocusMove };

/**
 * Where focus goes when `closing` closes. The origin wins while it is still
 * reachable; otherwise the open panel that most recently held focus, so closing
 * the upper of two stacked panels lands in the lower one.
 *
 * @param input.recentPanels the open panels, most recently focused first
 * @param input.current the current window at the moment of closing
 */
export function panelReturnTarget<P, E>(input: {
  origin: PanelOrigin<P, E>;
  closing: P;
  isOpen: (panel: P) => boolean;
  isConnected: (element: E) => boolean;
  recentPanels: readonly P[];
  current: { windowId: string } | null;
}): PanelReturn<P, E> {
  const { origin, closing, isOpen } = input;
  if (
    origin.kind === 'panel' &&
    origin.panel !== closing &&
    isOpen(origin.panel) &&
    input.isConnected(origin.element)
  ) {
    return { kind: 'element', element: origin.element };
  }
  if (origin.kind !== 'carried') {
    const lower = input.recentPanels.find((panel) => panel !== closing && isOpen(panel));
    if (lower !== undefined) {
      return { kind: 'panel', panel: lower };
    }
  }
  return { kind: 'move', move: restoreFocusMove(input.current) };
}

/** What {@link createPendingFocus} needs from its host. */
export interface PendingFocusDeps {
  /** Whether any modal dialog is open right now. */
  isModalOpen: () => boolean;
  /** Resolves once the view has rendered what the move targets. */
  settle: () => Promise<void>;
  /** Performs the move. */
  run: (move: FocusMove) => void;
}

/**
 * Holds one requested focus move and runs it once no modal dialog is open.
 * Modals open in a chain, and the browser restores focus each time one closes,
 * so a move run at the first close would be undone by the next restore.
 */
export function createPendingFocus(deps: PendingFocusDeps): {
  request(move: FocusMove): void;
  flush(): void;
} {
  let held: FocusMove | null = null;

  const attempt = async (): Promise<void> => {
    await deps.settle();
    if (held === null || deps.isModalOpen()) {
      return;
    }
    const move = held;
    held = null;
    deps.run(move);
  };

  return {
    /** Replaces the held move with `move` and tries to run it. */
    request(move: FocusMove): void {
      held = move;
      void attempt();
    },
    /** Tries to run the held move; call when a modal dialog may have closed. */
    flush(): void {
      void attempt();
    },
  };
}
