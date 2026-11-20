import { describe, it, expect, vi } from 'vitest';

import type { WindowStatus } from '../ipc/session-boundary';
import { dummyMessages } from '../test-utils/dummy-messages';
import { appendEvent, failureEvent } from './event-log';
import type { EventLog, NewEvent } from './event-log';
import {
  emptyNeedsYouToastState,
  stepNeedsYouToasts,
  type NeedsYouToastState,
  type NeedsYouToastWindow,
} from './needs-you-toasts';
import { emptyNoticeMemory, emptyToastSlots, dismissToast, noticeFeed } from './notice-feed';
import type { NoticeMemory, ToastSlots } from './notice-feed';
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
type ToastItem = FeedOutput['toasts']['visible'][number];

/** Everything the shell keeps between two runs of the feed, and the inputs it passes. */
interface Shell {
  drafts: NoticeDraft[];
  log: EventLog;
  memory: NoticeMemory;
  slots: ToastSlots;
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
    slots: emptyToastSlots(),
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
    slots: s.slots,
    windowNames: s.windowNames,
  });
  s.memory = out.memory;
  s.slots = out.toasts.slots;
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
  action: NoticeAction | null = null
): NoticeDraft {
  return { key, blocking: false, tone: 'warning', dismissible: true, title, detail: null, action };
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

function exitOf(windowId: string, exitCode: number, toast = true): NewEvent {
  return {
    key: 'window-exit',
    tone: exitCode === 0 ? 'info' : 'warning',
    text: `exit-${windowId}`,
    toast,
    windowId,
  };
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
  return out.toasts.visible.map((item) => item.title);
}

function callIds(out: FeedOutput): (string | null)[] {
  return out.toasts.visible
    .filter((item) => item.kind === 'needs-you')
    .map((item) => item.windowId);
}

function toastTitled(out: FeedOutput, title: string): ToastItem {
  const item = out.toasts.visible.find((entry) => entry.title === title);
  if (item === undefined) {
    throw new Error(`no toast titled ${title}`);
  }
  return item;
}

function toastOfKind(
  out: FeedOutput,
  kind: ToastItem['kind'],
  windowId: string | null = null
): ToastItem {
  const item = out.toasts.visible.find(
    (entry) => entry.kind === kind && (windowId === null || entry.windowId === windowId)
  );
  if (item === undefined) {
    throw new Error(`no ${kind} toast`);
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
  s.slots = dismissToast(s.slots, toastTitled(out, title).id);
}

function allOff(): NotificationPreferences {
  return NOTICE_CATEGORIES.reduce(
    (prefs, row: NoticeCategory) => withPreference(prefs, row, false),
    readPreferences(null)
  );
}

const more = (sentence: string, count: number): string =>
  dummyMessages.banners.noticeAndMore(sentence, count);

describe('notice feed: when a notice pops up', () => {
  it('pops a state notice up once while it says the same, and again once it says something new', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'setup-a')];
    const first = run(s);
    expect(titles(first)).toEqual(['setup-a']);

    dismiss(s, first, 'setup-a');
    expect(titles(run(s))).toEqual([]);

    s.drafts = [setup('detection-warning', 'setup-b')];
    expect(titles(run(s))).toEqual(['setup-b']);
  });

  it('pops up and numbers two kinds that say the same sentence, one each', () => {
    const s = shell();
    s.drafts = [setup('detection-warning', 'same'), setup('keymap-notice', 'same')];
    const out = run(s);
    expect(out.toasts.visible).toHaveLength(2);
    expect(new Set(out.center.map((notice) => notice.seq)).size).toBe(2);
  });

  it('keeps a blocking notice off the toasts and puts it in the strip, in the order built', () => {
    const s = shell();
    s.drafts = [
      blocking('startup-env-failure', 'error', 'block-1'),
      blocking('daemon-protocol-stale', 'warning', 'block-2'),
      setup('detection-warning', 'setup-a'),
    ];
    const out = run(s);
    expect(out.strip.map((notice) => notice.title)).toEqual(['block-1', 'block-2']);
    expect(titles(out)).toEqual(['setup-a']);
  });

  it('does not pop up an event something else on screen already shows, but reads it and lists it', () => {
    const s = shell();
    record(s, exitOf('w-a', 1, false));
    const out = run(s);
    expect(titles(out)).toEqual([]);
    expect(out.center.map((notice) => notice.title)).toContain('exit-w-a');
    expect(out.announce.polite).toContain('exit-w-a');
  });

  it('holds toasts and announcements while a modal is open and lets them out once it closes', () => {
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

describe('notice feed: three slots', () => {
  it('shows three toasts at most and counts the rest as waiting in Notifications', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2', 's-3', 's-4'].map((title) => setup('detection-warning', title));
    const out = run(s);
    expect(titles(out)).toEqual(['s-1', 's-2', 's-3']);
    expect(out.toasts.overflowNotices).toBe(1);
    expect(out.toasts.overflowWaiting).toBe(0);
  });

  it('lets the notice that waited longest in once a toast goes', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2', 's-3', 's-4', 's-5'].map((title) =>
      setup('detection-warning', title)
    );
    const first = run(s);
    dismiss(s, first, 's-1');
    expect(titles(run(s))).toEqual(['s-2', 's-3', 's-4']);
  });

  it('pushes the oldest other toast out for a window call, puts the call first, counts it until it is seen, and never brings it back', () => {
    const s = shell();
    s.drafts = ['s-1', 's-2', 's-3'].map((title) => setup('detection-warning', title));
    const shown = run(s);

    observe(s, [win('w-a', 'waiting_approval')], ['w-a']);
    const pushed = run(s);
    expect(pushed.toasts.visible.map((item) => item.kind)).toEqual([
      'needs-you',
      'notice',
      'notice',
    ]);
    expect(callIds(pushed)).toEqual(['w-a']);
    expect(titles(pushed).slice(1)).toEqual(['s-2', 's-3']);
    expect(pushed.toasts.overflowNotices).toBe(1);

    observe(s, [win('w-a', 'waiting_approval'), win('w-b', 'waiting_approval')], ['w-b']);
    const twice = run(s);
    expect(titles(twice).slice(2)).toEqual(['s-3']);
    expect(twice.toasts.overflowNotices).toBe(2);

    // Seeing the list up to the first pushed-out notice leaves only the one not yet seen.
    s.seenThrough = seqOf(shown, 's-1');
    const seenFirst = run(s);
    expect(seenFirst.toasts.overflowNotices).toBe(1);
    expect(titles(seenFirst)).not.toContain('s-1');

    s.seenThrough = seqOf(shown, 's-2');
    expect(run(s).toasts.overflowNotices).toBe(0);

    observe(s, [win('w-a', 'running', false), win('w-b', 'running', false)], []);
    expect(titles(run(s))).toEqual(['s-3']);
  });

  it('does not push one window call out for another: a later one waits, approvals first once a slot frees', () => {
    const s = shell();
    const windows = (statusOfB: WindowStatus, needsB: boolean) => [
      win('w-a', 'waiting_approval'),
      win('w-b', statusOfB, needsB),
      win('w-c', 'waiting_input'),
      win('w-d', 'waiting_input'),
      win('w-e', 'waiting_approval'),
    ];
    observe(s, windows('waiting_input', true).slice(0, 3), ['w-a', 'w-b', 'w-c']);
    run(s);
    observe(s, windows('waiting_input', true).slice(0, 4), ['w-d']);
    run(s);
    observe(s, windows('waiting_input', true), ['w-e']);
    const full = run(s);
    expect(callIds(full)).toEqual(['w-a', 'w-b', 'w-c']);
    expect(full.toasts.overflowWaiting).toBe(2);
    expect(full.toasts.overflowNotices).toBe(0);

    observe(s, windows('running', false), []);
    const freed = run(s);
    expect(callIds(freed)).toEqual(['w-a', 'w-e', 'w-c']);
    expect(freed.toasts.overflowWaiting).toBe(1);
  });
});

