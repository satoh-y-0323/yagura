/**
 * The DOM side of one window's terminal: a canvas the grid is drawn on and a
 * hidden textarea that takes the keyboard and the input method. It turns the
 * DOM's events into the input messages the daemon encodes, and measures how
 * many cells the pane fits. It interprets nothing the window wrote: the
 * controller's grid is drawn as it is.
 */
import type { TerminalTheme } from '../components/terminal-theme';
import type { WindowInput } from '../ipc/session-client';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';
import type { SessionController, TerminalSize } from '../session/session-controller';
import { CanvasRenderer, type TerminalFont } from './canvas-renderer';
import { layoutComposition } from './composition';
import { hasSelection } from './frame-grid';
import { textOfInputEvent, type Modifiers } from './key-input';
import { cellsThatFit, inputPlacement } from './pane-fit';
import { cellAtPoint, type CellPosition } from './pointer-input';
import { PointerTracker, isMousePointer } from './pointer-tracker';
import { TextFlow } from './text-flow';
import { keyDownEffect, pasteInput, textInput, wheelInput } from './window-input';

/** What the view needs from the pane around it. */
export interface TerminalViewOptions {
  controller: SessionController;
  platform: KeyboardPlatform;
  font: TerminalFont;
  theme: TerminalTheme;
  /** A clipboard chord was pressed. */
  onClipboard: (action: 'copy' | 'paste') => void;
  /**
   * Whether a right button press goes to the window; `false` leaves it to
   * the pane's own right-click handling.
   */
  passesRightPress: (event: MouseEvent) => boolean;
}

function modifiersOf(event: MouseEvent | KeyboardEvent): Modifiers {
  return { shift: event.shiftKey, alt: event.altKey, ctrl: event.ctrlKey };
}

export class TerminalView {
  private readonly renderer: CanvasRenderer;
  private readonly controller: SessionController;
  private readonly removers: (() => void)[] = [];
  private readonly pointer = new PointerTracker(() => this.appCapturesMouse());
  private readonly text = new TextFlow();
  private isVisible = true;
  private isSelectingPress = false;

  constructor(
    private readonly root: HTMLElement,
    canvas: HTMLCanvasElement,
    private readonly input: HTMLTextAreaElement,
    private readonly options: TerminalViewOptions
  ) {
    this.controller = options.controller;
    this.renderer = new CanvasRenderer(
      canvas,
      () => this.controller.currentGrid(),
      options.font,
      options.theme
    );
    this.listen(input, 'keydown', (event) => {
      this.handleKeyDown(event);
    });
    this.listen(input, 'input', (event) => {
      this.handleInput(event);
    });
    this.listen(input, 'compositionstart', () => {
      this.text.startComposition();
    });
    this.listen(input, 'compositionupdate', (event) => {
      const grid = this.controller.currentGrid();
      this.renderer.setComposition(layoutComposition(grid.cursor, event.data, grid.cols));
    });
    this.listen(input, 'compositionend', (event) => {
      const committed = this.text.endComposition(event.data);
      this.renderer.setComposition(null);
      this.input.value = '';
      this.send(textInput(committed));
    });
    this.listen(input, 'paste', (event) => {
      event.preventDefault();
      const text = event.clipboardData?.getData('text/plain') ?? '';
      this.paste(text);
    });
    // macOS copies through its app menu, whose copy event lands here.
    this.listen(input, 'copy', (event) => {
      event.preventDefault();
      this.options.onClipboard('copy');
    });
    this.listen(input, 'focus', () => {
      this.renderer.setFocused(true);
      this.send({ type: 'focus', focused: true });
    });
    this.listen(input, 'blur', () => {
      this.renderer.setFocused(false);
      this.send({ type: 'focus', focused: false });
    });
    // Read before anything this press triggers (raising the window) moves the focus.
    const noteSelectingPress = (event: PointerEvent) => {
      if (event.target instanceof Node && root.contains(event.target)) {
        this.isSelectingPress = document.activeElement !== this.input;
      }
    };
    window.addEventListener('pointerdown', noteSelectingPress, { capture: true });
    this.removers.push(() => {
      window.removeEventListener('pointerdown', noteSelectingPress, { capture: true });
    });
    this.listen(root, 'mousedown', (event) => {
      this.handleMouseDown(event);
    });
    this.listen(root, 'pointerdown', (event) => {
      if (isMousePointer(event.pointerType)) root.setPointerCapture(event.pointerId);
    });
    this.listen(root, 'pointermove', (event) => {
      this.handlePointerMove(event);
    });
    this.listen(root, 'pointerup', (event) => {
      this.handlePointerUp(event);
    });
    this.listen(root, 'wheel', (event) => {
      this.handleWheel(event);
    });

    this.controller.setSink({
      invalidate: (rows) => {
        this.renderer.invalidate(rows);
        this.placeInput();
      },
      measure: () => this.measure(),
      focus: () => {
        this.input.focus();
      },
    });
    // The grid may already hold frames applied while no pane was mounted.
    this.renderer.invalidate('all');
    this.placeInput();
  }

