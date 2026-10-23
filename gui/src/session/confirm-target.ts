/**
 * Whether a destructive window operation may be offered, and whether it needs
 * a confirmation first.
 *
 * A pure function so the same decision can be made in a dialog, in a menu and
 * in a test without any of them re-implementing the matrix.
 */

import type { WindowStatus } from '../ipc/session-boundary';
import { buttons, chat, dialogs } from '../ui-messages';

/** A destructive operation on a window. */
export type WindowOperation = 'close' | 'interrupt';

/** A destructive operation on a chat room. */
export type RoomOperation = 'clear_room' | 'delete_room';

/** An operation the confirmation dialog can be raised for. */
export type ConfirmOperation = WindowOperation | 'broadcast' | RoomOperation;

/** What the confirmation dialog shows for one operation. */
export interface ConfirmCopy {
  /** The dialog heading. */
  title: string;
  /** The question the dialog asks. */
  message: string;
  /** The label of the button that carries the operation out. */
  confirmLabel: string;
}

/**
 * The wording of the confirmation dialog for `operation`.
 *
 * One source for every operation, so the dialog itself carries no
 * per-operation branch.
 *
 * @param operation the operation being confirmed
 * @param roomName the room a room operation acts on; the other operations ignore it
 */
export function confirmCopy(operation: ConfirmOperation, roomName = ''): ConfirmCopy {
  switch (operation) {
    case 'close':
      return { title: buttons.close, message: dialogs.closeConfirm, confirmLabel: buttons.close };
    case 'interrupt':
      return {
        title: buttons.interrupt,
        message: dialogs.interruptConfirm,
        confirmLabel: buttons.interrupt,
      };
    case 'broadcast':
      return {
        title: chat.broadcastConfirmTitle,
        message: chat.broadcastConfirmMessage,
        confirmLabel: chat.broadcastConfirmLabel,
      };
    case 'clear_room':
      return {
        title: chat.clearHistoryConfirmTitle(roomName),
        message: chat.clearHistoryConfirmMessage,
        confirmLabel: chat.clearHistoryConfirmLabel,
      };
    case 'delete_room':
      return {
        title: chat.deleteRoomConfirmTitle(roomName),
        message: chat.deleteRoomConfirmMessage,
        confirmLabel: chat.deleteRoomConfirmLabel,
      };
    default: {
      const unreachable: never = operation;
      return unreachable;
    }
  }
}

/** Why `interrupt` may not be offered for a window. */
export type InterruptBlockReason = 'exited' | 'unknown';

/**
 * Whether an operation may be offered, and whether it needs a confirmation.
 *
 * The reason lives on the refused arm alone, so no caller can read a reason
 * for an operation that was never refused -- and none has to re-derive it
 * from the status, which would make a second gate out of the same decision.
 */
export type ConfirmTarget =
  | {
      /** The operation may be offered. */
      allowed: true;
      /** Whether the user has to confirm before it runs. */
      requiresConfirmation: boolean;
    }
  | {
      /** The operation may not be offered. */
      allowed: false;
      /** Whether the user has to confirm before it runs. */
      requiresConfirmation: boolean;
      /** Why it may not be offered. */
      reason: InterruptBlockReason;
    };

/**
 * Resolves how a destructive operation must be presented for a window in
 * `status`.
 *
 * `interrupt` is suppressed in `unknown` and `exited`: the daemon's state gate
 * rejects it there anyway. `close` stays available in both, because it is the
 * only way to clean up a window whose state detection cannot classify it.
 */
export function resolveConfirmTarget(
  status: WindowStatus,
  operation: WindowOperation
): ConfirmTarget {
  if (operation === 'interrupt' && !isActionable(status)) {
    return { allowed: false, requiresConfirmation: false, reason: status };
  }
  return { allowed: true, requiresConfirmation: true };
}

/**
 * An unknown window is refused by the daemon's status gate, and an exited one
 * has nothing left to interrupt. The predicate names the statuses it excludes,
 * so the refused arm takes its reason from the status itself.
 */
function isActionable(status: WindowStatus): status is Exclude<WindowStatus, InterruptBlockReason> {
  return status !== 'unknown' && status !== 'exited';
}
