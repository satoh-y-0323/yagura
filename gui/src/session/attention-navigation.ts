/**
 * The sidebar's "go to the next window that needs you" jump. It lives here
 * rather than in `room-actions.ts`, whose operations are all defined over a
 * single room, while this one walks across rooms.
 *
 * The target is derived from the current state on every press, never from a
 * remembered position: a window stops needing the user the moment it is typed
 * into, so a cursor into the waiting windows would skip the next one.
 */

import type { DetectedStatusView } from '../ipc/session-boundary';
import { visibleWindowOrder, type NeedsYouView } from '../sidebar/needs-you-view';
import type { RoomGroup } from '../sidebar/room-groups';
import { windowsNeedingYou } from '../status-tone';
import { revealWindow, roomOfWindow, type RevealDeps } from './room-navigation';

/** The part of a window entry the jump reads. */
export interface NavigableWindow {
  windowId: string;
  detected: DetectedStatusView;
}

/** The window to jump to, with the room that has to be expanded to reach it. */
export interface AttentionTarget<T> {
  entry: T;
  roomId: string;
}

/** What the jump acts through; the view injects the store and the actions. */
export interface AttentionNavigationDeps<T extends NavigableWindow> extends RevealDeps<T> {
  /** The window the jump starts from: the current window of the work view. */
  current: () => { windowId: string } | null;
  /** The Needs you filter's view, `null` while it is off. */
  view: () => NeedsYouView | null;
}

/** The jump, bound to its dependencies. */
export interface AttentionNavigation<T> {
  /** Jumps to the next waiting window; returns it, or null when none waits. */
  goToNextWindowNeedingYou: () => T | null;
}

/**
 * The window the jump would go to next: the first window needing the user
 * below the current one in the list the sidebar shows ({@link visibleWindowOrder}:
 * the filter's rows while it is on), wrapping to the top of the list when the
 * current window sits below the last of them. The current
 * window itself is never the answer unless it is the only window waiting. With
 * no current window the search starts from the top of the list.
 *
 * Membership of "needs you" comes from {@link windowsNeedingYou}, so the jump
 * and the badge counting it can never disagree.
 *
 * @param groups the sidebar's rooms, in the order they are rendered.
 * @param current the current window of the work view; one absent from the
 * shown list starts the search from the top too.
 * @param view the Needs you filter's view, or `null` while it is off.
 * @returns the target and its room, or null when no window is waiting.
 */
export function nextWindowNeedingYou<T extends NavigableWindow>(
  groups: readonly RoomGroup<T>[],
  current: { windowId: string } | null,
  view: NeedsYouView | null = null
): AttentionTarget<T> | null {
  const onScreen = visibleWindowOrder(groups, view);
  const waiting = windowsNeedingYou(onScreen);
  const from =
    current === null ? -1 : onScreen.findIndex((entry) => entry.windowId === current.windowId);
  const next = waiting.find((entry) => onScreen.indexOf(entry) > from) ?? waiting[0];
  if (next === undefined) {
    return null;
  }
  const roomId = roomOfWindow(groups, next);
  return roomId === null ? null : { entry: next, roomId };
}

/** Builds an {@link AttentionNavigation} bound to `deps`. */
export function createAttentionNavigation<T extends NavigableWindow>(
  deps: AttentionNavigationDeps<T>
): AttentionNavigation<T> {
  return {
    goToNextWindowNeedingYou: () => {
      const target = nextWindowNeedingYou(deps.groups(), deps.current(), deps.view());
      if (target === null) {
        return null;
      }
      // Nothing else about the list moves: the room stays open afterwards, and
      // no other room is touched.
      revealWindow(deps, target.entry);
      return target.entry;
    },
  };
}
