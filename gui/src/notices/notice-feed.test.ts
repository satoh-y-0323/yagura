import { describe, it, expect, vi } from 'vitest';

import type { WindowStatus } from '../ipc/session-boundary';
import { dummyMessages } from '../test-utils/dummy-messages';
import type { TitleParts } from '../ui-messages';
import { appendEvent, failureEvent } from './event-log';
import type { EventLog, NewEvent } from './event-log';
import {
  emptyNeedsYouToastState,
  stepNeedsYouToasts,
  type NeedsYouToastState,
  type NeedsYouToastWindow,
} from './needs-you-toasts';
import { emptyNoticeMemory, emptyLaneState, dismissLaneNotice, noticeFeed } from './notice-feed';
import type { LaneState, NoticeFeedInput, NoticeMemory } from './notice-feed';
import type { NoticeAction, NoticeCategory, NoticeDraft, StateNoticeKey } from './notice-model';
import { NOTICE_CATEGORIES } from './notice-model';
import {
  readPreferences,
  withPreference,
  type NotificationPreferences,
} from './notification-preferences';

// Every sentence the feed builds goes through the facade; binding the facade to
// the dummy locale makes a sentence typed into the feed show up as a mismatch.
vi.mock('../i18n/messages', async () => {
  const dummy = await import('../test-utils/dummy-messages');
  return { resolveMessages: () => dummy.dummyMessages };
});

type FeedOutput = ReturnType<typeof noticeFeed>;
type LaneItem = FeedOutput['lane']['visible'][number];
type Capacity = NoticeFeedInput['capacity'];

/** Everything the shell keeps between two runs of the feed, and the inputs it passes. */
interface Shell {
  drafts: NoticeDraft[];
  log: EventLog;
  memory: NoticeMemory;
  lane: LaneState;
  capacity: Capacity;
  needsYou: NeedsYouToastState;
  prefs: NotificationPreferences;
  openModalCount: number;
  seenThrough: number;
  windowNames: ReadonlyMap<string, string>;
}

const NAMES: ReadonlyMap<string, string> = new Map([
  ['w-a', 'alpha'],
  ['w-b', 'bravo'],
  ['w-c', 'charlie'],
  ['w-d', 'delta'],
  ['w-e', 'echo'],
]);

function shell(): Shell {
  return {
    drafts: [],
    log: [],
    memory: emptyNoticeMemory(),
    lane: emptyLaneState(),
    // Three cards fit with or without the counts, as on a lane 900px wide.
    capacity: { all: 3, withMore: 3 },
    needsYou: emptyNeedsYouToastState(),
    prefs: readPreferences(null),
    openModalCount: 0,
    // Nothing has been seen yet.
    seenThrough: -1,
    windowNames: NAMES,
  };
}

/** Runs the feed once and keeps what it hands back, as the shell does. */
function run(s: Shell): FeedOutput {
  const out = noticeFeed({
    drafts: s.drafts,
    log: s.log,
    memory: s.memory,
    seenThrough: s.seenThrough,
    needsYou: s.needsYou,
    prefs: s.prefs,
    openModalCount: s.openModalCount,
    lane: s.lane,
    capacity: s.capacity,
    windowNames: s.windowNames,
  });
  s.memory = out.memory;
  s.lane = out.lane.state;
  return out;
}

function record(s: Shell, event: NewEvent): void {
  const next = appendEvent(s.log, s.memory, event, `at-${event.text}`);
  s.log = next.log;
  s.memory = next.memory;
}

function setup(
  key: StateNoticeKey,
  title: string,
  action: NoticeAction | null = null,
  detail: string | null = null
): NoticeDraft {
  return { key, blocking: false, tone: 'warning', dismissible: true, title, detail, action };
}

function blocking(key: StateNoticeKey, tone: NoticeDraft['tone'], title: string): NoticeDraft {
  return {
    key,
    blocking: true,
    tone,
    dismissible: false,
    title,
    detail: `${title}-detail`,
    action: null,
  };
}

function exitOf(
  windowId: string,
  exitCode: number,
  toast = true,
  parts: TitleParts | null = null
): NewEvent {
  return {
    key: 'window-exit',
    tone: exitCode === 0 ? 'info' : 'warning',
    text: `exit-${windowId}`,
    parts,
    toast,
    windowId,
  };
}

/** The exited window's sentence split around its display name, as the work area builds it. */
function exitPartsOf(windowId: string, exitCode: number): TitleParts {
  return dummyMessages.workArea.windowExitedWithCodeParts(
    NAMES.get(windowId) ?? windowId,
    exitCode
  );
}

