/**
 * `status-tone.ts` is the single place that counts the windows needing attention and
 * gathers a group of windows into one Rail mark; every display location reads these
 * functions rather than re-deriving the classification. `windowsNeedingYou` /
 * `attentionCount` take a minimal structural shape rather than `WindowEntry`, so the
 * module stays independent of the store.
 */
import { describe, it, expect } from 'vitest';

import type { DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import { STATUS_PRESENTATION } from './status-presentation';
import {
  attentionCount,
  needsYou,
  summarizeWindowStatuses,
  windowsNeedingYou,
} from './status-tone';

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
  for (const [status, expected] of Object.entries(EXPECTED_NEEDS_YOU) as [
    WindowStatus,
    boolean,
  ][]) {
    it(`windowsNeedingYou ${expected ? 'includes' : 'excludes'} a lone ${status} window`, () => {
      const entry = entryOf(status, 'solo');
      expect(windowsNeedingYou([entry]).map((e) => e.id)).toEqual(expected ? ['solo'] : []);
    });
  }

  it('returns an empty array for an empty entry list', () => {
    expect(windowsNeedingYou<Entry>([])).toEqual([]);
  });

  it('keeps entries in input order and drops every entry not in the table above', () => {
    const entries: Entry[] = [
      entryOf('idle', 'a'),
      entryOf('waiting_input', 'b'),
      entryOf('running', 'c'),
      entryOf('waiting_approval', 'd'),
      entryOf('unknown', 'e'),
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

  it.each(NOT_STANDING_STATES)(
    'keeps in a waiting_input window whose report reads %s: only a standing report is left out',
    (state) => {
      expect(needsYou(reportedEntryOf('waiting_input', 'w', state))).toBe(true);
    }
  );

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

describe('attentionCount(x) === windowsNeedingYou(x).length', () => {
  it('holds for every combination of the seven statuses appearing together', () => {
    const allStatuses = Object.keys(EXPECTED_NEEDS_YOU) as WindowStatus[];
    const entries: Entry[] = [...allStatuses, ...allStatuses].map((status, i) =>
      entryOf(status, `e${i}`)
    );

    expect(attentionCount(entries)).toBe(windowsNeedingYou(entries).length);
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
  it('counts only the needs-you windows, the same as attentionCount', () => {
    for (const statuses of [
      ['idle', 'running'],
      ['waiting_input', 'unknown', 'running'],
      ['waiting_approval', 'waiting_input', 'exited', 'unknown'],
    ] as WindowStatus[][]) {
      const { summary, entries } = summarize(statuses);
      expect(summary.count).toBe(attentionCount(entries));
    }
  });

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
