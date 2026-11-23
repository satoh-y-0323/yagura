import type { UiText } from '../display/ui-text';
import { sanitizeForDisplay, type DisplayLine } from '../display/untrusted-text';
import { profileLimit, type LaunchProfile } from '../profile/launch-profile-choices';
import { plans } from '../ui-messages';

/** One choice of the Default role picker: the row's name and that name for display. */
export interface DefaultRoleChoice {
  name: string;
  label: DisplayLine;
}

/**
 * Why a task of a role that uses `profileId` cannot open a window here,
 * `null` when it can or the profile is unknown. Only a profile that cannot
 * take the task's instruction at launch is noted.
 */
export function roleLaunchNote(
  profiles: readonly Pick<LaunchProfile, 'id' | 'displayName' | 'limits'>[],
  profileId: string
): UiText | null {
  const limit = profileLimit(profiles, profileId, 'launch_prompt');
  const profile = profiles.find((entry) => entry.id === profileId);
  if (limit === null || profile === undefined) {
    return null;
  }
  return plans.roleLaunchLimited(profile.displayName, limit.guidance?.text ?? null);
}

/**
 * Whether a role table that just arrived may replace the rows being edited.
 * An ordinary load never erases an unsaved edit; the echo of the editor's
 * own save is always taken, so the rows settle on what was stored.
 *
 * @param hasUnsavedEdit - whether the rows differ from the table last taken
 * @param reason - `'saved'` when the arriving table is the one this editor
 *   saved, `'loaded'` otherwise
 */
export function acceptsRoleTableReload(
  hasUnsavedEdit: boolean,
  reason: 'loaded' | 'saved'
): boolean {
  return reason === 'saved' || !hasUnsavedEdit;
}

/**
 * The default role once the rows are edited: kept while a row still carries
 * its name, `null` once that row is deleted or renamed.
 */
export function defaultRoleAfterEdit(
  defaultRole: string | null,
  rows: readonly { name: string }[]
): string | null {
  return rows.some((row) => row.name === defaultRole) ? defaultRole : null;
}

/** The Default role picker's choices, one per row in row order; a row's name may be one just typed. */
export function defaultRoleChoices(rows: readonly { name: string }[]): DefaultRoleChoice[] {
  return rows.map((row) => ({ name: row.name, label: sanitizeForDisplay(row.name) }));
}
