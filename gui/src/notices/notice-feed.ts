/**
 * The one place that decides which notice goes where, which cards the lane
 * shows, and what is read out. The strip, the lane, the Notifications list and
 * the announcer only draw what this returns. Pure: it reads no clock and holds
 * no timer.
 */
import {
  banners,
  describeAnnouncement,
  describeNoticeAndMore,
  describeWindowCall,
  describeWindowCallParts,
  type TitleParts,
} from '../ui-messages';
import type { EventKey, EventLog } from './event-log';
import type { NeedsYouToast, NeedsYouToastState, NeedsYouToastStatus } from './needs-you-toasts';
import type { LaneCapacity } from './lane-capacity';
import {
  classifyNotice,
  type NoticeAction,
  type NoticeCategory,
  type NoticeClass,
  type NoticeDraft,
  type StateNoticeKey,
} from './notice-model';
import type { NotificationPreferences } from './notification-preferences';

type Tone = NoticeClass['tone'];

/** What every notice carries, wherever it is shown. */
interface NoticeBody extends Pick<NoticeClass, 'blocking' | 'tone' | 'dismissible'> {
  /** Unique and only growing, so the list's order and the unseen count need no clock. */
  seq: number;
  title: string;
  detail: string | null;
  action: NoticeAction | null;
  /** When an event was recorded, `HH:mm`; `null` for a state notice. */
  at: string | null;
}

/** A numbered notice: a state the app is in, or something that happened. */
export type Notice = NoticeBody &
  ({ source: 'state'; key: StateNoticeKey } | { source: 'event'; key: EventKey });

/** What the feed remembers between runs about numbering. */
export interface NoticeMemory {
  /** The next number handed out, to state notices here and to events by `appendEvent`. */
  readonly nextSeq: number;
  /** The number of each state notice on show, by what it says; forgotten once it is gone. */
  readonly stateSeqs: ReadonlyMap<string, number>;
  /** Every notice numbered up to this has been put on the lane, read out, or passed over. */
  readonly handledThrough: number;
}

/** One item on the lane: a window calling the user, or notices of one kind folded together. */
export type LaneEntry =
  | { readonly kind: 'call'; readonly windowId: string }
  | {
      readonly kind: 'notice';
      readonly id: string;
      readonly seqs: readonly number[];
      readonly tone: Tone;
      /** Changes when an event is folded in, so a running timer starts over. */
      readonly restart: number;
    };

/**
 * What the lane holds between runs; the shell hands it back as it was returned.
 * It records only what is on the lane and what was read, never what was left
 * out, so which cards show is derived afresh from the lane and the width.
 */
export interface LaneState {
  /** In the order the items came. */
  readonly lane: readonly LaneEntry[];
  /** The sentence revision read out for each calling window. */
  readonly readCalls: ReadonlyMap<string, number>;
  readonly nextMark: number;
}

interface LaneItemBody {
  id: string;
  /** The whole sentence, with "and N more" when events are folded in: the name and the tooltip. */
  title: string;
  /** The title split around the window's name so only the name is cut short; `null` when it names none. */
  parts: TitleParts | null;
  /** How many other events are folded into the card. */
  folded: number;
  tone: Tone;
  /** Whether it goes with time; otherwise it stays until closed. */
  timed: boolean;
  /** A new value tells the part to start the time over. */
  restart: number;
}

/** One card on the lane as the parts draw it. */
export type LaneItem = LaneItemBody &
  (
    | { kind: 'notice'; windowId: null; action: NoticeAction | null }
    | { kind: 'needs-you'; windowId: string; status: NeedsYouToastStatus }
  );

/** What one run is decided from. */
export interface NoticeFeedInput {
  drafts: NoticeDraft[];
  log: EventLog;
  memory: NoticeMemory;
  /** The newest number the user has seen in the list, `-1` when none. */
  seenThrough: number;
  needsYou: NeedsYouToastState;
  prefs: NotificationPreferences;
  openModalCount: number;
  lane: LaneState;
  /** How many cards fit on the lane, `null` until it has been measured. */
  capacity: LaneCapacity | null;
  /** Display names by window id, for the window calls' sentences. */
  windowNames: ReadonlyMap<string, string>;
}

