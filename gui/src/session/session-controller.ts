/**
 * Drives one window's session: asks the core to attach it, keeps that window's
 * store in step with the core's link reports, applies the frames the daemon
 * sends to the window's grid and forwards the user's input.
 *
 * The grid lives here rather than in the pane, because the controller
 * outlives it: a frame that arrives while no pane is mounted is applied and
 * acknowledged all the same, so the daemon's pacing never waits on a pane.
 * The view only draws the grid ([`TerminalSink`]) and hands input over.
 *
 * It does not subscribe to events itself: the subscription is taken once for
 * the application and routed by window id, so the controller exposes its
 * handlers instead of installing them. It also does not open or tear down the
 * connection to the daemon, nor restore what a new connection forgot: the
 * Tauri core owns that lifecycle and the window's state, so everything the
 * view asks for is handed to the core whatever the phase.
 */

import { get } from 'svelte/store';

import { isValidSize, type Frame, type WindowLinkPayload } from '../ipc/session-boundary';
import {
  invokeAttachWindow,
  invokeClearWindowSelection,
  invokeCopyWindowSelection,
  invokeResize,
  invokeSendWindowInput,
  invokeSetWindowPalette,
  invokeSetWindowVisible,
  type WindowEventHandlers,
  type WindowInput,
  type WirePalette,
} from '../ipc/session-client';
import { isSessionInteractive, type SessionStore } from '../stores/session-store';
import type { DirtyRows } from '../terminal/canvas-renderer';
import { applyFrame, createGrid, type TerminalGrid } from '../terminal/frame-grid';
import { canApplyTerminalSize } from './size-report';

/** Measured size of the terminal view, in character cells. */
export interface TerminalSize {
  /** Number of columns. */
  cols: number;
  /** Number of rows. */
  rows: number;
}

/** What a controller needs from the terminal view. */
export interface TerminalSink {
  /** The grid changed in `rows`; draw them on the next frame. */
  invalidate: (rows: DirtyRows) => void;
  /** Measures the view and reports the size it fits, or `null` when none fits. */
  measure: () => TerminalSize | null;
  /** Moves keyboard focus into the terminal. */
  focus: () => void;
  /** Whether the view's pane is displayed. */
  isVisible: () => boolean;
}

/** Session lifecycle for a single terminal pane. */
export class SessionController {
  /** What the window shows, as the frames applied so far left it. */
  private grid: TerminalGrid = createGrid();
  private sink: TerminalSink | null = null;
  private isRecovering = false;
  /** Whether the pane is on screen now, so the same value is not sent twice. */
  private isShown = true;
  /** The epoch of the last link report applied; an older one is stale. */
  private appliedEpoch = -1;

  constructor(
    /** The window this controller drives. */
    readonly windowId: string,
    private readonly store: SessionStore
  ) {}

  /** The grid the view draws. */
  currentGrid(): TerminalGrid {
    return this.grid;
  }

  /** Connects the view that renders this session. */
  setSink(sink: TerminalSink): void {
    this.sink = sink;
  }

  /**
   * The handlers for this window's events, to be registered with the window
   * registry so the single subscription can route to them.
   */
  eventHandlers(): WindowEventHandlers {
    return {
      onFrame: (frame) => this.applyFrame(frame),
      onExit: (payload) => {
        this.store.markExited(payload.exit_code);
      },
      onLink: (payload) => {
        this.applyLink(payload);
      },
    };
  }

  /**
   * Asks the core to attach the window at the size the terminal is drawn at,
   * so the daemon lays the screen out for it before sending it as frames. How
   * it went arrives as link reports; a window another client took over is
   * left to it.
   */
  async start(): Promise<void> {
    if (get(this.store).phase === 'superseded') {
      return;
    }
    try {
      await invokeAttachWindow(this.windowId, this.reportableSize());
    } catch (error: unknown) {
      console.warn(`failed to attach ${this.windowId}:`, error);
    }
  }

  /** Forwards input to the window; whether it is held while the window reconnects is the core's call. */
  sendInput(input: WindowInput): void {
    this.reportQuietly(invokeSendWindowInput(this.windowId, input), 'forward the input');
  }

