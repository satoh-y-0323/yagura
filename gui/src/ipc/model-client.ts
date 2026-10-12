/**
 * Thin client for the model-alias commands of the Tauri core, and the
 * subscription to the model-list pushes it forwards from the daemon.
 *
 * Every value received from the core passes through `model-boundary` before
 * it reaches a caller. A refusal from the core -- including one from a
 * daemon that predates model aliases -- rejects the returned promise.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import {
  narrowModelAliasesSaved,
  narrowModelChoicesPayload,
  narrowModelChoicesRequested,
  type LaunchOptionAlias,
  type ModelAliasesSaved,
  type ModelChoicesPayload,
  type ModelChoicesRequested,
} from './model-boundary';
import { SESSION_EVENT } from './session-boundary';

/**
 * Asks the daemon for the models `profile` can take. The reply only
 * acknowledges the request; the list arrives as a `session://model-choices`
 * push carrying the same `requestId`, which the caller records before calling
 * so a push that overtakes the reply is still recognized.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeListModelChoices(
  profile: string,
  requestId: number
): Promise<ModelChoicesRequested> {
  return narrowModelChoicesRequested(
    await invoke<unknown>('list_model_choices', { profile, request_id: requestId })
  );
}

/**
 * Replaces the values of `profile`'s model aliases.
 *
 * @returns the aliases as the daemon stored them.
 * @throws when the core rejects the aliases or the reply fails validation.
 */
export async function invokeSaveModelAliases(
  profile: string,
  aliases: LaunchOptionAlias[]
): Promise<ModelAliasesSaved> {
  return narrowModelAliasesSaved(await invoke<unknown>('save_model_aliases', { profile, aliases }));
}

/**
 * Subscribes to the model-list pushes the core forwards from the daemon.
 *
 * A payload that fails validation is reported and dropped, the same way the
 * plan-changed pushes are.
 *
 * @returns a function that cancels the subscription.
 */
export async function listenModelChoices(
  onModelChoices: (payload: ModelChoicesPayload) => void,
  onBoundaryError: (error: unknown) => void
): Promise<UnlistenFn> {
  return listen<unknown>(SESSION_EVENT.modelChoices, (event) => {
    let payload: ModelChoicesPayload;
    try {
      payload = narrowModelChoicesPayload(event.payload);
    } catch (error) {
      onBoundaryError(error);
      return;
    }
    onModelChoices(payload);
  });
}
