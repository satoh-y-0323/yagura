/**
 * TS-boundary contract for the ticket bridge: the three read-only Tauri
 * commands the WebView calls (`list_ticket_workspaces`, `list_tickets`,
 * `get_ticket`) and the subscription to `session://ticket-changed`
 * (`SESSION_EVENT.ticketChanged`).
 *
 * `@tauri-apps/api/core` and `@tauri-apps/api/event` are mocked, so every
 * assertion is about the calls this module makes and the shape it produces.
 * Same arrangement as `plan-client.test.ts`.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock, listenMock, unlistenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  unlistenMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

import { BoundaryError, SESSION_EVENT } from './session-boundary';
import {
  invokeGetTicket,
  invokeListTicketWorkspaces,
  invokeListTickets,
  subscribeTicketChanged,
} from './ticket-client';

/** A Tauri event callback, as registered through the mocked `listen`. */
type ListenCallback = (event: { payload: unknown }) => void;

function listWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    workspace_id: 4,
    config: { guide: 'g', states: [], fields: [] },
    milestones: [],
    tickets: [
      { number: 3, title: 'A ticket', state: 'todo', milestone: null, updated_at: 1_790_000_000 },
    ],
    truncated: false,
    ...overrides,
  };
}

function detailWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    number: 3,
    title: 'A ticket',
    body: 'text',
    state: 'todo',
    fields: {},
    results: [],
    plans: [],
    created_at: 1,
    updated_at: 2,
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  unlistenMock.mockReset();
});

describe('invokeListTicketWorkspaces', () => {
  it('calls list_ticket_workspaces with no arguments', async () => {
    invokeMock.mockResolvedValueOnce({ workspaces: [] });

    await invokeListTicketWorkspaces();

    expect(invokeMock).toHaveBeenCalledWith('list_ticket_workspaces');
  });

  it('narrows the reply into the workspace rows', async () => {
    invokeMock.mockResolvedValueOnce({
      workspaces: [{ workspace_id: 4, kind: 'git', path: 'C:\\repo' }],
    });

    const rows = await invokeListTicketWorkspaces();

    expect(rows).toEqual([{ workspace_id: 4, kind: 'git', path: 'C:\\repo' }]);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce({
      workspaces: [{ workspace_id: 'x', kind: 'git', path: 'p' }],
    });

    await expect(invokeListTicketWorkspaces()).rejects.toThrow(BoundaryError);
  });
});

describe('invokeListTickets', () => {
  it('calls list_tickets with the workspace and every filter it was given', async () => {
    invokeMock.mockResolvedValueOnce(listWire());

    await invokeListTickets({ workspaceId: 4, state: 'todo', milestone: 'v1', limit: 50 });

    expect(invokeMock).toHaveBeenCalledWith('list_tickets', {
      workspace_id: 4,
      state: 'todo',
      milestone: 'v1',
      limit: 50,
    });
  });

  it('sends null for a filter it was not given, so the daemon applies no filter', async () => {
    invokeMock.mockResolvedValueOnce(listWire());

    await invokeListTickets({ workspaceId: 4 });

    expect(invokeMock).toHaveBeenCalledWith('list_tickets', {
      workspace_id: 4,
      state: null,
      milestone: null,
      limit: null,
    });
  });

  it('narrows the reply into a ticket list', async () => {
    invokeMock.mockResolvedValueOnce(listWire({ truncated: true }));

    const list = await invokeListTickets({ workspaceId: 4, state: 'todo' });

    expect(list.workspace_id).toBe(4);
    expect(list.tickets[0]?.number).toBe(3);
    expect(list.truncated).toBe(true);
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    invokeMock.mockResolvedValueOnce(listWire({ tickets: 'nope' }));

    await expect(invokeListTickets({ workspaceId: 4 })).rejects.toThrow(BoundaryError);
  });
});

describe('invokeGetTicket', () => {
  it('calls get_ticket with the workspace and the ticket number', async () => {
    invokeMock.mockResolvedValueOnce(detailWire());

    await invokeGetTicket(4, 3);

    expect(invokeMock).toHaveBeenCalledWith('get_ticket', { workspace_id: 4, ticket: 3 });
  });

  it('narrows the reply into a ticket', async () => {
    invokeMock.mockResolvedValueOnce(detailWire({ title: 'Renamed' }));

    const ticket = await invokeGetTicket(4, 3);

    expect(ticket.title).toBe('Renamed');
  });

  it('rejects with BoundaryError when the reply fails validation', async () => {
    const wire = detailWire();
    delete wire['number'];
    invokeMock.mockResolvedValueOnce(wire);

    await expect(invokeGetTicket(4, 3)).rejects.toThrow(BoundaryError);
  });
});

describe('subscribeTicketChanged', () => {
  it('subscribes to session://ticket-changed', async () => {
    expect(SESSION_EVENT.ticketChanged).toBe('session://ticket-changed');
    listenMock.mockResolvedValueOnce(unlistenMock);

    await subscribeTicketChanged(vi.fn(), vi.fn());

    expect(listenMock).toHaveBeenCalledWith(SESSION_EVENT.ticketChanged, expect.any(Function));
  });

  it('delivers a well-formed payload to onTicketChanged', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((_name: string, callback: ListenCallback) => {
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onTicketChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribeTicketChanged(onTicketChanged, onBoundaryError);
    registered?.({ payload: { workspace_id: 4 } });

    expect(onTicketChanged).toHaveBeenCalledWith({ workspace_id: 4 });
    expect(onBoundaryError).toHaveBeenCalledTimes(0);
  });

  it('reports a malformed payload through onBoundaryError instead of throwing', async () => {
    let registered: ListenCallback | undefined;
    listenMock.mockImplementation((_name: string, callback: ListenCallback) => {
      registered = callback;
      return Promise.resolve(unlistenMock);
    });
    const onTicketChanged = vi.fn();
    const onBoundaryError = vi.fn();

    await subscribeTicketChanged(onTicketChanged, onBoundaryError);

    expect(() => registered?.({ payload: {} })).not.toThrow();
    expect(onTicketChanged).toHaveBeenCalledTimes(0);
    expect(onBoundaryError).toHaveBeenCalledTimes(1);
  });

  it('returns a function that cancels the subscription', async () => {
    listenMock.mockResolvedValueOnce(unlistenMock);

    const cancel = await subscribeTicketChanged(vi.fn(), vi.fn());
    cancel();

    expect(unlistenMock).toHaveBeenCalledTimes(1);
  });
});
