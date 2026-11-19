/**
 * `status-tone.ts` is the single place that counts the windows needing attention and
 * gathers a group of windows into one Rail mark; every display location reads these
 * functions rather than re-deriving the classification. `windowsNeedingYou` /
 * `attentionCount` take a minimal structural shape rather than `WindowEntry`, so the
 * module stays independent of the store.
 */
import { describe, it, expect } from 'vitest';

import type {
  DetectedStatusView,
  StatusBoot,
  TaskReportMark,
  WindowStatus,
} from './ipc/session-boundary';
import { STATUS_PRESENTATION } from './status-presentation';
import {
  attentionCount,
  beginAttentionSend,
  emptyArrivalMemory,
  emptyAttentionSendState,
  isProminentWindow,
  needsYou,
  noteAttentionArrival,
  observeNeedsYouArrivals,
  settleAttentionSend,
  summarizeWindowStatuses,
  windowsNeedingYou,
} from './status-tone';
import type { AttentionSend, AttentionSendState, AttentionTotals } from './status-tone';

/**
 * Minimal shape `windowsNeedingYou` / `attentionCount` need from an entry: the
 * status and its reason as one value, so a caller cannot hand these helpers a
 * status taken apart from the reason that belongs to it.
 */
interface Entry {
  detected: DetectedStatusView;
  id: string;
  taskReport?: { seq: number; state: ReportStateValue };
}

/** The states a report mark reads as at the boundary. */
type ReportStateValue = 'standing' | 'instruction' | 'no_reader' | 'unknown';

/** Every state but `standing`: none says the window still waits on its owner. */
const NOT_STANDING_STATES: readonly ReportStateValue[] = ['instruction', 'no_reader', 'unknown'];

/** An entry holding `status`, with the reason arm the status allows. */
function entryOf(status: WindowStatus, id: string): Entry {
  return { detected: status === 'unknown' ? { status, reason: null } : { status }, id };
}

describe('attentionCount: matches the windowsNeedingYou set', () => {
  it.each([
    [[] as WindowStatus[], 0],
    [['running', 'idle', 'exited'] as WindowStatus[], 0],
    [['waiting_input'] as WindowStatus[], 1],
    [['waiting_input', 'waiting_approval', 'waiting_approval'] as WindowStatus[], 3],
  ])('matches windowsNeedingYou length for statuses %j', (statuses, expectedCount) => {
    const entries: Entry[] = statuses.map((status, i) => entryOf(status, `e${i}`));
    expect(attentionCount(entries)).toBe(expectedCount);
    expect(attentionCount(entries)).toBe(windowsNeedingYou(entries).length);
  });
});

/**
 * The seven `WindowStatus` values written as a literal table here rather than
 * derived from `STATUS_PRESENTATION` or any other production helper, so the
 * expectation cannot drift together with a change to the classification it is
 * meant to pin down.
 */
const EXPECTED_NEEDS_YOU: Record<WindowStatus, boolean> = {
  waiting_input: true,
  waiting_approval: true,
  running: false,
  interrupted: false,
  idle: false,
  exited: false,
  unknown: false,
};

describe('windowsNeedingYou / attentionCount: membership in the waiting set', () => {
  it('keeps entries in input order and drops every entry not in the table above', () => {
    const entries: Entry[] = [
      entryOf('idle', 'a'),
      entryOf('waiting_input', 'b'),
      entryOf('running', 'c'),
      entryOf('waiting_approval', 'd'),
      entryOf('unknown', 'e'),
      entryOf('interrupted', 'f'),
      entryOf('exited', 'g'),
    ];

    expect(windowsNeedingYou(entries).map((e) => e.id)).toEqual(['b', 'd']);
  });
});

/** `entryOf` with the task report mark the daemon attached. */
function reportedEntryOf(status: WindowStatus, id: string, state: ReportStateValue): Entry {
  return { ...entryOf(status, id), taskReport: { seq: 1, state } };
}

