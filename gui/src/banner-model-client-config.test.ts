import { describe, it, expect } from 'vitest';

import { buildBanners, applyClientConfigFailure } from './banner-model';
import type { BannerEntry, BannerState } from './banner-model';

/**
 * The text a spawn warning arrives with. Any non-empty sentence does: the
 * banner carries whatever the launcher sent, so the value only has to be
 * distinguishable from the other fixtures.
 */
const DAEMON_SPAWN_NOTICE = 'spawn-probe: started without the strongest detachment mode';

/**
 * A full BannerState fixture, overridable per field. Duplicated from the
 * sibling banner-model test modules rather than imported: they are test
 * modules themselves and export no such helper.
 */
function baseState(overrides: Partial<BannerState> = {}): BannerState {
  return {
    daemonSpawnNotice: null,
    operationError: null,
    detectionWarning: null,
    keymapNotice: null,
    boundaryNotice: null,
    hasThemeLoadFailure: false,
    isThemeLoadFailureDismissed: false,
    themeIssueCount: 0,
    isThemeIssuesDismissed: false,
    approvalExpiredUnseen: null,
    approverRegistrationRefused: false,
    daemonRecovery: 'idle',
    daemonRecoveryRev: 0,
    isDaemonRecoveryDismissed: false,
    clientConfigFailure: null,
    isDaemonProtocolStale: false,
    startupEnvFailure: null,
    ...overrides,
  };
}

function clientConfigBanner(state: BannerState): BannerEntry | undefined {
  return buildBanners(state).find((entry) => entry.id === 'client-config-failure');
}

describe('buildBanners: an absent client settings file raises no banner', () => {
  it('does not show client-config-failure when clientConfigFailure is "not_found"', () => {
    expect(clientConfigBanner(baseState({ clientConfigFailure: 'not_found' }))).toBeUndefined();
  });

  it('returns an empty array when "not_found" is the only active condition', () => {
    expect(buildBanners(baseState({ clientConfigFailure: 'not_found' }))).toEqual([]);
  });

  it('leaves the surrounding banners in their fixed order when the failure is "not_found"', () => {
    const operationErrorText = 'client-config-probe: operation error';
    const result = buildBanners(
      baseState({
        daemonSpawnNotice: DAEMON_SPAWN_NOTICE,
        clientConfigFailure: 'not_found',
        operationError: operationErrorText,
      })
    );

    expect(result).toEqual([
      { id: 'daemon-spawn', text: DAEMON_SPAWN_NOTICE },
      { id: 'operation-error', text: operationErrorText },
    ]);
  });
});

describe('applyClientConfigFailure: keeps the reason even when it raises no banner', () => {
  it('stores "not_found" in the state it returns', () => {
    const next = applyClientConfigFailure(baseState(), 'not_found');
    expect(next.clientConfigFailure).toBe('not_found');
  });

  it('stores "not_found" while buildBanners stays silent about it', () => {
    const next = applyClientConfigFailure(baseState(), 'not_found');
    expect(next.clientConfigFailure).toBe('not_found');
    expect(clientConfigBanner(next)).toBeUndefined();
  });

  it('replaces a banner-raising failure with "not_found", keeping the value and dropping the banner', () => {
    const shown = applyClientConfigFailure(baseState(), 'read_failed');
    expect(clientConfigBanner(shown)).toBeDefined();

    const next = applyClientConfigFailure(shown, 'not_found');
    expect(next.clientConfigFailure).toBe('not_found');
    expect(clientConfigBanner(next)).toBeUndefined();
  });
});
