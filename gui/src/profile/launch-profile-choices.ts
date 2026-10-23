/**
 * The choices every launch-profile picker offers: the profile options, the
 * model / effort fields the chosen profile maps, and which of that text is
 * kept for the chosen profile. The window dialog, the role table and the plan
 * board all read these, so the three pickers cannot drift apart.
 */
import type {
  LaunchOptionInfo,
  ProfileCapability,
  ProfileGuidance,
  ProfileLimit,
  ProfileUnavailableKind,
} from '../ipc/session-client';
import { forms } from '../ui-messages';

/** The least of a daemon profile entry the pickers read. */
export interface LaunchProfile {
  id: string;
  displayName: string;
  available: boolean;
  reason: string | null;
  unavailableKind: ProfileUnavailableKind | null;
  model: LaunchOptionInfo | null;
  effort: LaunchOptionInfo | null;
  /** What the profile cannot do on this host, apart from `available`. */
  limits: readonly ProfileLimit[];
}

/** One option a profile select renders. */
export interface ProfileChoice {
  id: string;
  displayName: string;
  available: boolean;
  reason: string | null;
  unavailableKind: ProfileUnavailableKind | null;
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
    unavailableKind: profile.unavailableKind,
  }));
  if (selected === '' || profiles.some((profile) => profile.id === selected)) {
    return choices;
  }
  return [
    ...choices,
    { id: selected, displayName: selected, available: false, reason: null, unavailableKind: null },
  ];
}

/**
 * The wording for why a profile is unavailable, from `wording` for a kind
 * this build knows. A daemon that names no kind, or one this build does not
 * know, is answered with its own reason text.
 */
export function unavailableText(
  kind: ProfileUnavailableKind | null,
  reason: string | null,
  wording: Readonly<Record<Exclude<ProfileUnavailableKind, 'unknown'>, string>>
): string | null {
  if (kind === null || kind === 'unknown') {
    return reason;
  }
  return wording[kind];
}

/** What a profile option reads: its name, then why it cannot be chosen when it cannot. */
export function profileChoiceLabel(choice: ProfileChoice): string {
  const why = unavailableText(choice.unavailableKind, choice.reason, forms.profileUnavailableShort);
  return why === null ? choice.displayName : `${choice.displayName} — ${why}`;
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

/** A profile's guidance for a limit, as the screens may show it. */
export interface LimitGuidance {
  text: string;
  command: string | null;
  /** An `https://` page only; anything else is dropped. */
  url: string | null;
}

/** The guidance of `limit`, with a page that is not `https://` left out. */
export function limitGuidance(limit: ProfileLimit): LimitGuidance | null {
  const guidance: ProfileGuidance | null = limit.guidance;
  if (guidance === null) {
    return null;
  }
  return {
    text: guidance.text,
    command: guidance.command,
    url: guidance.url !== null && guidance.url.startsWith('https://') ? guidance.url : null,
  };
}

/** The limit `profileId` has on `capability`, `null` for no such limit or an unlisted profile. */
export function profileLimit(
  profiles: readonly Pick<LaunchProfile, 'id' | 'limits'>[],
  profileId: string,
  capability: ProfileCapability
): ProfileLimit | null {
  const profile = profiles.find((entry) => entry.id === profileId);
  return profile?.limits.find((limit) => limit.capability === capability) ?? null;
}

/** Why a launch-option field cannot be used with the chosen profile here, and what to do instead. */
export interface LaunchOptionLimit {
  reason: string;
  guidance: LimitGuidance | null;
}

/** The limit that disables the `key` field of the chosen profile, `null` when the field is usable. */
export function launchOptionLimit(
  profiles: readonly LaunchProfile[],
  profileId: string,
  key: 'model' | 'effort'
): LaunchOptionLimit | null {
  const limit = profileLimit(profiles, profileId, key);
  if (limit === null) {
    return null;
  }
  return {
    reason: key === 'model' ? forms.modelLimited : forms.effortLimited,
    guidance: limitGuidance(limit),
  };
}

function isLimited(profile: LaunchProfile, capability: 'model' | 'effort'): boolean {
  return profile.limits.some((limit) => limit.capability === capability);
}

/**
 * Blanks the text of an axis the chosen profile does not map or cannot take
 * here, so a field the picker hides or disables is never saved or sent. An
 * unlisted profile's axes cannot be known, so its text is left as entered for
 * the daemon to judge.
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
    model: profile.model === null || isLimited(profile, 'model') ? '' : values.model,
    effort: profile.effort === null || isLimited(profile, 'effort') ? '' : values.effort,
  };
}