describe('needsYou: a task window waiting on its owner after its report is not the turn of the user', () => {
  it('leaves out a waiting_input window whose report still stands', () => {
    expect(needsYou(reportedEntryOf('waiting_input', 'w', 'standing'))).toBe(false);
  });

  it('keeps in a waiting_approval window whose report stands, and a waiting_input one that never reported', () => {
    expect(needsYou(reportedEntryOf('waiting_approval', 'w', 'standing'))).toBe(true);
    expect(needsYou(entryOf('waiting_input', 'w'))).toBe(true);
  });

  it('agrees with the status table for every status that carries no standing report', () => {
    for (const [status, expected] of Object.entries(EXPECTED_NEEDS_YOU) as [
      WindowStatus,
      boolean,
    ][]) {
      expect(needsYou(entryOf(status, 'w')), status).toBe(expected);
      for (const state of NOT_STANDING_STATES) {
        expect(needsYou(reportedEntryOf(status, 'w', state)), `${status} ${state}`).toBe(expected);
      }
    }
    expect(needsYou(reportedEntryOf('unknown', 'w', 'standing'))).toBe(false);
  });

  it('narrows windowsNeedingYou, attentionCount and the group summary the same way', () => {
    const entries: Entry[] = [
      reportedEntryOf('waiting_input', 'standing', 'standing'),
      reportedEntryOf('waiting_input', 'instruction', 'instruction'),
      reportedEntryOf('waiting_input', 'no-reader', 'no_reader'),
      reportedEntryOf('waiting_input', 'unread', 'unknown'),
      entryOf('waiting_input', 'plain'),
      reportedEntryOf('waiting_approval', 'approval', 'standing'),
      entryOf('unknown', 'unknown'),
    ];

    expect(windowsNeedingYou(entries).map((e) => e.id)).toEqual([
      'instruction',
      'no-reader',
      'unread',
      'plain',
      'approval',
    ]);
    expect(attentionCount(entries)).toBe(5);
    expect(summarizeWindowStatuses(entries).count).toBe(5);

    const onlyStanding = [
      reportedEntryOf('waiting_input', 'standing', 'standing'),
      entryOf('idle', 'i'),
    ];
    expect(summarizeWindowStatuses(onlyStanding)).toEqual({
      count: 0,
      mark: 'waiting_input',
      unknownOnly: false,
    });
  });
});

describe('isProminentWindow: a window that needs the user or whose status is unknown stands out', () => {
  it('is true for the windows needing the user and for unknown, false for the reported input wait and the quiet statuses', () => {
    for (const [status, needs] of Object.entries(EXPECTED_NEEDS_YOU) as [WindowStatus, boolean][]) {
      expect(isProminentWindow(entryOf(status, 'w')), status).toBe(needs || status === 'unknown');
    }
    expect(isProminentWindow(reportedEntryOf('waiting_input', 'w', 'standing'))).toBe(false);
    expect(isProminentWindow(reportedEntryOf('waiting_approval', 'w', 'standing'))).toBe(true);
  });
});

/** The seven statuses from the highest attention rank to the lowest. */
const STATUSES_BY_RANK: WindowStatus[] = [
  'waiting_approval',
  'waiting_input',
  'unknown',
  'interrupted',
  'running',
  'idle',
  'exited',
];

const table = STATUS_PRESENTATION;

function moves(motion: unknown): boolean {
  return Boolean(motion) && motion !== 'none';
}

