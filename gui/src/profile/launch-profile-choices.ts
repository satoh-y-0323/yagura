/**
 * The choices every launch-profile picker offers: the profile options, the
 * model / effort fields the chosen profile maps, and which of that text is
 * kept for the chosen profile. The window dialog, the role table and the plan
 * board all read these, so the three pickers cannot drift apart.
 */
import type { LaunchOptionInfo } from '../ipc/session-client';

/** The least of a daemon profile entry the pickers read. */
export interface LaunchProfile {
  id: string;
  displayName: string;
  available: boolean;
  reason: string | null;
  model: LaunchOptionInfo | null;
  effort: LaunchOptionInfo | null;
}

/** One option a profile select renders. */
export interface ProfileChoice {
  id: string;
  displayName: string;
  available: boolean;
  reason: string | null;
}

/** One launch-option field the chosen profile maps, with its alias candidates. */
export interface LaunchOptionAxis {
  key: 'model' | 'effort';
  aliases: LaunchOptionInfo['aliases'];
}

/** The model / effort text of a picker. */
export interface LaunchOptionValues {
  model: string;
  effort: string;
}

/**
 * Every profile, in the order given. A non-blank selection the list no longer
 * holds is kept as a choice of its own: a `<select>` cannot show a value it
 * has no option for, and would otherwise show another profile as chosen.
 */
export function profileChoices(
  profiles: readonly LaunchProfile[],
  selected: string
): ProfileChoice[] {
  const choices = profiles.map((profile) => ({
    id: profile.id,
    displayName: profile.displayName,
    available: profile.available,
    reason: profile.reason,
  }));
  if (selected === '' || profiles.some((profile) => profile.id === selected)) {
    return choices;
  }
  return [...choices, { id: selected, displayName: selected, available: false, reason: null }];
}

/** The axes the chosen profile maps, model before effort; none for an unlisted profile. */
export function launchOptionAxes(
  profiles: readonly LaunchProfile[],
  profileId: string
): LaunchOptionAxis[] {
  const profile = profiles.find((entry) => entry.id === profileId);
  if (profile === undefined) {
    return [];
  }
  const axes: LaunchOptionAxis[] = [];
  if (profile.model !== null) {
    axes.push({ key: 'model', aliases: profile.model.aliases });
  }
  if (profile.effort !== null) {
    axes.push({ key: 'effort', aliases: profile.effort.aliases });
  }
  return axes;
}

/**
 * Blanks the text of an axis the chosen profile does not map, so a field the
 * picker hides is never saved or sent. An unlisted profile's axes cannot be
 * known, so its text is left as entered for the daemon to judge.
 */
export function mappedLaunchOptionValues(
  profiles: readonly LaunchProfile[],
  profileId: string,
  values: LaunchOptionValues
): LaunchOptionValues {
  const profile = profiles.find((entry) => entry.id === profileId);
  if (profile === undefined) {
    return { model: values.model, effort: values.effort };
  }
  return {
    model: profile.model === null ? '' : values.model,
    effort: profile.effort === null ? '' : values.effort,
  };
}
