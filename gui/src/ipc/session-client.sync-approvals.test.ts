/**
 * TS-boundary contract for `invokeSyncApprovals`, the WebView-initiated pull
 * of the pending approval list. The daemon-side round trip is
 * covered by the Tauri core's own tests; what is measured here is the command
 * this module invokes, and that every reply it hands on has been through
 * `narrowApprovalSyncOutcome`.
 *
 * A pull that is delivered as a typed outcome is what lets a refused approver
 * registration reach the WebView without depending on an event that may have
 * been emitted before the subscription opened.
 *
 * `@tauri-apps/api/core` is mocked, so every assertion is about the calls this
 * module makes. It lives in its own file rather than alongside the other
 * `session-client` cases so the reply shapes below are read in one place.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { BoundaryError, type ApprovalSyncOutcome } from './session-boundary';
import { invokeSyncApprovals } from './session-client';

/** One pending request as the daemon serializes it, snake_case throughout. */
function approvalRequestWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 'appr1',
    tool: 'close_window',
    requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
    target: { window_id: 'w-target', name: 'target window', status: 'running' },
    text: null,
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeSyncApprovals: the command it calls', () => {
  it('calls sync_approvals with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'registered', pending: [] });

    await invokeSyncApprovals();

    expect(invokeMock).toHaveBeenCalledWith('sync_approvals');
  });
});

describe('invokeSyncApprovals: a registered outcome', () => {
  it('narrows a registered reply into the pending list, preserving order', async () => {
    invokeMock.mockResolvedValueOnce({
      status: 'registered',
      pending: [
        approvalRequestWire({ request_id: 'appr1' }),
        approvalRequestWire({ request_id: 'appr2', tool: 'interrupt_window' }),
      ],
    });

    const outcome: ApprovalSyncOutcome = await invokeSyncApprovals();

    expect(outcome).toEqual({
      status: 'registered',
      pending: [
        {
          request_id: 'appr1',
          tool: 'close_window',
          requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
          target: { window_id: 'w-target', name: 'target window', status: 'running' },
          text: null,
        },
        {
          request_id: 'appr2',
          tool: 'interrupt_window',
          requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
          target: { window_id: 'w-target', name: 'target window', status: 'running' },
          text: null,
        },
      ],
    });
  });

  it('narrows a registered reply with nothing pending into an empty list', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'registered', pending: [] });

    const outcome = await invokeSyncApprovals();

    expect(outcome).toEqual({ status: 'registered', pending: [] });
  });
});

describe('invokeSyncApprovals: a refused outcome', () => {
  it('narrows a refused reply into the refused outcome, carrying no list', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'refused' });

    const outcome = await invokeSyncApprovals();

    expect(outcome).toEqual({ status: 'refused' });
  });
});

describe('invokeSyncApprovals: replies that violate the contract', () => {
  it('rejects an unrecognized status', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'unregistered' });

    await expect(invokeSyncApprovals()).rejects.toThrow(BoundaryError);
  });

  it('rejects a registered reply whose pending is not an array', async () => {
    invokeMock.mockResolvedValueOnce({ status: 'registered', pending: 'nope' });

    await expect(invokeSyncApprovals()).rejects.toThrow(BoundaryError);
  });

  it('rejects a registered reply whose pending entry has no request_id', async () => {
    const wire = approvalRequestWire();
    delete wire['request_id'];
    invokeMock.mockResolvedValueOnce({ status: 'registered', pending: [wire] });

    await expect(invokeSyncApprovals()).rejects.toThrow(BoundaryError);
  });

  it('rejects a null reply', async () => {
    invokeMock.mockResolvedValueOnce(null);

    await expect(invokeSyncApprovals()).rejects.toThrow(BoundaryError);
  });
});
