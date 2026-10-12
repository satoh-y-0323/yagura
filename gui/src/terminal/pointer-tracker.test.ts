import { describe, it, expect } from 'vitest';
import type { Modifiers } from './key-input';
import type { CellPosition } from './pointer-input';
import { PointerTracker, isMousePointer } from './pointer-tracker';

const MODS: Modifiers = { shift: true, alt: false, ctrl: false };
const CELL_A: CellPosition = { col: 1, row: 2 };
const CELL_B: CellPosition = { col: 3, row: 2 };

const at = (cell: CellPosition) => () => cell;
const always = () => true;
const never = () => false;

function tracker(capturesMouse = false): PointerTracker {
  return new PointerTracker(() => capturesMouse);
}

describe('pressing a button', () => {
  it('sends the button, the cell, the modifiers and the click count', () => {
    expect(tracker().down({ button: 0, detail: 2, mods: MODS }, always, at(CELL_A))).toEqual({
      type: 'pointer',
      kind: 'down',
      button: 'left',
      cell: CELL_A,
      mods: MODS,
      clicks: 2,
    });
  });

  it('names the middle and right buttons', () => {
    const sent = (button: number) => {
      const input = tracker().down({ button, detail: 1, mods: MODS }, always, at(CELL_A));
      return input?.type === 'pointer' ? input.button : null;
    };
    expect(sent(1)).toBe('middle');
    expect(sent(2)).toBe('right');
  });

  it('sends nothing for a button the wire does not name', () => {
    expect(tracker().down({ button: 3, detail: 1, mods: MODS }, always, at(CELL_A))).toBeNull();
    expect(tracker().down({ button: 4, detail: 1, mods: MODS }, always, at(CELL_A))).toBeNull();
  });

  it('keeps the click count from 1 to 255', () => {
    const clicks = (detail: number) => {
      const input = tracker().down({ button: 0, detail, mods: MODS }, always, at(CELL_A));
      return input?.type === 'pointer' ? input.clicks : null;
    };
    expect(clicks(0)).toBe(1);
    expect(clicks(1)).toBe(1);
    expect(clicks(255)).toBe(255);
    expect(clicks(300)).toBe(255);
  });

  it('leaves a right press to the pane when it does not pass it to the window', () => {
    let measured = false;
    const input = tracker().down({ button: 2, detail: 1, mods: MODS }, never, () => {
      measured = true;
      return CELL_A;
    });
    expect(input).toBeNull();
    expect(measured).toBe(false);
  });

  it('asks whether to pass a press only for the right button', () => {
    let asked = 0;
    const counting = () => {
      asked += 1;
      return true;
    };
    const subject = tracker();
    subject.down({ button: 0, detail: 1, mods: MODS }, counting, at(CELL_A));
    subject.down({ button: 1, detail: 1, mods: MODS }, counting, at(CELL_A));
    expect(asked).toBe(0);
    subject.down({ button: 2, detail: 1, mods: MODS }, counting, at(CELL_A));
    expect(asked).toBe(1);
  });
});

describe('moving the pointer', () => {
  const move = (subject: PointerTracker, buttons: number, cell: CellPosition, type = 'mouse') =>
    subject.move({ pointerType: type, buttons, mods: MODS }, at(cell));

  it('sends a drag with the button held, into another cell only', () => {
    const subject = tracker();
    expect(move(subject, 1, CELL_A)).toEqual({
      type: 'pointer',
      kind: 'move',
      button: 'left',
      cell: CELL_A,
      mods: MODS,
      clicks: 1,
    });
    expect(move(subject, 1, CELL_A)).toBeNull();
    expect(move(subject, 1, CELL_B)).toMatchObject({ cell: CELL_B });
  });

  it('does not send a move over the cell the press was sent for', () => {
    const subject = tracker();
    subject.down({ button: 0, detail: 1, mods: MODS }, always, at(CELL_A));
    expect(move(subject, 1, CELL_A)).toBeNull();
    expect(move(subject, 1, CELL_B)).not.toBeNull();
  });

  it('names the button held: left before middle before right', () => {
    const subject = tracker();
    subject.down({ button: 2, detail: 1, mods: MODS }, always, at(CELL_A));
    const held = (buttons: number, cell: CellPosition) => {
      const input = move(subject, buttons, cell);
      return input?.type === 'pointer' ? input.button : null;
    };
    expect(held(1 | 2 | 4, { col: 0, row: 0 })).toBe('left');
    expect(held(2 | 4, { col: 1, row: 0 })).toBe('middle');
    expect(held(2, { col: 2, row: 0 })).toBe('right');
  });

  it('does not name a right button whose press stayed with the pane', () => {
    const subject = tracker(true);
    subject.down({ button: 2, detail: 1, mods: MODS }, never, at(CELL_A));
    expect(move(subject, 2, CELL_B)).toMatchObject({ button: 'none' });
    const quiet = tracker(false);
    quiet.down({ button: 2, detail: 1, mods: MODS }, never, at(CELL_A));
    expect(move(quiet, 2, CELL_B)).toBeNull();
  });

  it('sends a move with no button only to a program that asked for mouse reports', () => {
    expect(move(tracker(false), 0, CELL_A)).toBeNull();
    expect(move(tracker(true), 0, CELL_A)).toMatchObject({ button: 'none', cell: CELL_A });
  });

  it('sends nothing for touch or pen', () => {
    expect(move(tracker(true), 1, CELL_A, 'touch')).toBeNull();
    expect(move(tracker(true), 1, CELL_A, 'pen')).toBeNull();
  });
});

