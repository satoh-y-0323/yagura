/**
 * The one place that decides which notice goes where, how many pop up, when,
 * and what is read out. The strip, the pop-ups, the Notifications list and the
 * announcer only draw what this returns. Pure: it reads no clock and holds no
 * timer.
 */
import {
  banners,
  describeAnnouncement,
  describeNoticeAndMore,
  describeWindowCall,
} from '../ui-messages';
import type { EventKey, EventLog } from './event-log';
import type { NeedsYouToast, NeedsYouToastState, NeedsYouToastStatus } from './needs-you-toasts';
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
  /** Every notice numbered up to this has been popped up, read out, or passed over. */
  readonly handledThrough: number;
}

/** One pop-up holding notices: one state notice, or the events of one kind folded together. */
interface NoticeSlot {
  readonly id: string;
  readonly seqs: readonly number[];
  readonly tone: Tone;
  /** Changes when an event is folded in, so a running timer starts over. */
  readonly restart: number;
}

/** What the pop-ups hold between runs. The shell hands it back as it was returned. */
export interface ToastSlots {
  /** In the order they came up. */
  readonly notices: readonly NoticeSlot[];
  /** Notices waiting for a slot, oldest first. */
  readonly waiting: readonly number[];
  /** Notices a window call pushed out; counted until seen, never shown again. */
  readonly pushedOut: readonly number[];
  /** The windows whose call is in a slot. */
  readonly calls: readonly string[];
  /** The sentence revision read out for each calling window. */
  readonly readCalls: ReadonlyMap<string, number>;
  readonly nextMark: number;
}

interface ToastBody {
  id: string;
  title: string;
  tone: Tone;
  /** Whether it goes with time; otherwise it stays until closed. */
  timed: boolean;
  /** A new value tells the part to start the time over. */
  restart: number;
}

/** One pop-up as the parts draw it. */
export type ToastItem = ToastBody &
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
  slots: ToastSlots;
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
  toasts: {
    visible: ToastItem[];
    /** Window calls waiting for a slot. */
    overflowWaiting: number;
    /** Notices that wait for a slot or were pushed out and are not yet seen. */
    overflowNotices: number;
    slots: ToastSlots;
  };
  /** What this run newly reads out, `null` for nothing. */
  announce: { polite: string | null; assertive: string | null };
}

const SLOT_COUNT = 3;

const CALL_CATEGORIES: Readonly<Record<NeedsYouToastStatus, NoticeCategory>> = {
  waiting_approval: 'needs-approval',
  waiting_input: 'needs-input',
};

const TONE_WEIGHT: Readonly<Record<Tone, number>> = { info: 0, warning: 1, error: 2 };

/** The memory a shell starts with. */
export function emptyNoticeMemory(): NoticeMemory {
  return { nextSeq: 0, stateSeqs: new Map(), handledThrough: -1 };
}

/** The pop-ups a shell starts with: none. */
export function emptyToastSlots(): ToastSlots {
  return {
    notices: [],
    waiting: [],
    pushedOut: [],
    calls: [],
    readCalls: new Map(),
    nextMark: 0,
  };
}

/**
 * Takes down one notice pop-up, closed or timed out; the list keeps its notices.
 * A window call's pop-up is closed through `stepNeedsYouToasts` instead, so its
 * id leaves the slots unchanged.
 * @param slots the slots the last run returned
 * @param id the pop-up's id
 */
export function dismissToast(slots: ToastSlots, id: string): ToastSlots {
  return { ...slots, notices: slots.notices.filter((slot) => slot.id !== id) };
}

interface Entry {
  notice: Notice;
  category: NoticeCategory | null;
  /** `false` when something else on screen already shows it. */
  poppable: boolean;
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
    });
  }
  return { entries, memory: { ...memory, nextSeq, stateSeqs } };
}

function heavier(a: Tone, b: Tone): Tone {
  return TONE_WEIGHT[b] > TONE_WEIGHT[a] ? b : a;
}

function spoken(notice: Pick<Notice, 'title' | 'action'>): string[] {
  return notice.action === null ? [notice.title] : [notice.title, banners.actionsAvailable];
}

function nameOf(names: ReadonlyMap<string, string>, windowId: string): string {
  return names.get(windowId) ?? windowId;
}

