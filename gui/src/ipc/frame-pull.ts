/**
 * The application's single receiving loop for window frames.
 *
 * Frames are not pushed as events: one `next_frames` request is always
 * pending, the core answers it with whatever frames are waiting, and the loop
 * asks again carrying the acknowledgements of what it just applied. One
 * request serves every window, so the number of pending requests does not grow
 * with the number of windows.
 */

import type { Frame } from './session-boundary';

/** The WebView applied the last part of frame `seq` of a window. */
export interface FrameAckEntry {
  window_id: string;
  seq: number;
}

export interface FramePullOptions {
  /** Sends the acknowledgements and resolves with the next frames, as `[window_id, frame]` pairs. */
  pull: (acks: FrameAckEntry[]) => Promise<[string, Frame][]>;
  /**
   * Hands one frame part to its window. Returns the frame number to
   * acknowledge, `null` when nothing is owed yet (a part that is not the
   * last), or `undefined` when no window listens for it, which drops the frame.
   */
  deliver: (windowId: string, frame: Frame) => number | null | undefined;
  /**
   * Receives a failure of `pull` or of `deliver`. A failed request is asked
   * again without acknowledgements; the loop ends when such a request fails.
   */
  onError?: (error: unknown) => void;
}

export interface FramePull {
  /**
   * Stops asking. The request in flight is not awaited: the core answers it
   * only when a frame arrives, and its answer is dropped.
   */
  stop: () => Promise<void>;
}

/** Starts the loop. The first request carries no acknowledgements. */
export function startFramePull(options: FramePullOptions): FramePull {
  let stopped = false;
  void run(options, () => stopped);
  return {
    stop: () => {
      stopped = true;
      return Promise.resolve();
    },
  };
}

async function run(options: FramePullOptions, isStopped: () => boolean): Promise<void> {
  let acks: FrameAckEntry[] = [];
  while (!isStopped()) {
    let frames: [string, Frame][];
    try {
      frames = await options.pull(acks);
    } catch (error) {
      options.onError?.(error);
      // A request that carried nothing to refuse failing means the transport
      // itself is gone; asking again would only spin.
      if (acks.length === 0) {
        return;
      }
      acks = [];
      continue;
    }
    if (isStopped()) {
      return;
    }
    // Only the latest frame number of a window is worth acknowledging.
    const latest = new Map<string, number>();
    for (const [windowId, frame] of frames) {
      try {
        const ack = options.deliver(windowId, frame);
        if (typeof ack === 'number') {
          latest.set(windowId, ack);
        }
      } catch (error) {
        options.onError?.(error);
      }
    }
    acks = [...latest].map(([window_id, seq]) => ({ window_id, seq }));
  }
}
