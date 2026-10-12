/**
 * What an approval request with no target window is addressed to, as the
 * approval dialog and the "expired unseen" banner both name it.
 */
import { sanitizeForDisplay, type ApprovalRequest } from '../ipc/session-boundary';
import { dialogs } from '../ui-messages';

/** The tool whose window does not exist yet, so its request names no target. */
const OPEN_WINDOW_TOOL = 'open_window';

/**
 * Names the addressee of a request that carries no target window: the room it
 * names, a window that is about to be opened, or -- for a chat request that
 * predates the `room` field -- the chat room as a whole.
 */
export function describeUntargeted(request: ApprovalRequest): string {
  if (request.room !== undefined) {
    return dialogs.describeApprovalRoom(sanitizeForDisplay(request.room.name));
  }
  return request.tool === OPEN_WINDOW_TOOL
    ? dialogs.approvalTargetNewWindow
    : dialogs.approvalTargetRoom;
}
