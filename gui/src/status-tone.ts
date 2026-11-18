import type {
  DetectedStatusView,
  StatusBoot,
  TaskReportMark,
  WindowStatus,
} from './ipc/session-boundary';
import { STATUS_PRESENTATION } from './status-presentation';

/**
 * What deciding "needs you" reads from an entry. A minimal structural shape
 * rather than the full `WindowEntry` type, so this module stays independent
 * of the store.
 */
export interface NeedsYouInput {
  detected: DetectedStatusView;
  taskReport?: TaskReportMark;
}

/**
 * Whether one entry needs the user: the single decision every "needs you"
 * display location reads. Which statuses count comes from the presentation
 * table's `needsYou` column; a task window waiting for input while its report
 * still stands is waiting on its owner, not on the user.
 */
export function needsYou(entry: NeedsYouInput): boolean {
  const { status } = entry.detected;
  if (status === 'waiting_input' && entry.taskReport?.state === 'standing') {
    return false;
  }
  return STATUS_PRESENTATION[status].needsYou;
}

/** The entries that need the user ({@link needsYou}), in input order. */
export function windowsNeedingYou<T extends NeedsYouInput>(entries: readonly T[]): T[] {
  return entries.filter(needsYou);
}

/**
 * Counts the entries that need the user's attention — exactly the size of
 * {@link windowsNeedingYou}'s result. Shared by every "needs you" display
 * location so none of them count independently.
 */
export function attentionCount<T extends NeedsYouInput>(entries: readonly T[]): number {
  return windowsNeedingYou(entries).length;
}

/** What a group of windows shows as one mark (a room heading, the Rail). */
export interface WindowStatusSummary {
  /** Windows that need the user. */
  count: number;
  /** The highest-ranked status in the group, `null` for an empty group. */
  mark: WindowStatus | null;
  /** Nothing needs the user, but a window is unknown: show the unknown mark alone, no count. */
  unknownOnly: boolean;
}

/** Gathers a group of windows into the one mark and count a collapsed display shows. */
export function summarizeWindowStatuses<T extends NeedsYouInput>(
  entries: readonly T[]
): WindowStatusSummary {
  let mark: WindowStatus | null = null;
  let hasUnknown = false;
  for (const { detected } of entries) {
    if (
      mark === null ||
      STATUS_PRESENTATION[detected.status].rank < STATUS_PRESENTATION[mark].rank
    ) {
      mark = detected.status;
    }
    hasUnknown ||= detected.status === 'unknown';
  }
  const count = attentionCount(entries);
  return { count, mark, unknownOnly: count === 0 && hasUnknown };
}

/**
 * What {@link observeNeedsYouArrivals} reads from an entry: the "needs you"
 * input, the window it belongs to, and the daemon boot its report mark was
 * counted in.
 */
export interface NeedsYouArrivalInput extends NeedsYouInput {
  windowId: string;
  statusBoot: StatusBoot | null;
}

/** The key a report mark is matched on: the boot it was counted in and its `seq` there. */
interface ReportKey {
  readonly boot: StatusBoot | null;
  readonly seq: number;
}

/** What one window's arrivals are decided against, carried from one observation to the next. */
export interface WindowArrivalMemory {
  /** The last status applied other than `unknown`; `null` while none has been. */
  readonly settled: WindowStatus | null;
  /** `running` or `waiting_approval` was applied after the last status that closes a turn. */
  readonly turnOpen: boolean;
  /** {@link needsYou} at the previous observation. */
  readonly needs: boolean;
  /** The key of the report last seen standing, while it can still be left with no reader. */
  readonly standingKey: ReportKey | null;
}

/**
 * Per-window memory of {@link observeNeedsYouArrivals}, keyed by window id.
 * Kept across daemon restarts and reconnects; only a fresh shell starts empty.
 */
export type NeedsYouArrivalMemory = ReadonlyMap<string, WindowArrivalMemory>;

/** The memory a shell starts with: no window seen yet. */
export function emptyArrivalMemory(): NeedsYouArrivalMemory {
  return new Map();
}

// A window seen for the first time is decided against this: with no open turn
// and no standing key, (B) and (C) cannot hold, so only (A) can arrive.
const UNSEEN: WindowArrivalMemory = {
  settled: null,
  turnOpen: false,
  needs: false,
  standingKey: null,
};

/**
 * Applies one observation of the windows and names those that started waiting
 * on the user in it. A window arrives at most once per observation, when any
 * of these holds against its memory from before the observation:
 * - (A) it is now `waiting_approval` and the last settled status was not;
 * - (B) it is now `waiting_input`, needs the user, and a turn was open;
 * - (C) it did not need the user and now does, because the report it was
 *   last seen standing on is now left with no reader.
 *
 * Every field of the memory is then updated by its own rule, whichever rule
 * (if any) made the window arrive. Windows absent from `entries` are dropped,
 * so one that comes back is seen for the first time again.
 * @param memory the memory returned by the previous observation
 * @param entries every window as this observation sees it
 * @returns the memory to pass to the next observation, and the arrived window ids in input order
 */
