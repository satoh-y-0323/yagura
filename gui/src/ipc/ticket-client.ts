/**
 * Thin client for the read-only ticket commands of the Tauri core, and the
 * subscription to the ticket-changed pushes it forwards from the daemon. The
 * GUI only reads tickets: the agents in the windows write them.
 *
 * Every value received from the core passes through `ticket-boundary` before
 * it reaches a caller. A refusal from the core -- including one from a
 * daemon that predates the ticket board -- rejects the returned promise
 * rather than resolving to an empty result.
 */

import { invoke } from '@tauri-apps/api/core';
import { type UnlistenFn } from '@tauri-apps/api/event';

import { SESSION_EVENT } from './session-boundary';
import { listenAppEvent } from './session-client';
import {
  narrowTicketChanged,
  narrowTicketDetail,
  narrowTicketList,
  narrowTicketWorkspaceList,
  type Ticket,
  type TicketChanged,
  type TicketList,
  type TicketWorkspace,
} from './ticket-boundary';

/** What `invokeListTickets` reads: one workspace, optionally narrowed. */
export interface TicketListQuery {
  workspaceId: number;
  /** Only tickets in this state; omitted applies no filter. */
  state?: string;
  /** Only tickets in this milestone; omitted applies no filter. */
  milestone?: string;
  /** The most summaries to return; omitted leaves the cap to the daemon. */
  limit?: number;
}

/**
 * Lists every workspace that has tickets.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListTicketWorkspaces(): Promise<TicketWorkspace[]> {
  return narrowTicketWorkspaceList(await invoke<unknown>('list_ticket_workspaces'));
}

/**
 * Reads one workspace's ticket settings, milestones and matching ticket
 * summaries. A board reads each column with its own `state`, so a column
 * that fills up never pushes another column's tickets out.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListTickets(query: TicketListQuery): Promise<TicketList> {
  return narrowTicketList(
    await invoke<unknown>('list_tickets', {
      workspace_id: query.workspaceId,
      state: query.state ?? null,
      milestone: query.milestone ?? null,
      limit: query.limit ?? null,
    })
  );
}

/**
 * Reads one ticket in full.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeGetTicket(workspaceId: number, ticket: number): Promise<Ticket> {
  return narrowTicketDetail(
    await invoke<unknown>('get_ticket', { workspace_id: workspaceId, ticket })
  );
}

/**
 * Subscribes to the ticket-changed pushes the core forwards from the daemon.
 *
 * A payload that fails validation is reported and dropped, the same way the
 * plan-changed pushes are: an unusable push never disturbs what is shown.
 *
 * @returns a function that cancels the subscription.
 */
export async function subscribeTicketChanged(
  onTicketChanged: (payload: TicketChanged) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listenAppEvent(
    SESSION_EVENT.ticketChanged,
    narrowTicketChanged,
    onTicketChanged,
    onBoundaryError
  );
}
