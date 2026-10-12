/**
 * The payload `AddWindowDialog`'s `onSubmit` prop hands to its caller, and
 * the parameter type `App.svelte`'s `handleOpenWindow` accepts, plus the
 * launch-profile option shape its `profileOptions` prop takes.
 *
 * Svelte's `<script lang="ts">` cannot export a type for another `.svelte`
 * file to import, so the shapes live here instead of being spelled out on
 * both sides, where the two could drift without a compiler error.
 */

import type { LaunchOptionInfo } from '../ipc/session-client';

/**
 * One launch profile the dialog offers, already narrowed to the WebView's
 * camelCase vocabulary by `session-client`.
 */
export interface ProfileOption {
  /** The profile identifier, which is also the option's value. */
  id: string;
  /** What the option shows. */
  displayName: string;
  /** The detection definition this profile names, `null` for the default. */
  detection: string | null;
  /** Why that definition could not be found, `null` when it was. */
  detectionWarning: string | null;
  /** The profile's own default working directory, `null` when it has none. */
  cwd: string | null;
  /**
   * The unresolved argv launching this profile would run, empty when the
   * daemon has none to offer on its operating system.
   */
  command: string[];
  /** Whether the profile currently resolves to something launchable. */
  available: boolean;
  /** The daemon's English reason text when `available` is false. */
  reason: string | null;
  /** This profile's `model` launch option, `null` when it maps no such axis. */
  model: LaunchOptionInfo | null;
  /** This profile's `effort` launch option, `null` when it maps no such axis. */
  effort: LaunchOptionInfo | null;
  /** Whether the user left this profile switched on. */
  enabled: boolean;
}

/**
 * What the dialog submits: a launch profile the daemon expands, or the
 * explicit command of the Custom form.
 *
 * `model` / `effort` only exist on the `profile` form: the daemon rejects
 * either one alongside a `command` launch (protocol
 * `validate_open_window_exclusivity`).
 */
export type OpenWindowSubmission =
  | {
      kind: 'profile';
      profile: string;
      cwd: string | undefined;
      name: string | undefined;
      model: string | undefined;
      effort: string | undefined;
    }
  | { kind: 'custom'; tokens: string[]; cwd: string | undefined; name: string | undefined };
