/**
 * TS-boundary contract for the chat bridge: the three narrowing functions
 * that turn a daemon reply into a typed value (`narrowChatPosted`,
 * `narrowChatMessages`, `narrowChatChanged`), the three Tauri commands that
 * post and read chat, and the subscription to the chat-changed event.
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and the shape it produces,
 * not about a real Tauri runtime. It lives in its own file rather than
 * alongside the other `session-client` cases so the wire shapes below are
 * read in one place.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  unlistenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import {
  BoundaryError,
  SESSION_EVENT,
  narrowChatPosted,
  narrowChatMessages,
  narrowChatChanged,
  narrowChatRooms,
  type ChatPosted,
  type ChatMessages,
  type ChatChanged,
  type ChatRoomInfo,
} from './session-boundary';
import {
  invokeSendChat,
  invokeBroadcastChat,
  invokeReadChat,
  invokeListChatRooms,
  subscribeChatChanged,
  subscribeChatParticipantsChanged,
} from './session-client';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

/** A well-formed `ChatPosted` reply, snake_case throughout. */
function chatPostedWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    room_id: 'main',
    seq: 5,
    targets: ['w-a', 'w-b'],
    user: false,
    unresolved: [],
    halted: false,
    ...overrides,
  };
}

/** One chat message as the daemon serializes it. */
function chatMessageWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    seq: 1,
    at_ms: 1_000,
    from_kind: 'window',
    from_window_id: 'w-a',
    from_name: 'window a',
    body: 'hello',
    mentions: [],
    broadcast: false,
    deliveries: [{ window_id: 'w-b', state: 'taken' }],
    ...overrides,
  };
}

/** A well-formed `ChatMessages` reply. */
function chatMessagesWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    room_id: 'main',
    messages: [chatMessageWire()],
    next_after_seq: 1,
    halted: null,
    needs_user: false,
    roster: [{ window_id: 'w-a', name: 'window a' }],
    ...overrides,
  };
}

/** A well-formed `ChatChanged` reply. */
function chatChangedWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    room_id: 'main',
    seq: 3,
    needs_user: true,
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
});

describe('narrowChatPosted', () => {
  it('accepts a well-formed reply', () => {
    const posted = narrowChatPosted(chatPostedWire());
    expect(posted).toEqual({
      room_id: 'main',
      seq: 5,
      targets: ['w-a', 'w-b'],
      user: false,
      unresolved: [],
      halted: false,
    });
  });

  it('rejects a non-object value', () => {
    expect(() => narrowChatPosted(null)).toThrow(BoundaryError);
    expect(() => narrowChatPosted('nope')).toThrow(BoundaryError);
  });

  it('rejects missing room_id', () => {
    const wire = chatPostedWire();
    delete wire['room_id'];
    expect(() => narrowChatPosted(wire)).toThrow(BoundaryError);
  });

  it('rejects a non-integer seq', () => {
    expect(() => narrowChatPosted(chatPostedWire({ seq: 'five' }))).toThrow(BoundaryError);
  });

  it('rejects targets that is not an array', () => {
    expect(() => narrowChatPosted(chatPostedWire({ targets: 'w-a' }))).toThrow(BoundaryError);
  });

  it('rejects a targets entry that is not a string', () => {
    expect(() => narrowChatPosted(chatPostedWire({ targets: [1] }))).toThrow(BoundaryError);
  });

  it('rejects a non-boolean user', () => {
    expect(() => narrowChatPosted(chatPostedWire({ user: 'yes' }))).toThrow(BoundaryError);
  });

  it('rejects unresolved that is not an array', () => {
    expect(() => narrowChatPosted(chatPostedWire({ unresolved: 'w-c' }))).toThrow(BoundaryError);
  });

  it('rejects a non-boolean halted', () => {
    expect(() => narrowChatPosted(chatPostedWire({ halted: 'no' }))).toThrow(BoundaryError);
  });
});

