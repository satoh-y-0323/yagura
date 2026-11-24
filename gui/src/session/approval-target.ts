/**
 * Who asked an approval request and what it would act on, as the approval
 * dialog draws it and the "expired unseen" notice names it.
 */
import { formatPathForDisplay } from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import type { ApprovalRequest, ApprovalRequester } from '../ipc/session-boundary';
import { describeStatus, dialogs, settings } from '../ui-messages';
import type { ApprovalFacts, ApprovalParty, HeadlineParts } from './approval-types';

type KnownApprovalTool = keyof typeof settings.approvalToolInfo;

const KNOWN_APPROVAL_TOOLS: ReadonlySet<string> = new Set(Object.keys(settings.approvalToolInfo));

/** Which heading each gated tool's body takes. */
const BODY_HEADING_OF = {
  open_window: 'command',
  send_chat: 'message',
  broadcast_chat: 'message',
  send_message: 'message',
  send_handoff: 'handoff',
  interrupt_window: null,
  close_window: null,
} as const satisfies Record<KnownApprovalTool, keyof typeof dialogs.approvalBodyHeadings | null>;

/** The tools that post to a room, whose requests are addressed to no window. */
const CHAT_TOOLS: ReadonlySet<string> = new Set(['send_chat', 'broadcast_chat']);

function knownTool(tool: string): KnownApprovalTool | null {
  return KNOWN_APPROVAL_TOOLS.has(tool) ? (tool as KnownApprovalTool) : null;
}

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

/**
 * The facts the approval dialog draws a request from: the heading, who asked,
 * what it would act on and how its body is headed.
 *
 * @param request the request shown
 * @param isRequesterOpen whether the requesting window is still in the window list
 */
export function approvalFactsOf(request: ApprovalRequest, isRequesterOpen: boolean): ApprovalFacts {
  const tool = knownTool(request.tool);
  const bodyHeading = tool === null ? 'details' : BODY_HEADING_OF[tool];
  return {
    headline: headlineOf(request, tool),
    tool: request.displayTool,
    from: requesterParty(request.requester, isRequesterOpen),
    to: targetParty(request),
    bodyHeading: bodyHeading === null ? null : dialogs.approvalBodyHeadings[bodyHeading],
  };
}

function headlineOf(request: ApprovalRequest, tool: KnownApprovalTool | null): HeadlineParts {
  const from = request.requester.kind === 'window' ? request.requester.name : null;
  const target = request.target;
  if (tool === null) {
    return dialogs.approvalHeadlineOther(
      from,
      request.displayTool,
      target === undefined ? null : { name: target.name }
    );
  }
  const to = target === undefined ? { words: describeUntargeted(request) } : { name: target.name };
  return dialogs.approvalHeadlines[tool](from, to);
}

function requesterParty(requester: ApprovalRequester, isRequesterOpen: boolean): ApprovalParty {
  if (requester.kind !== 'window') {
    return {
      kind: 'external',
      label: dialogs.approvalRequesterExternal,
      profile: null,
      status: null,
      windowId: null,
      mark: null,
    };
  }
  return {
    kind: 'window',
    label: requester.name,
    profile: requester.profile,
    status: isRequesterOpen ? describeStatus(requester.status) : dialogs.approvalRequesterClosed,
    windowId: requester.displayWindowId,
    mark: requester.mark,
  };
}

function targetParty(request: ApprovalRequest): ApprovalParty {
  const target = request.target;
  if (target === undefined) {
    return {
      kind: 'untargeted',
      label: describeUntargeted(request),
      profile: null,
      status: null,
      windowId: null,
      mark: null,
    };
  }
  return {
    kind: 'window',
    label: target.name,
    profile: target.profile,
    status: describeStatus(target.status),
    windowId: target.displayWindowId,
    mark: target.mark,
  };
}
