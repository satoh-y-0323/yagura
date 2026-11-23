/**
 * The room rail's menu: whether a room may be deleted, why not when it may
 * not, and what the user is told when the daemon refuses a removal.
 */
import { describe, it, expect } from 'vitest';

import {
  describeRoomRemovalFailure,
  readRefusedForRemovedRoom,
  roomDeleteNote,
} from './room-actions';
import { formatPathForDisplay } from '../display/untrusted-text';
import { CommandFailureError, type ChatRoomInfo } from '../ipc/session-boundary';
import { banners } from '../ui-messages';

const MAIN_NOTE = "The main room can't be deleted.";
const WINDOWS_NOTE = "Close this room's windows first.";

function room(overrides: Partial<ChatRoomInfo> = {}): ChatRoomInfo {
  return {
    room_id: 'r-a',
    name: 'yagura',
    displayName: formatPathForDisplay('yagura'),
    participant_count: 0,
    needs_user: false,
    halted: null,
    last_at_ms: null,
    has_open_windows: false,
    ...overrides,
  };
}

describe('roomDeleteNote', () => {
  it('allows deleting a workspace room no window is open in', () => {
    expect(roomDeleteNote(room())).toBeNull();
  });

  it('refuses main, whatever its windows', () => {
    expect(roomDeleteNote(room({ room_id: 'main', name: 'main' }))).toBe(MAIN_NOTE);
    expect(roomDeleteNote(room({ room_id: 'main', has_open_windows: true }))).toBe(MAIN_NOTE);
  });

  it('refuses a room a window is open in', () => {
    expect(roomDeleteNote(room({ has_open_windows: true }))).toBe(WINDOWS_NOTE);
  });

  it('refuses a row that does not say whether windows are open', () => {
    const row = room();
    delete row.has_open_windows;
    expect(roomDeleteNote(row)).toBe(WINDOWS_NOTE);
  });
});

/** A request the daemon refused with `code`, the way the session client rejects it. */
function rejected(code: string, reason = ''): CommandFailureError {
  return new CommandFailureError({
    kind: 'rejected',
    code,
    message: reason,
    rejectedArgument: null,
    text: `the daemon rejected the request: ${code}${reason === '' ? '' : `: ${reason}`}`,
  });
}

describe('describeRoomRemovalFailure', () => {
  it('tells the user to close the windows when the daemon says the room is in use', () => {
    const text = describeRoomRemovalFailure(
      rejected('room_in_use', 'a window is open in this room')
    );
    expect(text).toContain(WINDOWS_NOTE);
    expect(text).not.toContain('room_in_use');
  });

  it('reads the refusal whether or not the daemon gave a reason', () => {
    expect(describeRoomRemovalFailure(rejected('room_in_use'))).toContain(WINDOWS_NOTE);
  });

  it('passes any other failure through as the operation failure it is', () => {
    const text = describeRoomRemovalFailure(rejected('invalid_argument', 'unknown room'));
    expect(text).toContain('unknown room');
    expect(text).not.toContain(WINDOWS_NOTE);
  });

  it('does not take a mention of the code elsewhere in the failure for the refusal', () => {
    expect(describeRoomRemovalFailure(rejected('internal', 'room_in_use'))).not.toContain(
      WINDOWS_NOTE
    );
  });

  it('shows the incompatible-daemon sentence alone for a gate refusal', () => {
    const text = describeRoomRemovalFailure(
      new CommandFailureError({
        kind: 'unsupported',
        requestType: 'delete_chat_room',
        text: 'not sent, as the connected daemon speaks another wire revision: delete_chat_room',
      })
    );
    expect(text).toBe(banners.daemonIncompatible);
  });
});

describe('readRefusedForRemovedRoom', () => {
  const refused = (): CommandFailureError => rejected('invalid_argument', 'unknown room');
  const rooms = (...ids: string[]): Map<string, ChatRoomInfo> =>
    new Map(ids.map((id) => [id, room({ room_id: id })]));

  it('reads a refusal of a room the listing no longer has as that room being removed', () => {
    expect(readRefusedForRemovedRoom(refused(), 'r-gone', rooms('main', 'r-other'))).toBe(true);
  });

  it('keeps a refusal of a room still listed a failure to report', () => {
    expect(readRefusedForRemovedRoom(refused(), 'r-a', rooms('main', 'r-a'))).toBe(false);
  });

  it('never reads main as removed', () => {
    expect(readRefusedForRemovedRoom(refused(), 'main', rooms('r-a'))).toBe(false);
  });

  it('keeps any other failure a failure to report', () => {
    expect(readRefusedForRemovedRoom(rejected('internal'), 'r-gone', rooms('main'))).toBe(false);
    expect(readRefusedForRemovedRoom(new Error('connection lost'), 'r-gone', rooms('main'))).toBe(
      false
    );
  });
});
