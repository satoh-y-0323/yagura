/**
 * The launch profiles a chooser may offer: the ones the user left switched
 * on. Whether a profile can launch on this machine (`available`) is not
 * decided here, because the choosers show that separately with its reason.
 */
export function offeredProfiles<T extends { enabled: boolean }>(profiles: readonly T[]): T[] {
  return profiles.filter((profile) => profile.enabled);
}
