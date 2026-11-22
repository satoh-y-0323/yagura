/**
 * The only module that calls the Tauri `invoke` bridge.
 *
 * A command is called through one of two doors, chosen by its name's type.
 * Every command that reaches the daemon fails with the core's structured
 * failure, so its door narrows the rejection and throws it as one Error type:
 * a caller reads the kind of failure, never the wording. Commands that do not
 * reach the daemon keep a refusal contract of their own, so their door hands
 * a rejection back as it came.
 *
 * Which commands reach the daemon is decided by their Rust signatures (they
 * take the connection pool and return a `Result`); the contract test in
 * `window-open-failure-wire.test.ts` keeps the two name lists equal to that.
 */
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';

import { CommandFailureError, narrowCommandFailure } from './session-boundary';

/** Every command that reaches the daemon. */
export const DAEMON_COMMAND_NAMES = [
  'approve_plan',
  'attach_window',
  'broadcast_chat',
  'clear_chat_room',
  'clear_task_assignment',
  'clear_window_selection',
  'close_window',
  'copy_window_selection',
  'decide_approval',
  'delete_chat_room',
  'delete_plan',
  'get_approval_settings',
  'get_daemon_health',
  'get_plan',
  'get_profile_detail',
  'get_ticket',
  'interrupt_window',
  'list_chat_rooms',
  'list_detections',
  'list_mcp_registrations',
  'list_model_choices',
  'list_plans',
  'list_profiles',
  'list_restore_candidates',
  'list_roles',
  'list_ticket_workspaces',
  'list_tickets',
  'list_windows',
  'mark_plan_seen',
  'next_frames',
  'open_window',
  'read_chat',
  'reconnect_daemon',
  'rename_window',
  'resize',
  'run_mcp_registration',
  'save_model_aliases',
  'save_roles',
  'send_chat',
  'send_window_input',
  'set_auto_approve',
  'set_default_palettes',
  'set_plan_approval_required',
  'set_plan_owner',
  'set_profile_command',
  'set_profile_enabled',
  'set_task_assignment',
  'set_window_palette',
  'set_window_visible',
  'sync_approvals',
  'update_window_layout',
] as const;

/** Every command this layer calls that does not reach the daemon. */
export const LOCAL_COMMAND_NAMES = [
  'get_daemon_compatibility',
  'get_daemon_connection',
  'get_recovery_status',
  'keymap_file_path',
  'load_keymap',
  'load_themes',
  'save_keymap',
  'set_attention_summary',
] as const;

/** The name of a command that reaches the daemon. */
export type DaemonCommandName = (typeof DAEMON_COMMAND_NAMES)[number];

/** The name of a command that does not reach the daemon. */
export type LocalCommandName = (typeof LOCAL_COMMAND_NAMES)[number];

/** Holds only when no name is in both lists, so the door a name takes is never ambiguous. */
type Disjoint = [Extract<DaemonCommandName, LocalCommandName>] extends [never] ? true : false;
const NAMES_ARE_DISJOINT: Disjoint = true;
void NAMES_ARE_DISJOINT;

/** Calls `name`, passing no argument object when there is none. */
function call(name: string, args: InvokeArgs | undefined): Promise<unknown> {
  return args === undefined ? invoke<unknown>(name) : invoke<unknown>(name, args);
}

/**
 * Calls a command that reaches the daemon.
 *
 * @throws {CommandFailureError} when the command fails, carrying the failure
 * the core reported.
 * @throws {BoundaryError} when the rejection claims the failure shape but
 * does not match it.
 */
export async function invokeDaemonCommand(
  name: DaemonCommandName,
  args?: InvokeArgs
): Promise<unknown> {
  try {
    return await call(name, args);
  } catch (rejection) {
    throw new CommandFailureError(narrowCommandFailure(rejection));
  }
}

/**
 * Calls a command that does not reach the daemon.
 *
 * @throws the rejection exactly as the command raised it.
 */
export function invokeLocalCommand(name: LocalCommandName, args?: InvokeArgs): Promise<unknown> {
  return call(name, args);
}
