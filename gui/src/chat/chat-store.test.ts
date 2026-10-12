/**
 * The chat panel's read model: folding one `read_chat` page into what the
 * panel already holds, and deciding from a `chat_changed` push whether a
 * re-read is needed. Both are pure, so the merge order, the cursor arithmetic
 * and the "is this stale" rule are settled here instead of inside a component.
 */
import { describe, it, expect } from 'vitest';

import {
  anyNeedsUser,
  applyChatChangedToRooms,
  applyRoomsList,
  chatReloadAfterSeq,
  emptyChatRoom,
  mergeChatMessages,
  resolveDisplayName,
} from './chat-store';
import type {
  ChatChanged,
  ChatMessage,
  ChatMessages,
  ChatParticipant,
  ChatRoomInfo,
} from '../ipc/session-boundary';

/** A window-posted message with the given sequence number. */
function message(seq: number, overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    seq,
    at_ms: 1_700_000_000_000 + seq,
    from_kind: 'window',
    from_window_id: 'w-a',
    from_name: 'builder',
    body: `body ${seq}`,
    mentions: [],
    broadcast: false,
    deliveries: [],
    ...overrides,
  };
}

/** One `read_chat` page, empty unless overridden. */
function page(overrides: Partial<ChatMessages> = {}): ChatMessages {
  return {
    room_id: 'main',
    messages: [],
    next_after_seq: 0,
    halted: null,
    needs_user: false,
    roster: [],
    ...overrides,
  };
}

const ROSTER: ChatParticipant[] = [
  { window_id: 'w-a', name: 'builder' },
  { window_id: 'w-b', name: 'reviewer' },
];

/** A `chat_changed` payload for the room `main`. */
function changed(overrides: Partial<ChatChanged> = {}): ChatChanged {
  return { room_id: 'main', seq: 1, needs_user: false, ...overrides };
}

/** One `list_chat_rooms` row, unremarkable unless overridden. */
function roomInfo(overrides: Partial<ChatRoomInfo> = {}): ChatRoomInfo {
  return {
    room_id: 'main',
    name: 'main',
    participant_count: 0,
    needs_user: false,
    halted: null,
    last_at_ms: null,
    ...overrides,
  };
}

describe('emptyChatRoom', () => {
  it('starts with no messages, a zero cursor, no halt, no attention and an empty roster', () => {
    expect(emptyChatRoom('main')).toEqual({
      roomId: 'main',
      messages: [],
      nextAfterSeq: 0,
      halted: null,
      needsUser: false,
      roster: [],
    });
  });
});

describe('mergeChatMessages: message list', () => {
  it('takes the messages of a first page into an empty room', () => {
    const merged = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(1), message(2)], next_after_seq: 2 })
    );
    expect(merged.messages.map((entry) => entry.seq)).toEqual([1, 2]);
  });

  it('appends a later page after what the room already holds', () => {
    const first = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(1), message(2)], next_after_seq: 2 })
    );
    const second = mergeChatMessages(
      first,
      page({ messages: [message(3), message(4)], next_after_seq: 4 })
    );
    expect(second.messages.map((entry) => entry.seq)).toEqual([1, 2, 3, 4]);
  });

  it('keeps one entry per seq and lets the page replace what the room holds (delivery states move on)', () => {
    const first = mergeChatMessages(
      emptyChatRoom('main'),
      page({
        messages: [message(1, { deliveries: [{ window_id: 'w-b', state: 'pending' }] })],
        next_after_seq: 1,
      })
    );
    const second = mergeChatMessages(
      first,
      page({
        messages: [message(1, { deliveries: [{ window_id: 'w-b', state: 'confirmed' }] })],
        next_after_seq: 1,
      })
    );
    expect(second.messages).toHaveLength(1);
    expect(second.messages[0]?.deliveries).toEqual([{ window_id: 'w-b', state: 'confirmed' }]);
  });

  it('sorts a page that arrives out of order', () => {
    const merged = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(3), message(1), message(2)], next_after_seq: 3 })
    );
    expect(merged.messages.map((entry) => entry.seq)).toEqual([1, 2, 3]);
  });

  it('collapses a seq repeated inside one page, keeping the last occurrence', () => {
    const merged = mergeChatMessages(
      emptyChatRoom('main'),
      page({
        messages: [message(1, { body: 'first' }), message(1, { body: 'second' })],
        next_after_seq: 1,
      })
    );
    expect(merged.messages).toHaveLength(1);
    expect(merged.messages[0]?.body).toBe('second');
  });

  it('merges an older page into a room that already holds newer messages', () => {
    const newer = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(5)], next_after_seq: 5 })
    );
    const merged = mergeChatMessages(
      newer,
      page({ messages: [message(2), message(3)], next_after_seq: 3 })
    );
    expect(merged.messages.map((entry) => entry.seq)).toEqual([2, 3, 5]);
  });

  it('leaves the state it was given untouched', () => {
    const before = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(1)], next_after_seq: 1 })
    );
    const snapshot = before.messages;
    mergeChatMessages(
      before,
      page({ messages: [message(2)], next_after_seq: 2, needs_user: true })
    );
    expect(before.messages).toBe(snapshot);
    expect(before.messages.map((entry) => entry.seq)).toEqual([1]);
    expect(before.needsUser).toBe(false);
  });
});

