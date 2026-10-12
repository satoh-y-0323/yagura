/**
 * The queue of approval requests the daemon has handed this GUI to decide,
 * oldest first, together with the record of which of them have already been
 * put in front of the user.
 *
 * Pure state -> state: no IPC, no store, and no clock of its own --
 * `isWithinGuard` takes `now` from its caller, so the guard is decided by
 * whoever owns the clock rather than by this module.
 */
import type { ApprovalRequest } from '../ipc/session-boundary';

/**
 * How long after the dialog opens a click on either decision does nothing, so
 * a dialog that appears under the pointer cannot be answered reflexively.
 */
const CLICK_GUARD_MS = 500;

/** The pending requests, and the ids that have been displayed at least once. */
export interface ApprovalQueueState {
  /** Pending requests, oldest first. */
  readonly items: readonly ApprovalRequest[];
  /** Ids of the requests that have already been displayed. */
  readonly shownIds: ReadonlySet<string>;
  /**
   * Ids of the requests whose decision is currently in flight (sent to the
   * daemon, no reply yet). Still counted by {@link size}, but skipped by
   * {@link head}: the dialog must not re-offer a decision that is already on
   * its way.
   */
  readonly inFlightIds: ReadonlySet<string>;
}

/** A queue holding nothing. */
export function emptyQueue(): ApprovalQueueState {
  return { items: [], shownIds: new Set(), inFlightIds: new Set() };
}

/**
 * Appends `request` at the end of the queue. An id that is already queued is
 * ignored rather than replacing its entry: the daemon resends the whole
 * pending list whenever approver registration completes, so the same request
 * can arrive twice and must keep its place in line.
 * @param state the current queue
 * @param request the request the daemon submitted
 */
export function enqueue(state: ApprovalQueueState, request: ApprovalRequest): ApprovalQueueState {
  if (state.items.some((entry) => entry.request_id === request.request_id)) {
    return state;
  }
  return { ...state, items: [...state.items, request] };
}

/**
 * Removes a request the daemon withdrew (decided elsewhere, timed out, or its
 * requester disconnected).
 * @param state the current queue
 * @param requestId the withdrawn request's id
 */
export function withdraw(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  return removeRequest(state, requestId);
}

/**
 * Removes a request this GUI has just decided, so the queue stops offering it
 * while the decision is in flight.
 * @param state the current queue
 * @param requestId the decided request's id
 */
export function resolve(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  return removeRequest(state, requestId);
}

/**
 * Replaces the queue with the daemon's authoritative pending list, keeping
 * the displayed record of every id that survived the replacement: a request
 * the user is already looking at has been displayed, whichever connection
 * reported it, and forgetting that would count it as never seen once it is
 * withdrawn. In-flight marks are dropped wholesale instead, because a
 * decision's round trip may have been lost with the connection and may be
 * retried.
 * @param state the current queue
 * @param requests the pending list the daemon reported
 */
export function reset(
  state: ApprovalQueueState,
  requests: readonly ApprovalRequest[]
): ApprovalQueueState {
  const shownIds = new Set(
    requests.map((entry) => entry.request_id).filter((id) => state.shownIds.has(id))
  );
  return { items: [...requests], shownIds, inFlightIds: new Set() };
}

/**
 * Applies a pending list this side pulled, on top of what is already queued:
 * ids the queue does not hold are appended in the order given, ids it holds
 * keep their entry and their place, and ids the list does not mention are
 * left alone. The displayed record and the in-flight marks are untouched.
 *
 * Merging rather than replacing ({@link reset}) because a pull and the
 * daemon's own events are separate routes with no guaranteed relative order:
 * a request that arrived by event while the pull was in flight would be
 * dropped by a full replace, while an entry the pull no longer lists is
 * removed by the withdrawal event that reports it.
 * @param state the current queue
 * @param requests the pending list this side pulled
 */
export function merge(
  state: ApprovalQueueState,
  requests: readonly ApprovalRequest[]
): ApprovalQueueState {
  return requests.reduce((queue, request) => enqueue(queue, request), state);
}

/**
 * Records that a request has been displayed.
 * @param state the current queue
 * @param requestId the displayed request's id
 */
export function markShown(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  const shownIds = new Set(state.shownIds);
  shownIds.add(requestId);
  return { ...state, shownIds };
}

/**
 * Whether a request has been displayed. Read before removing an entry, it
 * tells a request that expired unseen from one the user watched time out.
 * @param state the current queue
 * @param requestId the request's id
 */
export function wasShown(state: ApprovalQueueState, requestId: string): boolean {
  return state.shownIds.has(requestId);
}

/**
 * The request to display, `null` when the queue is empty or every remaining
 * request is in flight ({@link markInFlight}).
 * @param state the current queue
 */
export function head(state: ApprovalQueueState): ApprovalRequest | null {
  return state.items.find((entry) => !state.inFlightIds.has(entry.request_id)) ?? null;
}

/**
 * How many requests are pending, in-flight ones included: they are still
 * awaiting a settled outcome, just not offered for display right now.
 * @param state the current queue
 */
export function size(state: ApprovalQueueState): number {
  return state.items.length;
}

/**
 * Marks a request's decision as sent to the daemon, awaiting a reply.
 * @param state the current queue
 * @param requestId the request whose decision is in flight
 */
export function markInFlight(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  const inFlightIds = new Set(state.inFlightIds);
  inFlightIds.add(requestId);
  return { ...state, inFlightIds };
}

/**
 * Clears a request's in-flight mark, e.g. after its decision failed to reach
 * the daemon and may be retried.
 * @param state the current queue
 * @param requestId the request to offer again
 */
export function clearInFlight(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  const inFlightIds = new Set(state.inFlightIds);
  inFlightIds.delete(requestId);
  return { ...state, inFlightIds };
}

/**
 * Whether a click at `now` falls inside the guard window that follows the
 * dialog opening at `openedAt`.
 * @param openedAt when the dialog opened
 * @param now when the click happened
 */
export function isWithinGuard(openedAt: number, now: number): boolean {
  return now - openedAt < CLICK_GUARD_MS;
}

/**
 * Drops the entry with `requestId`, keeping the order of the rest, and drops
 * it from `inFlightIds` too: a decided or withdrawn id must not stay hidden
 * from `head()` forever if it is ever re-enqueued (e.g. after a reconnect
 * that never went through `reset`).
 */
function removeRequest(state: ApprovalQueueState, requestId: string): ApprovalQueueState {
  const inFlightIds = new Set(state.inFlightIds);
  inFlightIds.delete(requestId);
  return {
    items: state.items.filter((entry) => entry.request_id !== requestId),
    shownIds: state.shownIds,
    inFlightIds,
  };
}
