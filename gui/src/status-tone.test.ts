/**
 * `status-tone.ts` is the single place that resolves a `WindowStatus` into a display
 * tone and a count of windows needing attention; every display location reads these
 * functions rather than re-deriving the classification. `windowsNeedingYou` /
 * `attentionCount` take a minimal structural shape rather than `WindowEntry`, so the
 * module stays independent of the store.
 */
import { describe, it, expect } from 'vitest';

import type { DetectedStatusView, WindowStatus } from './ipc/session-boundary';
import { resolveTone, attentionCount, windowsNeedingYou, type StatusTone } from './status-tone';

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

const EXPECTED_TONES: Record<WindowStatus, StatusTone> = {
  waiting_input: 'attention',
  waiting_approval: 'attention',
  running: 'active',
  interrupted: 'halted',
  idle: 'quiet',
  exited: 'quiet',
  unknown: 'unknown',
};

describe('resolveTone: total map over the seven WindowStatus values', () => {
  for (const [status, tone] of Object.entries(EXPECTED_TONES) as [WindowStatus, StatusTone][]) {
    it(`resolves ${status} to ${tone}`, () => {
      expect(resolveTone(status)).toBe(tone);
    });
  }
});

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
