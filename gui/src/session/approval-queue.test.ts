/**
 * `approval-queue.ts` is expected to be pure: no IPC, no svelte store, no
 * `Date.now()` inside the queue functions themselves (`isWithinGuard` takes
 * `now` as an argument so a caller supplies it and a test can too).
 *
 * `wasShown` is queried before `withdraw`/`resolve` remove an entry, not
 * folded into their return value: that keeps the queue's four mutators
 * (`enqueue` / `withdraw` / `resolve` / `reset`) uniform (state in, state
 * out) and lets a caller decide, at the call site, whether an expiring
 * request needs to feed `banner-model.ts`'s unseen-approval count.
 */
import { describe, it, expect } from 'vitest';

import {
  emptyQueue,
  enqueue,
  merge,
  withdraw,
  resolve,
  reset,
  markShown,
  wasShown,
  head,
  size,
  isWithinGuard,
  markInFlight,
  clearInFlight,
  type ApprovalQueueState,
} from './approval-queue';
import type { ApprovalRequest } from '../ipc/session-boundary';

function makeRequest(requestId: string, overrides: Partial<ApprovalRequest> = {}): ApprovalRequest {
  return {
    request_id: requestId,
    tool: 'close_window',
    requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
    target: { window_id: 'w-target', name: 'target window', status: 'running' },
    text: null,
    ...overrides,
  };
}

describe('emptyQueue', () => {
  it('starts with no items', () => {
    const state = emptyQueue();
    expect(size(state)).toBe(0);
    expect(head(state)).toBeNull();
  });
});

describe('enqueue: ordering', () => {
  it('appends in the order requests arrive, oldest first', () => {
    const a = makeRequest('req-a');
    const b = makeRequest('req-b');
    const c = makeRequest('req-c');

    let state = emptyQueue();
    state = enqueue(state, a);
    state = enqueue(state, b);
    state = enqueue(state, c);

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a', 'req-b', 'req-c']);
  });

  it('the first-enqueued request is the display target (head)', () => {
    const a = makeRequest('req-a');
    const b = makeRequest('req-b');

    let state = emptyQueue();
    state = enqueue(state, a);
    state = enqueue(state, b);

    expect(head(state)).toEqual(a);
  });

  it('does not mutate the state object passed in', () => {
    const before = emptyQueue();
    enqueue(before, makeRequest('req-a'));
    expect(size(before)).toBe(0);
  });
});

describe('enqueue: duplicate id is ignored', () => {
  it('a second enqueue with the same request_id does not add a second entry', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a', { tool: 'close_window' }));
    state = enqueue(state, makeRequest('req-a', { tool: 'interrupt_window' }));

    expect(size(state)).toBe(1);
  });

  it('keeps the first-enqueued request unchanged rather than overwriting it', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a', { tool: 'close_window' }));
    state = enqueue(state, makeRequest('req-a', { tool: 'interrupt_window' }));

    expect(head(state)?.tool).toBe('close_window');
  });

  it('does not disturb the position of an existing entry among others', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = enqueue(state, makeRequest('req-a'));

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a', 'req-b']);
  });
});

describe('withdraw: removes the matching request', () => {
  it('removes the entry with the given request_id', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));

    state = withdraw(state, 'req-a');

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-b']);
  });

  it('leaves the remaining entries in their original order', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = enqueue(state, makeRequest('req-c'));

    state = withdraw(state, 'req-b');

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a', 'req-c']);
  });

  it('is a no-op when the request_id is not present', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));

    state = withdraw(state, 'req-missing');

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a']);
  });
});

describe('resolve (a decision was made): removes the matching request', () => {
  it('removes the entry with the given request_id', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));

    state = resolve(state, 'req-a');

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-b']);
  });

  it('is a no-op when the request_id is not present', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));

    state = resolve(state, 'req-missing');

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a']);
  });
});

describe('reset: full replace', () => {
  it('replaces the entire queue with the given list, in the given order', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));

    state = reset(state, [makeRequest('req-x'), makeRequest('req-y'), makeRequest('req-z')]);

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-x', 'req-y', 'req-z']);
  });

  it('replaces a non-empty queue with an empty list', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));

    state = reset(state, []);

    expect(size(state)).toBe(0);
    expect(head(state)).toBeNull();
  });

  it('keeps the displayed record of a surviving id, drops it for a vanished one, and clears every in-flight mark', () => {
    // A reconnect resends the same pending list, so a request the user is
    // already looking at must not be counted as never displayed once it is
    // withdrawn; only the record of an id the daemon no longer reports is
    // stale. In-flight marks are cleared regardless: the decision's round
    // trip may have been lost with the connection, so it may be retried.
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-gone'));
    state = markShown(state, 'req-a');
    state = markShown(state, 'req-gone');
    state = markInFlight(state, 'req-a');

    state = reset(state, [makeRequest('req-a')]);

    expect(wasShown(state, 'req-a')).toBe(true);
    expect(wasShown(state, 'req-gone')).toBe(false);
    expect(state.inFlightIds.size).toBe(0);
  });
});

