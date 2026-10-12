/**
 * `offeredProfiles` is the one filter every launch-profile chooser reads
 * through: it drops the profiles the user switched off and nothing else.
 * Whether a profile can launch on this machine (`available`) is a separate
 * judgement the choosers already show, so it must not be folded in here.
 */
import { describe, it, expect } from 'vitest';

import { offeredProfiles } from './offered-profiles';

interface Candidate {
  id: string;
  enabled: boolean;
  available: boolean;
}

function candidate(id: string, enabled: boolean, available = true): Candidate {
  return { id, enabled, available };
}

describe('offeredProfiles', () => {
  it('drops the disabled profiles and keeps the rest in the order given', () => {
    const profiles = [
      candidate('shell', true),
      candidate('codex-cli', false),
      candidate('claude-code', true),
      candidate('gemini-cli', false),
      candidate('wsl-shell', true),
    ];

    expect(offeredProfiles(profiles).map((profile) => profile.id)).toEqual([
      'shell',
      'claude-code',
      'wsl-shell',
    ]);
  });

  it('keeps an enabled profile that cannot launch here, so its reason stays visible', () => {
    const profiles = [candidate('shell', true), candidate('wsl-codex', true, false)];

    expect(offeredProfiles(profiles).map((profile) => profile.id)).toEqual(['shell', 'wsl-codex']);
  });

  it('drops a disabled profile whether or not it could launch', () => {
    const profiles = [candidate('ready-off', false, true), candidate('missing-off', false, false)];

    expect(offeredProfiles(profiles)).toEqual([]);
  });

  it('leaves the list it was given untouched', () => {
    const profiles = [candidate('shell', true), candidate('codex-cli', false)];

    offeredProfiles(profiles);

    expect(profiles.map((profile) => profile.id)).toEqual(['shell', 'codex-cli']);
  });
});