describe('narrowChatMessages', () => {
  it('accepts a well-formed reply, preserving the roster', () => {
    const messages: ChatMessages = narrowChatMessages(chatMessagesWire());
    expect(messages).toEqual({
      room_id: 'main',
      messages: [
        {
          seq: 1,
          at_ms: 1_000,
          from_kind: 'window',
          from_window_id: 'w-a',
          from_name: 'window a',
          body: 'hello',
          mentions: [],
          broadcast: false,
          deliveries: [{ window_id: 'w-b', state: 'taken' }],
        },
      ],
      next_after_seq: 1,
      halted: null,
      needs_user: false,
      roster: [{ window_id: 'w-a', name: 'window a' }],
    });
  });

  it('accepts a non-null halted', () => {
    const messages = narrowChatMessages(
      chatMessagesWire({ halted: { reason: 'repetition', since_ms: 2_000 } })
    );
    expect(messages.halted).toEqual({ reason: 'repetition', since_ms: 2_000 });
  });

  it('rejects a non-object value', () => {
    expect(() => narrowChatMessages(null)).toThrow(BoundaryError);
  });

  it('rejects missing room_id', () => {
    const wire = chatMessagesWire();
    delete wire['room_id'];
    expect(() => narrowChatMessages(wire)).toThrow(BoundaryError);
  });

  it('rejects messages that is not an array', () => {
    expect(() => narrowChatMessages(chatMessagesWire({ messages: 'nope' }))).toThrow(BoundaryError);
  });

  it('rejects a message missing from_name', () => {
    const message = chatMessageWire();
    delete message['from_name'];
    expect(() => narrowChatMessages(chatMessagesWire({ messages: [message] }))).toThrow(
      BoundaryError
    );
  });

  it('rejects a message with an unrecognized from_kind', () => {
    expect(() =>
      narrowChatMessages(
        chatMessagesWire({ messages: [chatMessageWire({ from_kind: 'a_future_kind' })] })
      )
    ).toThrow(BoundaryError);
  });

  it('rejects a message whose mentions is not an array', () => {
    expect(() =>
      narrowChatMessages(chatMessagesWire({ messages: [chatMessageWire({ mentions: 'x' })] }))
    ).toThrow(BoundaryError);
  });

  it('rejects a message whose deliveries entry has an unrecognized state', () => {
    expect(() =>
      narrowChatMessages(
        chatMessagesWire({
          messages: [chatMessageWire({ deliveries: [{ window_id: 'w-b', state: 'not_a_state' }] })],
        })
      )
    ).toThrow(BoundaryError);
  });

  it('rejects a non-integer next_after_seq', () => {
    expect(() => narrowChatMessages(chatMessagesWire({ next_after_seq: 'one' }))).toThrow(
      BoundaryError
    );
  });

  it('rejects a non-boolean needs_user', () => {
    expect(() => narrowChatMessages(chatMessagesWire({ needs_user: 'yes' }))).toThrow(
      BoundaryError
    );
  });

  it('rejects roster that is not an array', () => {
    expect(() => narrowChatMessages(chatMessagesWire({ roster: 'nope' }))).toThrow(BoundaryError);
  });

  it('rejects a roster entry missing name', () => {
    expect(() => narrowChatMessages(chatMessagesWire({ roster: [{ window_id: 'w-a' }] }))).toThrow(
      BoundaryError
    );
  });
});

describe('narrowChatChanged', () => {
  it('accepts a well-formed reply', () => {
    const changed: ChatChanged = narrowChatChanged(chatChangedWire());
    expect(changed).toEqual({ room_id: 'main', seq: 3, needs_user: true });
  });

  it('rejects a non-object value', () => {
    expect(() => narrowChatChanged(null)).toThrow(BoundaryError);
  });

  it('rejects missing room_id', () => {
    const wire = chatChangedWire();
    delete wire['room_id'];
    expect(() => narrowChatChanged(wire)).toThrow(BoundaryError);
  });

  it('rejects a non-integer seq', () => {
    expect(() => narrowChatChanged(chatChangedWire({ seq: 'three' }))).toThrow(BoundaryError);
  });

  it('rejects a non-boolean needs_user', () => {
    expect(() => narrowChatChanged(chatChangedWire({ needs_user: 'yes' }))).toThrow(BoundaryError);
  });
});

