/**
 * Direct-product acceptance coverage for `narrowChatMessages`, complementing
 * `session-client.chat.test.ts`'s single-representative fixtures: every
 * documented `from_kind` value crossed with every documented delivery
 * `state` value must decode without throwing and must preserve the value
 * verbatim, and an unknown value of either must still throw (fail-closed),
 * exercised once per axis rather than assumed from a single example.
 */
import { describe, it, expect } from 'vitest';

import { narrowChatMessages, narrowChatRooms, BoundaryError } from './session-boundary';

/** Every `from_kind` value the wire documents. */
const FROM_KINDS = ['window', 'user', 'external', 'system'] as const;

/** Every delivery state a chat envelope can be in. */
const DELIVERY_STATES = [
  'pending',
  'confirmed',
  'unconfirmed',
  'not_observed',
  'not_written',
  'write_failed',
  'undelivered_unknown',
  'undelivered_halted',
  'undelivered_exhausted',
] as const;

/** One message wire shape for `from_kind` x `state`. */
function messageWire(fromKind: string, state: string): Record<string, unknown> {
  return {
    seq: 1,
    at_ms: 1_000,
    from_kind: fromKind,
    from_window_id: fromKind === 'window' ? 'w-a' : null,
    from_name: `${fromKind}-name`,
    body: `body from ${fromKind}`,
    mentions: fromKind === 'system' ? ['user'] : [],
    broadcast: false,
    deliveries: [{ window_id: 'w-b', state }],
  };
}

function messagesWire(message: Record<string, unknown>): Record<string, unknown> {
  return {
    room_id: 'main',
    messages: [message],
    next_after_seq: 1,
    halted: null,
    needs_user: false,
    roster: [],
  };
}

describe('narrowChatMessages: every from_kind crossed with every delivery state', () => {
  for (const fromKind of FROM_KINDS) {
    for (const state of DELIVERY_STATES) {
      it(`accepts from_kind=${fromKind} state=${state} and preserves both verbatim`, () => {
        const wire = messageWire(fromKind, state);
        const decoded = narrowChatMessages(messagesWire(wire));
        const [message, ...rest] = decoded.messages;
        expect(rest).toHaveLength(0);
        expect(message?.from_kind).toBe(fromKind);
        expect(message?.deliveries[0]?.state).toBe(state);
      });
    }
  }

  it('ran the full 4x9 direct product', () => {
    let count = 0;
    for (const fromKind of FROM_KINDS) {
      for (const state of DELIVERY_STATES) {
        narrowChatMessages(messagesWire(messageWire(fromKind, state)));
        count += 1;
      }
    }
    expect(count).toBe(FROM_KINDS.length * DELIVERY_STATES.length);
  });
});

describe('narrowChatMessages: from_window_id Option discipline across from_kind', () => {
  it('accepts null from_window_id for every non-window kind', () => {
    for (const fromKind of ['user', 'external', 'system'] as const) {
      const decoded = narrowChatMessages(messagesWire(messageWire(fromKind, 'confirmed')));
      const [message] = decoded.messages;
      expect(message?.from_window_id).toBeNull();
    }
  });

  it('accepts a present from_window_id for the window kind', () => {
    const decoded = narrowChatMessages(messagesWire(messageWire('window', 'confirmed')));
    const [message] = decoded.messages;
    expect(message?.from_window_id).toBe('w-a');
  });
});

describe('narrowChatMessages: fail-closed on values outside the closed sets', () => {
  it('rejects every from_kind value not in the documented set, one at a time', () => {
    for (const bogus of ['agent', 'daemon', '', 'Window']) {
      expect(() => narrowChatMessages(messagesWire(messageWire(bogus, 'confirmed')))).toThrow(
        BoundaryError
      );
    }
  });

  it('rejects every delivery state value not in the documented set, one at a time', () => {
    for (const bogus of ['delivered', 'queued', '', 'Confirmed']) {
      expect(() => narrowChatMessages(messagesWire(messageWire('window', bogus)))).toThrow(
        BoundaryError
      );
    }
  });
});

