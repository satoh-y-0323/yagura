/**
 * `Unknown` and `Exited` are asymmetric on purpose: the daemon's state gate
 * (`control.rs`) applies only to `interrupt`, so `close` stays the only way to
 * clean up a window that detection cannot classify.
 */
import { describe, it, expect } from 'vitest';

import {
  confirmCopy,
  resolveConfirmTarget,
  type ConfirmOperation,
  type WindowOperation,
} from './confirm-target';
import { buttons, chat, dialogs } from '../ui-messages';
import type { WindowStatus } from '../ipc/session-boundary';

const OPERATIONS: WindowOperation[] = ['close', 'interrupt'];

const ALWAYS_ALLOWED_STATUSES: WindowStatus[] = [
  'running',
  'waiting_input',
  'waiting_approval',
  'interrupted',
  'idle',
];

describe('resolveConfirmTarget: normal states', () => {
  describe.each(ALWAYS_ALLOWED_STATUSES)('status %s', (status) => {
    it.each(OPERATIONS)('allows %s with a confirmation dialog', (operation) => {
      expect(resolveConfirmTarget(status, operation)).toEqual({
        allowed: true,
        requiresConfirmation: true,
      });
    });
  });
});

describe('resolveConfirmTarget: status unknown', () => {
  it('allows close with a confirmation dialog (the only cleanup path for undetectable windows)', () => {
    expect(resolveConfirmTarget('unknown', 'close')).toEqual({
      allowed: true,
      requiresConfirmation: true,
    });
  });

  it('suppresses interrupt (the daemon state gate would reject it anyway), naming unknown as the reason', () => {
    expect(resolveConfirmTarget('unknown', 'interrupt')).toEqual({
      allowed: false,
      requiresConfirmation: false,
      reason: 'unknown',
    });
  });
});

describe('resolveConfirmTarget: status exited', () => {
  it('allows close with a confirmation dialog (lets a human clean up a stray job)', () => {
    expect(resolveConfirmTarget('exited', 'close')).toEqual({
      allowed: true,
      requiresConfirmation: true,
    });
  });

  it('suppresses interrupt (there is no child left to interrupt), naming exited as the reason', () => {
    expect(resolveConfirmTarget('exited', 'interrupt')).toEqual({
      allowed: false,
      requiresConfirmation: false,
      reason: 'exited',
    });
  });
});

/**
 * The blocked arm must carry a reason a caller
 * can branch on, distinct between `unknown` and `exited`, and the allowed
 * arm must not carry one at all -- a caller cannot read a reason for an
 * operation that was never refused.
 */
describe('resolveConfirmTarget: the blocked reason is distinguishable', () => {
  it('gives unknown and exited distinct reasons for a suppressed interrupt', () => {
    const unknownTarget = resolveConfirmTarget('unknown', 'interrupt');
    const exitedTarget = resolveConfirmTarget('exited', 'interrupt');
    if (unknownTarget.allowed || exitedTarget.allowed) {
      throw new Error('expected interrupt to be blocked for both unknown and exited');
    }
    expect(unknownTarget.reason).not.toBe(exitedTarget.reason);
  });

  it('does not carry a reason when close is allowed', () => {
    const target = resolveConfirmTarget('running', 'close');
    expect(target.allowed).toBe(true);
    expect('reason' in target).toBe(false);
  });

  it('does not carry a reason when interrupt is allowed for an actionable status', () => {
    const target = resolveConfirmTarget('running', 'interrupt');
    expect(target.allowed).toBe(true);
    expect('reason' in target).toBe(false);
  });
});

const CONFIRM_OPERATIONS: ConfirmOperation[] = [
  'close',
  'interrupt',
  'broadcast',
  'clear_room',
  'delete_room',
];

describe('confirmCopy: one wording source for the confirmation dialog', () => {
  it.each(CONFIRM_OPERATIONS)(
    'returns a non-empty title, message and label for %s',
    (operation) => {
      const copy = confirmCopy(operation);
      expect(copy.title.length).toBeGreaterThan(0);
      expect(copy.message.length).toBeGreaterThan(0);
      expect(copy.confirmLabel.length).toBeGreaterThan(0);
    }
  );

  it('gives each operation its own title', () => {
    const titles = CONFIRM_OPERATIONS.map((operation) => confirmCopy(operation).title);
    expect(new Set(titles).size).toBe(CONFIRM_OPERATIONS.length);
  });

  it('gives each operation its own message', () => {
    const bodies = CONFIRM_OPERATIONS.map((operation) => confirmCopy(operation).message);
    expect(new Set(bodies).size).toBe(CONFIRM_OPERATIONS.length);
  });

  it('gives each operation its own action label', () => {
    const labels = CONFIRM_OPERATIONS.map((operation) => confirmCopy(operation).confirmLabel);
    expect(new Set(labels).size).toBe(CONFIRM_OPERATIONS.length);
  });

  it('keeps the close wording the window buttons and dialogs already carry', () => {
    expect(confirmCopy('close')).toEqual({
      title: buttons.close,
      message: dialogs.closeConfirm,
      confirmLabel: buttons.close,
    });
  });

  it('keeps the interrupt wording the window buttons and dialogs already carry', () => {
    expect(confirmCopy('interrupt')).toEqual({
      title: buttons.interrupt,
      message: dialogs.interruptConfirm,
      confirmLabel: buttons.interrupt,
    });
  });

  it('takes the broadcast wording from the chat resources, not from buttons or dialogs', () => {
    expect(confirmCopy('broadcast')).toEqual({
      title: chat.broadcastConfirmTitle,
      message: chat.broadcastConfirmMessage,
      confirmLabel: chat.broadcastConfirmLabel,
    });
  });
});

describe('confirmCopy: the room operations name the room', () => {
  it('asks before clearing a room, in the wording the room menu was agreed with', () => {
    expect(confirmCopy('clear_room', 'yagura')).toEqual({
      title: 'Clear the history of “yagura”?',
      message:
        "Every message in this room is deleted, and messages a window has not received yet are taken back. This can't be undone.",
      confirmLabel: 'Clear history',
    });
  });

  it('asks before deleting a room, in the wording the room menu was agreed with', () => {
    expect(confirmCopy('delete_room', 'yagura_test')).toEqual({
      title: 'Delete the room “yagura_test”?',
      message:
        "The room and all its messages are deleted. If a window in this folder uses chat later, the room comes back empty. This can't be undone.",
      confirmLabel: 'Delete room',
    });
  });
});