describe('invokeSendChat', () => {
  it('calls send_chat with the body and room id', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire());

    await invokeSendChat('hello there', 'main');

    expect(invokeMock).toHaveBeenCalledWith('send_chat', { body: 'hello there', room_id: 'main' });
  });

  it('calls send_chat with an undefined room id when none is given', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire());

    await invokeSendChat('hello there');

    expect(invokeMock).toHaveBeenCalledWith('send_chat', {
      body: 'hello there',
      room_id: undefined,
    });
  });

  it('narrows the reply into a ChatPosted', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire({ seq: 9 }));

    const posted: ChatPosted = await invokeSendChat('hello there');

    expect(posted.seq).toBe(9);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire({ targets: 'nope' }));

    await expect(invokeSendChat('hello there')).rejects.toThrow(BoundaryError);
  });
});

describe('invokeBroadcastChat', () => {
  it('calls broadcast_chat with the body and room id', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire({ user: true }));

    await invokeBroadcastChat('hello everyone', 'main');

    expect(invokeMock).toHaveBeenCalledWith('broadcast_chat', {
      body: 'hello everyone',
      room_id: 'main',
    });
  });

  it('narrows the reply into a ChatPosted', async () => {
    invokeMock.mockResolvedValueOnce(chatPostedWire({ user: true }));

    const posted = await invokeBroadcastChat('hello everyone');

    expect(posted.user).toBe(true);
  });
});

describe('invokeReadChat', () => {
  it('calls read_chat with room id, after_seq and limit', async () => {
    invokeMock.mockResolvedValueOnce(chatMessagesWire());

    await invokeReadChat('main', 10, 25);

    expect(invokeMock).toHaveBeenCalledWith('read_chat', {
      room_id: 'main',
      after_seq: 10,
      limit: 25,
    });
  });

  it('calls read_chat with every argument omitted', async () => {
    invokeMock.mockResolvedValueOnce(chatMessagesWire());

    await invokeReadChat();

    expect(invokeMock).toHaveBeenCalledWith('read_chat', {
      room_id: undefined,
      after_seq: undefined,
      limit: undefined,
    });
  });

  it('narrows the reply into a ChatMessages, carrying the roster', async () => {
    invokeMock.mockResolvedValueOnce(chatMessagesWire());

    const messages: ChatMessages = await invokeReadChat();

    expect(messages.roster).toEqual([{ window_id: 'w-a', name: 'window a' }]);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(chatMessagesWire({ roster: 'nope' }));

    await expect(invokeReadChat()).rejects.toThrow(BoundaryError);
  });
});

