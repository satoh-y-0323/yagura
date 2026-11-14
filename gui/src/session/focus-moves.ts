/**
 * Where keyboard focus goes when it crosses the sidebar boundary, when a panel
 * or a modal dialog closes and when a window comes to the front, and how a
 * requested move waits for the open modal dialogs to finish. Decisions only:
 * the caller reads the DOM and performs the moves.
 */

/** A place keyboard focus can be sent to. */
export type FocusMove =
  | { kind: 'terminal'; windowId: string }
  | { kind: 'sidebar-row'; windowId: string }
  | { kind: 'sidebar-start' }
  /** {@link restoreFocusMove} if, when the move runs, focus has no input target ({@link lostFocusMove}). */
  | { kind: 'current-if-lost' }
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

/** Where focus was when a modal dialog opened. */
export interface ModalOrigin<P, E> {
  /** The focused element, or null when focus was on the body. */
  element: E | null;
  /** The panel holding focus then; the last focused one when focus was on the body. */
  panel: P | null;
}

/**
 * Where focus goes when a modal dialog closes: to the next thing the user types
 * into, whether the dialog was confirmed or cancelled. A dialog about the front
 * window goes to its terminal; otherwise back to where the user was, then to the
 * panel that held focus, then through {@link restoreFocusMove}.
 *
 * @param input.subject the window the dialog was about, or null
 * @param input.current the current window at the moment of closing
 * @param input.isUsable whether an element is still an input target ({@link isInputTarget})
 */
export function modalReturnTarget<P, E>(input: {
  subject: string | null;
  origin: ModalOrigin<P, E>;
  current: { windowId: string } | null;
  isUsable: (element: E) => boolean;
  isOpen: (panel: P) => boolean;
}): PanelReturn<P, E> {
  const { subject, origin, current } = input;
  if (subject !== null && current !== null && current.windowId === subject) {
    return { kind: 'move', move: { kind: 'terminal', windowId: subject } };
  }
  if (origin.element !== null && input.isUsable(origin.element)) {
    return { kind: 'element', element: origin.element };
  }
  if (origin.panel !== null && input.isOpen(origin.panel)) {
    return { kind: 'panel', panel: origin.panel };
  }
  return { kind: 'move', move: restoreFocusMove(current) };
}

/**
 * Where focus goes when a window comes to the front. Only a window the user
 * chose takes the keyboard: a push or a poll never moves it, so keys typed
 * meanwhile keep reaching the CLI they were meant for. A control that was
 * pressed owns its own focus.
 *
 * @param input.target the one window the user made the target, or null when
 * the operation has none (minimize, close, restore)
 */
export function windowActivationMove(input: {
  byUser: boolean;
  target: string | null;
  pressOnControl: boolean;
}): FocusMove {
  if (!input.byUser || input.pressOnControl) {
    return { kind: 'none' };
  }
  if (input.target !== null) {
    return { kind: 'terminal', windowId: input.target };
  }
  return { kind: 'current-if-lost' };
}

/** What the view reads about one focused element. */
export interface FocusFacts {
  onBody: boolean;
  connected: boolean;
  /** Whether it is drawn: false inside a `display: none` ancestor, such as a minimized window. */
  rendered: boolean;
  disabled: boolean;
  /** Whether it is one of a window's operation buttons, whose target can change under it. */
  onWindowControl: boolean;
}

/**
 * Whether focus has somewhere to type into: a visible, enabled element that is
 * not a window control. A window control does not count because Space or Enter
 * on it would run the operation again, possibly on another window.
 */
export function isInputTarget(focus: FocusFacts): boolean {
  return (
    !focus.onBody && focus.connected && focus.rendered && !focus.disabled && !focus.onWindowControl
  );
}

/** The move for `current-if-lost`: back through {@link restoreFocusMove} only when focus has no input target. */
export function lostFocusMove(focus: FocusFacts, current: { windowId: string } | null): FocusMove {
  return isInputTarget(focus) ? { kind: 'none' } : restoreFocusMove(current);
}

/** What {@link createPendingFocus} needs from its host. */
export interface PendingFocusDeps<M> {
  /** Whether any modal dialog is open right now. */
  isModalOpen: () => boolean;
  /** Resolves once the view has rendered what the move targets. */
  settle: () => Promise<void>;
  /** Performs the move. */
  run: (move: M) => void;
}

/**
 * Holds one requested focus move and runs it once no modal dialog is open.
 * Modals open in a chain, and the browser restores focus each time one closes,
 * so a move run at the first close would be undone by the next restore.
 */
export function createPendingFocus<M>(deps: PendingFocusDeps<M>): {
  request(move: M): void;
  offer(move: M): void;
} {
  let held: M | null = null;

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
    request(move: M): void {
      held = move;
      void attempt();
    },
    /**
     * Holds `move` only when nothing is held, then tries to run what is held. A
     * closing modal offers its own return target, which loses to a move asked
     * for while it was open and to the offer of a modal that closed before it.
     */
    offer(move: M): void {
      held ??= move;
      void attempt();
    },
  };
}