export function observeNeedsYouArrivals(
  memory: NeedsYouArrivalMemory,
  entries: readonly NeedsYouArrivalInput[]
): { memory: NeedsYouArrivalMemory; arrived: string[] } {
  const next = new Map<string, WindowArrivalMemory>();
  const arrived: string[] = [];
  for (const entry of entries) {
    const before = memory.get(entry.windowId) ?? UNSEEN;
    const { status } = entry.detected;
    const needs = needsYou(entry);
    const reportState = entry.taskReport?.state;
    const reportKey: ReportKey | null =
      entry.taskReport === undefined ? null : { boot: entry.statusBoot, seq: entry.taskReport.seq };
    const leftByItsReader =
      reportState === 'no_reader' &&
      before.standingKey !== null &&
      reportKey !== null &&
      sameReportKey(before.standingKey, reportKey);

    const approvalBegan = status === 'waiting_approval' && before.settled !== 'waiting_approval';
    const turnEnded = status === 'waiting_input' && before.turnOpen && needs;
    const readerLeft = !before.needs && needs && leftByItsReader;
    if (approvalBegan || turnEnded || readerLeft) {
      arrived.push(entry.windowId);
    }

    next.set(entry.windowId, {
      settled: status === 'unknown' ? before.settled : status,
      turnOpen: turnOpenAfter(status, before.turnOpen),
      needs,
      // Seeing the window need the user while its report no longer stands
      // counts as the reader's leaving having been shown, so the key goes.
      standingKey:
        reportState === 'standing'
          ? reportKey
          : leftByItsReader && !needs
            ? before.standingKey
            : null,
    });
  }
  return { memory: next, arrived };
}

/** Whether a turn is open after `status` is applied; `unknown` leaves it as it was. */
function turnOpenAfter(status: WindowStatus, wasOpen: boolean): boolean {
  switch (status) {
    case 'running':
    case 'waiting_approval':
      return true;
    case 'waiting_input':
    case 'idle':
    case 'interrupted':
    case 'exited':
      return false;
    case 'unknown':
      return wasOpen;
  }
}

/** Two report keys are equal when their `seq` and their boot are; two unnamed boots are equal. */
function sameReportKey(a: ReportKey, b: ReportKey): boolean {
  if (a.seq !== b.seq) {
    return false;
  }
  if (a.boot === null || b.boot === null) {
    return a.boot === b.boot;
  }
  return a.boot.origin === b.boot.origin && a.boot.seq === b.boot.seq;
}

/** The totals the shell tells the core about. */
export interface AttentionTotals {
  needsYou: number;
  pendingPermissions: number;
}

/** One message to the core: the totals and whether an arrival rides on it. */
export interface AttentionSend extends AttentionTotals {
  arrived: boolean;
}

/**
 * What the shell remembers about telling the core. Arrivals are counted, and
 * the count the core has acknowledged is kept apart, so an arrival that lands
 * while a send is in flight, or a send that fails, never loses one.
 */
export interface AttentionSendState {
  readonly lastSent: AttentionTotals | null;
  readonly arrivals: number;
  readonly acknowledged: number;
  readonly inFlight: readonly { send: AttentionSend; through: number }[];
}

/** The state a shell starts with: nothing sent, nothing arrived. */
export function emptyAttentionSendState(): AttentionSendState {
  return { lastSent: null, arrivals: 0, acknowledged: 0, inFlight: [] };
}

/** Records that something arrived for the user since the last acknowledged send. */
export function noteAttentionArrival(state: AttentionSendState): AttentionSendState {
  return { ...state, arrivals: state.arrivals + 1 };
}

/**
 * Decides whether to send now. A send is made when nothing was sent yet, the
 * totals differ from the last acknowledged send, an arrival is unacknowledged,
 * or `always` is set. The returned state tracks the send until it is settled.
 */
export function beginAttentionSend(
  state: AttentionSendState,
  totals: AttentionTotals,
  always: boolean
): { state: AttentionSendState; send: AttentionSend | null } {
  // An arrival already carried by a send in flight is not carried again; if that send
  // fails it leaves `inFlight` and the arrival is owed once more.
  const carried = state.inFlight.reduce(
    (most, entry) => (entry.send.arrived ? Math.max(most, entry.through) : most),
    state.acknowledged
  );
  const arrived = state.arrivals > carried;
  const last = state.lastSent;
  const moved =
    last === null ||
    last.needsYou !== totals.needsYou ||
    last.pendingPermissions !== totals.pendingPermissions;
  if (!always && !moved && !arrived) {
    return { state, send: null };
  }
  const send: AttentionSend = {
    needsYou: totals.needsYou,
    pendingPermissions: totals.pendingPermissions,
    arrived,
  };
  return {
    state: {
      ...state,
      inFlight: [...state.inFlight, { send, through: arrived ? state.arrivals : carried }],
    },
    send,
  };
}

/**
 * Applies the outcome of a send returned by {@link beginAttentionSend}.
 * Success acknowledges only the arrivals that send carried and records its
 * totals; failure changes nothing, so the next send carries them again.
 */
export function settleAttentionSend(
  state: AttentionSendState,
  send: AttentionSend,
  succeeded: boolean
): AttentionSendState {
  const entry = state.inFlight.find((item) => item.send === send);
  const inFlight = state.inFlight.filter((item) => item.send !== send);
  if (!succeeded || entry === undefined) {
    return { ...state, inFlight };
  }
  return {
    ...state,
    inFlight,
    lastSent: { needsYou: send.needsYou, pendingPermissions: send.pendingPermissions },
    acknowledged: Math.max(state.acknowledged, entry.through),
  };
}
