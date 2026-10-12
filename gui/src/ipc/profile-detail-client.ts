/**
 * Thin client for the profile-detail commands of the Tauri core.
 *
 * Every value received from the core passes through `profile-detail-boundary`
 * before it reaches a caller. A refusal from the core -- including one from a
 * daemon that predates profile details -- rejects the returned promise.
 */

import { invoke } from '@tauri-apps/api/core';

import {
  narrowProfileCommandSet,
  narrowProfileDetail,
  type ProfileCommandSet,
  type ProfileDetail,
} from './profile-detail-boundary';

/**
 * Reads every value `profile` uses, with the layer each comes from.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeGetProfileDetail(profile: string): Promise<ProfileDetail> {
  return narrowProfileDetail(await invoke<unknown>('get_profile_detail', { profile }));
}

/**
 * Sets the argv `profile` launches on this host, or with `argv` null puts the
 * shipped one back.
 *
 * @throws when the core rejects the request or the reply fails validation.
 */
export async function invokeSetProfileCommand(
  profile: string,
  argv: readonly string[] | null
): Promise<ProfileCommandSet> {
  return narrowProfileCommandSet(
    await invoke<unknown>('set_profile_command', {
      profile,
      argv: argv === null ? null : [...argv],
    })
  );
}