describe('status presentation table', () => {
  it('ranks waiting_approval < waiting_input < unknown < interrupted < running < idle < exited', () => {
    const sorted = [...STATUSES_BY_RANK].sort((x, y) => table[x].rank - table[y].rank);
    expect(sorted).toEqual(STATUSES_BY_RANK);
    expect(new Set(STATUSES_BY_RANK.map((s) => table[s].rank)).size).toBe(7);
  });

  it('tells approval from input by icon and by colour role, not by colour alone', () => {
    expect(table.waiting_approval.icon).not.toBe(table.waiting_input.icon);
    expect(table.waiting_approval.role).not.toBe(table.waiting_input.role);
  });

  it('gives unknown an icon of its own, apart from idle', () => {
    expect(table.unknown.icon).not.toBe(table.idle.icon);
  });

  it('does not colour interrupted as an error or danger', () => {
    expect(String(table.interrupted.role)).not.toMatch(/error|danger/i);
  });

  it('moves only a running window', () => {
    for (const status of STATUSES_BY_RANK) {
      expect(moves(table[status].motion), status).toBe(status === 'running');
    }
  });
});

interface RailSummary {
  count: number;
  mark: WindowStatus | null;
  unknownOnly: boolean;
}

function summarize(statuses: WindowStatus[]): { summary: RailSummary; entries: Entry[] } {
  const entries = statuses.map((status, i) => entryOf(status, `e${i}`));
  return { summary: summarizeWindowStatuses(entries), entries };
}

describe('Rail summary of a group of windows', () => {
  it('marks the group with the highest-ranked status present', () => {
    const cases: [WindowStatus[], WindowStatus][] = [
      [['idle', 'running'], 'running'],
      [['exited', 'idle', 'interrupted'], 'interrupted'],
      [['running', 'unknown', 'idle'], 'unknown'],
      [['running', 'waiting_input', 'unknown'], 'waiting_input'],
      [['waiting_input', 'waiting_approval', 'running'], 'waiting_approval'],
    ];
    for (const [statuses, expected] of cases) {
      const { summary } = summarize(statuses);
      expect(summary.mark, statuses.join(',')).toBe(expected);
    }
  });

  it('reports count 0 and unknownOnly when no window needs you but one is unknown', () => {
    const { summary } = summarize(['unknown', 'idle', 'running']);
    expect(summary.count).toBe(0);
    expect(summary.unknownOnly).toBe(true);
  });

  it('does not report unknownOnly while a needs-you window exists, nor without an unknown window', () => {
    expect(summarize(['unknown', 'waiting_input']).summary.unknownOnly).toBe(false);
    expect(summarize(['running', 'idle']).summary.unknownOnly).toBe(false);
  });

  it('has count 0, no mark and no unknownOnly for an empty group', () => {
    const { summary } = summarize([]);
    expect(summary).toEqual({ count: 0, mark: null, unknownOnly: false });
  });
});

// ---------------------------------------------------------------------------
// observeNeedsYouArrivals: which windows newly started waiting on the user in
// one observation. Each sequence below is one window observed poll after poll;
// the expected value lists, per observation, the windows that arrived.
// ---------------------------------------------------------------------------

/** What one observation of a window carries into `observeNeedsYouArrivals`. */
interface ArrivalEntry {
  windowId: string;
  detected: DetectedStatusView;
  statusBoot: StatusBoot | null;
  taskReport?: TaskReportMark;
}

type ArrivalMemory = Parameters<typeof observeNeedsYouArrivals>[0];

/** An empty memory, as the shell starts with (and starts again with after a WebView reload). */
function emptyMemory(): ArrivalMemory {
  return emptyArrivalMemory();
}

const ORIGIN = 7;
type Boot = 'boot1' | 'boot2' | null;

/** A fresh boot object each time, as each window list and status push decodes its own. */
function bootOf(boot: Boot): StatusBoot | null {
  if (boot === null) {
    return null;
  }
  return { origin: ORIGIN, seq: boot === 'boot1' ? 1 : 2 };
}

/** One observation of a window: its status, the boot it was counted in, and its report mark. */
interface Seen {
  status: WindowStatus;
  boot?: Boot;
  report?: TaskReportMark['state'];
  seq?: number;
}

