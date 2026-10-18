/**
 * Which addressee a request with no target window is shown with. Expected
 * wording is read from the i18n resource, so these tests pin which message
 * each request picks rather than the message itself.
 */
import { describe, it, expect } from 'vitest';

import { describeUntargeted } from './approval-target';
import type { ApprovalRequest } from '../ipc/session-boundary';
import { dialogs } from '../ui-messages';

function request(overrides: Partial<ApprovalRequest>): ApprovalRequest {
  return {
    request_id: 'a1-test',
    tool: 'send_chat',
    requester: { kind: 'window', window_id: 'w1', name: 'builder' },
    text: null,
    ...overrides,
  };
}

describe('describeUntargeted', () => {
  it('names the room a request carries', () => {
    const named = request({ room: { room_id: 'main', name: 'Main' } });

    expect(describeUntargeted(named)).toBe(dialogs.describeApprovalRoom('Main'));
  });

  it('names a new window for a request to open one', () => {
    const opening = request({ tool: 'open_window' });

    expect(describeUntargeted(opening)).toBe(dialogs.approvalTargetNewWindow);
  });

  it('names a new window for a handoff that opens one from a profile', () => {
    const opening = request({ tool: 'send_handoff', text: 'profile: claude-code\n\nenvelope' });

    expect(describeUntargeted(opening)).toBe(dialogs.approvalTargetNewWindow);
  });

  it.each(['send_chat', 'broadcast_chat'])(
    'falls back to the chat room as a whole for a %s request that names no room',
    (tool) => {
      expect(describeUntargeted(request({ tool }))).toBe(dialogs.approvalTargetRoom);
    }
  );
});
