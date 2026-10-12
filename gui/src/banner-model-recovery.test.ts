import { describe, it, expect } from 'vitest';

import {
  buildBanners,
  dismissBanner,
  applyRecoveryUpdate,
  applyClientConfigFailure,
} from './banner-model';
import type { BannerState } from './banner-model';
import { banners } from './ui-messages';

/**
 * The text a spawn warning arrives with. Any non-empty sentence does: the
 * banner carries whatever the launcher sent, so the value only has to be
 * distinguishable from the other fixtures.
 */
const DAEMON_SPAWN_NOTICE = 'spawn-probe: started without the strongest detachment mode';

/**
 * A full BannerState fixture, overridable per field. Duplicated from
 * `banner-model.test.ts` rather than imported: that file is a sibling test
 * module and does not export its own `baseState` helper.
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

const FULL_ORDER = [
  'daemon-spawn',
  'daemon-recovery',
  'client-config-failure',
  'operation-error',
  'detection-warning',
  'keymap-notice',
  'boundary-error',
  'themes-load-failure',
  'themes-issues',
  'approval-expired-unseen',
  'approver-registration-refused',
] as const;

describe('buildBanners: daemon-recovery derivation', () => {
  it('shows daemon-recovery with the reconnecting text when the status is retrying', () => {
    const result = buildBanners(baseState({ daemonRecovery: 'retrying' }));
    const entry = result.find((e) => e.id === 'daemon-recovery');
    expect(entry).toBeDefined();
    expect(entry?.text).toBe(banners.daemonReconnecting);
  });

  it('shows daemon-recovery with the failed text when the status is exhausted', () => {
    const result = buildBanners(baseState({ daemonRecovery: 'exhausted' }));
    const entry = result.find((e) => e.id === 'daemon-recovery');
    expect(entry).toBeDefined();
    expect(entry?.text).toBe(banners.daemonReconnectFailed);
  });
});

describe('buildBanners: fixed display order including the two new banners', () => {
  it('returns all eleven banners in the order daemon-spawn -> daemon-recovery -> client-config-failure -> operation-error -> ... when all are active', () => {
    const state = baseState({
      daemonSpawnNotice: DAEMON_SPAWN_NOTICE,
      daemonRecovery: 'retrying',
      clientConfigFailure: 'read_failed',
      operationError: 'wiring-probe: operation error',
      detectionWarning: 'wiring-probe: detection warning',
      keymapNotice: 'wiring-probe: keymap notice',
      boundaryNotice: 'wiring-probe: boundary notice',
      hasThemeLoadFailure: true,
      themeIssueCount: 2,
      approvalExpiredUnseen: {
        count: 1,
        latest: { requester: 'builder window', tool: 'close_window', target: 'target window' },
      },
      approverRegistrationRefused: true,
    });

    const result = buildBanners(state);

    expect(result.map((entry) => entry.id)).toEqual(FULL_ORDER);
  });
});

describe('buildBanners: daemon-recovery action button', () => {
  it('carries no action while retrying', () => {
    const result = buildBanners(baseState({ daemonRecovery: 'retrying' }));
    const entry = result.find((e) => e.id === 'daemon-recovery');
    expect(entry).toBeDefined();
    expect(entry?.action).toBeUndefined();
  });

  it('carries a reconnect action, with a label key and no function, once exhausted', () => {
    const result = buildBanners(baseState({ daemonRecovery: 'exhausted' }));
    const entry = result.find((e) => e.id === 'daemon-recovery');
    expect(entry).toBeDefined();
    expect(entry?.action).toEqual({ labelKey: 'reconnect' });
    expect(Object.values(entry?.action ?? {}).every((value) => typeof value !== 'function')).toBe(
      true
    );
  });
});

describe('dismissBanner: daemon-recovery and client-config-failure', () => {
  it('dismissing daemon-recovery hides it from buildBanners while leaving the status and generation untouched', () => {
    const state = baseState({
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 4,
      isDaemonRecoveryDismissed: false,
    });

    const before = buildBanners(state);
    expect(before.find((entry) => entry.id === 'daemon-recovery')).toBeDefined();

    const next = dismissBanner(state, 'daemon-recovery');
    expect(next.daemonRecovery).toBe('retrying');
    expect(next.daemonRecoveryRev).toBe(4);
    expect(next.isDaemonRecoveryDismissed).toBe(true);

    const after = buildBanners(next);
    expect(after.find((entry) => entry.id === 'daemon-recovery')).toBeUndefined();
  });

  it('dismissing client-config-failure clears the failure so it does not reappear', () => {
    const state = baseState({ clientConfigFailure: 'invalid_toml' });

    const before = buildBanners(state);
    expect(before.find((entry) => entry.id === 'client-config-failure')).toBeDefined();

    const next = dismissBanner(state, 'client-config-failure');
    expect(next.clientConfigFailure).toBeNull();

    const after = buildBanners(next);
    expect(after.find((entry) => entry.id === 'client-config-failure')).toBeUndefined();
  });
});

describe('applyRecoveryUpdate: generation-ordered adoption', () => {
  it('does not change state when the incoming generation equals the current one', () => {
    const state = baseState({
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 3,
      isDaemonRecoveryDismissed: true,
    });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 3 });
    expect(next).toEqual(state);
  });

  it('does not change state when the incoming generation is less than the current one', () => {
    const state = baseState({
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 3,
      isDaemonRecoveryDismissed: true,
    });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 2 });
    expect(next).toEqual(state);
  });

  it('adopts a larger generation carrying a different status, and clears the dismissed flag', () => {
    const state = baseState({
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 3,
      isDaemonRecoveryDismissed: true,
    });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 4 });
    expect(next.daemonRecovery).toBe('exhausted');
    expect(next.daemonRecoveryRev).toBe(4);
    expect(next.isDaemonRecoveryDismissed).toBe(false);
  });

  it('adopts a larger generation carrying the same status, without clearing the dismissed flag', () => {
    const state = baseState({
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 3,
      isDaemonRecoveryDismissed: true,
    });
    const next = applyRecoveryUpdate(state, { status: 'retrying', rev: 5 });
    expect(next.daemonRecovery).toBe('retrying');
    expect(next.daemonRecoveryRev).toBe(5);
    expect(next.isDaemonRecoveryDismissed).toBe(true);
  });

  it('does not go from idle to idle: the first adopted generation is 1, applied from the initial idle/0 state', () => {
    const state = baseState({ daemonRecovery: 'idle', daemonRecoveryRev: 0 });
    const next = applyRecoveryUpdate(state, { status: 'retrying', rev: 1 });
    expect(next.daemonRecovery).toBe('retrying');
    expect(next.daemonRecoveryRev).toBe(1);
  });

  it('leaves every other banner-relevant field untouched', () => {
    const state = baseState({
      daemonSpawnNotice: DAEMON_SPAWN_NOTICE,
      operationError: 'wiring-probe: unrelated error',
      daemonRecovery: 'retrying',
      daemonRecoveryRev: 1,
    });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 2 });
    expect(next.daemonSpawnNotice).toBe(DAEMON_SPAWN_NOTICE);
    expect(next.operationError).toBe('wiring-probe: unrelated error');
  });
});

describe('applyClientConfigFailure: replaces or clears the failure value', () => {
  it('sets the failure when the current value is null', () => {
    const state = baseState({ clientConfigFailure: null });
    const next = applyClientConfigFailure(state, 'read_failed');
    expect(next.clientConfigFailure).toBe('read_failed');
  });

  it('replaces one failure value with another', () => {
    const state = baseState({ clientConfigFailure: 'not_found' });
    const next = applyClientConfigFailure(state, 'too_large');
    expect(next.clientConfigFailure).toBe('too_large');
  });

  it('clears the failure when passed null', () => {
    const state = baseState({ clientConfigFailure: 'invalid_toml' });
    const next = applyClientConfigFailure(state, null);
    expect(next.clientConfigFailure).toBeNull();
  });

  it('leaves every other banner-relevant field untouched', () => {
    const state = baseState({
      boundaryNotice: 'wiring-probe: boundary notice',
      clientConfigFailure: null,
    });
    const next = applyClientConfigFailure(state, 'read_failed');
    expect(next.boundaryNotice).toBe('wiring-probe: boundary notice');
  });
});
