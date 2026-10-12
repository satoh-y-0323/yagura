/**
 * Drives one window's session: runs the attach handshake, keeps that window's
 * store in sync, applies the frames the daemon sends to the window's grid and
 * forwards the user's input.
 *
 * The grid lives here rather than in the pane, because the controller
 * outlives it: a frame that arrives while no pane is mounted is applied and
 * acknowledged all the same, so the daemon's pacing never waits on a pane.
 * The view only draws the grid ([`TerminalSink`]) and hands input over.
 *
 * It does not subscribe to events itself: the subscription is taken once for
 * the application and routed by window id, so the controller exposes its
 * handlers instead of installing them. It also does not open or
 * tear down the connection to the daemon; the Tauri core owns that lifecycle.
 */

import { get } from 'svelte/store';

import { isValidSize, type Frame } from '../ipc/session-boundary';
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
}

/** Session lifecycle for a single terminal pane. */
export class SessionController {
  /** What the window shows, as the frames applied so far left it. */
  private grid: TerminalGrid = createGrid();
  private sink: TerminalSink | null = null;
  private isSuperseded = false;
  private isRecovering = false;
  private lastSentSize: TerminalSize | null = null;
  private palette: WirePalette | null = null;
  /** Whether the window is on screen; the daemon assumes so on every attach. */
  private isShown = true;

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
        this.handleExit(payload.exit_code);
      },
      onDisconnected: (payload) => {
        this.store.markFailed(payload.reason);
      },
      onSuperseded: () => {
        this.isSuperseded = true;
        this.store.markSuperseded();
      },
    };
  }

  /**
   * Runs the attach handshake; the window's screen follows as frames.
   * Failures land in the store as `failed`, which is what the retry banner
   * reacts to.
   */
  async start(): Promise<void> {
    this.store.markConnecting();
    try {
      // The attach names the size the terminal is drawn at, so the daemon lays
      // the screen out for it before sending it.
      const size = this.measuredSize();
      const response = await invokeAttachWindow(this.windowId, size);
      if (response.child_exited) {
        // The daemon does not resize a window whose child has exited.
        this.handleExit(response.exit_code);
      } else {
        this.lastSentSize = size;
        this.store.markAttached();
      }
      this.sendPalette();
      if (!this.isShown) {
        this.sendVisibility();
      }
    } catch (error) {
      this.store.markFailed(describeError(error));
    }
  }

  /**
   * Retries a failed connection. A superseded session never reconnects: the
   * client that took over owns the window now.
   */
  async retry(): Promise<void> {
    if (this.isSuperseded) {
      return;
    }
    await this.start();
  }

  /** Forwards input to the window, unless input is currently disabled. */
  sendInput(input: WindowInput): void {
    if (!isSessionInteractive(get(this.store))) {
      return;
    }
    this.report(invokeSendWindowInput(this.windowId, input));
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
    this.report(invokeClearWindowSelection(this.windowId));
  }

  /** Records the colors the window is drawn with and gives them to the daemon. */
  setPalette(palette: WirePalette): void {
    this.palette = palette;
    this.sendPalette();
  }

  /**
   * Tells the daemon when the window goes on or off screen (its pane hidden,
   * or the whole app hidden), so it builds no frames for it meanwhile.
   */
  setVisible(isVisible: boolean): void {
    if (this.isShown === isVisible) {
      return;
    }
    this.isShown = isVisible;
    this.sendVisibility();
  }

  /** Moves keyboard focus into this window's terminal; a no-op while none is connected. */
  focusTerminal(): void {
    this.sink?.focus();
  }

  /** Reports a new terminal size to the PTY. */
  notifyResize(size: TerminalSize): void {
    if (!isSessionInteractive(get(this.store))) {
      return;
    }
    if (!isValidSize(size)) {
      return;
    }
    if (this.lastSentSize?.cols === size.cols && this.lastSentSize.rows === size.rows) {
      return;
    }
    this.lastSentSize = size;
    this.report(invokeResize(this.windowId, size.cols, size.rows));
  }

  /** Releases the view; the connection itself is the core's to close. */
  dispose(): void {
    this.sink = null;
  }

  /** The terminal's measured size, or `null` when there is none a window can take. */
  private measuredSize(): TerminalSize | null {
    const size = this.sink?.measure() ?? null;
    return size !== null && isValidSize(size) ? size : null;
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
      // The first frame of a view: a daemon that came back has no palette yet.
      this.sendPalette();
      this.sink?.invalidate('all');
    } else {
      this.sink?.invalidate([cursorRow, ...frame.lines.map((line) => line.row)]);
    }
    return result.ack;
  }

  private recover(): void {
    if (this.isRecovering || this.isSuperseded) {
      return;
    }
    this.isRecovering = true;
    void this.start().finally(() => {
      this.isRecovering = false;
    });
  }

  private sendPalette(): void {
    if (this.palette === null) {
      return;
    }
    if (!isSessionInteractive(get(this.store))) {
      return;
    }
    this.reportQuietly(invokeSetWindowPalette(this.windowId, this.palette), 'send the palette');
  }

  private sendVisibility(): void {
    if (!isSessionInteractive(get(this.store))) {
      return;
    }
    this.reportQuietly(invokeSetWindowVisible(this.windowId, this.isShown), 'send the visibility');
  }

  /** The single termination path: a live `exit` and re-attaching to an already
   * exited child are reported the same way. */
  private handleExit(exitCode: number | null): void {
    this.store.markExited(exitCode);
  }

  /**
   * Logs a rejected command that only follows the connection: when it fails
   * the connection is gone, which the disconnect or exit path reports.
   */
  private reportQuietly(operation: Promise<void>, what: string): void {
    operation.catch((error: unknown) => {
      console.warn(`failed to ${what} for ${this.windowId}:`, error);
    });
  }

  /** Surfaces a rejected fire-and-forget command through the banner. */
  private report(operation: Promise<void>): void {
    operation.catch((error: unknown) => {
      this.store.markFailed(describeError(error));
    });
  }
}

/** Tauri command errors arrive as the string the Rust side returned. */
function describeError(error: unknown): string {
  if (typeof error === 'string') {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return JSON.stringify(error) ?? 'unexpected error';
}