function win(windowId: string, status: WindowStatus, needsYou = true): NeedsYouToastWindow {
  return { windowId, status, needsYou };
}

/** One observation of the windows: the ones named arrive, every one listed is as given. */
function observe(
  s: Shell,
  windows: readonly NeedsYouToastWindow[],
  arrived: readonly string[]
): void {
  s.needsYou = stepNeedsYouToasts(s.needsYou, { arrived, windows, seenWindowId: null });
}

function titles(out: FeedOutput): string[] {
  return out.lane.visible.map((item) => item.title);
}

function callIds(out: FeedOutput): (string | null)[] {
  return out.lane.visible.filter((item) => item.kind === 'needs-you').map((item) => item.windowId);
}

/** The lane left to right: a window call by its window, a notice by its title. */
function shown(out: FeedOutput): string[] {
  return out.lane.visible.map((item) => item.windowId ?? item.title);
}

function cardTitled(out: FeedOutput, title: string): LaneItem {
  const item = out.lane.visible.find((entry) => entry.title === title);
  if (item === undefined) {
    throw new Error(`no card titled ${title}`);
  }
  return item;
}

function noticeCard(item: LaneItem): Extract<LaneItem, { kind: 'notice' }> {
  if (item.kind !== 'notice') {
    throw new Error(`card ${item.id} is not a notice`);
  }
  return item;
}

function cardOfKind(
  out: FeedOutput,
  kind: LaneItem['kind'],
  windowId: string | null = null
): LaneItem {
  const item = out.lane.visible.find(
    (entry) => entry.kind === kind && (windowId === null || entry.windowId === windowId)
  );
  if (item === undefined) {
    throw new Error(`no ${kind} card`);
  }
  return item;
}

function seqOf(out: FeedOutput, title: string): number {
  const notice = out.center.find((entry) => entry.title === title);
  if (notice === undefined) {
    throw new Error(`no notice titled ${title} in the list`);
  }
  return notice.seq;
}

function dismiss(s: Shell, out: FeedOutput, title: string): void {
  s.lane = dismissLaneNotice(s.lane, cardTitled(out, title).id);
}

function allOff(): NotificationPreferences {
  return NOTICE_CATEGORIES.reduce(
    (prefs, row: NoticeCategory) => withPreference(prefs, row, false),
    readPreferences(null)
  );
}

const more = (sentence: string, count: number): string =>
  dummyMessages.banners.noticeAndMore(sentence, count);

/** What a card on the lane reads out: its sentence, then that it can be acted on. */
const card = (sentence: string): string =>
  dummyMessages.formats
    .sentences([sentence, dummyMessages.banners.actionsAvailable])
    .slice('DUMMY_sentences('.length, -1);

const {
  windowNeedsApproval,
  windowNeedsInput,
  waitingForYouMoreAnnouncement,
  notificationsMoreAnnouncement,
} = dummyMessages.banners;

describe('notice feed: which notices come onto the lane', () => {
  it('puts a state notice on the lane once while it says the same, and again once it says something new, with the detail of the notice it shows', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'setup-a', null, 'setup-a-detail')];
    const first = run(s);
    expect(titles(first)).toEqual(['setup-a']);
    expect(noticeCard(cardTitled(first, 'setup-a')).detail).toBe('setup-a-detail');

    dismiss(s, first, 'setup-a');
    expect(titles(run(s))).toEqual([]);

    s.drafts = [setup('detection-warning', 'setup-b')];
    const third = run(s);
    expect(titles(third)).toEqual(['setup-b']);
    expect(noticeCard(cardTitled(third, 'setup-b')).detail).toBeNull();
  });

  it('puts on the lane and numbers two kinds that say the same sentence, one card each', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'same'), setup('keymap-notice', 'same')];
    const out = run(s);
    expect(out.lane.visible).toHaveLength(2);
    expect(new Set(out.center.map((notice) => notice.seq)).size).toBe(2);
  });

  it('keeps a blocking notice off the lane and puts it in the strip, in the order built', () => {
    const s = shell();
    s.drafts = [
      blocking('startup-env-failure', 'error', 'block-1'),
      blocking('daemon-incompatible', 'warning', 'block-2'),
      setup('detection-warning', 'setup-a'),
    ];
    const out = run(s);
    expect(out.strip.map((notice) => notice.title)).toEqual(['block-1', 'block-2']);
    expect(titles(out)).toEqual(['setup-a']);
  });

  it('keeps an event off the lane that is not poppable because something else on screen already shows it, but reads it and lists it', () => {
    const s = shell();
    record(s, exitOf('w-a', 1, false));
    const out = run(s);
    expect(titles(out)).toEqual([]);
    expect(out.center.map((notice) => notice.title)).toContain('exit-w-a');
    expect(out.announce.polite).toContain('exit-w-a');
  });

  it('holds lane cards and announcements while a modal is open and lets them out once it closes', () => {
    const s = shell();
    s.openModalCount = 1;
    s.drafts = [
      blocking('startup-env-failure', 'error', 'block-1'),
      setup('detection-warning', 'setup-a'),
    ];
    const held = run(s);
    expect(titles(held)).toEqual([]);
    expect(held.strip.map((notice) => notice.title)).toEqual(['block-1']);
    expect(held.announce).toEqual({ polite: null, assertive: null });

    s.openModalCount = 0;
    const released = run(s);
    expect(titles(released)).toEqual(['setup-a']);
    expect(released.announce.polite).toContain('setup-a');
    expect(released.announce.assertive).toContain('block-1');
  });
});