describe('mergeChatMessages: cursor', () => {
  it('advances the cursor to the page cursor', () => {
    const merged = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(1), message(2)], next_after_seq: 2 })
    );
    expect(merged.nextAfterSeq).toBe(2);
  });

  it('does not move the cursor back when a page reports an older cursor', () => {
    const ahead = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(9)], next_after_seq: 9 })
    );
    const merged = mergeChatMessages(ahead, page({ messages: [message(4)], next_after_seq: 4 }));
    expect(merged.nextAfterSeq).toBe(9);
  });

  it('keeps the cursor where it was when an empty page comes back', () => {
    const ahead = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(7)], next_after_seq: 7 })
    );
    expect(mergeChatMessages(ahead, page({ next_after_seq: 7 })).nextAfterSeq).toBe(7);
  });
});

describe('mergeChatMessages: halt, attention and roster', () => {
  it('takes the halt the page reports', () => {
    const merged = mergeChatMessages(
      emptyChatRoom('main'),
      page({ halted: { reason: 'repetition', since_ms: 1_700_000_000_000 } })
    );
    expect(merged.halted).toEqual({ reason: 'repetition', since_ms: 1_700_000_000_000 });
  });

  it('clears a halt the room held once a page reports none', () => {
    const halted = mergeChatMessages(
      emptyChatRoom('main'),
      page({ halted: { reason: 'monologue', since_ms: 1_700_000_000_000 } })
    );
    expect(mergeChatMessages(halted, page()).halted).toBeNull();
  });

  it('takes the attention flag the page reports, in both directions', () => {
    const raised = mergeChatMessages(emptyChatRoom('main'), page({ needs_user: true }));
    expect(raised.needsUser).toBe(true);
    expect(mergeChatMessages(raised, page({ needs_user: false })).needsUser).toBe(false);
  });

  it('replaces the roster with the page roster, including an emptied one', () => {
    const withRoster = mergeChatMessages(emptyChatRoom('main'), page({ roster: ROSTER }));
    expect(withRoster.roster).toEqual(ROSTER);
    expect(mergeChatMessages(withRoster, page({ roster: [] })).roster).toEqual([]);
  });
});

describe('mergeChatMessages: another room', () => {
  it('drops what it held and follows the page when the page is for a different room', () => {
    const held = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(1), message(2)], next_after_seq: 2, needs_user: true })
    );
    const merged = mergeChatMessages(
      held,
      page({ room_id: 'other', messages: [message(1)], next_after_seq: 1 })
    );
    expect(merged.roomId).toBe('other');
    expect(merged.messages.map((entry) => entry.seq)).toEqual([1]);
    expect(merged.nextAfterSeq).toBe(1);
    expect(merged.needsUser).toBe(false);
  });
});

describe('chatReloadAfterSeq', () => {
  const held = mergeChatMessages(
    emptyChatRoom('main'),
    page({ messages: [message(1), message(2)], next_after_seq: 2 })
  );

  it('reads from the cursor when the push carries a seq past it', () => {
    expect(chatReloadAfterSeq(held, changed({ seq: 3 }))).toBe(2);
  });

  it('reads the row again when the push carries the cursor itself', () => {
    expect(chatReloadAfterSeq(held, changed({ seq: 2 }))).toBe(1);
  });

  it('reads the row again when the push carries an older seq the room still holds', () => {
    expect(chatReloadAfterSeq(held, changed({ seq: 1 }))).toBe(0);
  });

  it('reads the row again when a push repeats a seq whose attention flag has not moved', () => {
    const raised = mergeChatMessages(held, page({ next_after_seq: 2, needs_user: true }));
    expect(chatReloadAfterSeq(raised, changed({ seq: 2, needs_user: true }))).toBe(1);
  });

  it('reads from the start when the first push arrives into a room holding nothing', () => {
    const empty = mergeChatMessages(emptyChatRoom('main'), page());
    expect(chatReloadAfterSeq(empty, changed({ seq: 1 }))).toBe(0);
  });

  it('reads from the cursor when a push older than everything held moves the attention flag', () => {
    const evicted = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(5), message(9)], next_after_seq: 9 })
    );
    expect(chatReloadAfterSeq(evicted, changed({ seq: 3, needs_user: true }))).toBe(9);
  });

  it('does not read for a push older than everything held that carries the same attention flag', () => {
    const evicted = mergeChatMessages(
      emptyChatRoom('main'),
      page({ messages: [message(5), message(9)], next_after_seq: 9 })
    );
    expect(chatReloadAfterSeq(evicted, changed({ seq: 3 }))).toBeNull();
  });

  it('ignores a push for another room, however far ahead its seq is', () => {
    expect(chatReloadAfterSeq(held, changed({ room_id: 'other', seq: 99, needs_user: true }))).toBe(
      null
    );
  });

  it('ignores every push while no room has been read yet', () => {
    expect(chatReloadAfterSeq(emptyChatRoom(''), changed({ seq: 1, needs_user: true }))).toBeNull();
  });
});

