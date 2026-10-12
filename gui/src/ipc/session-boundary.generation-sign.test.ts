/**
 * The sign of the optional integers this boundary reads.
 *
 * A status generation counts observations, so a negative value is not a
 * generation at all and must never reach the registry as one. What a route
 * does with it follows what that route already does with a reason it may not
 * keep: the status-change route carries a single record and refuses it, while
 * the list route carries many and refuses the field alone, so one malformed
 * entry never costs the window it belongs to.
 *
 * The other optional integers are not generations and are guarded here
 * against being narrowed along with it: `exit_code` is negative in
 * production (a wait that failed is stored as -1, and a Windows crash code
 * can be negative too), and `last_at_ms` is not a counter either.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowAttachResponse,
  narrowChatRooms,
  narrowStatusPayload,
  narrowWindowInfo,
  narrowWindowInfoList,
} from './session-boundary';

/** Sample window id; the boundary treats it as an opaque string. */
const WINDOW_ID = 'w1-k3f9xa';

/** A well-formed `list_windows` entry. */
function windowInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    window_id: WINDOW_ID,
    name: 'main',
    vendor: null,
    cwd: '/x',
    status: 'running',
    exit_code: null,
    ...overrides,
  };
}

/** A well-formed `session://status` payload. */
function statusPayloadWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { window_id: WINDOW_ID, status: 'running', ...overrides };
}

/** A well-formed `list_chat_rooms` row. */
function chatRoomWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    room_id: 'main',
    name: 'yagura',
    participant_count: 2,
    needs_user: false,
    halted: null,
    ...overrides,
  };
}

/** Values a generation can never legitimately take. */
const NEGATIVE_GENERATIONS: readonly number[] = [-1, -42, Number.MIN_SAFE_INTEGER];

describe('a negative status generation on the status-change route', () => {
  for (const generation of NEGATIVE_GENERATIONS) {
    it(`refuses the whole payload for status_rev=${generation}`, () => {
      expect(() => narrowStatusPayload(statusPayloadWire({ status_rev: generation }))).toThrow(
        BoundaryError
      );
    });
  }

  it('still accepts the smallest generation a counter can report', () => {
    expect(narrowStatusPayload(statusPayloadWire({ status_rev: 0 })).status_rev).toBe(0);
  });
});

describe('a negative status generation on the list route', () => {
  for (const generation of NEGATIVE_GENERATIONS) {
    it(`keeps the window and drops the field for status_rev=${generation}`, () => {
      const info = narrowWindowInfo(windowInfoWire({ status_rev: generation }));
      expect(info.window_id).toBe(WINDOW_ID);
      expect(info.detected).toEqual({ status: 'running' });
      expect(Object.prototype.hasOwnProperty.call(info, 'status_rev')).toBe(false);
    });
  }

  it('keeps every other window of a list, generations included', () => {
    const list = narrowWindowInfoList([
      windowInfoWire({ window_id: 'w-a', status_rev: 7 }),
      windowInfoWire({ window_id: 'w-b', status_rev: -1 }),
      windowInfoWire({ window_id: 'w-c', status_rev: 9 }),
    ]);
    expect(list.map((entry) => entry.window_id)).toEqual(['w-a', 'w-b', 'w-c']);
    expect(list.map((entry) => entry.status_rev)).toEqual([7, undefined, 9]);
    expect(Object.prototype.hasOwnProperty.call(list[1], 'status_rev')).toBe(false);
  });

  it('still accepts the smallest generation a counter can report', () => {
    expect(narrowWindowInfo(windowInfoWire({ status_rev: 0 })).status_rev).toBe(0);
  });
});

describe('the optional integers that are not generations keep their range', () => {
  it('accepts the exit code of a wait that failed on a listed window', () => {
    expect(narrowWindowInfo(windowInfoWire({ exit_code: -1 })).exit_code).toBe(-1);
  });

  it('keeps a window of a list whose exit code is negative', () => {
    const list = narrowWindowInfoList([
      windowInfoWire({ window_id: 'w-a', exit_code: 0 }),
      windowInfoWire({ window_id: 'w-b', exit_code: -1 }),
      windowInfoWire({ window_id: 'w-c', exit_code: null }),
    ]);
    expect(list.map((entry) => entry.window_id)).toEqual(['w-a', 'w-b', 'w-c']);
    expect(list.map((entry) => entry.exit_code)).toEqual([0, -1, null]);
  });

  it('accepts a negative exit code on the attach reply', () => {
    const response = narrowAttachResponse({
      child_exited: true,
      exit_code: -1,
    });
    expect(response.exit_code).toBe(-1);
  });

  it('accepts a negative last_at_ms on a room row', () => {
    const rooms = narrowChatRooms({ rooms: [chatRoomWire({ last_at_ms: -1 })] });
    expect(rooms.map((room) => room.last_at_ms)).toEqual([-1]);
  });
});
