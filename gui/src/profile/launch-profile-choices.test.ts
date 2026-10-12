/**
 * The launch-profile choices every profile picker shares: the options a
 * profile select offers, the model / effort fields the chosen profile maps,
 * and which launch-option text is kept for the chosen profile. The window
 * dialog, the role table and the plan board's assignment editor all read
 * these, so the three cannot drift apart.
 */
import { describe, expect, it } from 'vitest';

import type { LaunchOptionInfo, ProfileEntry } from '../ipc/session-client';
import {
  launchOptionAxes,
  mappedLaunchOptionValues,
  profileChoices,
} from './launch-profile-choices';

function profile(
  id: string,
  displayName: string,
  model: LaunchOptionInfo | null,
  effort: LaunchOptionInfo | null,
  reason: string | null = null
): ProfileEntry {
  return {
    id,
    displayName,
    detection: null,
    detectionWarning: null,
    cwd: null,
    command: [id],
    available: reason === null,
    reason,
    model,
    effort,
    enabled: true,
  };
}

const CODEX_MODEL: LaunchOptionInfo = {
  aliases: [
    { alias: 'fast', value: 'gpt-mini' },
    { alias: 'deep', value: 'gpt-large' },
  ],
};
const CLAUDE_MODEL: LaunchOptionInfo = { aliases: [{ alias: 'opus', value: 'claude-opus' }] };
const CLAUDE_EFFORT: LaunchOptionInfo = {
  aliases: [
    { alias: 'high', value: 'high' },
    { alias: 'low', value: 'low' },
  ],
};

const profiles: ProfileEntry[] = [
  profile('codex-cli', 'Codex CLI', CODEX_MODEL, null),
  profile('claude-code', 'Claude Code', CLAUDE_MODEL, CLAUDE_EFFORT),
  profile('plain-shell', 'Shell', null, null, 'not installed'),
];

describe('profileChoices', () => {
  it('offers every profile by id with its display name, in the order it was given', () => {
    const offered = profileChoices(profiles, 'codex-cli');
    expect(offered.map((choice) => [choice.id, choice.displayName])).toEqual([
      ['codex-cli', 'Codex CLI'],
      ['claude-code', 'Claude Code'],
      ['plain-shell', 'Shell'],
    ]);
  });

  it("carries each profile's availability and reason, so an unavailable one can be shown as such", () => {
    const shell = profileChoices(profiles, 'codex-cli').find(
      (choice) => choice.id === 'plain-shell'
    );
    expect(shell).toMatchObject({ available: false, reason: 'not installed' });
  });

  it('adds nothing when the selection is one of the listed profiles', () => {
    expect(profileChoices(profiles, 'claude-code')).toHaveLength(profiles.length);
  });

  it('adds nothing for a blank selection', () => {
    const offered = profileChoices(profiles, '');
    expect(offered.map((choice) => choice.id)).not.toContain('');
    expect(offered).toHaveLength(profiles.length);
  });

  it('keeps a selected profile the list no longer holds as a choice of its own, so the select cannot fall to another profile', () => {
    const offered = profileChoices(profiles, 'removed-profile');
    const ids = offered.map((choice) => choice.id);
    expect(ids.filter((id) => id === 'removed-profile')).toHaveLength(1);
    for (const listed of profiles) {
      expect(ids.filter((id) => id === listed.id)).toHaveLength(1);
    }
  });

  it('keeps that selection even when the daemon lists no profiles at all', () => {
    const offered = profileChoices([], 'removed-profile');
    expect(offered.map((choice) => choice.id)).toEqual(['removed-profile']);
  });
});

describe('launchOptionAxes', () => {
  it('offers only the axes the chosen profile maps, with its aliases as candidates', () => {
    expect(launchOptionAxes(profiles, 'codex-cli')).toEqual([
      { key: 'model', aliases: CODEX_MODEL.aliases },
    ]);
  });

  it('offers model before effort when the profile maps both', () => {
    expect(launchOptionAxes(profiles, 'claude-code')).toEqual([
      { key: 'model', aliases: CLAUDE_MODEL.aliases },
      { key: 'effort', aliases: CLAUDE_EFFORT.aliases },
    ]);
  });

  it('still offers an axis the profile maps with no aliases, with no candidates', () => {
    const bare = [profile('bare', 'Bare', { aliases: [] }, null)];
    expect(launchOptionAxes(bare, 'bare')).toEqual([{ key: 'model', aliases: [] }]);
  });

  it('offers no axis for a profile that maps none, one the list does not hold, or a blank selection', () => {
    expect(launchOptionAxes(profiles, 'plain-shell')).toEqual([]);
    expect(launchOptionAxes(profiles, 'removed-profile')).toEqual([]);
    expect(launchOptionAxes(profiles, '')).toEqual([]);
  });
});

describe('mappedLaunchOptionValues', () => {
  it('drops the text of an axis the chosen profile does not map, so a hidden field cannot be saved', () => {
    expect(
      mappedLaunchOptionValues(profiles, 'codex-cli', { model: 'fast', effort: 'high' })
    ).toEqual({ model: 'fast', effort: '' });
    expect(
      mappedLaunchOptionValues(profiles, 'plain-shell', { model: 'fast', effort: 'high' })
    ).toEqual({ model: '', effort: '' });
  });

  it('keeps the text of every axis the chosen profile maps, including text that is not an alias', () => {
    expect(
      mappedLaunchOptionValues(profiles, 'claude-code', {
        model: 'my-own-model',
        effort: 'high',
      })
    ).toEqual({ model: 'my-own-model', effort: 'high' });
  });

  it('leaves the text as entered for a profile the list does not hold, whose axes cannot be known', () => {
    expect(
      mappedLaunchOptionValues(profiles, 'removed-profile', {
        model: 'fast',
        effort: 'high',
      })
    ).toEqual({ model: 'fast', effort: 'high' });
  });
});

describe('profileChoices: a selection the list no longer holds', () => {
  it('offers it under its own id as unavailable, with no reason', () => {
    const offered = profileChoices(profiles, 'removed-profile');

    expect(offered[offered.length - 1]).toEqual({
      id: 'removed-profile',
      displayName: 'removed-profile',
      available: false,
      reason: null,
    });
  });
});