describe('resolveDisplayName', () => {
  it("returns the roster's name for a current participant", () => {
    const state = mergeChatMessages(emptyChatRoom('main'), page({ roster: ROSTER }));
    expect(resolveDisplayName(state, 'w-a')).toBe('builder');
  });

  it('falls back to a held message from_name for a window no longer in the roster', () => {
    const state = mergeChatMessages(
      emptyChatRoom('main'),
      page({
        messages: [message(1, { from_window_id: 'w-c', from_name: 'closed-window' })],
        next_after_seq: 1,
        roster: ROSTER,
      })
    );
    expect(resolveDisplayName(state, 'w-c')).toBe('closed-window');
  });

  it('prefers the roster over stale history when both name the same window', () => {
    const state = mergeChatMessages(
      emptyChatRoom('main'),
      page({
        messages: [message(1, { from_window_id: 'w-a', from_name: 'stale-name' })],
        next_after_seq: 1,
        roster: ROSTER,
      })
    );
    expect(resolveDisplayName(state, 'w-a')).toBe('builder');
  });

  it('falls back to the window id when neither the roster nor any held message names it', () => {
    const state = mergeChatMessages(emptyChatRoom('main'), page({ roster: ROSTER }));
    expect(resolveDisplayName(state, 'w-z')).toBe('w-z');
  });
});

describe('applyRoomsList', () => {
  it('keeps the wire order (main first, then by recency) rather than re-sorting', () => {
    const wire = [
      roomInfo({ room_id: 'main', name: 'main' }),
      roomInfo({ room_id: 'r-yagura', name: 'yagura' }),
      roomInfo({ room_id: 'r-test', name: 'yagura_test' }),
    ];
    const rooms = applyRoomsList(wire);
    expect([...rooms.values()].map((entry) => entry.room_id)).toEqual([
      'main',
      'r-yagura',
      'r-test',
    ]);
  });

  it('keys each row by its room_id', () => {
    const row = roomInfo({ room_id: 'r-yagura', name: 'yagura', participant_count: 3 });
    const rooms = applyRoomsList([row]);
    expect(rooms.get('r-yagura')).toEqual(row);
  });

  it('accepts an empty list', () => {
    expect(applyRoomsList([]).size).toBe(0);
  });

  it('holds nothing left over from an earlier call (a fresh list fully replaces what came before)', () => {
    const first = applyRoomsList([roomInfo({ room_id: 'main' }), roomInfo({ room_id: 'r-old' })]);
    expect(first.has('r-old')).toBe(true);
    const second = applyRoomsList([roomInfo({ room_id: 'main' })]);
    expect(second.has('r-old')).toBe(false);
    expect(second.size).toBe(1);
  });
});

describe('anyNeedsUser', () => {
  it('is false for an empty map', () => {
    expect(anyNeedsUser(new Map())).toBe(false);
  });

  it('is false when every room reports needs_user false', () => {
    const rooms = applyRoomsList([
      roomInfo({ room_id: 'main', needs_user: false }),
      roomInfo({ room_id: 'r-yagura', needs_user: false }),
    ]);
    expect(anyNeedsUser(rooms)).toBe(false);
  });

  it('is true when exactly one room needs the user, regardless of its position', () => {
    const rooms = applyRoomsList([
      roomInfo({ room_id: 'main', needs_user: false }),
      roomInfo({ room_id: 'r-yagura', needs_user: false }),
      roomInfo({ room_id: 'r-test', needs_user: true }),
    ]);
    expect(anyNeedsUser(rooms)).toBe(true);
  });

  it('is true when every room needs the user', () => {
    const rooms = applyRoomsList([
      roomInfo({ room_id: 'main', needs_user: true }),
      roomInfo({ room_id: 'r-yagura', needs_user: true }),
    ]);
    expect(anyNeedsUser(rooms)).toBe(true);
  });
});