/**
 * Decides one run of the notices: what the strip, the list and the pop-ups
 * show, the unseen count, and what is newly read out.
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

  // A modal makes everything behind it inert, so nothing new pops up or is read until it closes.
  const held = input.openModalCount > 0;
  const polite: string[] = [];
  const assertive: string[] = [];
  let nextMark = input.slots.nextMark;

  let notices: NoticeSlot[] = input.slots.notices
    .map((slot) => ({ ...slot, seqs: slot.seqs.filter(listed) }))
    .filter((slot) => slot.seqs.length > 0);
  // A kind switched off while it waited for a slot no longer pops up; the list still has it.
  let waiting = input.slots.waiting.filter(
    (seq) => listed(seq) && switchedOn(bySeq.get(seq)?.category ?? null)
  );
  const pushedOut = input.slots.pushedOut.filter(listed);

  const calls = input.needsYou.toasts.filter((toast) => switchedOn(CALL_CATEGORIES[toast.status]));
  const shownCalls = new Set(
    calls
      .filter((toast) => input.slots.calls.includes(toast.windowId))
      .map((toast) => toast.windowId)
  );
  if (!held) {
    for (const toast of calls) {
      if (shownCalls.size >= SLOT_COUNT) {
        break;
      }
      shownCalls.add(toast.windowId);
    }
  }
  // A window call takes its slot from the oldest other pop-up; that one stays in the list only.
  while (notices.length > SLOT_COUNT - shownCalls.size) {
    const [oldest, ...rest] = notices;
    if (oldest === undefined) {
      break;
    }
    pushedOut.push(...oldest.seqs);
    notices = rest;
  }

  const foldableSlot = (entry: Entry): number =>
    entry.notice.source === 'event'
      ? notices.findIndex((slot) => {
          const first = bySeq.get(slot.seqs[0] ?? -1)?.notice;
          return first?.source === 'event' && first.key === entry.notice.key;
        })
      : -1;
  /** Puts the notice in a pop-up, folding an event into its kind's; `false` when no slot is free. */
  const place = (entry: Entry): boolean => {
    const at = foldableSlot(entry);
    const fold = notices[at];
    if (fold !== undefined) {
      notices = notices.map((slot, index) =>
        index === at
          ? {
              ...slot,
              seqs: [...slot.seqs, entry.notice.seq],
              tone: heavier(slot.tone, entry.notice.tone),
              restart: nextMark++,
            }
          : slot
      );
    } else if (notices.length < SLOT_COUNT - shownCalls.size) {
      notices = [
        ...notices,
        {
          id: `notice-${entry.notice.seq}`,
          seqs: [entry.notice.seq],
          tone: entry.notice.tone,
          restart: nextMark++,
        },
      ];
    } else {
      return false;
    }
    polite.push(...spoken(entry.notice));
    return true;
  };

  let handledThrough = input.memory.handledThrough;
  if (!held) {
    waiting = waiting.filter((seq) => {
      const entry = bySeq.get(seq);
      return entry !== undefined && !place(entry);
    });
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
      } else if (switchedOn(entry.category) && !place(entry)) {
        waiting.push(notice.seq);
      }
    }
    handledThrough = numbered.memory.nextSeq - 1;
  }

  const visibleCalls = calls.filter((toast) => shownCalls.has(toast.windowId));
  const readCalls = new Map(
    [...input.slots.readCalls].filter(([windowId]) =>
      input.needsYou.toasts.some((toast) => toast.windowId === windowId)
    )
  );
  const callItems = visibleCalls.map((toast: NeedsYouToast): ToastItem => {
    const title = describeWindowCall(nameOf(input.windowNames, toast.windowId), toast.status);
    if (!held && readCalls.get(toast.windowId) !== toast.sentenceRev) {
      readCalls.set(toast.windowId, toast.sentenceRev);
      polite.push(title, banners.actionsAvailable);
    }
    return {
      kind: 'needs-you',
      id: `needs-you-${toast.windowId}`,
      windowId: toast.windowId,
      status: toast.status,
      title,
      tone: toast.tone,
      timed: toast.timed,
      restart: toast.arrival,
    };
  });
  const noticeItems = notices.flatMap((slot): ToastItem[] => {
    const latest = bySeq.get(slot.seqs[slot.seqs.length - 1] ?? -1)?.notice;
    if (latest === undefined) {
      return [];
    }
    const folded = slot.seqs.length - 1;
    return [
      {
        kind: 'notice',
        id: slot.id,
        windowId: null,
        action: latest.action,
        title: folded > 0 ? describeNoticeAndMore(latest.title, folded) : latest.title,
        tone: slot.tone,
        timed: slot.tone === 'info',
        restart: slot.restart,
      },
    ];
  });

  const unseenPushedOut = pushedOut.filter((seq) => seq > input.seenThrough);
  return {
    memory: { ...numbered.memory, handledThrough },
    strip: blocking,
    center: [...blocking, ...others],
    unseen: others.filter((notice) => notice.seq > input.seenThrough).length,
    toasts: {
      visible: [...callItems, ...noticeItems],
      overflowWaiting: calls.length - visibleCalls.length,
      overflowNotices: waiting.length + unseenPushedOut.length,
      slots: {
        notices,
        waiting,
        pushedOut: unseenPushedOut,
        calls: visibleCalls.map((toast) => toast.windowId),
        readCalls,
        nextMark,
      },
    },
    announce: {
      polite: polite.length > 0 ? describeAnnouncement(polite) : null,
      assertive: assertive.length > 0 ? describeAnnouncement(assertive) : null,
    },
  };
}