/** What the shell draws and keeps. */
export interface NoticeFeedOutput {
  memory: NoticeMemory;
  /** Blocking notices, in the order they were built. */
  strip: Notice[];
  /** The list: blocking notices first, then the rest newest first. */
  center: Notice[];
  /** Notices newer than `seenThrough`, blocking ones left out. */
  unseen: number;
  lane: {
    /** The cards that fit, in the order they came. */
    visible: LaneItem[];
    /** Window calls on the lane that did not fit. */
    moreCalls: number;
    /** Notices on the lane that did not fit, whether or not they have been seen. */
    moreNotices: number;
    state: LaneState;
  };
  /** What this run newly reads out, `null` for nothing. */
  announce: { polite: string | null; assertive: string | null };
}

const CALL_CATEGORIES: Readonly<Record<NeedsYouToastStatus, NoticeCategory>> = {
  waiting_approval: 'needs-approval',
  waiting_input: 'needs-input',
};

const TONE_WEIGHT: Readonly<Record<Tone, number>> = { info: 0, warning: 1, error: 2 };

/** The memory a shell starts with. */
export function emptyNoticeMemory(): NoticeMemory {
  return { nextSeq: 0, stateSeqs: new Map(), handledThrough: -1 };
}

/** The lane a shell starts with: empty. */
export function emptyLaneState(): LaneState {
  return { lane: [], readCalls: new Map(), nextMark: 0 };
}

/**
 * Takes one notice card off the lane, closed, timed out or pressed; the list
 * keeps its notices. A window call's card is closed through
 * `stepNeedsYouToasts` instead, so its id leaves the lane unchanged.
 * @param state the lane the last run returned
 * @param id the card's id
 */
export function dismissLaneNotice(state: LaneState, id: string): LaneState {
  return {
    ...state,
    lane: state.lane.filter((entry) => entry.kind !== 'notice' || entry.id !== id),
  };
}

interface Entry {
  notice: Notice;
  category: NoticeCategory | null;
  /** `false` when something else on screen already shows it. */
  poppable: boolean;
  parts: TitleParts | null;
}

function identity(draft: NoticeDraft): string {
  return JSON.stringify([draft.key, draft.title, draft.detail]);
}

function numberNotices(
  drafts: readonly NoticeDraft[],
  log: EventLog,
  memory: NoticeMemory
): { entries: Entry[]; memory: NoticeMemory } {
  let nextSeq = memory.nextSeq;
  const stateSeqs = new Map<string, number>();
  const entries: Entry[] = [];
  for (const draft of drafts) {
    const id = identity(draft);
    // The same fact built twice is one notice.
    if (stateSeqs.has(id)) {
      continue;
    }
    const seq = memory.stateSeqs.get(id) ?? nextSeq++;
    stateSeqs.set(id, seq);
    const { key, blocking, tone, dismissible, title, detail, action } = draft;
    entries.push({
      notice: {
        source: 'state',
        key,
        seq,
        blocking,
        tone,
        dismissible,
        title,
        detail,
        action,
        at: null,
      },
      category: classifyNotice(key, {}).category,
      poppable: true,
      parts: null,
    });
  }
  for (const event of log) {
    const { blocking, dismissible, category } = classifyNotice(event.key, {});
    entries.push({
      notice: {
        source: 'event',
        key: event.key,
        seq: event.seq,
        blocking,
        tone: event.tone,
        dismissible,
        title: event.text,
        detail: null,
        action: null,
        at: event.at,
      },
      category,
      poppable: event.toast,
      parts: event.parts,
    });
  }
  return { entries, memory: { ...memory, nextSeq, stateSeqs } };
}

function heavier(a: Tone, b: Tone): Tone {
  return TONE_WEIGHT[b] > TONE_WEIGHT[a] ? b : a;
}

/** How the strip and an event shown elsewhere are read: actions only when there are some. */
function spoken(notice: Pick<Notice, 'title' | 'action'>): string[] {
  return notice.action === null ? [notice.title] : [notice.title, banners.actionsAvailable];
}

/** How a card on the lane is read: the whole card acts, so every card offers an action. */
function spokenAsCard(title: string): string[] {
  return [title, banners.actionsAvailable];
}

function nameOf(names: ReadonlyMap<string, string>, windowId: string): string {
  return names.get(windowId) ?? windowId;
}

function entryKey(entry: LaneEntry): string {
  return entry.kind === 'call' ? `needs-you-${entry.windowId}` : entry.id;
}

