/**
 * The payload `AddWindowDialog`'s `onSubmit` prop hands to its caller, and
 * the parameter type `App.svelte`'s `handleOpenWindow` accepts, plus the
 * launch-profile option shape its `profileOptions` prop takes.
 *
 * Svelte's `<script lang="ts">` cannot export a type for another `.svelte`
 * file to import, so the shapes live here instead of being spelled out on
 * both sides, where the two could drift without a compiler error.
 */

import type { ProfileEntry } from '../ipc/session-client';

/**
 * One launch profile the dialog offers, already narrowed to the WebView's
 * camelCase vocabulary by `session-client`.
 */
export type ProfileOption = ProfileEntry;

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