function arrivalEntry(windowId: string, seen: Seen): ArrivalEntry {
  const { status } = seen;
  const detected: DetectedStatusView = status === 'unknown' ? { status, reason: null } : { status };
  const entry: ArrivalEntry = {
    windowId,
    detected,
    statusBoot: bootOf(seen.boot === undefined ? 'boot1' : seen.boot),
  };
  return seen.report === undefined
    ? entry
    : { ...entry, taskReport: { seq: seen.seq ?? 1, state: seen.report } };
}

/** Runs the observations in order from an empty memory; returns the sorted arrivals of each. */
function observeAll(observations: readonly (readonly ArrivalEntry[])[]): string[][] {
  let memory = emptyMemory();
  return observations.map((entries) => {
    const result = observeNeedsYouArrivals(memory, entries);
    memory = result.memory;
    return [...result.arrived].sort();
  });
}

/** One window `w` observed once per step; `null` is an observation the window is absent from. */
function watch(steps: readonly (Seen | WindowStatus | null)[]): string[][] {
  return observeAll(
    steps.map((step) => {
      if (step === null) {
        return [];
      }
      return [arrivalEntry('w', typeof step === 'string' ? { status: step } : step)];
    })
  );
}

const NONE: string[] = [];
const W = ['w'];

describe('observeNeedsYouArrivals: a window seen for the first time (a fresh memory, as after a WebView reload)', () => {
  it.each(['waiting_input', 'running', 'idle'] as WindowStatus[])(
    'does not arrive when first seen %s',
    (status) => {
      expect(watch([status])).toEqual([NONE]);
    }
  );

  it('arrives when first seen waiting_approval, and again at the input wait after it is answered', () => {
    expect(watch(['waiting_approval'])).toEqual([W]);
    expect(watch(['waiting_approval', 'waiting_input'])).toEqual([W, W]);
  });
});

describe('observeNeedsYouArrivals: (B) a turn the GUI saw open ends in an input wait', () => {
  it('arrives at running -> waiting_input, also when unknown comes in between', () => {
    expect(watch(['running', 'waiting_input'])).toEqual([NONE, W]);
    expect(watch(['running', 'unknown', 'waiting_input'])).toEqual([NONE, NONE, W]);
  });

  it('does not arrive at unknown -> waiting_input, nor when a waiting window wobbles through unknown', () => {
    expect(watch(['unknown', 'waiting_input'])).toEqual([NONE, NONE]);
    expect(watch(['waiting_input', 'unknown', 'waiting_input'])).toEqual([NONE, NONE, NONE]);
  });

  it.each(['idle', 'interrupted', 'exited'] as WindowStatus[])(
    'does not arrive at running -> %s -> waiting_input: the turn closed before the input wait',
    (closing) => {
      expect(watch(['running', closing, 'waiting_input'])).toEqual([NONE, NONE, NONE]);
    }
  );

  it('arrives twice at running -> waiting_approval -> waiting_input: the approval does not close the turn', () => {
    expect(watch(['running', 'waiting_approval', 'waiting_input'])).toEqual([NONE, W, W]);
  });

  it('arrives once when the list shows the end of a turn after a reconnect, and not again on the next identical list', () => {
    expect(watch(['running', 'waiting_input', 'waiting_input'])).toEqual([NONE, W, NONE]);
  });
});

describe('observeNeedsYouArrivals: (A) a window starts waiting for approval', () => {
  it('arrives at waiting_input -> waiting_approval', () => {
    expect(watch(['waiting_input', 'waiting_approval'])).toEqual([NONE, W]);
  });

  it('does not arrive again at waiting_approval -> unknown -> waiting_approval, nor on a repeated list', () => {
    expect(watch(['waiting_approval', 'unknown', 'waiting_approval'])).toEqual([W, NONE, NONE]);
    expect(watch(['waiting_approval', 'waiting_approval'])).toEqual([W, NONE]);
  });

  it('arrives twice at waiting_input -> waiting_approval -> waiting_input, without running in between', () => {
    expect(watch(['waiting_input', 'waiting_approval', 'waiting_input'])).toEqual([NONE, W, W]);
  });
});