/**
 * The lane items that show, derived from the lane and the capacity alone: all
 * of them while they fit, else approvals, then input waits, then notices, as
 * many as fit beside the counts, kept in the order they came.
 */
function pickShown(
  lane: readonly LaneEntry[],
  capacity: LaneCapacity | null,
  calls: ReadonlyMap<string, NeedsYouToast>
): ReadonlySet<string> {
  if (capacity === null) {
    return new Set();
  }
  if (lane.length <= capacity.all) {
    return new Set(lane.map(entryKey));
  }
  const rank = (entry: LaneEntry): number => {
    if (entry.kind === 'notice') {
      return 2;
    }
    return calls.get(entry.windowId)?.status === 'waiting_approval' ? 0 : 1;
  };
  // `sort` is stable, so each rank keeps the lane's order.
  const ranked = [...lane].sort((a, b) => rank(a) - rank(b));
  return new Set(ranked.slice(0, capacity.withMore).map(entryKey));
}

/**
 * Decides one run of the notices: what the strip, the list and the lane show,
 * the unseen count, and what is newly read out.
 * @param input the notices, the user's choices, and what the last run returned
 */
export function noticeFeed(input: NoticeFeedInput): NoticeFeedOutput {
  const numbered = numberNotices(input.drafts, input.log, input.memory);
  const entries = numbered.entries;
  const bySeq = new Map(entries.map((entry) => [entry.notice.seq, entry]));
  const blocking = entries.filter((entry) => entry.notice.blocking).map((entry) => entry.notice);
  const others = entries
    .filter((entry) => !entry.notice.blocking)
    .map((entry) => entry.notice)
    .sort((a, b) => b.seq - a.seq);
  const listed = (seq: number): boolean => bySeq.get(seq)?.notice.blocking === false;
  const switchedOn = (category: NoticeCategory | null): boolean =>
    category === null || input.prefs.inApp[category];
  const calls = new Map(input.needsYou.toasts.map((toast) => [toast.windowId, toast]));
  // The one condition for a window call to be on the lane: it calls, and its row is on.
  const callOnLane = (windowId: string): boolean => {
    const toast = calls.get(windowId);
    return toast !== undefined && switchedOn(CALL_CATEGORIES[toast.status]);
  };

  // A modal makes everything behind it inert, so nothing new joins the lane or is read until it closes.
  const held = input.openModalCount > 0;
  const polite: string[] = [];
  const assertive: string[] = [];
  let nextMark = input.lane.nextMark;

  const lane: LaneEntry[] = input.lane.lane.flatMap((entry): LaneEntry[] => {
    if (entry.kind === 'call') {
      return callOnLane(entry.windowId) ? [entry] : [];
    }
    const seqs = entry.seqs.filter(listed);
    const category = bySeq.get(seqs[0] ?? -1)?.category ?? null;
    return seqs.length > 0 && switchedOn(category) ? [{ ...entry, seqs }] : [];
  });

  /** The notices that joined the lane this run, with the lane item each went into. */
  const arrived: { notice: Notice; key: string }[] = [];
  /** Puts a notice on the lane, folding an event into its kind's item wherever that is. */
  const join = (notice: Notice): void => {
    const at =
      notice.source === 'event'
        ? lane.findIndex((entry) => {
            const first = entry.kind === 'notice' ? bySeq.get(entry.seqs[0] ?? -1)?.notice : null;
            return first?.source === 'event' && first.key === notice.key;
          })
        : -1;
    const fold = lane[at];
    if (fold?.kind === 'notice') {
      lane[at] = {
        ...fold,
        seqs: [...fold.seqs, notice.seq],
        tone: heavier(fold.tone, notice.tone),
        restart: nextMark++,
      };
      arrived.push({ notice, key: fold.id });
      return;
    }
    const id = `notice-${notice.seq}`;
    lane.push({ kind: 'notice', id, seqs: [notice.seq], tone: notice.tone, restart: nextMark++ });
    arrived.push({ notice, key: id });
  };

  let handledThrough = input.memory.handledThrough;
  if (!held) {
    const arrivals = entries
      .filter((entry) => entry.notice.seq > handledThrough)
      .sort((a, b) => a.notice.seq - b.notice.seq);
    for (const entry of arrivals) {
      const { notice } = entry;
      if (notice.blocking) {
        (notice.tone === 'info' ? polite : assertive).push(...spoken(notice));
      } else if (!entry.poppable) {
        // Shown where it happened already, so it is read out whatever the settings say.
        polite.push(...spoken(notice));
      } else if (switchedOn(entry.category)) {
        join(notice);
      }
    }
    handledThrough = numbered.memory.nextSeq - 1;
    // One run tells no order between a notice and a call that came in it, so calls go after.
    for (const toast of input.needsYou.toasts) {
      const onLane = lane.some(
        (entry) => entry.kind === 'call' && entry.windowId === toast.windowId
      );
      if (!onLane && callOnLane(toast.windowId)) {
        lane.push({ kind: 'call', windowId: toast.windowId });
      }
    }
  }

  const shown = pickShown(lane, input.capacity, calls);
  // Before the lane is measured nothing shows, so what arrives is read as the card it will be.
  const readAsCard = (key: string): boolean => input.capacity === null || shown.has(key);
  const callTitle = (toast: NeedsYouToast): string =>
    describeWindowCall(nameOf(input.windowNames, toast.windowId), toast.status);

  // Read once on arrival, whether or not it shows; what does not show is only counted.
  let unreadNotices = 0;
  for (const { notice, key } of arrived) {
    if (readAsCard(key)) {
      polite.push(...spokenAsCard(notice.title));
    } else {
      unreadNotices++;
    }
  }
  const readCalls = new Map([...input.lane.readCalls].filter(([windowId]) => calls.has(windowId)));
  let unreadCalls = 0;
  if (!held) {
    for (const entry of lane) {
      const toast = entry.kind === 'call' ? calls.get(entry.windowId) : undefined;
      if (toast === undefined || readCalls.get(toast.windowId) === toast.sentenceRev) {
        continue;
      }
      readCalls.set(toast.windowId, toast.sentenceRev);
      if (readAsCard(entryKey(entry))) {
        polite.push(...spokenAsCard(callTitle(toast)));
      } else {
        unreadCalls++;
      }
    }
  }
  if (unreadCalls > 0) {
    polite.push(banners.waitingForYouMore(unreadCalls));
  }
  if (unreadNotices > 0) {
    polite.push(banners.notificationsMore(unreadNotices));
  }

  const visible = lane
    .filter((entry) => shown.has(entryKey(entry)))
    .flatMap((entry): LaneItem[] => {
      if (entry.kind === 'call') {
        const toast = calls.get(entry.windowId);
        if (toast === undefined) {
          return [];
        }
        const name = nameOf(input.windowNames, toast.windowId);
        return [
          {
            kind: 'needs-you',
            id: entryKey(entry),
            windowId: toast.windowId,
            status: toast.status,
            title: callTitle(toast),
            parts: describeWindowCallParts(name, toast.status),
            folded: 0,
            tone: toast.tone,
            timed: toast.timed,
            restart: toast.arrival,
          },
        ];
      }
      const latest = bySeq.get(entry.seqs[entry.seqs.length - 1] ?? -1);
      if (latest === undefined) {
        return [];
      }
      const folded = entry.seqs.length - 1;
      return [
        {
          kind: 'notice',
          id: entry.id,
          windowId: null,
          action: latest.notice.action,
          title:
            folded > 0 ? describeNoticeAndMore(latest.notice.title, folded) : latest.notice.title,
          parts: latest.parts,
          folded,
          tone: entry.tone,
          timed: entry.tone === 'info',
          restart: entry.restart,
        },
      ];
    });
  const measured = input.capacity !== null;
  const left = lane.filter((entry) => measured && !shown.has(entryKey(entry)));

  return {
    memory: { ...numbered.memory, handledThrough },
    strip: blocking,
    center: [...blocking, ...others],
    unseen: others.filter((notice) => notice.seq > input.seenThrough).length,
    lane: {
      visible,
      moreCalls: left.filter((entry) => entry.kind === 'call').length,
      moreNotices: left.filter((entry) => entry.kind === 'notice').length,
      state: { lane, readCalls, nextMark },
    },
    announce: {
      polite: polite.length > 0 ? describeAnnouncement(polite) : null,
      assertive: assertive.length > 0 ? describeAnnouncement(assertive) : null,
    },
  };
}