describe('head and size', () => {
  it('size counts every entry currently in the queue', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = enqueue(state, makeRequest('req-c'));

    expect(size(state)).toBe(3);
  });

  it('head is null once every entry has been removed', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = withdraw(state, 'req-a');

    expect(head(state)).toBeNull();
  });
});

describe('markShown / wasShown: the displayed-once record', () => {
  it('a freshly enqueued request has not been shown', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));

    expect(wasShown(state, 'req-a')).toBe(false);
  });

  it('markShown makes wasShown return true for that id', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markShown(state, 'req-a');

    expect(wasShown(state, 'req-a')).toBe(true);
  });

  it('wasShown is false for an id that was never enqueued', () => {
    const state = emptyQueue();
    expect(wasShown(state, 'req-never-seen')).toBe(false);
  });

  it('marking one request shown does not mark a different request shown', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = markShown(state, 'req-a');

    expect(wasShown(state, 'req-b')).toBe(false);
  });

  /**
   * The scenario this queue exists to support: a caller reads `wasShown`
   * immediately before removing an entry, so a request that expired without
   * ever being displayed is distinguishable from one the user saw and let
   * time out.
   */
  it('distinguishes a withdrawn-while-unseen request from a withdrawn-after-shown request', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-unseen'));
    state = enqueue(state, makeRequest('req-seen'));
    state = markShown(state, 'req-seen');

    const unseenWasShown = wasShown(state, 'req-unseen');
    const seenWasShown = wasShown(state, 'req-seen');

    state = withdraw(state, 'req-unseen');
    state = withdraw(state, 'req-seen');

    expect(unseenWasShown).toBe(false);
    expect(seenWasShown).toBe(true);
    expect(size(state)).toBe(0);
  });
});

describe('isWithinGuard: the 500 ms post-open click guard', () => {
  it('is true immediately after opening (elapsed 0 ms)', () => {
    expect(isWithinGuard(1_000, 1_000)).toBe(true);
  });

  it('is true just under 500 ms after opening', () => {
    expect(isWithinGuard(1_000, 1_499)).toBe(true);
  });

  it('is false at exactly 500 ms after opening', () => {
    expect(isWithinGuard(1_000, 1_500)).toBe(false);
  });

  it('is false well after opening', () => {
    expect(isWithinGuard(1_000, 5_000)).toBe(false);
  });
});

/** Sanity check that the invented state shape round-trips through the type. */
describe('ApprovalQueueState shape', () => {
  it('emptyQueue() satisfies the ApprovalQueueState contract used above', () => {
    const state: ApprovalQueueState = emptyQueue();
    expect(state.items).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// In-flight tracking: `decideApproval` marks a
// request in-flight before invoking `decide_approval` and clears it again on
// failure, so the dialog does not re-offer a request whose decision is still
// in transit, without needing an optimistic removal that a failed round trip
// would then have to undo.
// ---------------------------------------------------------------------------

describe('markInFlight / clearInFlight: head() and size()', () => {
  it('head() skips an in-flight request and offers the next one instead', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = markInFlight(state, 'req-a');

    expect(head(state)?.request_id).toBe('req-b');
  });

  it('head() is null when every remaining request is in-flight', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markInFlight(state, 'req-a');

    expect(head(state)).toBeNull();
  });

  it('size() counts an in-flight request (it is still pending, just not offered)', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = markInFlight(state, 'req-a');

    expect(size(state)).toBe(2);
  });

  it('clearInFlight makes head() offer the request again', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markInFlight(state, 'req-a');
    state = clearInFlight(state, 'req-a');

    expect(head(state)?.request_id).toBe('req-a');
  });

  it('marking one request in-flight does not affect another', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = enqueue(state, makeRequest('req-b'));
    state = markInFlight(state, 'req-a');

    expect(head(state)?.request_id).toBe('req-b');
    state = clearInFlight(state, 'req-b');
    expect(head(state)?.request_id).toBe('req-b');
  });
});

describe('resolve / withdraw: also drop the id from inFlightIds', () => {
  it('resolve removes an in-flight id from inFlightIds as well as from items', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markInFlight(state, 'req-a');

    state = resolve(state, 'req-a');

    expect(size(state)).toBe(0);
    // If the id were left in inFlightIds, re-enqueuing the same id (e.g.
    // after a reconnect that never went through reset) would wrongly stay
    // hidden from head() forever.
    state = enqueue(state, makeRequest('req-a'));
    expect(head(state)?.request_id).toBe('req-a');
  });

  it('withdraw removes an in-flight id from inFlightIds as well as from items', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markInFlight(state, 'req-a');

    state = withdraw(state, 'req-a');
    state = enqueue(state, makeRequest('req-a'));

    expect(head(state)?.request_id).toBe('req-a');
  });
});