describe('observeNeedsYouArrivals: the memory carries over a daemon restart', () => {
  it.each([
    ['boot1', 'boot2'],
    [null, null],
  ] as [Boot, Boot][])(
    'arrives when a turn seen running in %s ends in %s after unknown',
    (before, after) => {
      expect(
        watch([
          { status: 'running', boot: before },
          { status: 'unknown', boot: after },
          { status: 'waiting_input', boot: after },
        ])
      ).toEqual([NONE, NONE, W]);
    }
  );

  it.each([
    ['boot1', 'boot2'],
    [null, null],
  ] as [Boot, Boot][])(
    'does not arrive for a window that was already waiting for input in %s and is taken back in %s',
    (before, after) => {
      expect(
        watch([
          { status: 'waiting_input', boot: before },
          { status: 'unknown', boot: after },
          { status: 'waiting_input', boot: after },
        ])
      ).toEqual([NONE, NONE, NONE]);
    }
  );

  it('does not arrive at the approval taken back, and arrives at the input wait after it is answered', () => {
    expect(
      watch([
        { status: 'waiting_approval', boot: 'boot1' },
        { status: 'unknown', boot: 'boot2' },
        { status: 'waiting_approval', boot: 'boot2' },
        { status: 'waiting_input', boot: 'boot2' },
      ])
    ).toEqual([W, NONE, NONE, W]);
  });

  it('does not arrive for a window whose standing report the new boot dropped', () => {
    expect(
      watch([
        { status: 'running', boot: 'boot1' },
        { status: 'waiting_input', boot: 'boot1', report: 'standing' },
        { status: 'unknown', boot: 'boot2' },
        { status: 'waiting_input', boot: 'boot2' },
      ])
    ).toEqual([NONE, NONE, NONE, NONE]);
  });
});

describe('observeNeedsYouArrivals: (C) the reader of a standing report is gone', () => {
  it('does not arrive anywhere when the report is released by an instruction and the window wobbles', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'standing' },
        { status: 'waiting_input', report: 'instruction' },
        { status: 'unknown', report: 'instruction' },
        { status: 'waiting_input', report: 'instruction' },
      ])
    ).toEqual([NONE, NONE, NONE, NONE, NONE]);
  });

  it('arrives once when the report is left with no reader, and not on the wobble after it', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'standing' },
        { status: 'waiting_input', report: 'no_reader' },
        { status: 'unknown', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
      ])
    ).toEqual([NONE, NONE, W, NONE, NONE]);
  });

  it('arrives when the window settles on waiting_input, not when no_reader comes while it is unknown', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'standing' },
        { status: 'unknown', report: 'standing' },
        { status: 'unknown', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
      ])
    ).toEqual([NONE, NONE, NONE, NONE, W]);
  });

  it('does not arrive when the mark turns unknown', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'standing' },
        { status: 'waiting_input', report: 'unknown' },
        { status: 'unknown', report: 'unknown' },
        { status: 'waiting_input', report: 'unknown' },
      ])
    ).toEqual([NONE, NONE, NONE, NONE, NONE]);
  });

  it('arrives at the approval and at the input wait only, when the owner dies during the approval and the window wobbles', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'standing' },
        { status: 'waiting_approval', report: 'standing' },
        { status: 'waiting_approval', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
        { status: 'unknown', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
      ])
    ).toEqual([NONE, NONE, W, NONE, W, NONE, NONE]);
  });

  it('arrives once when the turn ends and the reader is gone in the same observation, and not on the wobble after it', () => {
    expect(
      watch([
        'running',
        { status: 'running', report: 'standing' },
        { status: 'running', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
        { status: 'unknown', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
      ])
    ).toEqual([NONE, NONE, NONE, W, NONE, NONE]);
  });

  it('arrives once at the end of the turn for a report born with no reader, and not on the wobble after it', () => {
    expect(
      watch([
        'running',
        { status: 'waiting_input', report: 'no_reader' },
        { status: 'unknown', report: 'no_reader' },
        { status: 'waiting_input', report: 'no_reader' },
      ])
    ).toEqual([NONE, W, NONE, NONE]);
  });

  it('does not arrive when a daemon naming no boot delivers a no_reader list after the instruction that released the same report', () => {
    expect(
      watch([
        { status: 'running', boot: null },
        { status: 'waiting_input', boot: null, report: 'standing' },
        { status: 'unknown', boot: null, report: 'instruction' },
        { status: 'waiting_input', boot: null, report: 'no_reader' },
      ])
    ).toEqual([NONE, NONE, NONE, NONE]);
  });
});

