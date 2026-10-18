/**
 * What an approval request with no target window is addressed to, as the
 * approval dialog and the "expired unseen" banner both name it.
 */
import { sanitizeForDisplay, type ApprovalRequest } from '../ipc/session-boundary';
import { dialogs } from '../ui-messages';

/** The tools that post to a room, whose requests are addressed to no window. */
const CHAT_TOOLS: ReadonlySet<string> = new Set(['send_chat', 'broadcast_chat']);

/**
 * Names the addressee of a request that carries no target window: the room it
 * names, the chat room as a whole for a chat request that predates the `room`
 * field, or otherwise a window that is about to be opened.
 */
export function describeUntargeted(request: ApprovalRequest): string {
  if (request.room !== undefined) {
    return dialogs.describeApprovalRoom(sanitizeForDisplay(request.room.name));
  }
  // Every other untargeted request (open_window, a handoff that opens a
  // window from a profile) is approved before its window exists.
  return CHAT_TOOLS.has(request.tool)
    ? dialogs.approvalTargetRoom
    : dialogs.approvalTargetNewWindow;
}