describe('notice feed: one toast per kind of event', () => {
  it('folds later events of a kind into the toast already up: the latest sentence, "and N more", every event listed', () => {
    const s = shell();
    let out = run(s);
    for (const windowId of ['w-a', 'w-b', 'w-c', 'w-d']) {
      record(s, exitOf(windowId, 1));
      out = run(s);
    }
    expect(titles(out)).toEqual([more('exit-w-d', 3)]);
    expect(out.center.filter((notice) => notice.key === 'window-exit')).toHaveLength(4);
  });

  it('keeps different kinds of event in their own toasts', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    run(s);
    record(s, failureEvent('operation-failure', 'failure-1'));
    expect(titles(run(s))).toEqual(['exit-w-a', 'failure-1']);
  });

  it('gives the folded toast the heaviest tone among its events', () => {
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
    const toast = toastTitled(out, more('exit-w-c', 2));
    expect(toast.tone).toBe('warning');
  });

  it('starts a new toast for an event that comes after its kind’s toast went', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    const first = run(s);
    dismiss(s, first, 'exit-w-a');
    run(s);
    record(s, exitOf('w-b', 1));
    expect(titles(run(s))).toEqual(['exit-w-b']);
  });

  it('marks an info toast to start its time over when an event is folded into it, and only then', () => {
    const s = shell();
    record(s, exitOf('w-a', 0));
    const first = toastTitled(run(s), 'exit-w-a').restart;

    record(s, failureEvent('operation-failure', 'failure-1'));
    expect(toastTitled(run(s), 'exit-w-a').restart).toBe(first);
    expect(toastTitled(run(s), 'exit-w-a').restart).toBe(first);

    record(s, exitOf('w-b', 0));
    const folded = toastTitled(run(s), more('exit-w-b', 1));
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

  it('gives an event of a kind pushed out by a window call a toast of its own, and never the pushed-out one', () => {
    const s = shell();
    record(s, exitOf('w-a', 1));
    run(s);
    s.drafts = [setup('detection-warning', 's-2'), setup('keymap-notice', 's-3')];
    expect(titles(run(s))).toEqual(['exit-w-a', 's-2', 's-3']);

    observe(s, [win('w-c', 'waiting_approval')], ['w-c']);
    const pushed = run(s);
    const call = toastOfKind(pushed, 'needs-you', 'w-c').title;
    expect(titles(pushed)).toEqual([call, 's-2', 's-3']);

    record(s, exitOf('w-b', 1));
    const waiting = run(s);
    expect(titles(waiting)).toEqual([call, 's-2', 's-3']);

    dismiss(s, waiting, 's-2');
    expect(titles(run(s))).toEqual([call, 's-3', 'exit-w-b']);
  });
});