describe('observeNeedsYouArrivals: windows in one observation', () => {
  it('starts a window that left the entries and came back as one seen for the first time', () => {
    expect(watch(['waiting_approval', null, 'waiting_approval'])).toEqual([W, NONE, W]);
    expect(watch(['running', null, 'waiting_input'])).toEqual([NONE, NONE, NONE]);
  });

  it('lists each arriving window once, and leaves out the windows that did not arrive', () => {
    const first = [
      arrivalEntry('a', { status: 'running' }),
      arrivalEntry('b', { status: 'waiting_input' }),
      arrivalEntry('c', { status: 'running' }),
    ];
    const second = [
      arrivalEntry('a', { status: 'waiting_input' }),
      arrivalEntry('b', { status: 'waiting_approval' }),
      arrivalEntry('c', { status: 'running' }),
    ];
    expect(observeAll([first, second])).toEqual([[], ['a', 'b']]);
  });
});

const totals = (needsYou: number, pendingPermissions: number): AttentionTotals => ({
  needsYou,
  pendingPermissions,
});

/** The send a decision made; fails the test when it made none. */
function sendOf(result: { send: AttentionSend | null }): AttentionSend {
  expect(result.send).not.toBeNull();
  return result.send as AttentionSend;
}

/** A state that has already told the core about `(1, 0)` with nothing outstanding. */
function seededState(): AttentionSendState {
  const seeded = beginAttentionSend(emptyAttentionSendState(), totals(1, 0), false);
  return settleAttentionSend(seeded.state, sendOf(seeded), true);
}

describe('attention send: whether the shell tells the core', () => {
  it('sends the first totals even when nothing arrived', () => {
    const { send } = beginAttentionSend(emptyAttentionSendState(), totals(0, 0), false);
    expect(send).toEqual({ needsYou: 0, pendingPermissions: 0, arrived: false });
  });

  it('sends again only when the totals moved or something arrived', () => {
    const first = beginAttentionSend(emptyAttentionSendState(), totals(1, 0), false);
    expect(first.send).not.toBeNull();
    const sent = settleAttentionSend(first.state, sendOf(first), true);

    expect(beginAttentionSend(sent, totals(1, 0), false).send).toBeNull();
    expect(beginAttentionSend(sent, totals(1, 1), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 1,
      arrived: false,
    });
    expect(beginAttentionSend(sent, totals(2, 0), false).send).toEqual({
      needsYou: 2,
      pendingPermissions: 0,
      arrived: false,
    });
    expect(beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: true,
    });
  });

  it('does not repeat totals a send in flight already carries, until that send fails', () => {
    const first = beginAttentionSend(emptyAttentionSendState(), totals(1, 0), false);
    const repeated = beginAttentionSend(first.state, totals(1, 0), false);
    expect(repeated.send).toBeNull();

    const failed = settleAttentionSend(repeated.state, sendOf(first), false);
    expect(beginAttentionSend(failed, totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: false,
    });

    // With two sends in flight, the newer one is what the totals are compared with.
    const older = beginAttentionSend(emptyAttentionSendState(), totals(1, 0), false);
    const newer = beginAttentionSend(older.state, totals(2, 0), false);
    expect(beginAttentionSend(newer.state, totals(2, 0), false).send).toBeNull();
    expect(beginAttentionSend(newer.state, totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: false,
    });
  });

  it('sends unchanged totals when told to send always', () => {
    const sent = seededState();
    expect(beginAttentionSend(sent, totals(1, 0), true).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: false,
    });
  });

  it('clears the arrival once a send carrying it succeeds', () => {
    const sent = seededState();

    const carrying = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    expect(carrying.send?.arrived).toBe(true);
    const after = settleAttentionSend(carrying.state, sendOf(carrying), true);
    expect(beginAttentionSend(after, totals(1, 0), false).send).toBeNull();
  });

  it('keeps the arrival and the last sent totals when the send fails', () => {
    const sent = seededState();

    const carrying = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    const failed = settleAttentionSend(carrying.state, sendOf(carrying), false);
    expect(beginAttentionSend(failed, totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: true,
    });

    const moved = beginAttentionSend(sent, totals(2, 0), false);
    const movedFailed = settleAttentionSend(moved.state, sendOf(moved), false);
    expect(beginAttentionSend(movedFailed, totals(2, 0), false).send).toEqual({
      needsYou: 2,
      pendingPermissions: 0,
      arrived: false,
    });
  });
});