describe('applyChatChangedToRooms: a room already in the list', () => {
  it('updates only needs_user, leaving name, participant_count, halted and last_at_ms untouched', () => {
    const rooms = applyRoomsList([
      roomInfo({
        room_id: 'r-yagura',
        name: 'yagura',
        participant_count: 3,
        needs_user: false,
        halted: { reason: 'monologue', since_ms: 1_700_000_000_000 },
        last_at_ms: 42,
      }),
    ]);
    const updated = applyChatChangedToRooms(
      rooms,
      changed({ room_id: 'r-yagura', seq: 9, needs_user: true })
    );
    expect(updated.get('r-yagura')).toEqual({
      room_id: 'r-yagura',
      name: 'yagura',
      participant_count: 3,
      needs_user: true,
      halted: { reason: 'monologue', since_ms: 1_700_000_000_000 },
      last_at_ms: 42,
    });
  });

  it('leaves a room the push does not name untouched', () => {
    const rooms = applyRoomsList([
      roomInfo({ room_id: 'main', needs_user: false }),
      roomInfo({ room_id: 'r-yagura', needs_user: false }),
    ]);
    const updated = applyChatChangedToRooms(rooms, changed({ room_id: 'r-yagura' }));
    expect(updated.get('main')).toEqual(rooms.get('main'));
  });

  it('does not mutate the map it was given', () => {
    const rooms = applyRoomsList([roomInfo({ room_id: 'r-yagura', needs_user: false })]);
    const updated = applyChatChangedToRooms(
      rooms,
      changed({ room_id: 'r-yagura', needs_user: true })
    );
    expect(rooms.get('r-yagura')?.needs_user).toBe(false);
    expect(updated).not.toBe(rooms);
  });
});

describe('applyChatChangedToRooms: a room not yet in the list', () => {
  it('adds a minimal row: empty name, zero participants, needs_user from the payload, no halted key', () => {
    const rooms = applyRoomsList([roomInfo({ room_id: 'main' })]);
    const updated = applyChatChangedToRooms(
      rooms,
      changed({ room_id: 'r-new', seq: 1, needs_user: true })
    );
    const added = updated.get('r-new');
    expect(added).toEqual({
      room_id: 'r-new',
      name: '',
      participant_count: 0,
      needs_user: true,
      last_at_ms: null,
    });
    if (added === undefined) {
      throw new Error('the push did not add a row for the unseen room');
    }
    expect('halted' in added).toBe(false);
  });

  it('keeps the rooms the push did not name, alongside the new minimal row', () => {
    const rooms = applyRoomsList([roomInfo({ room_id: 'main', name: 'main' })]);
    const updated = applyChatChangedToRooms(rooms, changed({ room_id: 'r-new' }));
    expect(updated.get('main')).toEqual(rooms.get('main'));
    expect(updated.size).toBe(2);
  });

  it('raises the aggregate badge for a push from a room the list has never reported', () => {
    const rooms = applyRoomsList([roomInfo({ room_id: 'main', needs_user: false })]);
    expect(anyNeedsUser(rooms)).toBe(false);
    const updated = applyChatChangedToRooms(rooms, changed({ room_id: 'r-new', needs_user: true }));
    expect(anyNeedsUser(updated)).toBe(true);
  });
});

describe('chatReloadAfterSeq: rooms with nothing held', () => {
  it('does not read for a push at the cursor of a room that holds nothing and has the same attention flag', () => {
    const empty = emptyChatRoom('main');
    expect(chatReloadAfterSeq(empty, changed({ seq: 0 }))).toBeNull();
  });

  it('reads from the cursor for such a push when the attention flag moved', () => {
    const empty = emptyChatRoom('main');
    expect(chatReloadAfterSeq(empty, changed({ seq: 0, needs_user: true }))).toBe(0);
  });

  it('ignores a push that names no room while no room has been read yet', () => {
    expect(chatReloadAfterSeq(emptyChatRoom(''), changed({ room_id: '', seq: 1 }))).toBeNull();
  });
});

describe('resolveDisplayName: the most recent message wins', () => {
  it('names a closed window by what it called itself last', () => {
    const state = mergeChatMessages(
      emptyChatRoom('main'),
      page({
        messages: [
          message(1, { from_window_id: 'w-c', from_name: 'first-name' }),
          message(2, { from_window_id: 'w-d', from_name: 'other' }),
          message(3, { from_window_id: 'w-c', from_name: 'renamed' }),
          message(4, { from_window_id: 'w-d', from_name: 'other-later' }),
        ],
        next_after_seq: 4,
      })
    );
    expect(resolveDisplayName(state, 'w-c')).toBe('renamed');
  });
});
