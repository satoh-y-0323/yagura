/**
 * The startup-env-failure banner: the GUI could
 * not resolve which instance it belongs to, so it started without spawning
 * or connecting a daemon and reports why instead. `StartupEnvFailure` has
 * two reasons -- an invalid `YAGURA_INSTANCE` and a non-default instance
 * combined with `YAGURA_STORE_DB` -- and both share one `BannerId`, the same
 * shape `client-config-failure` already uses for its four reasons
 * (banner-model-client-config.test.ts).
 */
import { describe, it, expect } from 'vitest';

import { buildBanners, dismissBanner } from './banner-model';
import type { BannerState } from './banner-model';
import type { StartupEnvFailure } from './session/recovery-types';
import { banners } from './ui-messages';

const INVALID_INSTANCE_ID: StartupEnvFailure = 'invalid_instance_id';
const STORE_OVERRIDE_WITH_NAMED_INSTANCE: StartupEnvFailure = 'store_override_with_named_instance';

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

function startupEnvFailureBanner(state: BannerState) {
  return buildBanners(state).find((entry) => entry.id === 'startup-env-failure');
}

describe('buildBanners: startup-env-failure derivation', () => {
  it('shows startup-env-failure with the invalid-identifier text when the reason is invalid_instance_id', () => {
    const entry = startupEnvFailureBanner(baseState({ startupEnvFailure: INVALID_INSTANCE_ID }));
    expect(entry).toBeDefined();
    expect(entry?.text).toBe(banners.invalidInstanceId);
  });

  it('shows startup-env-failure with the store-override text when the reason is store_override_with_named_instance', () => {
    const entry = startupEnvFailureBanner(
      baseState({ startupEnvFailure: STORE_OVERRIDE_WITH_NAMED_INSTANCE })
    );
    expect(entry).toBeDefined();
    expect(entry?.text).toBe(banners.storeOverrideWithNamedInstance);
  });
});

// The two reasons must read as different
// sentences, or the store-override user would try to fix an identifier that
// was never wrong.
describe('buildBanners: the two startup-env-failure reasons read different text', () => {
  it('invalid_instance_id and store_override_with_named_instance do not produce the same banner text', () => {
    const invalidText = startupEnvFailureBanner(
      baseState({ startupEnvFailure: INVALID_INSTANCE_ID })
    )?.text;
    const overrideText = startupEnvFailureBanner(
      baseState({ startupEnvFailure: STORE_OVERRIDE_WITH_NAMED_INSTANCE })
    )?.text;
    expect(invalidText).toBeDefined();
    expect(overrideText).toBeDefined();
    expect(invalidText).not.toBe(overrideText);
  });
});

describe('dismissBanner: startup-env-failure clears the reason to null', () => {
  it('clears startupEnvFailure back to null when dismissed', () => {
    const state = baseState({ startupEnvFailure: INVALID_INSTANCE_ID });
    const next = dismissBanner(state, 'startup-env-failure');
    expect(next.startupEnvFailure).toBeNull();
  });

  it('leaves no startup-env-failure banner once dismissed', () => {
    const next = dismissBanner(
      baseState({ startupEnvFailure: STORE_OVERRIDE_WITH_NAMED_INSTANCE }),
      'startup-env-failure'
    );
    expect(startupEnvFailureBanner(next)).toBeUndefined();
  });

  it('does not clear the reason when a different banner is dismissed', () => {
    const state = baseState({
      startupEnvFailure: INVALID_INSTANCE_ID,
      operationError: 'instance-probe: operation error',
    });
    const next = dismissBanner(state, 'operation-error');
    expect(next.startupEnvFailure).toBe(INVALID_INSTANCE_ID);
  });
});