describe('reset: empties inFlightIds', () => {
  it('a request that survives a reset (daemon resent the same id) is offered again, not hidden as in-flight', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markInFlight(state, 'req-a');

    state = reset(state, [makeRequest('req-a')]);

    expect(head(state)?.request_id).toBe('req-a');
  });
});

// ---------------------------------------------------------------------------
// `merge` applies a list this side pulled, rather than one the daemon pushed.
// The pull and the push are separate routes with no
// guaranteed relative order, so a request that arrived by push while the pull
// was in flight must survive the pull's result being applied -- which rules
// out the full replace `reset` performs.
// ---------------------------------------------------------------------------

describe('merge: applies a pulled list on top of what is already queued', () => {
  it('adds no second entry for an id that is already queued', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a', { tool: 'close_window' }));

    state = merge(state, [makeRequest('req-a', { tool: 'interrupt_window' })]);

    expect(size(state)).toBe(1);
    expect(head(state)?.tool).toBe('close_window');
  });

  it('appends an id the queue does not hold yet, at the end', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));

    state = merge(state, [makeRequest('req-a'), makeRequest('req-b')]);

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-a', 'req-b']);
  });

  it('keeps an id that is queued locally but absent from the merged list', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-pushed'));
    state = enqueue(state, makeRequest('req-b'));

    state = merge(state, [makeRequest('req-b')]);

    expect(state.items.map((entry) => entry.request_id)).toEqual(['req-pushed', 'req-b']);
  });

  it('changes neither the displayed record nor the in-flight marks', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-a'));
    state = markShown(state, 'req-a');
    state = markInFlight(state, 'req-a');

    state = merge(state, [makeRequest('req-a'), makeRequest('req-b')]);

    expect(wasShown(state, 'req-a')).toBe(true);
    expect([...state.inFlightIds]).toEqual(['req-a']);
    expect(head(state)?.request_id).toBe('req-b');
  });
});

describe('reset: which requests count as displayed', () => {
  it('keeps the displayed record of requests that survive and starts a request that is new as not displayed', () => {
    let state = emptyQueue();
    state = enqueue(state, makeRequest('req-old'));
    state = enqueue(state, makeRequest('req-gone'));
    state = markShown(state, 'req-old');
    state = markShown(state, 'req-gone');

    state = reset(state, [makeRequest('req-old'), makeRequest('req-new')]);

    expect(wasShown(state, 'req-old')).toBe(true);
    expect(wasShown(state, 'req-new')).toBe(false);
    expect(wasShown(state, 'req-gone')).toBe(false);
  });

  it('drops every in-flight mark', () => {
    let state = enqueue(emptyQueue(), makeRequest('req-a'));
    state = markInFlight(state, 'req-a');

    state = reset(state, [makeRequest('req-a')]);

    expect(head(state)?.request_id).toBe('req-a');
  });
});

describe('in-flight marks: edges', () => {
  it('clears only the named request, and ignores one that was never marked', () => {
    let state = enqueue(enqueue(emptyQueue(), makeRequest('req-a')), makeRequest('req-b'));
    state = markInFlight(markInFlight(state, 'req-a'), 'req-b');

    state = clearInFlight(state, 'req-a');
    state = clearInFlight(state, 'req-unknown');

    expect(head(state)?.request_id).toBe('req-a');
    state = markInFlight(state, 'req-a');
    expect(head(state)).toBeNull();
  });

  it('releases the in-flight mark of a request that is withdrawn, so it can be offered if it returns', () => {
    let state = markInFlight(enqueue(emptyQueue(), makeRequest('req-a')), 'req-a');

    state = withdraw(state, 'req-a');
    state = enqueue(state, makeRequest('req-a'));

    expect(head(state)?.request_id).toBe('req-a');
  });

  it('keeps the in-flight mark of the other requests when one is withdrawn', () => {
    let state = enqueue(enqueue(emptyQueue(), makeRequest('req-a')), makeRequest('req-b'));
    state = markInFlight(state, 'req-b');

    state = withdraw(state, 'req-a');

    expect(head(state)).toBeNull();
    expect(size(state)).toBe(1);
  });
});

describe('isWithinGuard: the boundary', () => {
  it('is inside the guard just before it ends and outside at its end', () => {
    expect(isWithinGuard(1000, 1000)).toBe(true);
    expect(isWithinGuard(1000, 1000 + 500 - 1)).toBe(true);
    expect(isWithinGuard(1000, 1000 + 500)).toBe(false);
  });
});
