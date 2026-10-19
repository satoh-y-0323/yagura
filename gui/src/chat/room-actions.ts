/**
 * The room rail's menu: whether a room may be deleted, and what the user is
 * told when the daemon refuses to remove one.
 */
import { daemonRejectionCode } from '../ipc/daemon-rejection';
import { MAIN_CHAT_ROOM_ID, ROOM_IN_USE_CODE, type ChatRoomInfo } from '../ipc/session-boundary';
import { chat, describeOperationFailure } from '../ui-messages';

const INVALID_ARGUMENT_CODE = 'invalid_argument';

/**
 * Why `room` may not be deleted, as the note the menu shows under the item;
 * `null` when it may. A row that does not say whether windows are open is
 * treated as having some.
 *
 * @param room the row the menu was opened on
 */
export function roomDeleteNote(room: ChatRoomInfo): string | null {
  if (room.room_id === MAIN_CHAT_ROOM_ID) {
    return chat.deleteMainRoomNote;
  }
  if (room.has_open_windows !== false) {
    return chat.deleteRoomWithWindowsNote;
  }
  return null;
}

/**
 * Whether a read of `roomId` refused as `invalid_argument` was refused because
 * the room is gone: the listing read after the refusal no longer has it. A
 * room the panel shows can be deleted while a read of it is under way.
 *
 * @param error the value the read rejected with
 * @param roomId the room that was read
 * @param rooms the room listing read after the refusal
 */
export function readRefusedForRemovedRoom(
  error: unknown,
  roomId: string,
  rooms: ReadonlyMap<string, ChatRoomInfo>
): boolean {
  return (
    daemonRejectionCode(error) === INVALID_ARGUMENT_CODE &&
    roomId !== MAIN_CHAT_ROOM_ID &&
    !rooms.has(roomId)
  );
}

/**
 * The notice for a clear or delete that failed. A room a window opened in
 * after the menu was read is refused as `room_in_use`, and is told in the
 * same words as the menu's note.
 *
 * @param error the value the command rejected with
 */
export function describeRoomRemovalFailure(error: unknown): string {
  const detail =
    daemonRejectionCode(error) === ROOM_IN_USE_CODE ? chat.deleteRoomWithWindowsNote : error;
  return describeOperationFailure(chat.panelTitle, detail);
}