describe('notice feed: how many cards the lane shows', () => {
  it('shows every item while they fit, picks approvals, then input waits, then notices once they do not, keeps them in the order they came, and brings back what a narrower lane left out, showing nothing before it is measured', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2'].map((title) => setup('detection-warning', title));
    run(s);
    observe(s, [win('w-b', 'waiting_input')], ['w-b']);
    run(s);
    observe(s, [win('w-b', 'waiting_input'), win('w-a', 'waiting_approval')], ['w-a']);
    s.capacity = { all: 4, withMore: 3 };
    const fits = run(s);
    expect(shown(fits)).toEqual(['s-1', 's-2', 'w-b', 'w-a']);
    expect([fits.lane.moreCalls, fits.lane.moreNotices]).toEqual([0, 0]);

    const narrower: readonly [Capacity, string[], number, number][] = [
      [{ all: 3, withMore: 3 }, ['s-1', 'w-b', 'w-a'], 0, 1],
      [{ all: 3, withMore: 2 }, ['w-b', 'w-a'], 0, 2],
      [{ all: 2, withMore: 1 }, ['w-a'], 1, 2],
      [{ all: 1, withMore: 1 }, ['w-a'], 1, 2],
      // Wide again: what the narrower lanes left out is back, where it came in.
      [{ all: 4, withMore: 3 }, ['s-1', 's-2', 'w-b', 'w-a'], 0, 0],
    ];
    for (const [capacity, cards, moreCalls, moreNotices] of narrower) {
      s.capacity = capacity;
      const out = run(s);
      expect(shown(out)).toEqual(cards);
      expect([out.lane.moreCalls, out.lane.moreNotices]).toEqual([moreCalls, moreNotices]);
      // A change of width shows or hides cards, and reads nothing again.
      expect(out.announce).toEqual({ polite: null, assertive: null });
    }

    // Until the lane is measured nothing is up, what comes stays, and each arrival is read as its card.
    const unmeasured = shell();
    unmeasured.capacity = null;
    unmeasured.drafts = ['s-1', 's-2', 's-3', 's-4'].map((title) =>
      setup('detection-warning', title)
    );
    const first = run(unmeasured);
    observe(unmeasured, [win('w-a', 'waiting_approval')], ['w-a']);
    const second = run(unmeasured);
    expect(shown(first)).toEqual([]);
    expect(shown(second)).toEqual([]);
    // Nothing is left out before the lane is measured, so no count shows.
    expect([second.lane.moreCalls, second.lane.moreNotices]).toEqual([0, 0]);
    expect(second.lane.moreCallsStatus).toBe('waiting_input');
    for (const title of ['s-1', 's-2', 's-3', 's-4']) {
      expect(first.announce.polite).toContain(card(title));
    }
    expect(second.announce.polite).toContain(card(windowNeedsApproval('alpha')));

    unmeasured.capacity = { all: 5, withMore: 4 };
    const measured = run(unmeasured);
    expect(shown(measured)).toEqual(['s-1', 's-2', 's-3', 's-4', 'w-a']);
    expect(measured.announce).toEqual({ polite: null, assertive: null });
  });

  it.each([
    [0, { all: 1, withMore: 1 }],
    [280, { all: 1, withMore: 1 }],
    [391, { all: 1, withMore: 1 }],
    [663, { all: 2, withMore: 2 }],
    [800, { all: 3, withMore: 2 }],
    [879, { all: 3, withMore: 2 }],
    [880, { all: 3, withMore: 3 }],
    [900, { all: 3, withMore: 3 }],
    [1063, { all: 4, withMore: 3 }],
  ] as const)('fits %ipx of work area to %o cards', async (width, capacity) => {
    const { laneCapacity } = await import('./lane-capacity');
    expect(laneCapacity(width, { card: 240, gap: 8, padding: 8, chip: 56 })).toEqual(capacity);
  });

  it('lets the notice that waited longest in once a card goes', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2', 's-3', 's-4', 's-5'].map((title) =>
      setup('detection-warning', title)
    );
    const first = run(s);
    // The cards up are read as cards; the ones left out only as how many they are.
    for (const title of ['s-1', 's-2', 's-3']) {
      expect(first.announce.polite).toContain(card(title));
    }
    expect(first.announce.polite).toContain(notificationsMoreAnnouncement(2));
    expect(first.announce.polite).not.toContain('s-4');
    expect(first.announce.polite).not.toContain('s-5');

    dismiss(s, first, 's-1');
    const freed = run(s);
    expect(titles(freed)).toEqual(['s-2', 's-3', 's-4']);
    expect(freed.announce.polite).toBeNull();

    record(s, failureEvent('operation-failure', 'failure-1'));
    const late = run(s);
    expect(titles(late)).toEqual(['s-2', 's-3', 's-4']);
    expect(late.announce.polite).toContain(notificationsMoreAnnouncement(1));
    expect(late.announce.polite).not.toContain('failure-1');
    expect(run(s).announce.polite).toBeNull();

    // A notice that takes the lane past what fits moves the last notice up into the count.
    const narrow = shell();
    narrow.capacity = { all: 3, withMore: 2 };
    narrow.drafts = ['s-1', 's-2', 's-3'].map((title) => setup('detection-warning', title));
    expect(titles(run(narrow))).toEqual(['s-1', 's-2', 's-3']);
    record(narrow, failureEvent('operation-failure', 'failure-1'));
    const fell = run(narrow);
    expect(titles(fell)).toEqual(['s-1', 's-2']);
    expect(fell.lane.moreNotices).toBe(2);
    dismiss(narrow, fell, 's-1');
    const back = run(narrow);
    expect(titles(back)).toEqual(['s-2', 's-3', 'failure-1']);
    expect(back.announce.polite).toBeNull();
  });

  it('puts a window call after the cards already up, moves the last notice into the count even once it is seen, and brings it back once the call goes', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2', 's-3'].map((title) => setup('detection-warning', title));
    const shownFirst = run(s);

    observe(s, [win('w-a', 'waiting_approval')], ['w-a']);
    const pushed = run(s);
    expect(pushed.lane.visible.map((item) => item.kind)).toEqual(['notice', 'notice', 'needs-you']);
    expect(callIds(pushed)).toEqual(['w-a']);
    expect(titles(pushed).slice(0, 2)).toEqual(['s-1', 's-2']);
    expect(pushed.lane.moreNotices).toBe(1);

    observe(s, [win('w-a', 'waiting_approval'), win('w-b', 'waiting_approval')], ['w-b']);
    const twice = run(s);
    expect(shown(twice)).toEqual(['s-1', 'w-a', 'w-b']);
    expect(twice.lane.moreNotices).toBe(2);

    // Seeing them in the list leaves them on the lane, still counted.
    s.seenThrough = seqOf(shownFirst, 's-3');
    expect(run(s).lane.moreNotices).toBe(2);

    observe(s, [win('w-a', 'running', false), win('w-b', 'running', false)], []);
    const gone = run(s);
    expect(titles(gone)).toEqual(['s-1', 's-2', 's-3']);
    expect(gone.announce.polite).toBeNull();
  });

  it('lets an approval push the last input wait out but never a call of its own kind, and shows the calls in the order they came', () => {
    const s = shell();
    const windows = (statusOfB: WindowStatus, needsB: boolean) => [
      win('w-a', 'waiting_approval'),
      win('w-b', statusOfB, needsB),
      win('w-c', 'waiting_input'),
      win('w-d', 'waiting_input'),
      win('w-e', 'waiting_approval'),
      win('w-f', 'waiting_approval'),
      win('w-g', 'waiting_approval'),
    ];
    observe(s, windows('waiting_input', true).slice(0, 3), ['w-a', 'w-b', 'w-c']);
    run(s);
    observe(s, windows('waiting_input', true).slice(0, 4), ['w-d']);
    const inputs = run(s);
    expect(callIds(inputs)).toEqual(['w-a', 'w-b', 'w-c']);
    expect(inputs.lane.moreCalls).toBe(1);
    expect(inputs.lane.moreCallsStatus).toBe('waiting_input');

    observe(s, windows('waiting_input', true).slice(0, 5), ['w-e']);
    const full = run(s);
    expect(callIds(full)).toEqual(['w-a', 'w-b', 'w-e']);
    expect(full.lane.moreCalls).toBe(2);
    expect(full.lane.moreNotices).toBe(0);
    expect(full.lane.moreCallsStatus).toBe('waiting_input');

    observe(s, windows('running', false).slice(0, 5), []);
    const freed = run(s);
    expect(callIds(freed)).toEqual(['w-a', 'w-c', 'w-e']);
    expect(freed.lane.moreCalls).toBe(1);

    observe(s, windows('running', false).slice(0, 6), ['w-f']);
    expect(callIds(run(s))).toEqual(['w-a', 'w-e', 'w-f']);
    observe(s, windows('running', false), ['w-g']);
    const approvals = run(s);
    expect(callIds(approvals)).toEqual(['w-a', 'w-e', 'w-f']);
    expect(approvals.lane.moreCalls).toBe(3);
    expect(approvals.lane.moreCallsStatus).toBe('waiting_approval');
  });
});