// ---------------------------------------------------------------------------
// The room list, crossed over the two fields whose absence the wire uses as a
// value of its own: `halted` (absent or null both mean "not halted", and the
// narrowed row keeps the key either way) and `last_at_ms` (absent or null both
// mean "nothing posted yet").
// ---------------------------------------------------------------------------

/** One way a nullable field can arrive, and the value it has to fold to. */
interface NullableForm {
  /** How the field arrived on the wire. */
  label: string;
  /** Applies this form to an otherwise well-formed row. */
  apply: (row: Record<string, unknown>) => void;
  /** The value the narrowed row has to carry. */
  expected: unknown;
}

/** Every form `halted` arrives in. */
const HALTED_FORMS: NullableForm[] = [
  {
    label: 'absent',
    apply: (row) => {
      delete row['halted'];
    },
    expected: null,
  },
  {
    label: 'null',
    apply: (row) => {
      row['halted'] = null;
    },
    expected: null,
  },
  {
    label: 'a halt',
    apply: (row) => {
      row['halted'] = { reason: 'turn_budget', since_ms: 4_000 };
    },
    expected: { reason: 'turn_budget', since_ms: 4_000 },
  },
];

/** Every form `last_at_ms` arrives in. */
const LAST_AT_FORMS: NullableForm[] = [
  {
    label: 'absent',
    apply: (row) => {
      delete row['last_at_ms'];
    },
    expected: null,
  },
  {
    label: 'null',
    apply: (row) => {
      row['last_at_ms'] = null;
    },
    expected: null,
  },
  {
    label: 'a timestamp',
    apply: (row) => {
      row['last_at_ms'] = 1_700_000_000_000;
    },
    expected: 1_700_000_000_000,
  },
];

/** A well-formed room row, before either form is applied. */
function roomRowWire(): Record<string, unknown> {
  return {
    room_id: 'r-2b7c1d',
    name: 'yagura',
    participant_count: 2,
    needs_user: false,
    halted: null,
    last_at_ms: 1_700_000_000_000,
  };
}

function roomsReplyWire(row: Record<string, unknown>): Record<string, unknown> {
  return { rooms: [row] };
}

describe('narrowChatRooms: every halted form crossed with every last_at_ms form', () => {
  for (const halted of HALTED_FORMS) {
    for (const lastAt of LAST_AT_FORMS) {
      it(`folds halted=${halted.label} and last_at_ms=${lastAt.label} while keeping the halted key`, () => {
        const row = roomRowWire();
        halted.apply(row);
        lastAt.apply(row);

        const rooms = narrowChatRooms(roomsReplyWire(row));

        expect(rooms).toHaveLength(1);
        const [decoded] = rooms;
        expect('halted' in (decoded ?? {})).toBe(true);
        expect(decoded?.halted).toEqual(halted.expected);
        expect(decoded?.last_at_ms).toEqual(lastAt.expected);
      });
    }
  }

  it('ran the full 3x3 direct product', () => {
    let count = 0;
    for (const halted of HALTED_FORMS) {
      for (const lastAt of LAST_AT_FORMS) {
        const row = roomRowWire();
        halted.apply(row);
        lastAt.apply(row);
        narrowChatRooms(roomsReplyWire(row));
        count += 1;
      }
    }
    expect(count).toBe(HALTED_FORMS.length * LAST_AT_FORMS.length);
  });
});

describe('narrowChatRooms: participant_count is a non-negative integer', () => {
  it('accepts every non-negative integer it is given, one at a time', () => {
    for (const count of [0, 1, 7, 4_294_967_295]) {
      const row = roomRowWire();
      row['participant_count'] = count;
      const [decoded] = narrowChatRooms(roomsReplyWire(row));
      expect(decoded?.participant_count).toBe(count);
    }
  });

  it('rejects every value outside that set, one at a time', () => {
    for (const bogus of [-1, -0.5, 1.5, '2', null, true]) {
      const row = roomRowWire();
      row['participant_count'] = bogus;
      expect(() => narrowChatRooms(roomsReplyWire(row))).toThrow(BoundaryError);
    }
  });
});