  /** The text selected in the window, `null` when nothing is. */
  async copySelection(): Promise<string | null> {
    return invokeCopyWindowSelection(this.windowId);
  }

  /** Clears the selection in the window, unless input is currently disabled. */
  clearSelection(): void {
    if (!isSessionInteractive(get(this.store))) {
      return;
    }
    this.reportQuietly(invokeClearWindowSelection(this.windowId), 'clear the selection');
  }

  /** Gives the core the colors the window is drawn with. */
  setPalette(palette: WirePalette): void {
    this.reportQuietly(invokeSetWindowPalette(this.windowId, palette), 'send the palette');
  }

  /**
   * Tells the core when the window goes on or off screen (its pane hidden,
   * or the whole app hidden), so the daemon builds no frames for it meanwhile.
   */
  setVisible(isVisible: boolean): void {
    if (this.isShown === isVisible) {
      return;
    }
    this.isShown = isVisible;
    this.reportQuietly(invokeSetWindowVisible(this.windowId, this.isShown), 'send the visibility');
  }

  /** Moves keyboard focus into this window's terminal; a no-op while none is connected. */
  focusTerminal(): void {
    this.sink?.focus();
  }

  /** Measures the terminal and reports the size to the PTY, when it is one a window may take. */
  reportMeasuredSize(): void {
    const size = this.reportableSize();
    if (size === null) {
      return;
    }
    this.reportQuietly(invokeResize(this.windowId, size.cols, size.rows), 'report the size');
  }

  /** Releases the view; the connection itself is the core's to close. */
  dispose(): void {
    this.sink = null;
  }

  /**
   * The terminal's measured size when the size gate lets it through, `null`
   * otherwise: every size this controller hands the core passes the gate here.
   */
  private reportableSize(): TerminalSize | null {
    if (this.sink === null) {
      return null;
    }
    const size = this.sink.measure();
    if (size === null || !isValidSize(size)) {
      return null;
    }
    return canApplyTerminalSize(this.sink.isVisible(), size) ? size : null;
  }

  /**
   * Applies a link report newer than the last one applied. A session that
   * ended (exited or superseded) stays ended.
   */
  private applyLink(payload: WindowLinkPayload): void {
    const { phase } = get(this.store);
    if (payload.epoch <= this.appliedEpoch || phase === 'exited' || phase === 'superseded') {
      return;
    }
    this.appliedEpoch = payload.epoch;
    switch (payload.state) {
      case 'connecting':
        this.store.markConnecting();
        break;
      case 'attached':
        this.store.markAttached();
        break;
      case 'lost':
        this.store.markFailed(payload.reason);
        break;
      case 'superseded':
        this.store.markSuperseded();
        break;
    }
  }

  /**
   * Applies one frame part, drawn or not, and returns the number of the frame
   * it completed, which the caller acknowledges; `null` for a part that does
   * not complete one. A part that breaks the wire rules leaves the grid
   * unusable, so the window is attached again, which starts over from a whole
   * frame.
   */
  private applyFrame(frame: Frame): number | null {
    const cursorRow = this.grid.cursor.row;
    let result = applyFrame(this.grid, frame);
    if (!result.ok && frame.reset) {
      // A reconnect starts a new view while the old one left a frame open.
      this.grid = createGrid();
      result = applyFrame(this.grid, frame);
    }
    if (!result.ok) {
      console.error(`frame ${frame.seq} of ${this.windowId} refused: ${result.error}`);
      this.grid = createGrid();
      this.sink?.invalidate('all');
      this.recover();
      return null;
    }
    if (frame.reset) {
      this.sink?.invalidate('all');
    } else {
      this.sink?.invalidate([cursorRow, ...frame.lines.map((line) => line.row)]);
    }
    return result.ack;
  }

  private recover(): void {
    if (this.isRecovering || get(this.store).phase === 'superseded') {
      return;
    }
    this.isRecovering = true;
    void this.start().finally(() => {
      this.isRecovering = false;
    });
  }

  /**
   * Logs a rejected command: the phase is the core's link reports' to set, so
   * a refusal changes nothing here.
   */
  private reportQuietly(operation: Promise<void>, what: string): void {
    operation.catch((error: unknown) => {
      console.warn(`failed to ${what} for ${this.windowId}:`, error);
    });
  }
}
