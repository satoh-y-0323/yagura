import { describe, it, expect } from 'vitest';
import type { DirtyState } from './paint-plan';
import { RedrawScheduler } from './redraw-scheduler';

/** An animation frame source that runs a frame only when the test says. */
class ManualClock {
  private nextId = 1;
  readonly pending = new Map<number, () => void>();
  readonly cancelled: number[] = [];

  request = (callback: () => void): number => {
    const id = this.nextId;
    this.nextId += 1;
    this.pending.set(id, callback);
    return id;
  };

  cancel = (id: number): void => {
    this.cancelled.push(id);
    this.pending.delete(id);
  };

  runFrame(): void {
    const callbacks = [...this.pending.values()];
    this.pending.clear();
    for (const callback of callbacks) callback();
  }
}

function setup(): { clock: ManualClock; scheduler: RedrawScheduler; painted: DirtyState[] } {
  const clock = new ManualClock();
  const painted: DirtyState[] = [];
  const scheduler = new RedrawScheduler(clock, (dirty) => painted.push(dirty));
  return { clock, scheduler, painted };
}

describe('drawing once per animation frame', () => {
  it('draws the rows invalidated since the last frame, once', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([1, 2]);
    scheduler.invalidate([2, 5]);
    expect(clock.pending.size).toBe(1);
    clock.runFrame();
    expect(painted).toHaveLength(1);
    expect([...(painted[0]?.rows ?? [])]).toEqual([1, 2, 5]);
  });

  it('draws everything the first time, as nothing has been drawn yet', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([3]);
    clock.runFrame();
    expect(painted[0]?.isAll).toBe(true);
    scheduler.invalidate([3]);
    clock.runFrame();
    expect(painted[1]).toEqual({ isAll: false, rows: new Set([3]) });
  });

  it('draws everything after invalidate all, whatever rows came with it', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([0]);
    clock.runFrame();
    scheduler.invalidate([4]);
    scheduler.invalidate('all');
    clock.runFrame();
    expect(painted[1]?.isAll).toBe(true);
    scheduler.invalidate([1]);
    clock.runFrame();
    expect(painted[2]).toEqual({ isAll: false, rows: new Set([1]) });
  });

  it('asks for another frame only after the last one ran', () => {
    const { clock, scheduler } = setup();
    scheduler.invalidate([0]);
    scheduler.invalidate([1]);
    expect(clock.pending.size).toBe(1);
    clock.runFrame();
    scheduler.invalidate([2]);
    expect(clock.pending.size).toBe(1);
  });
});

describe('a window that is not shown', () => {
  it('asks for no frame while hidden', () => {
    const { clock, scheduler } = setup();
    scheduler.setVisible(false);
    scheduler.invalidate([1]);
    expect(clock.pending.size).toBe(0);
  });

  it('draws nothing from a frame that was already asked for when it was hidden', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([1]);
    scheduler.setVisible(false);
    clock.runFrame();
    expect(painted).toEqual([]);
  });

  it('draws it in full when it is shown again', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([0]);
    clock.runFrame();
    scheduler.setVisible(false);
    scheduler.invalidate([2]);
    scheduler.setVisible(true);
    expect(clock.pending.size).toBe(1);
    clock.runFrame();
    expect(painted[1]?.isAll).toBe(true);
    expect(painted[1]?.rows.has(2)).toBe(true);
  });

  it('does nothing when told what it already knows', () => {
    const { clock, scheduler } = setup();
    scheduler.setVisible(true);
    expect(clock.pending.size).toBe(0);
    scheduler.setVisible(false);
    scheduler.setVisible(false);
    expect(clock.pending.size).toBe(0);
  });
});

describe('disposing', () => {
  it('cancels the frame that is waiting', () => {
    const { clock, scheduler, painted } = setup();
    scheduler.invalidate([1]);
    scheduler.dispose();
    expect(clock.cancelled).toHaveLength(1);
    clock.runFrame();
    expect(painted).toEqual([]);
  });

  it('cancels nothing when no frame is waiting', () => {
    const { clock, scheduler } = setup();
    scheduler.dispose();
    expect(clock.cancelled).toEqual([]);
  });
});