describe('releasing a button', () => {
  const up = (subject: PointerTracker, button: number, cell: CellPosition, type = 'mouse') =>
    subject.up({ pointerType: type, button, mods: MODS }, at(cell));

  it('sends the button and the cell', () => {
    expect(up(tracker(), 0, CELL_B)).toEqual({
      type: 'pointer',
      kind: 'up',
      button: 'left',
      cell: CELL_B,
      mods: MODS,
      clicks: 1,
    });
    expect(up(tracker(), 1, CELL_B)).toMatchObject({ button: 'middle' });
  });

  it('sends a right release only if its press went to the window', () => {
    const passed = tracker();
    passed.down({ button: 2, detail: 1, mods: MODS }, always, at(CELL_A));
    expect(up(passed, 2, CELL_A)).toMatchObject({ button: 'right' });
    const kept = tracker();
    kept.down({ button: 2, detail: 1, mods: MODS }, never, at(CELL_A));
    expect(up(kept, 2, CELL_A)).toBeNull();
    expect(up(tracker(), 2, CELL_A)).toBeNull();
  });

  it('sends nothing for a button the wire does not name, or for touch', () => {
    expect(up(tracker(), 3, CELL_A)).toBeNull();
    expect(up(tracker(), 0, CELL_A, 'touch')).toBeNull();
  });

  it('counts the released cell as sent, so a move over it is not sent again', () => {
    const subject = tracker(true);
    up(subject, 0, CELL_A);
    expect(subject.move({ pointerType: 'mouse', buttons: 0, mods: MODS }, at(CELL_A))).toBeNull();
  });
});

describe('which pointers count', () => {
  it('counts only the mouse', () => {
    expect(isMousePointer('mouse')).toBe(true);
    expect(isMousePointer('touch')).toBe(false);
    expect(isMousePointer('pen')).toBe(false);
    expect(isMousePointer('')).toBe(false);
  });
});

describe('a press that selects the window', () => {
  const move = (subject: PointerTracker, buttons: number, cell: CellPosition) =>
    subject.move({ pointerType: 'mouse', buttons, mods: MODS }, at(cell));
  const up = (subject: PointerTracker, button: number, cell: CellPosition) =>
    subject.up({ pointerType: 'mouse', button, mods: MODS }, at(cell));

  it('keeps the press, its drag and its release from a program tracking the mouse', () => {
    const subject = tracker(true);
    expect(subject.swallowSelectingPress()).toBe(true);
    expect(move(subject, 1, CELL_B)).toBeNull();
    expect(up(subject, 0, CELL_B)).toBeNull();
  });

  it('sends the pointer again once the swallowed press is released', () => {
    const subject = tracker(true);
    subject.swallowSelectingPress();
    up(subject, 0, CELL_A);
    expect(move(subject, 0, CELL_B)).toMatchObject({ button: 'none', cell: CELL_B });
    expect(subject.down({ button: 0, detail: 1, mods: MODS }, always, at(CELL_B))).toMatchObject({
      kind: 'down',
      button: 'left',
    });
  });

  it('swallows nothing for a program that does not track the mouse, which selects text with it', () => {
    const subject = tracker(false);
    expect(subject.swallowSelectingPress()).toBe(false);
    expect(move(subject, 1, CELL_B)).toMatchObject({ button: 'left' });
  });
});
