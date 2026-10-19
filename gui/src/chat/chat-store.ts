/**
 * The chat panel's read model: folding one `read_chat` page into what the
 * panel already holds, and deciding from a `chat_changed` push whether
 * another read is needed.
 *
 * Pure, so the merge order and the cursor arithmetic are settled in one place
 * rather than inside a component.
 */

import {
  MAIN_CHAT_ROOM_ID,
  type ChatChanged,
  type ChatHalt,
  type ChatMessage,
  type ChatMessages,
  type ChatParticipant,
  type ChatRoomInfo,
} from '../ipc/session-boundary';

/** What the panel holds about one room. */
export interface ChatRoomState {
  /** The room these messages belong to. */
  roomId: string;
  /** The messages held, ascending by `seq`. */
  messages: ChatMessage[];
  /** The cursor to pass as `after_seq` on the next read. */
  nextAfterSeq: number;
  /** The room's halt, `null` while it is not halted. */
  halted: ChatHalt | null;
  /** Whether the room needs the user's attention. */
  needsUser: boolean;
  /** The room's participants as of the last page read. */
  roster: ChatParticipant[];
}

/**
 * The state of a room nothing has been read from yet.
 *
 * @param roomId the room the panel is showing
 */
export function emptyChatRoom(roomId: string): ChatRoomState {
  return {
    roomId,
    messages: [],
    nextAfterSeq: 0,
    halted: null,
    needsUser: false,
    roster: [],
  };
}

/**
 * Folds one page into `state` and returns the result; `state` is left as it
 * was.
 *
 * A page for another room replaces what was held, so two rooms never mix. A
 * page for the same room is merged by `seq` with the page winning, since a
 * re-read is how a delivery state moves on, and the cursor only ever moves
 * forward.
 *
 * @param state what the panel holds now
 * @param page the page the daemon answered with
 */
export function mergeChatMessages(state: ChatRoomState, page: ChatMessages): ChatRoomState {
  const sameRoom = page.room_id === state.roomId;
  const bySeq = new Map<number, ChatMessage>();
  if (sameRoom) {
    for (const message of state.messages) {
      bySeq.set(message.seq, message);
    }
  }
  for (const message of page.messages) {
    bySeq.set(message.seq, message);
  }
  return {
    roomId: page.room_id,
    messages: [...bySeq.values()].sort((left, right) => left.seq - right.seq),
    nextAfterSeq: sameRoom
      ? Math.max(state.nextAfterSeq, page.next_after_seq)
      : page.next_after_seq,
    halted: page.halted,
    needsUser: page.needs_user,
    roster: page.roster,
  };
}

/**
 * The display name to show for `windowId`: the roster's, the most recent
 * held message's if the roster no longer carries it, and the id itself if
 * neither names it.
 *
 * A delivery carries only a window id, so a destination that has since
 * closed has left the roster and can only be named from what it said while
 * it was there.
 *
 * @param state what the panel holds now
 * @param windowId the window to name
 */
export function resolveDisplayName(state: ChatRoomState, windowId: string): string {
  const participant = state.roster.find((entry) => entry.window_id === windowId);
  if (participant !== undefined) {
    return participant.name;
  }
  const latest = [...state.messages]
    .reverse()
    .find((message) => message.from_window_id === windowId);
  return latest?.from_name ?? windowId;
}

/**
 * Where to re-read from after a `chat_changed` push, or `null` when the
 * push needs no read.
 *
 * A push naming a seq past the cursor extends the held range from the
 * cursor. A push naming a seq the panel still holds (at or behind the
 * cursor, at or after the oldest held row) re-reads that one row, so
 * `mergeChatMessages` replaces it in place -- this is how a delivery state
 * settling on an already-read message reaches the panel. A push older than
 * everything held cannot be re-read as a single row (the row is gone), but
 * still moves the attention flag when that flag differs from what the panel
 * holds, by reading from the cursor onward.
 *
 * @param state what the panel holds now
 * @param changed the push the daemon sent
 */
export function chatReloadAfterSeq(state: ChatRoomState, changed: ChatChanged): number | null {
  if (state.roomId === '' || changed.room_id !== state.roomId) {
    return null;
  }
  if (changed.seq > state.nextAfterSeq) {
    return state.nextAfterSeq;
  }
  const oldestHeldSeq = state.messages[0]?.seq ?? Number.POSITIVE_INFINITY;
  if (changed.seq >= oldestHeldSeq) {
    return changed.seq - 1;
  }
  if (changed.needs_user !== state.needsUser) {
    return state.nextAfterSeq;
  }
  return null;
}

