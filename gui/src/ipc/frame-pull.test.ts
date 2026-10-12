import { describe, it, expect, vi } from 'vitest';
import { startFramePull, type FrameAckEntry } from './frame-pull';
import type { Frame } from './session-boundary';

function frame(seq: number, last = true): Frame {
  return {
    seq,
    cols: 2,
    rows: 1,
    reset: false,
    last,
    attrs: [],
    lines: [],
    cursor: { row: 0, col: 0, visible: true, shape: 'block' },
    view: { display_offset: 0, history_rows: 0, mouse_reporting: false, alt_screen: false },
    overrides: [],
  };
}

/** A pull the test answers by hand, one request at a time. */
function manualPull() {
  const requests: { acks: FrameAckEntry[]; answer: (frames: [string, Frame][]) => void }[] = [];
  let inFlight = 0;
  let maxInFlight = 0;
  const pull = (acks: FrameAckEntry[]): Promise<[string, Frame][]> => {
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    return new Promise((resolve) => {
      requests.push({
        acks,
        answer: (frames) => {
          inFlight -= 1;
          resolve(frames);
        },
      });
    });
  };
  return { pull, requests, maxInFlight: () => maxInFlight };
}

/** Lets every continuation that is already runnable run. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) {
    await Promise.resolve();
  }
}

describe('the frame pull loop', () => {
  it('holds exactly one request while waiting and asks again only after an answer', async () => {
    const { pull, requests, maxInFlight } = manualPull();
    const loop = startFramePull({ pull, deliver: () => null });
    await settle();
    expect(requests).toHaveLength(1);

    requests[0]?.answer([['a', frame(1)]]);
    await settle();
    expect(requests).toHaveLength(2);

    requests[1]?.answer([]);
    await settle();
    expect(requests).toHaveLength(3);
    expect(maxInFlight()).toBe(1);
    await loop.stop();
  });

  it('starts with no acknowledgements', async () => {
    const { pull, requests } = manualPull();
    const loop = startFramePull({ pull, deliver: () => null });
    await settle();
    expect(requests[0]?.acks).toEqual([]);
    await loop.stop();
  });

  it('hands each frame to the window it belongs to, in order', async () => {
    const { pull, requests } = manualPull();
    const deliver = vi.fn<(windowId: string, frame: Frame) => number | null | undefined>(
      () => null
    );
    const loop = startFramePull({ pull, deliver });
    await settle();

    requests[0]?.answer([
      ['a', frame(1)],
      ['b', frame(7)],
      ['a', frame(2)],
    ]);
    await settle();

    expect(deliver.mock.calls.map(([id, f]) => [id, f.seq])).toEqual([
      ['a', 1],
      ['b', 7],
      ['a', 2],
    ]);
    await loop.stop();
  });

  it('drops the frames of a window nobody listens for and keeps pulling', async () => {
    const { pull, requests } = manualPull();
    const deliver = (windowId: string): number | null | undefined =>
      windowId === 'known' ? 4 : undefined;
    const loop = startFramePull({ pull, deliver });
    await settle();

    requests[0]?.answer([
      ['gone', frame(9)],
      ['known', frame(4)],
    ]);
    await settle();

    expect(requests).toHaveLength(2);
    // The dropped window earns no acknowledgement; the known one does.
    expect(requests[1]?.acks).toEqual([{ window_id: 'known', seq: 4 }]);
    await loop.stop();
  });

  it('acknowledges the seq each window reported applying, and nothing for a part that is not last', async () => {
    const { pull, requests } = manualPull();
    const reported: Record<string, number | null> = { a: 5, b: null };
    const loop = startFramePull({ pull, deliver: (id) => reported[id] ?? null });
    await settle();

    requests[0]?.answer([
      ['a', frame(5)],
      ['b', frame(8, false)],
    ]);
    await settle();

    expect(requests[1]?.acks).toEqual([{ window_id: 'a', seq: 5 }]);

    // The acknowledgements are sent once, not carried into later requests.
    requests[1]?.answer([]);
    await settle();
    expect(requests[2]?.acks).toEqual([]);
    await loop.stop();
  });

  it('acknowledges every window that applied a frame in one answer', async () => {
    const { pull, requests } = manualPull();
    const seqs: Record<string, number> = { a: 2, b: 3 };
    const loop = startFramePull({ pull, deliver: (id) => seqs[id] ?? null });
    await settle();

    requests[0]?.answer([
      ['a', frame(2)],
      ['b', frame(3)],
    ]);
    await settle();

    expect(requests[1]?.acks).toEqual(
      expect.arrayContaining([
        { window_id: 'a', seq: 2 },
        { window_id: 'b', seq: 3 },
      ])
    );
    expect(requests[1]?.acks).toHaveLength(2);
    await loop.stop();
  });

  it('keeps only the latest acknowledgement of a window within one answer', async () => {
    const { pull, requests } = manualPull();
    const loop = startFramePull({ pull, deliver: (_id, f) => f.seq });
    await settle();

    requests[0]?.answer([
      ['a', frame(1)],
      ['a', frame(2)],
    ]);
    await settle();

    expect(requests[1]?.acks.filter((ack) => ack.window_id === 'a')).toEqual([
      { window_id: 'a', seq: 2 },
    ]);
    await loop.stop();
  });

  it('stops asking once stopped', async () => {
    const { pull, requests } = manualPull();
    const deliver = vi.fn(() => null);
    const loop = startFramePull({ pull, deliver });
    await settle();

    const stopped = loop.stop();
    requests[0]?.answer([['a', frame(1)]]);
    await stopped;
    await settle();

    expect(requests).toHaveLength(1);
    expect(deliver).not.toHaveBeenCalled();
  });

  it('keeps going when a window fails to take a frame and no error handler is given', async () => {
    const { pull, requests } = manualPull();
    const deliver = vi.fn((windowId: string) => {
      if (windowId === 'a') {
        throw new Error('window gone');
      }
      return 4;
    });
    const loop = startFramePull({ pull, deliver });
    await settle();
    requests[0]?.answer([
      ['a', frame(1)],
      ['b', frame(4)],
    ]);
    await settle();

    expect(requests).toHaveLength(2);
    expect(requests[1]?.acks).toEqual([{ window_id: 'b', seq: 4 }]);
    await loop.stop();
  });

  it('ends quietly when a request fails and no error handler is given', async () => {
    let calls = 0;
    const pull = (): Promise<[string, Frame][]> => {
      calls += 1;
      return Promise.reject(new Error('no transport'));
    };
    startFramePull({ pull, deliver: () => null });
    await settle();
    expect(calls).toBe(1);
  });

  it('reports a failing deliver and still acknowledges the other windows', async () => {
    const { pull, requests } = manualPull();
    const onError = vi.fn();
    const failure = new Error('window gone');
    const deliver = (windowId: string): number => {
      if (windowId === 'a') {
        throw failure;
      }
      return 2;
    };
    const loop = startFramePull({ pull, deliver, onError });
    await settle();
    requests[0]?.answer([
      ['a', frame(1)],
      ['b', frame(2)],
    ]);
    await settle();
    expect(onError).toHaveBeenCalledWith(failure);
    expect(requests[1]?.acks).toEqual([{ window_id: 'b', seq: 2 }]);
    await loop.stop();
  });

  it('asks again without acknowledgements after a failed request', async () => {
    const calls: FrameAckEntry[][] = [];
    let answered = false;
    const pull = (acks: FrameAckEntry[]): Promise<[string, Frame][]> => {
      calls.push(acks);
      if (calls.length === 1) {
        return Promise.resolve([['a', frame(1)]]);
      }
      if (calls.length === 2) {
        return Promise.reject(new Error('broken answer'));
      }
      answered = true;
      return new Promise(() => {});
    };
    const onError = vi.fn();
    const loop = startFramePull({ pull, deliver: () => 1, onError });
    await settle();

    expect(calls[1]).toEqual([{ window_id: 'a', seq: 1 }]);
    expect(calls[2]).toEqual([]);
    expect(answered).toBe(true);
    expect(onError).toHaveBeenCalledTimes(1);
    await loop.stop();
  });

  it('stops when a request without acknowledgements fails', async () => {
    let calls = 0;
    const pull = (): Promise<[string, Frame][]> => {
      calls += 1;
      return Promise.reject(new Error('no transport'));
    };
    const onError = vi.fn();
    startFramePull({ pull, deliver: () => null, onError });
    await settle();

    expect(calls).toBe(1);
    expect(onError).toHaveBeenCalledTimes(1);
  });
});
