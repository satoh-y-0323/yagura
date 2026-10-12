import { describe, it, expect } from 'vitest';

import { planPanes, pairPanes, type Pane } from './pane-plan';
import type { WindowEntry } from '../stores/window-registry';

describe('planPanes', () => {
  it('opens every window when no panes exist yet', () => {
    const plan = planPanes(['w-a', 'w-b', 'w-c'], []);

    expect(plan).toEqual({ toOpen: ['w-a', 'w-b', 'w-c'], toClose: [] });
  });

  it('is idempotent when the live panes already match the daemon window list', () => {
    const plan = planPanes(['w-a', 'w-b', 'w-c'], ['w-a', 'w-b', 'w-c']);

    expect(plan).toEqual({ toOpen: [], toClose: [] });
  });

  it('closes only the pane whose window disappeared from the daemon list', () => {
    const plan = planPanes(['w-a', 'w-c'], ['w-a', 'w-b', 'w-c']);

    expect(plan).toEqual({ toOpen: [], toClose: ['w-b'] });
  });

  it('opens only the window newly reported by the daemon', () => {
    const plan = planPanes(['w-a', 'w-b', 'w-c'], ['w-a', 'w-b']);

    expect(plan).toEqual({ toOpen: ['w-c'], toClose: [] });
  });

  it('closes every live pane when the daemon reports no windows', () => {
    const plan = planPanes([], ['w-a', 'w-b']);

    expect(plan).toEqual({ toOpen: [], toClose: ['w-a', 'w-b'] });
  });

  it('does not depend on the order of livePaneIds', () => {
    const forward = planPanes(['w-a', 'w-b'], ['w-c', 'w-a']);
    const reversed = planPanes(['w-a', 'w-b'], ['w-a', 'w-c']);

    expect(forward).toEqual({ toOpen: ['w-b'], toClose: ['w-c'] });
    expect(reversed).toEqual(forward);
  });
});

/** Builds a minimal WindowEntry; only windowId matters to pairPanes. */
function makeEntry(windowId: string): WindowEntry {
  return {
    windowId,
    name: windowId,
    vendor: null,
    cwd: '/home/user/project',
    detected: { status: 'running' },
    statusRev: null,
    exitCode: null,
    room: null,
    detectionWarning: null,
    mcpConnected: null,
    statusReportsMissing: null,
    display: {
      position: null,
      size: null,
      zOrder: 0,
      isMinimized: false,
      isMaximized: false,
      restoreBounds: null,
    },
  };
}

/** A Pane stub with a stable identity, so a paired entry can be traced back to it. */
function makePane(tag: string): Pane {
  return { controller: tag, sessionStore: tag } as unknown as Pane;
}

describe('pairPanes', () => {
  it('excludes an entry that has no pane yet', () => {
    const entryA = makeEntry('w-a');
    const entryB = makeEntry('w-b');
    const paneA = makePane('a');
    const panes = new Map([['w-a', paneA]]);

    const result = pairPanes([entryA, entryB], panes);

    expect(result).toEqual([{ entry: entryA, pane: paneA }]);
  });

  it('excludes a pane whose window is not in the entry list', () => {
    const entryA = makeEntry('w-a');
    const paneA = makePane('a');
    const paneB = makePane('b');
    const panes = new Map([
      ['w-a', paneA],
      ['w-b', paneB],
    ]);

    const result = pairPanes([entryA], panes);

    expect(result).toEqual([{ entry: entryA, pane: paneA }]);
  });

  it('pairs entries and panes in the entries order, regardless of the panes map order', () => {
    const entryA = makeEntry('w-a');
    const entryB = makeEntry('w-b');
    const entryC = makeEntry('w-c');
    const paneA = makePane('a');
    const paneB = makePane('b');
    const paneC = makePane('c');
    const panes = new Map([
      ['w-c', paneC],
      ['w-a', paneA],
      ['w-b', paneB],
    ]);

    const result = pairPanes([entryA, entryB, entryC], panes);

    expect(result).toEqual([
      { entry: entryA, pane: paneA },
      { entry: entryB, pane: paneB },
      { entry: entryC, pane: paneC },
    ]);
  });

  it('does not mutate the entries array or the panes map it is given', () => {
    const entryA = makeEntry('w-a');
    const paneA = makePane('a');
    const entries = [entryA];
    const panes = new Map([['w-a', paneA]]);
    const entriesSnapshot = [...entries];
    const panesSnapshot = new Map(panes);

    pairPanes(entries, panes);

    expect(entries).toEqual(entriesSnapshot);
    expect(panes).toEqual(panesSnapshot);
  });

  it('returns an empty array for empty inputs', () => {
    expect(pairPanes([], new Map())).toEqual([]);
  });
});
