/**
 * Thin client for the two MCP registration commands of the Tauri core, and the
 * subscriptions to the two pushes it forwards from the daemon.
 *
 * Every value received from the core passes through `mcp-registration-boundary`
 * before it reaches a caller. A refusal from the core -- including one from a
 * daemon that predates MCP registration -- rejects the returned promise.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  narrowMcpRegistrationRequested,
  narrowMcpRegistrationResultPayload,
  narrowMcpRegistrationStatusPayload,
  narrowMcpRegistrationsListed,
  type McpRegistrationAction,
  type McpRegistrationRequested,
  type McpRegistrationResultPayload,
  type McpRegistrationsListed,
  type McpRegistrationStatusPayload,
} from './mcp-registration-boundary';
import { SESSION_EVENT } from './session-boundary';

/**
 * Asks the daemon which profiles carry an MCP registration table and how
 * each one currently reads. The reply carries the rows themselves; per-row
 * status arrives afterward as `session://mcp-registration-status` pushes
 * carrying the same `requestId`, which the caller records before calling so a
 * push that overtakes the reply is still recognized.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListMcpRegistrations(
  requestId: number
): Promise<McpRegistrationsListed> {
  return narrowMcpRegistrationsListed(
    await invoke<unknown>('list_mcp_registrations', { request_id: requestId })
  );
}

/**
 * Asks the daemon to register, update, or remove `profile`'s MCP
 * registration with its CLI. The reply only acknowledges the request; the
 * outcome arrives as a `session://mcp-registration-result` push carrying the
 * same `requestId`.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeRunMcpRegistration(
  profile: string,
  action: McpRegistrationAction,
  requestId: number
): Promise<McpRegistrationRequested> {
  return narrowMcpRegistrationRequested(
    await invoke<unknown>('run_mcp_registration', { profile, action, request_id: requestId })
  );
}

/**
 * Subscribes to the per-row status pushes the core forwards from the daemon.
 *
 * A payload that fails validation is reported to `onBoundaryError` and
 * dropped rather than reaching `onStatus`, the same way the model-choices
 * push is handled.
 *
 * @returns a function that cancels the subscription.
 */
export async function listenMcpRegistrationStatus(
  onStatus: (payload: McpRegistrationStatusPayload) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listen<unknown>(SESSION_EVENT.mcpRegistrationStatus, (event) => {
    let payload: McpRegistrationStatusPayload;
    try {
      payload = narrowMcpRegistrationStatusPayload(event.payload);
    } catch (error) {
      onBoundaryError(error);
      return;
    }
    onStatus(payload);
  });
}

/**
 * Subscribes to the operation-result pushes the core forwards from the
 * daemon.
 *
 * A payload that fails validation is reported to `onBoundaryError` and
 * dropped rather than reaching `onResult`.
 *
 * @returns a function that cancels the subscription.
 */
export async function listenMcpRegistrationResult(
  onResult: (payload: McpRegistrationResultPayload) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listen<unknown>(SESSION_EVENT.mcpRegistrationResult, (event) => {
    let payload: McpRegistrationResultPayload;
    try {
      payload = narrowMcpRegistrationResultPayload(event.payload);
    } catch (error) {
      onBoundaryError(error);
      return;
    }
    onResult(payload);
  });
}
