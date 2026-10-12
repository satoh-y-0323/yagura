/**
 * The room-list refresh serializer: at most one run in flight, every call
 * arriving mid-run folded into a single follow-up run, and the wait chain
 * bounded at two runs so a steady stream of pushes cannot make an early
 * caller wait forever. Exercised by running it, with the run's completion
 * held open by hand, so the ordering is measured rather than read off the
 * call site.
 */
import { describe, it, expect } from 'vitest';

import { serializeRefresh } from './refresh-serializer';

interface Deferred {
  promise: Promise<void>;
  resolve: () => void;
  reject: (reason: unknown) => void;
}

/** A promise whose settlement this test triggers by hand. */
function deferred(): Deferred {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

interface Tracked {
  settled: boolean;
  promise: Promise<void>;
}

/** Records whether `promise` has settled, without ever rejecting itself. */
function track(promise: Promise<void>): Tracked {
  const tracked: Tracked = {
    settled: false,
    promise: promise.then(
      () => {
        tracked.settled = true;
      },
      () => {
        tracked.settled = true;
      }
    ),
  };
  return tracked;
}

interface Runner {
  run: () => Promise<void>;
  calls: () => number;
  gate: (index: number) => Deferred;
}

/** A `run` that never finishes on its own: each call opens a new gate. */
function gatedRunner(): Runner {
  const gates: Deferred[] = [];
  return {
    run: () => {
      const gate = deferred();
      gates.push(gate);
      return gate.promise;
    },
    calls: () => gates.length,
    gate: (index: number) => {
      const gate = gates[index];
      if (gate === undefined) {
        throw new Error(`run has not been called ${index + 1} time(s) yet`);
      }
      return gate;
    },
  };
}

/** Lets every queued microtask (and continuation of one) run. */
async function flush(): Promise<void> {
  for (let i = 0; i < 8; i++) {
    await Promise.resolve();
  }
}

describe('serializeRefresh', () => {
  it('runs once when nothing is in flight, and resolves the caller when that run finishes', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    const first = track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);
    expect(first.settled).toBe(false);

    runner.gate(0).resolve();
    await first.promise;
    expect(first.settled).toBe(true);
    expect(runner.calls()).toBe(1);
  });

  it('folds every call arriving mid-run into one follow-up run, and holds those callers until the follow-up has finished', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);

    const second = track(refresh());
    const third = track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);
    expect(second.settled).toBe(false);
    expect(third.settled).toBe(false);

    runner.gate(0).resolve();
    await flush();
    expect(runner.calls()).toBe(2);
    expect(second.settled).toBe(false);
    expect(third.settled).toBe(false);

    runner.gate(1).resolve();
    await flush();
    expect(second.settled).toBe(true);
    expect(third.settled).toBe(true);
    expect(runner.calls()).toBe(2);
  });

  it('books one more follow-up for a call arriving during the follow-up, and does not stretch the first caller wait onto that third run', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    const first = track(refresh());
    await flush();
    const second = track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);

    runner.gate(0).resolve();
    await flush();
    expect(runner.calls()).toBe(2);

    const third = track(refresh());
    await flush();
    expect(runner.calls()).toBe(2);

    runner.gate(1).resolve();
    await flush();
    expect(runner.calls()).toBe(3);
    expect(first.settled).toBe(true);
    expect(second.settled).toBe(true);
    expect(third.settled).toBe(false);

    runner.gate(2).resolve();
    await flush();
    expect(third.settled).toBe(true);
    expect(runner.calls()).toBe(3);
  });

  it('keeps accepting work after a failed run instead of staying stuck as if one were still in flight', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);

    runner.gate(0).reject(new Error('room list read failed'));
    await flush();

    const afterFailure = track(refresh());
    await flush();
    expect(runner.calls()).toBe(2);

    runner.gate(1).resolve();
    await flush();
    expect(afterFailure.settled).toBe(true);
  });

  it('resolves a call folded into the follow-up by that follow-up finishing, even though the run it was queued behind rejected', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    track(refresh());
    await flush();
    expect(runner.calls()).toBe(1);

    const foldedPromise = refresh();
    const folded = track(foldedPromise);
    await flush();
    expect(runner.calls()).toBe(1);
    expect(folded.settled).toBe(false);

    runner.gate(0).reject(new Error('room list read failed'));
    await flush();
    expect(runner.calls()).toBe(2);
    expect(folded.settled).toBe(false);

    runner.gate(1).resolve();
    await expect(foldedPromise).resolves.toBeUndefined();
    expect(folded.settled).toBe(true);
    expect(runner.calls()).toBe(2);
  });

  it('returns a promise from a plain synchronous call rather than throwing at the call site', async () => {
    const runner = gatedRunner();
    const refresh = serializeRefresh(runner.run);

    const returned: Promise<void>[] = [];
    expect(() => {
      returned.push(refresh());
    }).not.toThrow();
    expect(returned).toHaveLength(1);
    expect(returned[0]).toBeInstanceOf(Promise);

    runner.gate(0).resolve();
    await flush();
  });
});