/**
 * Keys `list_chat_rooms`'s rows by `room_id`. The wire's order (`main`
 * first, then the rest by recency, decided by the daemon) is preserved as
 * the map's insertion order, so a caller that reads `[...map.values()]`
 * gets that order back without re-sorting.
 *
 * A fresh call fully replaces what an earlier one produced: the panel holds
 * the returned map itself, and this function does not merge into it.
 *
 * @param rooms the rows the daemon answered with
 */
export function applyRoomsList(rooms: ChatRoomInfo[]): Map<string, ChatRoomInfo> {
  const next = new Map<string, ChatRoomInfo>();
  for (const room of rooms) {
    next.set(room.room_id, room);
  }
  return next;
}

/**
 * Whether any room in `rooms` currently needs the user's attention -- the
 * value the app rail's aggregate chat badge is driven from.
 *
 * @param rooms the room map held now
 */
export function anyNeedsUser(rooms: Map<string, ChatRoomInfo>): boolean {
  for (const room of rooms.values()) {
    if (room.needs_user) {
      return true;
    }
  }
  return false;
}

/**
 * Folds one `chat_changed` push into `rooms` while the panel is closed: a
 * badge-only update, since no page is read here.
 *
 * A room `rooms` already lists keeps every field but `needs_user`, which
 * takes the push's value. A room `rooms` has never listed gets a minimal
 * placeholder row -- empty name, so the room rail does not draw it until a
 * full `list_chat_rooms` read names it -- with no `halted` key at all, since
 * the push carries nothing to set it from.
 *
 * @param rooms the room map held now
 * @param changed the push the daemon sent
 */
export function applyChatChangedToRooms(
  rooms: Map<string, ChatRoomInfo>,
  changed: ChatChanged
): Map<string, ChatRoomInfo> {
  const next = new Map(rooms);
  const existing = next.get(changed.room_id);
  if (existing !== undefined) {
    next.set(changed.room_id, { ...existing, needs_user: changed.needs_user });
  } else {
    next.set(changed.room_id, {
      room_id: changed.room_id,
      name: '',
      participant_count: 0,
      needs_user: changed.needs_user,
      last_at_ms: null,
    });
  }
  return next;
}

/**
 * Everything the panel has read, room by room, with each room's generation.
 * A room moves to a new generation when its history is cleared, so a read
 * sent before that can be told apart from one sent after.
 */
export interface ChatHistory {
  /** What each room's page reads have merged in. */
  states: Map<string, ChatRoomState>;
  /** Each room's generation; a room missing here is at generation 0. */
  generations: Map<string, number>;
}

/**
 * The generation `roomId` is at now; pass it to {@link foldChatPage} with
 * the page read under it.
 *
 * @param generations the generations held now
 * @param roomId the room about to be read
 */
export function chatRoomGeneration(
  generations: ReadonlyMap<string, number>,
  roomId: string
): number {
  return generations.get(roomId) ?? 0;
}

/**
 * Drops what the panel holds for a room whose history was cleared or which
 * was deleted, and moves the room to a new generation. `history` is left as
 * it was.
 *
 * @param history what the panel holds now
 * @param roomId the room the daemon cleared
 */
export function applyChatHistoryCleared(history: ChatHistory, roomId: string): ChatHistory {
  const states = new Map(history.states);
  states.delete(roomId);
  const generations = new Map(history.generations);
  generations.set(roomId, chatRoomGeneration(history.generations, roomId) + 1);
  return { states, generations };
}

/**
 * Folds a page read for `roomId` into the held rooms and returns them. A
 * page read under an older generation predates a clear, so it would bring
 * cleared messages back; it is dropped and the rooms are returned unchanged.
 *
 * @param history what the panel holds now, not when the read was sent
 * @param roomId the room the read was sent for
 * @param issuedGeneration the room's generation when the read was sent
 * @param page the page the daemon answered with
 */
export function foldChatPage(
  history: ChatHistory,
  roomId: string,
  issuedGeneration: number,
  page: ChatMessages
): Map<string, ChatRoomState> {
  const states = new Map(history.states);
  if (issuedGeneration !== chatRoomGeneration(history.generations, roomId)) {
    return states;
  }
  states.set(roomId, mergeChatMessages(states.get(roomId) ?? emptyChatRoom(roomId), page));
  return states;
}

/**
 * The room to show once the room list was read again after a removal: the
 * shown room while it is still listed, `main` once it is not, and no room
 * while none was shown.
 *
 * @param currentRoomId the room shown now, `''` for none
 * @param rooms the room list just read
 */
export function roomAfterRemoval(
  currentRoomId: string,
  rooms: ReadonlyMap<string, ChatRoomInfo>
): string {
  if (currentRoomId === '' || rooms.has(currentRoomId)) {
    return currentRoomId;
  }
  return MAIN_CHAT_ROOM_ID;
}