describe('subscribeChatChanged', () => {
  it('subscribes to session://chat-changed', () => {
    expect(SESSION_EVENT.chatChanged).toBe('session://chat-changed');
  });

  it('delivers a well-formed payload to onChatChanged', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((name: string, callback: ListenCallback) => {
      expect(name).toBe(SESSION_EVENT.chatChanged);
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onChatChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribeChatChanged(onChatChanged, onBoundaryError);
    registered?.({ payload: chatChangedWire() });

    expect(onChatChanged).toHaveBeenCalledWith({ room_id: 'main', seq: 3, needs_user: true });
    expect(onBoundaryError).toHaveBeenCalledTimes(0);
  });

  it('reports a malformed payload through onBoundaryError instead of throwing', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((_name: string, callback: ListenCallback) => {
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onChatChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribeChatChanged(onChatChanged, onBoundaryError);
    const wire = chatChangedWire();
    delete wire['room_id'];

    expect(() => registered?.({ payload: wire })).not.toThrow();
    expect(onChatChanged).toHaveBeenCalledTimes(0);
    expect(onBoundaryError).toHaveBeenCalledTimes(1);
  });

  it('returns a function that cancels the subscription', async () => {
    listenMock.mockResolvedValueOnce(unlistenMock);

    const cancel = await subscribeChatChanged(vi.fn(), vi.fn());
    cancel();

    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});

describe('subscribeChatParticipantsChanged', () => {
  it('subscribes to session://chat-participants-changed, the name the core emits it under', () => {
    expect(SESSION_EVENT.chatParticipantsChanged).toBe('session://chat-participants-changed');
  });

  it('calls onChanged with no arguments for each push, which carries nothing to validate', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((name: string, callback: ListenCallback) => {
      expect(name).toBe(SESSION_EVENT.chatParticipantsChanged);
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onChanged = vi.fn<() => void>();
    const onBoundaryError = vi.fn<(error: unknown) => void>();

    await subscribeChatParticipantsChanged(onChanged, onBoundaryError);
    registered?.({ payload: null });
    registered?.({ payload: null });

    expect(onChanged).toHaveBeenCalledTimes(2);
    expect(onChanged).toHaveBeenNthCalledWith(1);
    expect(onChanged).toHaveBeenNthCalledWith(2);
    expect(onBoundaryError).toHaveBeenCalledTimes(0);
  });

  it('returns a function that cancels the subscription', async () => {
    listenMock.mockResolvedValueOnce(unlistenMock);

    const cancel = await subscribeChatParticipantsChanged(
      vi.fn<() => void>(),
      vi.fn<(error: unknown) => void>()
    );
    cancel();

    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------
// The room list. One row per room the daemon knows of, carried under `rooms`
// the way the daemon's own reply carries them, and read through the
// `list_chat_rooms` command, which takes no arguments.
// ---------------------------------------------------------------------------

/** One row of the room list, snake_case as the daemon serializes it. */
function chatRoomWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    room_id: 'main',
    name: 'main',
    participant_count: 2,
    needs_user: false,
    halted: null,
    last_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

/** A well-formed room-list reply, whose rows live under `rooms`. */
function chatRoomsWire(rooms: unknown = [chatRoomWire()]): Record<string, unknown> {
  return { rooms };
}

describe('narrowChatRooms', () => {
  it('accepts a well-formed reply and keeps the rows in the order they arrived', () => {
    const rooms: ChatRoomInfo[] = narrowChatRooms(
      chatRoomsWire([
        chatRoomWire(),
        chatRoomWire({
          room_id: 'r-2b7c1d',
          name: 'yagura',
          participant_count: 1,
          needs_user: true,
          last_at_ms: null,
        }),
      ])
    );

    expect(rooms).toEqual([
      {
        room_id: 'main',
        name: 'main',
        participant_count: 2,
        needs_user: false,
        halted: null,
        last_at_ms: 1_700_000_000_000,
      },
      {
        room_id: 'r-2b7c1d',
        name: 'yagura',
        participant_count: 1,
        needs_user: true,
        halted: null,
        last_at_ms: null,
      },
    ]);
  });

  it('accepts an empty room list', () => {
    expect(narrowChatRooms(chatRoomsWire([]))).toEqual([]);
  });

  it('keeps a present halt verbatim', () => {
    const [room] = narrowChatRooms(
      chatRoomsWire([chatRoomWire({ halted: { reason: 'monologue', since_ms: 2_000 } })])
    );
    expect(room?.halted).toEqual({ reason: 'monologue', since_ms: 2_000 });
  });

  it('folds an absent halted key to null, and still carries the key', () => {
    const wire = chatRoomWire();
    delete wire['halted'];
    const [room] = narrowChatRooms(chatRoomsWire([wire]));
    expect(room).toBeDefined();
    expect('halted' in (room ?? {})).toBe(true);
    expect(room?.halted).toBeNull();
  });

  it('folds an explicit null halted to null, and still carries the key', () => {
    const [room] = narrowChatRooms(chatRoomsWire([chatRoomWire({ halted: null })]));
    expect(room).toBeDefined();
    expect('halted' in (room ?? {})).toBe(true);
    expect(room?.halted).toBeNull();
  });

  it('rejects a non-object value', () => {
    expect(() => narrowChatRooms(null)).toThrow(BoundaryError);
    expect(() => narrowChatRooms('nope')).toThrow(BoundaryError);
  });

  it('rejects a bare array in place of the reply object', () => {
    expect(() => narrowChatRooms([chatRoomWire()])).toThrow(BoundaryError);
  });

  it('rejects a missing rooms field', () => {
    expect(() => narrowChatRooms({})).toThrow(BoundaryError);
  });

  it('rejects rooms that is not an array', () => {
    expect(() => narrowChatRooms(chatRoomsWire('nope'))).toThrow(BoundaryError);
  });

  it('rejects a row that is not an object', () => {
    expect(() => narrowChatRooms(chatRoomsWire(['main']))).toThrow(BoundaryError);
  });

  it('rejects a row missing room_id', () => {
    const wire = chatRoomWire();
    delete wire['room_id'];
    expect(() => narrowChatRooms(chatRoomsWire([wire]))).toThrow(BoundaryError);
  });

  it('rejects a row whose name is not a string', () => {
    expect(() => narrowChatRooms(chatRoomsWire([chatRoomWire({ name: 7 })]))).toThrow(
      BoundaryError
    );
  });

  it('accepts a row whose name is empty', () => {
    const [room] = narrowChatRooms(chatRoomsWire([chatRoomWire({ name: '' })]));
    expect(room?.name).toBe('');
  });

  it('rejects a row missing needs_user', () => {
    const wire = chatRoomWire();
    delete wire['needs_user'];
    expect(() => narrowChatRooms(chatRoomsWire([wire]))).toThrow(BoundaryError);
  });

  it('rejects a row whose needs_user is not a boolean', () => {
    expect(() => narrowChatRooms(chatRoomsWire([chatRoomWire({ needs_user: 'yes' })]))).toThrow(
      BoundaryError
    );
  });

  it('rejects a row missing participant_count', () => {
    const wire = chatRoomWire();
    delete wire['participant_count'];
    expect(() => narrowChatRooms(chatRoomsWire([wire]))).toThrow(BoundaryError);
  });

  it('rejects a negative participant_count', () => {
    expect(() => narrowChatRooms(chatRoomsWire([chatRoomWire({ participant_count: -1 })]))).toThrow(
      BoundaryError
    );
  });

  it('rejects a fractional participant_count', () => {
    expect(() =>
      narrowChatRooms(chatRoomsWire([chatRoomWire({ participant_count: 1.5 })]))
    ).toThrow(BoundaryError);
  });

  it('rejects a participant_count sent as a string', () => {
    expect(() =>
      narrowChatRooms(chatRoomsWire([chatRoomWire({ participant_count: '2' })]))
    ).toThrow(BoundaryError);
  });

  it('accepts a participant_count of zero', () => {
    const [room] = narrowChatRooms(chatRoomsWire([chatRoomWire({ participant_count: 0 })]));
    expect(room?.participant_count).toBe(0);
  });

  it('rejects a halt missing since_ms', () => {
    expect(() =>
      narrowChatRooms(chatRoomsWire([chatRoomWire({ halted: { reason: 'repetition' } })]))
    ).toThrow(BoundaryError);
  });

  it('rejects a halt that is not an object', () => {
    expect(() => narrowChatRooms(chatRoomsWire([chatRoomWire({ halted: 'repetition' })]))).toThrow(
      BoundaryError
    );
  });

  it('folds an absent last_at_ms to null', () => {
    const wire = chatRoomWire();
    delete wire['last_at_ms'];
    const [room] = narrowChatRooms(chatRoomsWire([wire]));
    expect(room?.last_at_ms).toBeNull();
  });

  it('rejects a last_at_ms that is not a number', () => {
    expect(() => narrowChatRooms(chatRoomsWire([chatRoomWire({ last_at_ms: 'recent' })]))).toThrow(
      BoundaryError
    );
  });
});

describe('invokeListChatRooms', () => {
  it('calls list_chat_rooms with no arguments', async () => {
    invokeMock.mockResolvedValueOnce(chatRoomsWire());

    await invokeListChatRooms();

    expect(invokeMock).toHaveBeenCalledWith('list_chat_rooms');
  });

  it('narrows the reply into the room rows', async () => {
    invokeMock.mockResolvedValueOnce(
      chatRoomsWire([chatRoomWire({ room_id: 'r-2b7c1d', name: 'yagura', needs_user: true })])
    );

    const rooms: ChatRoomInfo[] = await invokeListChatRooms();

    expect(rooms).toHaveLength(1);
    expect(rooms[0]?.room_id).toBe('r-2b7c1d');
    expect(rooms[0]?.needs_user).toBe(true);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(chatRoomsWire([chatRoomWire({ participant_count: -1 })]));

    await expect(invokeListChatRooms()).rejects.toThrow(BoundaryError);
  });
});
