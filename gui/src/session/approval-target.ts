/**
 * What an approval request is addressed to, as the approval dialog and the
 * "expired unseen" notice both name it.
 */
import {
  formatPathForDisplay,
  sanitizeForDisplay,
  type DisplayLine,
} from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import type { ApprovalRequest } from '../ipc/session-boundary';
import { describeStatus, dialogs } from '../ui-messages';

/** The tools that post to a room, whose requests are addressed to no window. */
const CHAT_TOOLS: ReadonlySet<string> = new Set(['send_chat', 'broadcast_chat']);

/**
 * Names the addressee of a request that carries no target window: the room it
 * names, the chat room as a whole for a chat request that predates the `room`
 * field, or otherwise a window that is about to be opened.
 */
export function describeUntargeted(request: ApprovalRequest): UiText {
  if (request.room !== undefined) {
    return dialogs.describeApprovalRoom(formatPathForDisplay(request.room.name));
  }
  // Every other untargeted request (open_window, a handoff that opens a
  // window from a profile) is approved before its window exists.
  return CHAT_TOOLS.has(request.tool)
    ? dialogs.approvalTargetRoom
    : dialogs.approvalTargetNewWindow;
}

/** Names the operation a request would run, by its external name. */
export function approvalToolName(request: ApprovalRequest): DisplayLine {
  return sanitizeForDisplay(request.tool);
}

/**
 * Names who asked: the requesting window, marked closed once it has left the
 * window list, or an external client.
 *
 * @param request the request shown
 * @param isRequesterOpen whether the requesting window is still in the window list
 */
export function describeApprovalRequester(
  request: ApprovalRequest,
  isRequesterOpen: boolean
): UiText | DisplayLine {
  const { kind, name } = request.requester;
  if (kind !== 'window') {
    return dialogs.approvalRequesterExternal;
  }
  // The boundary names every window requester; an empty name stands in if one ever lacks it.
  const shown = name ?? sanitizeForDisplay('');
  return isRequesterOpen ? shown : dialogs.approvalRequesterGone(shown);
}

/** Names what a request would act on: its target window, or what {@link describeUntargeted} names. */
export function approvalTargetName(request: ApprovalRequest): UiText | DisplayLine {
  return request.target?.name ?? describeUntargeted(request);
}

/**
 * Names what a request would act on for the approval dialog: its target
 * window with the status it had when the request was submitted, or what
 * {@link describeUntargeted} names.
 */
export function describeApprovalTarget(request: ApprovalRequest): UiText {
  const target = request.target;
  return target === undefined
    ? describeUntargeted(request)
    : dialogs.approvalTargetWindow(target.name, describeStatus(target.status));
}
