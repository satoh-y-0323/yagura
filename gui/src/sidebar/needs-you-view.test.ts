/**
 * The "Needs you" filter's row set. The set is rebuilt only when the user
 * operates; between operations a row that stops needing the user stays (as
 * settled) and a window that starts needing the user is appended, so no row
 * moves without the user asking.
 */
import { describe, it, expect } from 'vitest';

import type { DetectedStatusView, TaskReportMark, WindowStatus } from '../ipc/session-boundary';
import { groupByRoom } from './room-groups';

interface Entry {
  windowId: string;
  cwd: string;
  detected: DetectedStatusView;
  taskReport?: TaskReportMark;
  room: { roomId: string; name: string } | null;
  display: { isMinimized: boolean };
}

function makeEntry(windowId: string, status: WindowStatus, overrides: Partial<Entry> = {}): Entry {
  return {
    windowId,
    cwd: 'C:\\work',
    detected: status === 'unknown' ? { status, reason: null } : { status },
    room: null,
    display: { isMinimized: false },
    ...overrides,
  };
}

/** The module under test is loaded per test so a missing function fails only its own test. */
async function needsYouView() {
  return import('./needs-you-view');
}

/** The sidebar's rooms for `entries`, in tab order `order` (default: the entries' own order). */
function groupsOf(entries: readonly Entry[], order = entries.map((entry) => entry.windowId)) {
  return groupByRoom(entries, new Set(), order);
}

const ROOM_A = { roomId: 'room-a', name: 'Room A' };
const ROOM_B = { roomId: 'room-b', name: 'Room B' };

describe('nextNeedsYouView: rebuilding on a user operation', () => {
  it('lists the windows that need you in the sidebar order, room by room', async () => {
    const { nextNeedsYouView } = await needsYouView();
    // Entries interleave the rooms; the sidebar reads room A (a2, a1) then room B (b1).
    const entries = [
      makeEntry('a1', 'waiting_input', { room: ROOM_A }),
      makeEntry('b1', 'waiting_approval', { room: ROOM_B }),
      makeEntry('a2', 'waiting_input', { room: ROOM_A }),
      makeEntry('x', 'running', { room: ROOM_A }),
    ];
    const groups = groupsOf(entries, ['a2', 'b1', 'a1', 'x']);

    const view = nextNeedsYouView(null, groups, true);

    expect(view.rows.map((row) => row.windowId)).toEqual(['a2', 'a1', 'b1']);
    expect(view.rows.every((row) => !row.settled)).toBe(true);
  });

  it('drops the settled rows and takes up the windows that came in', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const before = [makeEntry('a', 'waiting_input'), makeEntry('b', 'waiting_input')];
    const first = nextNeedsYouView(null, groupsOf(before), true);
    const after = [
      makeEntry('a', 'running'),
      makeEntry('b', 'waiting_input'),
      makeEntry('c', 'waiting_input'),
    ];
    const kept = nextNeedsYouView(first, groupsOf(after), false);
    expect(kept.rows.map((row) => row.windowId)).toEqual(['a', 'b', 'c']);

    const rebuilt = nextNeedsYouView(kept, groupsOf(after), true);

    expect(rebuilt.rows.map((row) => row.windowId)).toEqual(['b', 'c']);
    expect(rebuilt.rows.every((row) => !row.settled)).toBe(true);
  });
});