describe('notice feed: the settings rows', () => {
  it('keeps a switched-off kind from popping up or being read, but lists it and counts it unseen', () => {
    const s = shell();
    s.prefs = withPreference(readPreferences(null), 'window-exits', false);
    record(s, exitOf('w-a', 1));
    const out = run(s);
    expect(titles(out)).toEqual([]);
    expect(out.announce.polite).toBeNull();
    expect(out.center.map((notice) => notice.title)).toContain('exit-w-a');
    expect(out.unseen).toBe(1);
  });

  it('pops up and reads blocking notices and expired approval requests whatever the table says', () => {
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

  it('keeps the window calls of a switched-off row from popping up or being read', () => {
    const s = shell();
    s.prefs = withPreference(readPreferences(null), 'needs-approval', false);
    observe(s, [win('w-a', 'waiting_approval'), win('w-b', 'waiting_input')], ['w-a', 'w-b']);
    const out = run(s);
    expect(callIds(out)).toEqual(['w-b']);
    expect(out.announce.polite).toContain(dummyMessages.banners.windowNeedsInput('bravo'));
    expect(out.announce.polite).not.toContain(dummyMessages.banners.windowNeedsApproval('alpha'));
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

  it('says actions are available only for a notice that offers one', () => {
    const withAction = shell();
    withAction.drafts = [setup('reopen-offer', 'reopen-1', { labelKey: 'chooseWindows' })];
    const offered = run(withAction).announce.polite;
    expect(offered).toContain('reopen-1');
    expect(offered).toContain(dummyMessages.banners.actionsAvailable);

    const without = shell();
    without.drafts = [setup('detection-warning', 'setup-a')];
    expect(run(without).announce.polite).not.toContain(dummyMessages.banners.actionsAvailable);
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
    expect(toastOfKind(first, 'needs-you', 'w-a').title).toBe(
      dummyMessages.banners.windowNeedsApproval('alpha')
    );
    expect(first.announce.polite).toContain(dummyMessages.banners.windowNeedsApproval('alpha'));
    expect(first.announce.polite).toContain(dummyMessages.banners.actionsAvailable);
    const before = toastOfKind(first, 'needs-you', 'w-a').restart;

    observe(s, [win('w-a', 'waiting_approval')], ['w-a']);
    const again = run(s);
    expect(again.announce.polite).toBeNull();
    expect(toastOfKind(again, 'needs-you', 'w-a').restart).not.toBe(before);
  });
});
