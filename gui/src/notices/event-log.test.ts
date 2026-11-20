import { describe, it, expect, vi } from 'vitest';

import { diffGoneWindows } from '../session/shell-state';
import type { OrderedWindow } from '../session/shell-state';
import type { DaemonRecoveryStatus } from '../session/recovery-types';
import { describeExitedWindow as renderDescribeExitedWindow } from '../i18n/render';
import { dummyMessages } from '../test-utils/dummy-messages';
import {
  appendEvent,
  daemonReconnectedEvent,
  dismissEvent,
  failureEvent,
  windowExitEvent,
} from './event-log';
import type { EventLog, GoneWindow, NewEvent } from './event-log';

// The facade binds English; resolving the exit sentence against the dummy
// locale proves the event's text is the card's sentence, not a copy of it.
vi.mock('../ui-messages', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../ui-messages')>();
  const dummy = await import('../test-utils/dummy-messages');
  const render = await import('../i18n/render');
  return {
    ...actual,
    describeExitedWindow: (name: string, exitCode: number | null) =>
      render.describeExitedWindow(dummy.dummyMessages, name, exitCode),
  };
});

function dummyExitSentence(name: string, exitCode: number | null): string {
  return renderDescribeExitedWindow(dummyMessages, name, exitCode);
}

function failure(text: string): NewEvent {
  return { key: 'operation-failure', tone: 'error', text, toast: true, windowId: null };
}

function appendAll(
  events: readonly NewEvent[],
  start: { log: EventLog; memory: { nextSeq: number } } = { log: [], memory: { nextSeq: 0 } }
): { log: EventLog; memory: { nextSeq: number } } {
  return events.reduce((acc, event) => appendEvent(acc.log, acc.memory, event, '12:00'), start);
}

function seqsOf(log: EventLog): number[] {
  return log.map((entry) => entry.seq).sort((a, b) => a - b);
}

function failures(count: number): NewEvent[] {
  return Array.from({ length: count }, (_, i) => failure(`failure ${String(i)}`));
}

const SIDEBAR: readonly OrderedWindow[] = [
  { windowId: 'w-1', name: 'planner', exitCode: null },
  { windowId: 'w-2', name: 'builder', exitCode: 1 },
  { windowId: 'w-3', name: 'reviewer', exitCode: 0 },
];

/** The one window that leaves the sidebar, as the registry removes windows one at a time. */
function goneFrom(
  windowId: string,
  exitCode: number | null,
  flags: { goneByUser: boolean; carded: boolean }
): GoneWindow {
  const before = SIDEBAR.map((entry) =>
    entry.windowId === windowId ? { ...entry, exitCode } : entry
  );
  const [gone, ...others] = diffGoneWindows(
    before,
    before.filter((entry) => entry.windowId !== windowId)
  );
  if (gone === undefined || others.length > 0) {
    throw new Error(`expected exactly ${windowId} to leave the sidebar`);
  }
  return { ...gone, ...flags };
}

describe('event log: what is kept', () => {
  it('keeps every failure of an operation that keeps failing, so an earlier one is not lost', () => {
    const { log } = appendAll([failure('save failed'), failure('approval failed')]);

    expect(log.map((entry) => entry.text).sort()).toEqual(['approval failed', 'save failed']);
    expect(log.every((entry) => entry.key === 'operation-failure')).toBe(true);
  });

  it('keeps fifty events', () => {
    const { log } = appendAll(failures(50));

    expect(seqsOf(log)).toEqual(Array.from({ length: 50 }, (_, i) => i));
  });

  it('drops the oldest event once more than fifty were added', () => {
    const { log } = appendAll(failures(51));

    expect(seqsOf(log)).toEqual(Array.from({ length: 50 }, (_, i) => i + 1));
  });

  it('removes only the dismissed event', () => {
    const { log } = appendAll([failure('a'), failure('b'), failure('c')]);

    const after = dismissEvent(log, 1);

    expect(seqsOf(after)).toEqual([0, 2]);
    expect(seqsOf(log)).toEqual([0, 1, 2]);
  });
});