describe('notice feed: one card per kind of event', () => {
  it('folds later events of a kind into the card already on the lane: the latest sentence, "and N more", every event listed', () => {
    const s = shell();
    let out = run(s);
    const byWindow = new Map<string, FeedOutput>();
    for (const windowId of ['w-a', 'w-b', 'w-c', 'w-d']) {
      record(s, exitOf(windowId, 1, true, exitPartsOf(windowId, 1)));
      out = run(s);
      byWindow.set(windowId, out);
    }
    const alone = byWindow.get('w-a');
    if (alone === undefined) {
      throw new Error('no run after the first exit');
    }
    expect(cardOfKind(alone, 'notice').folded).toBe(0);
    expect(titles(out)).toEqual([more('exit-w-d', 3)]);
    expect(out.center.filter((notice) => notice.key === 'window-exit')).toHaveLength(4);

    // The card draws the latest event alone and takes its number from it.
    const folded = noticeCard(cardOfKind(out, 'notice'));
    expect(folded.folded).toBe(3);
    expect(folded.sentence).toBe('exit-w-d');
    expect(folded.parts).toEqual(exitPartsOf('w-d', 1));
    expect(folded.titleSeq).toBe(seqOf(out, 'exit-w-d'));
    expect(folded.detail).toBeNull();
  });

  it('keeps different kinds of event in their own cards', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    run(s);
    record(s, failureEvent('operation-failure', 'failure-1'));
    expect(titles(run(s))).toEqual(['exit-w-a', 'failure-1']);
  });

  it('gives the folded card the heaviest tone among its events', () => {
    const s = shell();
    let out = run(s);
    for (const [windowId, code] of [
      ['w-a', 0],
      ['w-b', 1],
      ['w-c', 0],
    ] as const) {
      record(s, exitOf(windowId, code));
      out = run(s);
    }
    const toast = cardTitled(out, more('exit-w-c', 2));
    expect(toast.tone).toBe('warning');
  });

  it('starts a new card for an event that comes after its kind’s card went', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    const first = run(s);
    dismiss(s, first, 'exit-w-a');
    run(s);
    record(s, exitOf('w-b', 1));
    expect(titles(run(s))).toEqual(['exit-w-b']);
  });

  it('marks an info card to start its time over when an event is folded into it, and only then', () => {
    const s = shell();
    record(s, exitOf('w-a', 0));
    const first = cardTitled(run(s), 'exit-w-a').restart;

    record(s, failureEvent('operation-failure', 'failure-1'));
    expect(cardTitled(run(s), 'exit-w-a').restart).toBe(first);
    expect(cardTitled(run(s), 'exit-w-a').restart).toBe(first);

    record(s, exitOf('w-b', 0));
    const folded = cardTitled(run(s), more('exit-w-b', 1));
    expect(folded.tone).toBe('info');
    expect(folded.restart).not.toBe(first);
  });

  it('reads out only the folded-in event’s sentence, once, and never the count', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    expect(run(s).announce.polite).toContain('exit-w-a');

    record(s, exitOf('w-b', 1));
    const polite = run(s).announce.polite;
    expect(polite).toContain('exit-w-b');
    expect(polite).not.toContain('exit-w-a');
    expect(polite).not.toContain(more('exit-w-b', 1));

    expect(run(s).announce).toEqual({ polite: null, assertive: null });
  });

  it('folds a later event into its kind’s card while a window call keeps that card off the lane, and brings the card back once there is room', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 's-2'), setup('keymap-notice', 's-3')];
    run(s);
    record(s, exitOf('w-a', 1));
    expect(titles(run(s))).toEqual(['s-2', 's-3', 'exit-w-a']);

    observe(s, [win('w-c', 'waiting_approval')], ['w-c']);
    const pushed = run(s);
    const call = cardOfKind(pushed, 'needs-you', 'w-c').title;
    expect(titles(pushed)).toEqual(['s-2', 's-3', call]);
    expect(pushed.lane.moreNotices).toBe(1);

    record(s, exitOf('w-b', 1));
    const folded = run(s);
    expect(titles(folded)).toEqual(['s-2', 's-3', call]);
    expect(folded.lane.moreNotices).toBe(1);

    dismiss(s, folded, 's-2');
    expect(titles(run(s))).toEqual(['s-3', more('exit-w-b', 1), call]);
  });
});

