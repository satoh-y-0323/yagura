/**
 * Whether a destructive window operation may be offered, and whether it needs
 * a confirmation first.
 *
 * A pure function so the same decision can be made in a dialog, in a menu and
 * in a test without any of them re-implementing the matrix.
 */

import type { UiText } from '../display/ui-text';
import type { DisplayLine } from '../display/untrusted-text';
import type { WindowStatus } from '../ipc/session-boundary';
import { buttons, chat, dialogs } from '../ui-messages';

/** A destructive operation on a window. */
export type WindowOperation = 'close' | 'interrupt';

/** A destructive operation on a chat room. */
export type RoomOperation = 'clear_room' | 'delete_room';

/** An operation the confirmation dialog can be raised for. */
export type ConfirmOperation = WindowOperation | 'broadcast' | RoomOperation;

/** An operation to confirm, with the room it acts on when it is a room operation. */
export type ConfirmSubject =
  | { operation: Exclude<ConfirmOperation, RoomOperation> }
  | { operation: RoomOperation; roomName: DisplayLine };

/** What the confirmation dialog shows for one operation. */
export interface ConfirmCopy {
  /** The dialog heading. */
  title: UiText;
  /** The question the dialog asks. */
  message: UiText;
  /** The label of the button that carries the operation out. */
  confirmLabel: UiText;
}

/**
 * The wording of the confirmation dialog for an operation.
 *
 * One source for every operation, so the dialog itself carries no
 * per-operation branch.
 *
 * @param subject the operation being confirmed, and the room a room operation acts on
 */
export function confirmCopy(subject: ConfirmSubject): ConfirmCopy {
  if (subject.operation === 'clear_room' || subject.operation === 'delete_room') {
    return roomConfirmCopy(subject.operation, subject.roomName);
  }
  const operation = subject.operation;
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
    default: {
      const unreachable: never = operation;
      return unreachable;
    }
  }
}

function roomConfirmCopy(operation: RoomOperation, roomName: DisplayLine): ConfirmCopy {
  return operation === 'clear_room'
    ? {
        title: chat.clearHistoryConfirmTitle(roomName),
        message: chat.clearHistoryConfirmMessage,
        confirmLabel: chat.clearHistoryConfirmLabel,
      }
    : {
        title: chat.deleteRoomConfirmTitle(roomName),
        message: chat.deleteRoomConfirmMessage,
        confirmLabel: chat.deleteRoomConfirmLabel,
      };
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