describe('attention send: an arrival during a send in flight', () => {
  it('is carried by the next send when the send in flight succeeds', () => {
    const sent = seededState();

    const inFlight = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    const arrivedMeanwhile = noteAttentionArrival(inFlight.state);
    const settled = settleAttentionSend(arrivedMeanwhile, sendOf(inFlight), true);
    expect(beginAttentionSend(settled, totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: true,
    });
  });

  it('is not lost when the later send fails and the earlier one succeeds after it', () => {
    const sent = seededState();

    const earlier = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    const later = beginAttentionSend(noteAttentionArrival(earlier.state), totals(1, 0), false);
    expect(later.send?.arrived).toBe(true);
    const laterFailed = settleAttentionSend(later.state, sendOf(later), false);
    const earlierSucceeded = settleAttentionSend(laterFailed, sendOf(earlier), true);
    expect(beginAttentionSend(earlierSucceeded, totals(1, 0), false).send).toEqual({
      needsYou: 1,
      pendingPermissions: 0,
      arrived: true,
    });
  });
});

describe('attention send: an arrival already carried by a send in flight', () => {
  it('is not carried again by a send started before the first one settles', () => {
    const sent = seededState();

    const carrier = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    expect(carrier.send?.arrived).toBe(true);
    // The totals move with no new arrival while the carrier is still unsettled.
    const next = beginAttentionSend(carrier.state, totals(2, 0), false);
    expect(next.send).toEqual({ needsYou: 2, pendingPermissions: 0, arrived: false });
  });

  it('is carried by the next send when the carrier fails', () => {
    const sent = seededState();

    const carrier = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    const during = beginAttentionSend(carrier.state, totals(2, 0), false);
    const carrierFailed = settleAttentionSend(during.state, sendOf(carrier), false);
    const duringFailed = settleAttentionSend(carrierFailed, sendOf(during), false);
    expect(beginAttentionSend(duringFailed, totals(2, 0), false).send).toEqual({
      needsYou: 2,
      pendingPermissions: 0,
      arrived: true,
    });
  });

  it('is not carried by any later send once the carrier succeeds', () => {
    const sent = seededState();

    const carrier = beginAttentionSend(noteAttentionArrival(sent), totals(1, 0), false);
    const during = beginAttentionSend(carrier.state, totals(2, 0), false);
    const carrierSucceeded = settleAttentionSend(during.state, sendOf(carrier), true);
    const duringSucceeded = settleAttentionSend(carrierSucceeded, sendOf(during), true);
    expect(beginAttentionSend(duringSucceeded, totals(3, 0), false).send).toEqual({
      needsYou: 3,
      pendingPermissions: 0,
      arrived: false,
    });
  });
});