describe('notice feed: the settings rows', () => {
  it('keeps a switched-off kind off the lane and unread, but lists it and counts it unseen', () => {
    const s = shell();
    s.prefs = withPreference(readPreferences(null), 'window-exits', false);
    record(s, exitOf('w-a', 1));
    const out = run(s);
    expect(titles(out)).toEqual([]);
    expect(out.announce.polite).toBeNull();
    expect(out.center.map((notice) => notice.title)).toContain('exit-w-a');
    expect(out.unseen).toBe(1);

    // A notice of the kind that was waiting for a slot when it was switched off stays down too.
    s.seenThrough = seqOf(out, 'exit-w-a');
    s.prefs = readPreferences(null);
    s.drafts = ['s-1', 's-2', 's-3'].map((title) => setup('detection-warning', title));
    run(s);
    record(s, exitOf('w-b', 1));
    const full = run(s);
    expect(titles(full)).toEqual(['s-1', 's-2', 's-3']);
    expect(full.lane.moreNotices).toBe(1);
    s.prefs = withPreference(readPreferences(null), 'window-exits', false);
    dismiss(s, full, 's-1');
    const freed = run(s);
    expect(titles(freed)).toEqual(['s-2', 's-3']);
    expect(freed.announce.polite).toBeNull();
    expect(freed.center.map((notice) => notice.title)).toContain('exit-w-b');
    expect(freed.unseen).toBe(4);

    // Switching a kind off takes its cards that are up off the lane too; the list keeps them.
    s.prefs = withPreference(s.prefs, 'setup-problems', false);
    const off = run(s);
    expect(titles(off)).toEqual([]);
    expect(off.lane.moreNotices).toBe(0);
    expect(off.center.map((notice) => notice.title)).toContain('s-2');
  });

  it('shows and reads blocking notices and expired approval requests whatever the table says', () => {
    const s = shell();
    s.prefs = allOff();
    s.drafts = [
      blocking('daemon-health', 'error', 'block-1'),
      setup('approval-expired-unseen', 'expired-1'),
    ];
    const out = run(s);
    expect(out.strip.map((notice) => notice.title)).toEqual(['block-1']);
    expect(out.announce.assertive).toContain('block-1');
    expect(titles(out)).toEqual(['expired-1']);
    expect(out.announce.polite).toContain('expired-1');
  });

  it('keeps the window calls of a switched-off row off the lane and unread', () => {
    const s = shell();
    s.prefs = withPreference(readPreferences(null), 'needs-approval', false);
    observe(s, [win('w-a', 'waiting_approval'), win('w-b', 'waiting_input')], ['w-a', 'w-b']);
    const out = run(s);
    expect(callIds(out)).toEqual(['w-b']);
    expect(out.lane.moreCalls).toBe(0);
    expect(out.announce.polite).toContain(windowNeedsInput('bravo'));
    expect(out.announce.polite).not.toContain(windowNeedsApproval('alpha'));

    // Switched back on, the call joins the end of the lane and is read once.
    s.prefs = readPreferences(null);
    const on = run(s);
    expect(callIds(on)).toEqual(['w-b', 'w-a']);
    expect(on.announce.polite).toContain(windowNeedsApproval('alpha'));
    expect(on.announce.polite).not.toContain(windowNeedsInput('bravo'));

    // A call read before its row went off is not read again when the row comes back.
    s.prefs = withPreference(readPreferences(null), 'needs-input', false);
    expect(callIds(run(s))).toEqual(['w-a']);
    s.prefs = readPreferences(null);
    const again = run(s);
    expect(callIds(again)).toEqual(['w-a', 'w-b']);
    expect(again.announce.polite).toBeNull();

    // A window waiting for input with that row off comes up, and is read, once it needs approval.
    s.prefs = withPreference(readPreferences(null), 'needs-input', false);
    observe(
      s,
      [win('w-a', 'waiting_approval'), win('w-b', 'waiting_input'), win('w-c', 'waiting_input')],
      ['w-c']
    );
    const hidden = run(s);
    expect(callIds(hidden)).toEqual(['w-a']);
    expect(hidden.announce.polite).toBeNull();
    observe(
      s,
      [win('w-a', 'waiting_approval'), win('w-b', 'waiting_input'), win('w-c', 'waiting_approval')],
      ['w-c']
    );
    const approval = run(s);
    expect(callIds(approval)).toEqual(['w-a', 'w-c']);
    expect(approval.announce.polite).toContain(windowNeedsApproval('charlie'));
  });
});

