/**
 * Which sidebar room a window sits in, and how a window is brought on screen.
 * Every jump to a window goes through here, so they all land the same way.
 */

import type { RoomGroup } from '../sidebar/room-groups';

/** The part of a window entry the lookup reads. */
export interface RoomMember {
  windowId: string;
}

/** What bringing a window on screen acts through; the view injects its own functions. */
export interface RevealDeps<T> {
  /** The sidebar's rooms as they are currently rendered, top to bottom. */
  groups: () => readonly RoomGroup<T>[];
  /** Opens a room without closing one that is already open. */
  expandRoom: (roomId: string) => void;
  /** Brings a window to the front, un-minimizing or switching tabs as needed. */
  focus: (entry: T) => void;
  /** Asks for keyboard focus to move into a window's terminal. */
  focusTerminal: (windowId: string) => void;
}

/**
 * The room holding `entry`, or null when no room in `groups` holds it. Windows
 * are matched by id, so an entry the registry has since replaced still resolves.
 */
export function roomOfWindow<T extends RoomMember>(
  groups: readonly RoomGroup<T>[],
  entry: T
): string | null {
  return (
    groups.find((group) => group.entries.some((member) => member.windowId === entry.windowId))
      ?.roomId ?? null
  );
}

/**
 * Brings `entry` on screen: opens its room, brings it to the front and moves
 * keyboard focus into its terminal. The rooms are read at the time of the call.
 */
export function revealWindow<T extends RoomMember>(deps: RevealDeps<T>, entry: T): void {
  // A window inside a collapsed room cannot be reached until the room opens.
  const roomId = roomOfWindow(deps.groups(), entry);
  if (roomId !== null) {
    deps.expandRoom(roomId);
  }
  deps.focus(entry);
  deps.focusTerminal(entry.windowId);
}
