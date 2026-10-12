/**
 * Turns the pointer events of a window's view into the pointer messages the
 * daemon encodes, and remembers what the next event needs: the last cell sent
 * and whether a right press went to the window.
 */
import type { WindowInput } from '../ipc/session-client';
import type { Modifiers } from './key-input';
import { shouldSendMove, type CellPosition } from './pointer-input';

/** The pointer buttons the wire names, by `MouseEvent.button`. */
const BUTTONS = ['left', 'middle', 'right'] as const;
type WireButton = (typeof BUTTONS)[number];

const RIGHT_BUTTON = 2;
/** `MouseEvent.buttons` bits of the left, right and middle buttons, in the order they are tried. */
const HELD_BITS: readonly (readonly [number, WireButton])[] = [
  [1, 'left'],
  [4, 'middle'],
  [2, 'right'],
];
const MAX_CLICKS = 255;

/** Whether `pointerType` is a mouse; touch and pen are not sent. */
export function isMousePointer(pointerType: string): boolean {
  return pointerType === 'mouse';
}

export class PointerTracker {
  private lastSentCell: CellPosition | null = null;
  private isRightPassed = false;
  private isPressSwallowed = false;

  /** `capturesMouse` tells whether the program in the window asked for mouse reports. */
  constructor(private readonly capturesMouse: () => boolean) {}

  /**
   * A button went down. A right press is sent only when `passesRight` says the
   * window gets it; `clicks` is the browser's click count, kept to 1-255.
   */
  down(
    event: { button: number; detail: number; mods: Modifiers },
    passesRight: () => boolean,
    cellOf: () => CellPosition
  ): WindowInput | null {
    const button = BUTTONS[event.button];
    if (button === undefined) return null;
    if (event.button === RIGHT_BUTTON) {
      this.isRightPassed = passesRight();
      if (!this.isRightPassed) return null;
    }
    const cell = cellOf();
    this.lastSentCell = cell;
    return {
      type: 'pointer',
      kind: 'down',
      button,
      cell,
      mods: event.mods,
      clicks: Math.min(Math.max(event.detail, 1), MAX_CLICKS),
    };
  }

  /**
   * Keeps a press that only selects the window, with its drag and release,
   * from a program tracking the mouse; such a program would take it as input.
   * Returns whether the press was kept back. A program that does not track
   * the mouse gets nothing either way, and the press selects text.
   */
  swallowSelectingPress(): boolean {
    this.isPressSwallowed = this.capturesMouse();
    return this.isPressSwallowed;
  }

  /**
   * The pointer moved. With no button held a move matters only to a program
   * tracking the mouse; either way only a move into another cell is sent.
   */
  move(
    event: { pointerType: string; buttons: number; mods: Modifiers },
    cellOf: () => CellPosition
  ): WindowInput | null {
    if (!isMousePointer(event.pointerType)) return null;
    const button = this.heldButton(event.buttons);
    if (this.isPressSwallowed && button !== 'none') return null;
    if (button === 'none' && !this.capturesMouse()) return null;
    const cell = cellOf();
    if (!shouldSendMove(this.lastSentCell, cell)) return null;
    this.lastSentCell = cell;
    return { type: 'pointer', kind: 'move', button, cell, mods: event.mods, clicks: 1 };
  }

  /** A button went up; a right release is sent only if its press was. */
  up(
    event: { pointerType: string; button: number; mods: Modifiers },
    cellOf: () => CellPosition
  ): WindowInput | null {
    if (!isMousePointer(event.pointerType)) return null;
    if (this.isPressSwallowed) {
      this.isPressSwallowed = false;
      return null;
    }
    const button = BUTTONS[event.button];
    if (button === undefined) return null;
    if (event.button === RIGHT_BUTTON && !this.isRightPassed) return null;
    const cell = cellOf();
    this.lastSentCell = cell;
    return { type: 'pointer', kind: 'up', button, cell, mods: event.mods, clicks: 1 };
  }

  private heldButton(buttons: number): WireButton | 'none' {
    for (const [bit, button] of HELD_BITS) {
      if ((buttons & bit) === 0) continue;
      if (button === 'right' && !this.isRightPassed) continue;
      return button;
    }
    return 'none';
  }
}