  /** Draws nothing while the window is not shown, and everything again once it is. */
  setVisible(isVisible: boolean): void {
    this.isVisible = isVisible;
    this.renderer.setVisible(isVisible);
  }

  setTheme(theme: TerminalTheme): void {
    this.renderer.setTheme(theme);
  }

  /**
   * The size the pane fits, in cells, or `null` while it is not shown. Also
   * picks up a change of the device pixel ratio.
   */
  measure(): TerminalSize | null {
    this.renderer.remeasureIfScaled();
    const { cellWidth, cellHeight } = this.renderer.metrics();
    const style = getComputedStyle(this.root);
    return cellsThatFit(
      {
        isVisible: this.isVisible,
        clientWidth: this.root.clientWidth,
        clientHeight: this.root.clientHeight,
        paddingLeft: style.paddingLeft,
        paddingRight: style.paddingRight,
        paddingTop: style.paddingTop,
        paddingBottom: style.paddingBottom,
      },
      { cellWidth, cellHeight }
    );
  }

  focus(): void {
    this.input.focus();
  }

  /** Sends `text` as a paste; nothing for empty text. */
  paste(text: string): void {
    this.send(pasteInput(text));
  }

  /** Whether the program in the window asked for mouse reports. */
  appCapturesMouse(): boolean {
    return this.controller.currentGrid().view.mouse_reporting;
  }

  /** Whether any cell shown is selected. */
  hasSelection(): boolean {
    return hasSelection(this.controller.currentGrid());
  }

  dispose(): void {
    for (const remove of this.removers) remove();
    this.removers.length = 0;
    this.renderer.dispose();
  }

  private listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    type: K,
    handler: (event: HTMLElementEventMap[K]) => void
  ): void {
    target.addEventListener(type, handler);
    this.removers.push(() => {
      target.removeEventListener(type, handler);
    });
  }

  private send(input: WindowInput | null): void {
    if (input !== null) this.controller.sendInput(input);
  }

  /** Keeps the textarea on the cursor's cell, where the input method opens its window. */
  private placeInput(): void {
    const placement = inputPlacement(this.renderer.cursorPosition(), this.renderer.metrics());
    this.input.style.left = placement.left;
    this.input.style.top = placement.top;
    this.input.style.width = placement.width;
    this.input.style.height = placement.height;
  }

  private handleKeyDown(event: KeyboardEvent): void {
    const effect = keyDownEffect(event, this.options.platform);
    if (effect.preventsDefault) event.preventDefault();
    if (effect.clipboard !== null) this.options.onClipboard(effect.clipboard);
    this.send(effect.input);
  }

  private handleInput(event: Event): void {
    if (!(event instanceof InputEvent)) return;
    const { clearsTextarea, send } = this.text.input(textOfInputEvent(event));
    if (clearsTextarea) {
      this.input.value = '';
    }
    this.send(textInput(send));
  }

  private cellOf(event: MouseEvent): CellPosition {
    const rect = this.root.getBoundingClientRect();
    const grid = this.controller.currentGrid();
    return cellAtPoint(
      { x: event.clientX - rect.left, y: event.clientY - rect.top },
      this.renderer.metrics(),
      { cols: grid.cols, rows: grid.rows }
    );
  }

  private handleMouseDown(event: MouseEvent): void {
    // Keeps the focus in the textarea rather than moving it to the canvas.
    event.preventDefault();
    this.input.focus();
    if (this.isSelectingPress && this.pointer.swallowSelectingPress()) return;
    this.send(
      this.pointer.down(
        { button: event.button, detail: event.detail, mods: modifiersOf(event) },
        () => this.options.passesRightPress(event),
        () => this.cellOf(event)
      )
    );
  }

  private handlePointerMove(event: PointerEvent): void {
    this.send(
      this.pointer.move(
        { pointerType: event.pointerType, buttons: event.buttons, mods: modifiersOf(event) },
        () => this.cellOf(event)
      )
    );
  }

  private handlePointerUp(event: PointerEvent): void {
    if (isMousePointer(event.pointerType) && this.root.hasPointerCapture(event.pointerId)) {
      this.root.releasePointerCapture(event.pointerId);
    }
    this.send(
      this.pointer.up(
        { pointerType: event.pointerType, button: event.button, mods: modifiersOf(event) },
        () => this.cellOf(event)
      )
    );
  }

  private handleWheel(event: WheelEvent): void {
    event.preventDefault();
    this.send(wheelInput(event, () => this.cellOf(event), modifiersOf(event)));
  }
}
