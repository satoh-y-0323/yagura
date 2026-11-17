/**
 * `status-tone.ts` is the single place that counts the windows needing attention and
 * gathers a group of windows into one Rail mark; every display location reads these
 * functions rather than re-deriving the classification. `windowsNeedingYou` /
 * `attentionCount` take a minimal structural shape rather than `WindowEntry`, so the
 * module stays independent of the store.
 */
import { describe, it, expect } from 'vitest';

import type { DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import { attentionCount, windowsNeedingYou } from './status-tone';

/**
 * Minimal shape `windowsNeedingYou` / `attentionCount` need from an entry: the
 * status and its reason as one value, so a caller cannot hand these helpers a
 * status taken apart from the reason that belongs to it.
 */
interface Entry {
  detected: DetectedStatusView;
  id: string;
}

/** An entry holding `status`, with the reason arm the status allows. */
function entryOf(status: WindowStatus, id: string): Entry {
  return { detected: status === 'unknown' ? { status, reason: null } : { status }, id };
}

describe('attentionCount: matches the windowsNeedingYou set', () => {
  it('is 0 for an empty entry list', () => {
    expect(attentionCount<Entry>([])).toBe(0);
  });

  it('counts waiting_input and waiting_approval entries only', () => {
    const entries: Entry[] = [
      entryOf('idle', 'a'),
      entryOf('waiting_input', 'b'),
      entryOf('running', 'c'),
      entryOf('waiting_approval', 'd'),
      entryOf('unknown', 'e'),
    ];
    expect(attentionCount(entries)).toBe(2);
  });

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
 * derived from `resolveTone` or any other production helper, so the
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

    it(`attentionCount is ${expected ? 1 : 0} for a lone ${status} window`, () => {
      const entry = entryOf(status, 'solo');
      expect(attentionCount([entry])).toBe(expected ? 1 : 0);
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

describe('attentionCount(x) === windowsNeedingYou(x).length', () => {
  it('holds for every combination of the seven statuses appearing together', () => {
    const allStatuses = Object.keys(EXPECTED_NEEDS_YOU) as WindowStatus[];
    const entries: Entry[] = [...allStatuses, ...allStatuses].map((status, i) =>
      entryOf(status, `e${i}`)
    );

    expect(attentionCount(entries)).toBe(windowsNeedingYou(entries).length);
  });

  it('holds for an empty entry list', () => {
    expect(attentionCount<Entry>([])).toBe(windowsNeedingYou<Entry>([]).length);
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

interface PresentationRow {
  rank: number;
  icon: string;
  role: string;
  motion: unknown;
  needsYou: boolean;
}

async function presentationTable(): Promise<Record<WindowStatus, PresentationRow>> {
  const mod = (await import('./status-presentation')) as Record<string, unknown>;
  return mod['STATUS_PRESENTATION'] as Record<WindowStatus, PresentationRow>;
}

function moves(motion: unknown): boolean {
  return Boolean(motion) && motion !== 'none';
}

describe('status presentation table', () => {
  it('gives every status a rank, an icon, a colour role and a motion', async () => {
    const table = await presentationTable();
    for (const status of STATUSES_BY_RANK) {
      const row = table[status];
      expect(typeof row.rank, status).toBe('number');
      expect(row.icon, status).toBeTruthy();
      expect(row.role, status).toBeTruthy();
      expect('motion' in row, status).toBe(true);
    }
  });

  it('ranks waiting_approval < waiting_input < unknown < interrupted < running < idle < exited', async () => {
    const table = await presentationTable();
    const sorted = [...STATUSES_BY_RANK].sort((x, y) => table[x].rank - table[y].rank);
    expect(sorted).toEqual(STATUSES_BY_RANK);
    expect(new Set(STATUSES_BY_RANK.map((s) => table[s].rank)).size).toBe(7);
  });

  it('tells approval from input by icon and by colour role, not by colour alone', async () => {
    const table = await presentationTable();
    expect(table.waiting_approval.icon).not.toBe(table.waiting_input.icon);
    expect(table.waiting_approval.role).not.toBe(table.waiting_input.role);
  });

  it('gives unknown an icon of its own, apart from idle', async () => {
    const table = await presentationTable();
    expect(table.unknown.icon).not.toBe(table.idle.icon);
  });

  it('does not colour interrupted as an error or danger', async () => {
    const table = await presentationTable();
    expect(String(table.interrupted.role)).not.toMatch(/error|danger/i);
  });

  it('moves only a running window', async () => {
    const table = await presentationTable();
    for (const status of STATUSES_BY_RANK) {
      expect(moves(table[status].motion), status).toBe(status === 'running');
    }
  });

  it('takes the needs-you membership of windowsNeedingYou from the one needs-you column of the table', async () => {
    const table = await presentationTable();
    for (const status of STATUSES_BY_RANK) {
      expect(windowsNeedingYou([entryOf(status, 'solo')]).length, status).toBe(
        table[status].needsYou ? 1 : 0
      );
    }
    expect(table.unknown.needsYou).toBe(false);
  });
});

interface RailSummary {
  count: number;
  mark: WindowStatus | null;
  unknownOnly: boolean;
}

async function summarize(
  statuses: WindowStatus[]
): Promise<{ summary: RailSummary; entries: Entry[] }> {
  const mod = (await import('./status-tone')) as Record<string, unknown>;
  const fn = mod['summarizeWindowStatuses'] as (entries: readonly Entry[]) => RailSummary;
  const entries = statuses.map((status, i) => entryOf(status, `e${i}`));
  return { summary: fn(entries), entries };
}

describe('Rail summary of a group of windows', () => {
  it('counts only the needs-you windows, the same as attentionCount', async () => {
    for (const statuses of [
      ['idle', 'running'],
      ['waiting_input', 'unknown', 'running'],
      ['waiting_approval', 'waiting_input', 'exited', 'unknown'],
    ] as WindowStatus[][]) {
      const { summary, entries } = await summarize(statuses);
      expect(summary.count).toBe(attentionCount(entries));
    }
  });

  it('marks the group with the highest-ranked status present', async () => {
    const cases: [WindowStatus[], WindowStatus][] = [
      [['idle', 'running'], 'running'],
      [['exited', 'idle', 'interrupted'], 'interrupted'],
      [['running', 'unknown', 'idle'], 'unknown'],
      [['running', 'waiting_input', 'unknown'], 'waiting_input'],
      [['waiting_input', 'waiting_approval', 'running'], 'waiting_approval'],
    ];
    for (const [statuses, expected] of cases) {
      const { summary } = await summarize(statuses);
      expect(summary.mark, statuses.join(',')).toBe(expected);
    }
  });

  it('reports count 0 and unknownOnly when no window needs you but one is unknown', async () => {
    const { summary } = await summarize(['unknown', 'idle', 'running']);
    expect(summary.count).toBe(0);
    expect(summary.unknownOnly).toBe(true);
  });

  it('does not report unknownOnly while a needs-you window exists, nor without an unknown window', async () => {
    expect((await summarize(['unknown', 'waiting_input'])).summary.unknownOnly).toBe(false);
    expect((await summarize(['running', 'idle'])).summary.unknownOnly).toBe(false);
  });

  it('has count 0, no mark and no unknownOnly for an empty group', async () => {
    const { summary } = await summarize([]);
    expect(summary).toEqual({ count: 0, mark: null, unknownOnly: false });
  });
});
