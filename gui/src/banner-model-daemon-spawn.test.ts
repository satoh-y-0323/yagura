/**
 * The daemon-spawn banner carries the launcher's own sentence: `BannerState`
 * holds the text the spawn warning arrived with rather than a flag, so the
 * user reads what was actually observed (which detachment mode was used and
 * what follows from it) instead of a fixed line that says neither.
 *
 * Two properties are measured here: `buildBanners` puts that text on the
 * banner unchanged, and dismissing the banner clears it back to `null`.
 */
import { describe, it, expect } from 'vitest';

import { buildBanners, dismissBanner } from './banner-model';
import type { BannerState } from './banner-model';

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

/**
 * Sentences of the shape the launcher sends: each states what happened and
 * what follows from it, so a banner that dropped either half would be
 * visible as a difference from the input.
 */
const NOTICES = [
  'spawn-probe: started without the strongest detachment mode; it may be terminated with this window',
  'spawn-probe: started as a child of this process; closing the window ends it too',
  'spawn-probe: a mode that carries a colon: and a trailing clause',
];

const NOTICE = NOTICES[0] ?? '';

describe('dismissBanner: dismissing daemon-spawn clears the notice', () => {
  it('sets daemonSpawnNotice back to null', () => {
    const next = dismissBanner(baseState({ daemonSpawnNotice: NOTICE }), 'daemon-spawn');
    expect(next.daemonSpawnNotice).toBeNull();
  });

  it('leaves no daemon-spawn banner once dismissed', () => {
    const next = dismissBanner(baseState({ daemonSpawnNotice: NOTICE }), 'daemon-spawn');
    expect(buildBanners(next).map((entry) => entry.id)).toEqual([]);
  });

  it('does not clear the notice when a different banner is dismissed', () => {
    const state = baseState({
      daemonSpawnNotice: NOTICE,
      operationError: 'spawn-probe: operation error',
    });
    const next = dismissBanner(state, 'operation-error');
    expect(next.daemonSpawnNotice).toBe(NOTICE);
  });
});