describe('nextNeedsYouView: between user operations nothing moves', () => {
  it('keeps a row that stopped needing you, marked settled, in place', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const first = nextNeedsYouView(
      null,
      groupsOf([
        makeEntry('a', 'waiting_input'),
        makeEntry('b', 'waiting_input'),
        makeEntry('c', 'waiting_input'),
      ]),
      true
    );

    const next = nextNeedsYouView(
      first,
      groupsOf([
        makeEntry('a', 'waiting_input'),
        makeEntry('b', 'running'),
        makeEntry('c', 'waiting_input'),
      ]),
      false
    );

    expect(next.rows).toEqual([
      expect.objectContaining({ windowId: 'a', settled: false }),
      expect.objectContaining({ windowId: 'b', settled: true }),
      expect.objectContaining({ windowId: 'c', settled: false }),
    ]);

    // A row that goes back to needing you is no longer settled.
    const back = nextNeedsYouView(
      next,
      groupsOf([
        makeEntry('a', 'waiting_input'),
        makeEntry('b', 'waiting_input'),
        makeEntry('c', 'waiting_input'),
      ]),
      false
    );
    expect(back.rows).toEqual([
      expect.objectContaining({ windowId: 'a', settled: false }),
      expect.objectContaining({ windowId: 'b', settled: false }),
      expect.objectContaining({ windowId: 'c', settled: false }),
    ]);

    // A window that was closed has nothing to open: its row goes even between operations,
    // whether it was still waiting (c) or settled (b).
    const closed = nextNeedsYouView(next, groupsOf([makeEntry('a', 'waiting_input')]), false);
    expect(closed.rows.map((row) => row.windowId)).toEqual(['a']);
  });

  it('appends a window that started needing you at the end, without pushing the rows above', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const first = nextNeedsYouView(
      null,
      groupsOf([makeEntry('b', 'waiting_input'), makeEntry('c', 'waiting_input')]),
      true
    );

    // `a` sits above `b` in the sidebar, yet it joins below.
    const next = nextNeedsYouView(
      first,
      groupsOf([
        makeEntry('a', 'waiting_approval'),
        makeEntry('b', 'waiting_input'),
        makeEntry('c', 'waiting_input'),
      ]),
      false
    );

    expect(next.rows.map((row) => row.windowId)).toEqual(['b', 'c', 'a']);
  });

  it('keeps a window whose report still stands out of the rows (the one decision is needsYou)', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const entries = [
      makeEntry('owner-wait', 'waiting_input', { taskReport: { seq: 1, standing: true } }),
      makeEntry('you', 'waiting_input'),
    ];

    const view = nextNeedsYouView(null, groupsOf(entries), true);

    expect(view.rows.map((row) => row.windowId)).toEqual(['you']);
  });
});

describe('nextNeedsYouView: windows with an unknown status', () => {
  it('lists them apart from the rows, so they are not counted among the windows that need you', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const entries = [makeEntry('a', 'waiting_input'), makeEntry('u', 'unknown')];

    const view = nextNeedsYouView(null, groupsOf(entries), true);

    expect(view.rows.map((row) => row.windowId)).toEqual(['a']);
    expect(view.unknown.map((row) => row.windowId)).toEqual(['u']);
  });

  it('has no unknown rows (so no heading) when no window is unknown', async () => {
    const { nextNeedsYouView } = await needsYouView();

    const view = nextNeedsYouView(null, groupsOf([makeEntry('a', 'waiting_input')]), true);

    expect(view.unknown).toEqual([]);
  });

  it('shows an unknown window that appears between operations, without moving the rows', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const first = nextNeedsYouView(null, groupsOf([makeEntry('a', 'waiting_input')]), true);

    const next = nextNeedsYouView(
      first,
      groupsOf([makeEntry('a', 'waiting_input'), makeEntry('u', 'unknown')]),
      false
    );

    expect(next.rows.map((row) => row.windowId)).toEqual(['a']);
    expect(next.unknown.map((row) => row.windowId)).toEqual(['u']);

    // A kept row whose window turns unknown stays a row; it is not listed twice.
    const turned = nextNeedsYouView(
      next,
      groupsOf([makeEntry('a', 'unknown'), makeEntry('u', 'unknown')]),
      false
    );
    expect(turned.rows).toEqual([expect.objectContaining({ windowId: 'a', settled: true })]);
    expect(turned.unknown.map((row) => row.windowId)).toEqual(['u']);
  });
});

describe('nextNeedsYouView: the empty states', () => {
  it('says nothing needs you when no window needs you and none is unknown', async () => {
    const { nextNeedsYouView } = await needsYouView();

    const view = nextNeedsYouView(null, groupsOf([makeEntry('a', 'running')]), true);

    expect(view.rows).toEqual([]);
    expect(view.unknown).toEqual([]);
    expect(view.showsNothingNeedsYou).toBe(true);
  });

  it('does not say nothing needs you while a settled row is still listed, until the next rebuild', async () => {
    const { nextNeedsYouView } = await needsYouView();
    const first = nextNeedsYouView(null, groupsOf([makeEntry('a', 'waiting_input')]), true);
    const running = groupsOf([makeEntry('a', 'running')]);

    const kept = nextNeedsYouView(first, running, false);
    const rebuilt = nextNeedsYouView(kept, running, true);

    expect(kept.rows).toEqual([expect.objectContaining({ windowId: 'a', settled: true })]);
    expect(kept.showsNothingNeedsYou).toBe(false);
    expect(rebuilt.showsNothingNeedsYou).toBe(true);
  });

  it('does not say nothing needs you when only unknown windows are left', async () => {
    const { nextNeedsYouView } = await needsYouView();

    const view = nextNeedsYouView(null, groupsOf([makeEntry('u', 'unknown')]), true);

    expect(view.rows).toEqual([]);
    expect(view.unknown.map((row) => row.windowId)).toEqual(['u']);
    expect(view.showsNothingNeedsYou).toBe(false);
  });
});
