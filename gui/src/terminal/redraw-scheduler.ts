/**
 * Collects the rows that need drawing and asks for one animation frame to
 * draw them, never more than one at a time and none while the window is not
 * shown.
 */
import type { DirtyState } from './paint-plan';

/** Rows to draw again: some, or all of them. */
export type DirtyRows = readonly number[] | 'all';

/** The animation frame source; the browser's, or a fake one under test. */
export interface FrameClock {
  request: (callback: () => void) => number;
  cancel: (id: number) => void;
}

export class RedrawScheduler {
  private readonly dirty = new Set<number>();
  private isAllDirty = true;
  private isVisible = true;
  private frameRequest: number | null = null;

  /** `paint` receives what is out of date once the frame comes. */
  constructor(
    private readonly clock: FrameClock,
    private readonly paint: (dirty: DirtyState) => void
  ) {}

  /** Schedules `rows` to be drawn on the next animation frame. */
  invalidate(rows: DirtyRows): void {
    if (rows === 'all') {
      this.isAllDirty = true;
    } else {
      for (const row of rows) this.dirty.add(row);
    }
    if (this.isVisible && this.frameRequest === null) {
      this.frameRequest = this.clock.request(() => {
        this.frameRequest = null;
        this.flush();
      });
    }
  }

  /** A window that becomes shown is drawn in full, as it may have changed unseen. */
  setVisible(isVisible: boolean): void {
    if (this.isVisible === isVisible) return;
    this.isVisible = isVisible;
    if (isVisible) this.invalidate('all');
  }

  dispose(): void {
    if (this.frameRequest !== null) {
      this.clock.cancel(this.frameRequest);
      this.frameRequest = null;
    }
  }

  private flush(): void {
    if (!this.isVisible) return;
    const state: DirtyState = { isAll: this.isAllDirty, rows: new Set(this.dirty) };
    this.dirty.clear();
    this.isAllDirty = false;
    this.paint(state);
  }
}