describe('notice feed: numbers and the unseen count', () => {
  it('keeps a state notice’s number while it says the same, and numbers it anew when it changes or comes back', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'setup-a')];
    const first = seqOf(run(s), 'setup-a');
    expect(seqOf(run(s), 'setup-a')).toBe(first);

    s.drafts = [];
    run(s);
    s.drafts = [setup('detection-warning', 'setup-a')];
    const back = seqOf(run(s), 'setup-a');
    expect(back).toBeGreaterThan(first);

    s.drafts = [setup('detection-warning', 'setup-b')];
    expect(seqOf(run(s), 'setup-b')).toBeGreaterThan(back);
  });

  it('keeps two notices of one kind apart when only their detail differs, and reads the new one out', () => {
    const s = shell();
    const health = (detail: string): NoticeDraft => ({
      ...blocking('daemon-health', 'error', 'health'),
      detail,
    });
    s.drafts = [health('fact-1')];
    const first = run(s);
    s.drafts = [health('fact-1'), health('fact-2')];
    const second = run(s);
    const seqs = second.strip.map((notice) => notice.seq);
    expect(seqs).toHaveLength(2);
    expect(new Set(seqs).size).toBe(2);
    expect(seqs).toContain(first.strip[0]?.seq);
    // The strip row shows the title, so the second fact is read by its title once more.
    expect(second.announce.assertive).toContain('health');
  });

  it('numbers events and state notices from one counter, in the order they came', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'setup-a')];
    const stateFirst = seqOf(run(s), 'setup-a');
    record(s, failureEvent('operation-failure', 'failure-1'));
    s.drafts = [setup('detection-warning', 'setup-a'), setup('keymap-notice', 'setup-b')];
    const out = run(s);
    const event = seqOf(out, 'failure-1');
    expect(event).toBeGreaterThan(stateFirst);
    expect(seqOf(out, 'setup-b')).toBeGreaterThan(event);
    expect(out.center.find((notice) => notice.title === 'failure-1')?.at).toBe('at-failure-1');
    expect(out.center.find((notice) => notice.title === 'setup-a')?.at).toBeNull();
  });

  it('counts the notices newer than the last one seen, leaving blocking notices out', () => {
    const s = shell();
    s.drafts = [
      blocking('daemon-health', 'error', 'block-1'),
      setup('detection-warning', 'setup-a'),
    ];
    run(s);
    record(s, failureEvent('operation-failure', 'failure-1'));
    const out = run(s);
    expect(out.unseen).toBe(2);

    s.seenThrough = seqOf(out, 'setup-a');
    expect(run(s).unseen).toBe(1);
  });
});