describe('event log: numbering', () => {
  it('numbers from the counter it is given and returns the counter moved past the event', () => {
    const memory = { nextSeq: 7, stateSeqs: new Map([['profile-unavailable', 3]]) };

    const first = appendEvent([], memory, failure('a'), '09:41');
    const second = appendEvent(first.log, first.memory, failure('b'), '09:42');

    expect(seqsOf(first.log)).toEqual([7]);
    expect(first.memory.nextSeq).toBe(8);
    expect(seqsOf(second.log)).toEqual([7, 8]);
    expect(second.memory.nextSeq).toBe(9);
    expect(second.memory.stateSeqs).toBe(memory.stateSeqs);
    expect(memory.nextSeq).toBe(7);
    expect(second.log.map((entry) => entry.at)).toEqual(['09:41', '09:42']);
  });

  it('does not hand out a number again after the log is cleared', () => {
    const added = appendAll([failure('a'), failure('b')]);
    const emptied: EventLog = [];

    const next = appendEvent(emptied, added.memory, failure('c'), '12:00');

    expect(seqsOf(next.log)).toEqual([2]);
    expect(next.memory.nextSeq).toBe(3);
  });
});

describe('windowExitEvent', () => {
  it('makes no event for a window this GUI closed', () => {
    expect(windowExitEvent(goneFrom('w-2', 1, { goneByUser: true, carded: false }))).toBeNull();
  });

  it('makes one event for the window that ended, named as the sidebar listed it', () => {
    const event = windowExitEvent(goneFrom('w-2', 1, { goneByUser: false, carded: false }));

    expect(event).toMatchObject({
      key: 'window-exit',
      windowId: 'w-2',
      text: dummyExitSentence('builder', 1),
      toast: true,
    });
  });

  it('uses the sentence without a code when the code is not known', () => {
    const event = windowExitEvent(goneFrom('w-3', null, { goneByUser: false, carded: false }));

    expect(event?.text).toBe(dummyExitSentence('reviewer', null));
  });

  it('marks the event of a window that left a card as not a pop-up, and keeps it in the log', () => {
    const event = windowExitEvent(goneFrom('w-2', 1, { goneByUser: false, carded: true }));
    if (event === null) {
      throw new Error('expected an event for a window that ended');
    }

    expect(event.toast).toBe(false);
    expect(
      appendEvent([], { nextSeq: 0 }, event, '12:00').log.map((entry) => entry.windowId)
    ).toEqual(['w-2']);
  });

  it.each([
    [0, 'info'],
    [1, 'warning'],
    [-1, 'warning'],
    [null, 'warning'],
  ] as const)('gives exit code %s the %s tone', (exitCode, tone) => {
    const event = windowExitEvent(goneFrom('w-2', exitCode, { goneByUser: false, carded: false }));

    expect(event?.tone).toBe(tone);
  });
});

describe('daemonReconnectedEvent', () => {
  const STATUSES: readonly DaemonRecoveryStatus[] = ['idle', 'retrying', 'exhausted'];
  const isReconnect = (before: DaemonRecoveryStatus, after: DaemonRecoveryStatus): boolean =>
    before !== 'idle' && after === 'idle';
  const PAIRS = STATUSES.flatMap((before) => STATUSES.map((after) => [before, after] as const));

  it.each(PAIRS.filter(([before, after]) => isReconnect(before, after)))(
    'makes one event when recovery comes back from %s to %s',
    (before, after) => {
      expect(daemonReconnectedEvent(before, after)).toMatchObject({
        key: 'daemon-reconnected',
        tone: 'info',
        windowId: null,
      });
    }
  );

  // idle to idle is also the first connection, which reconnects nothing.
  it.each(PAIRS.filter(([before, after]) => !isReconnect(before, after)))(
    'makes no event from %s to %s',
    (before, after) => {
      expect(daemonReconnectedEvent(before, after)).toBeNull();
    }
  );
});

describe('failureEvent', () => {
  // A failed action reads as an error; a restore that could not load or finish is a warning.
  it.each([
    ['operation-failure', 'error'],
    ['restore-load-failed', 'warning'],
    ['restore-not-restored', 'warning'],
  ] as const)('gives %s the %s tone and a pop-up', (key, tone) => {
    expect(failureEvent(key, 'text')).toEqual({
      key,
      tone,
      text: 'text',
      toast: true,
      windowId: null,
    });
  });
});
