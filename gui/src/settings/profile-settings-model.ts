/**
 * Pure decisions behind the Settings panel's Profiles tab. No IPC, no DOM:
 * the component renders these rows and hands the switch back to its caller.
 */
import type { ProfileOption } from '../components/add-window-dialog-types';
import type { DisplayLine } from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import type { ProfileGuidance } from '../ipc/session-client';
import type { RoleEntry } from '../ipc/plan-boundary';
import {
  limitGuidance,
  unavailableText,
  type LimitGuidance,
} from '../profile/launch-profile-choices';
import { settings } from '../ui-messages';

/** What a ready profile cannot do on this host, and what its profile says to do instead. */
export interface ProfileLimitNote {
  /** One line per limit. */
  items: (UiText | DisplayLine)[];
  /** The first guidance a limit carries, `null` when none does. */
  guidance: LimitGuidance | null;
}

/** What one row of the Profiles tab shows. */
export interface ProfileSettingsRow {
  /** The profile identifier. */
  id: string;
  /** `id` for display. */
  displayId: DisplayLine;
  /** The profile's declared display name. */
  displayName: DisplayLine;
  /** The argv the daemon would run, on one line. */
  commandLine: DisplayLine;
  /** Whether the profile currently resolves to something launchable. */
  isReady: boolean;
  /**
   * What the row says when the profile is not ready: "Not found" only for a
   * program that could not be found, "Unavailable" for every other reason.
   * `null` when ready.
   */
  unavailableLine: UiText | null;
  /** The profile's own install guidance, shown under the heading; `null` when it has none. */
  guidance: ProfileGuidance | null;
  /** Whether the user left this profile switched on. */
  enabled: boolean;
  /**
   * The roles that still point at this profile, only while it is disabled:
   * those roles' tasks cannot open windows until it is switched back on.
   */
  rolesUsing: string[];
  /** What this profile cannot do here although it is launchable; `null` when nothing. */
  limitNote: ProfileLimitNote | null;
}

/** Names the roles whose profile is exactly `profileId`, in table order. */
export function rolesUsingProfile(roles: readonly RoleEntry[], profileId: string): string[] {
  return roles.filter((role) => role.profile === profileId).map((role) => role.name);
}

function unavailableLine(profile: ProfileOption): UiText {
  const why = unavailableText(
    profile.unavailableKind,
    profile.reason,
    settings.profileUnavailableReason
  );
  return profile.unavailableKind === 'not_found'
    ? settings.profileNotFound(why)
    : settings.profileUnavailable(why);
}

function limitNoteOf(profile: ProfileOption): ProfileLimitNote | null {
  if (profile.limits.length === 0) {
    return null;
  }
  return {
    items: profile.limits.map((limit) =>
      limit.capability === 'unknown' ? limit.reason : settings.profileLimitItem[limit.capability]
    ),
    guidance: profile.limits.map(limitGuidance).find((guidance) => guidance !== null) ?? null,
  };
}

/** Builds one row per profile, in the order the profiles arrived in. */
export function profileSettingsRows(
  profiles: readonly ProfileOption[],
  roles: readonly RoleEntry[]
): ProfileSettingsRow[] {
  return profiles.map((profile) => ({
    id: profile.id,
    displayId: profile.displayId,
    displayName: profile.displayName,
    commandLine: profile.displayCommand,
    isReady: profile.available,
    unavailableLine: profile.available ? null : unavailableLine(profile),
    guidance:
      !profile.available && profile.unavailableKind === 'needs_native_program'
        ? profile.guidance
        : null,
    enabled: profile.enabled,
    rolesUsing: profile.enabled ? [] : rolesUsingProfile(roles, profile.id),
    limitNote: limitNoteOf(profile),
  }));
}