describe('notice feed: what is read out', () => {
  it('reads a blocking error or warning assertively', () => {
    const s = shell();
    s.drafts = [blocking('approver-registration-refused', 'warning', 'block-1')];
    const out = run(s);
    expect(out.announce.assertive).toContain('block-1');
    expect(out.announce.polite).toBeNull();
  });

  it('reads a blocking info notice politely', () => {
    const s = shell();
    s.drafts = [blocking('daemon-recovery', 'info', 'retrying-1')];
    const out = run(s);
    expect(out.announce.polite).toContain('retrying-1');
    expect(out.announce.assertive).toBeNull();
  });

  it('reads any other notice politely, an error one too', () => {
    const s = shell();
    record(s, failureEvent('operation-failure', 'failure-1'));
    const out = run(s);
    expect(out.announce.polite).toContain('failure-1');
    expect(out.announce.assertive).toBeNull();
  });

  it('says actions are available for every card on the lane, and for the strip or a notice shown elsewhere only when it offers one', () => {
    const withAction = shell();
    withAction.drafts = [setup('reopen-offer', 'reopen-1', { labelKey: 'chooseWindows' })];
    expect(run(withAction).announce.polite).toContain(card('reopen-1'));

    const without = shell();
    without.drafts = [setup('detection-warning', 'setup-a')];
    expect(run(without).announce.polite).toContain(card('setup-a'));

    const strip = shell();
    strip.drafts = [blocking('approver-registration-refused', 'warning', 'block-1')];
    const stripped = run(strip).announce.assertive;
    expect(stripped).toContain('block-1');
    expect(stripped).not.toContain(dummyMessages.banners.actionsAvailable);

    const elsewhere = shell();
    record(elsewhere, exitOf('w-a', 1, false));
    const shownElsewhere = run(elsewhere).announce.polite;
    expect(shownElsewhere).toContain('exit-w-a');
    expect(shownElsewhere).not.toContain(dummyMessages.banners.actionsAvailable);
  });

  it('reads everything one update brings in one sentence, and each thing once', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'setup-a'), setup('keymap-notice', 'setup-b')];
    const polite = run(s).announce.polite;
    expect(polite).toContain('setup-a');
    expect(polite).toContain('setup-b');
    expect(run(s).announce).toEqual({ polite: null, assertive: null });
  });

  it('reads a window call with its actions, and not again when the same call comes back', () => {
    const s = shell();
    observe(s, [win('w-a', 'waiting_approval')], ['w-a']);
    const first = run(s);
    expect(cardOfKind(first, 'needs-you', 'w-a').title).toBe(windowNeedsApproval('alpha'));
    // The card cuts only the window's display name short, and folds nothing in.
    const call = cardOfKind(first, 'needs-you', 'w-a');
    expect(call.parts?.name).toBe('alpha');
    expect(call.sentence).toBe(call.title);
    expect(call.folded).toBe(0);
    expect(first.announce.polite).toContain(windowNeedsApproval('alpha'));
    expect(first.announce.polite).toContain(dummyMessages.banners.actionsAvailable);
    const before = cardOfKind(first, 'needs-you', 'w-a').restart;

    observe(s, [win('w-a', 'waiting_approval')], ['w-a']);
    const again = run(s);
    expect(again.announce.polite).toBeNull();
    expect(cardOfKind(again, 'needs-you', 'w-a').restart).not.toBe(before);

    // More calls in one update than fit: the cards up are read, the rest only as how many.
    const narrow = shell();
    narrow.capacity = { all: 1, withMore: 1 };
    observe(
      narrow,
      [win('w-a', 'waiting_approval'), win('w-b', 'waiting_approval')],
      ['w-a', 'w-b']
    );
    const crowded = run(narrow);
    expect(callIds(crowded)).toEqual(['w-a']);
    expect(crowded.lane.moreCalls).toBe(1);
    expect(crowded.announce.polite).toContain(card(windowNeedsApproval('alpha')));
    expect(crowded.announce.polite).toContain(waitingForYouMoreAnnouncement(1));
    expect(crowded.announce.polite).not.toContain(windowNeedsApproval('bravo'));

    // A call off the lane whose sentence changes is read in that update, as how many.
    observe(narrow, [win('w-a', 'waiting_approval'), win('w-b', 'waiting_input')], ['w-b']);
    const changed = run(narrow);
    expect(callIds(changed)).toEqual(['w-a']);
    expect(changed.announce.polite).toContain(waitingForYouMoreAnnouncement(1));
    expect(changed.announce.polite).not.toContain(windowNeedsInput('bravo'));

    observe(narrow, [win('w-a', 'running', false), win('w-b', 'waiting_input')], []);
    const later = run(narrow);
    expect(callIds(later)).toEqual(['w-b']);
    expect(later.announce.polite).toBeNull();
  });
});
