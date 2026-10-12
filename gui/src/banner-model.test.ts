import { describe, it, expect } from 'vitest';

import {
  buildBanners,
  dismissBanner,
  recordApprovalExpiredUnseen,
  dismissApprovalExpiredUnseen,
} from './banner-model';
import type { BannerEntry, BannerId, BannerState, ApprovalExpiredSummary } from './banner-model';
import { banners, describeThemeIssues } from './ui-messages';

const DAEMON_SPAWN_NOTICE = 'spawn-probe: started without the strongest detachment mode';

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

function summary(overrides: Partial<ApprovalExpiredSummary> = {}): ApprovalExpiredSummary {
  return {
    requester: 'builder window',
    tool: 'close_window',
    target: 'target window',
    ...overrides,
  };
}

const LATEST = summary();

/** Every banner in display order: the state that turns it on, and what it then shows. */
const BANNERS: { on: Partial<BannerState>; entry: BannerEntry }[] = [
  {
    on: { startupEnvFailure: 'invalid_instance_id' },
    entry: { id: 'startup-env-failure', text: banners.invalidInstanceId },
  },
  {
    on: { daemonSpawnNotice: DAEMON_SPAWN_NOTICE },
    entry: { id: 'daemon-spawn', text: DAEMON_SPAWN_NOTICE },
  },
  {
    on: { daemonRecovery: 'retrying' },
    entry: { id: 'daemon-recovery', text: banners.daemonReconnecting },
  },
  {
    on: { clientConfigFailure: 'read_failed' },
    entry: { id: 'client-config-failure', text: banners.clientConfigLoadFailed },
  },
  {
    on: { operationError: 'probe: operation error' },
    entry: { id: 'operation-error', text: 'probe: operation error' },
  },
  {
    on: { detectionWarning: 'probe: detection warning' },
    entry: { id: 'detection-warning', text: 'probe: detection warning' },
  },
  {
    on: { keymapNotice: 'probe: keymap notice' },
    entry: { id: 'keymap-notice', text: 'probe: keymap notice' },
  },
  {
    on: { boundaryNotice: 'probe: boundary notice' },
    entry: { id: 'boundary-error', text: 'probe: boundary notice' },
  },
  {
    on: { hasThemeLoadFailure: true },
    entry: { id: 'themes-load-failure', text: banners.themesLoadFailed },
  },
  {
    on: { themeIssueCount: 4 },
    entry: { id: 'themes-issues', text: describeThemeIssues(4) },
  },
  {
    on: { approvalExpiredUnseen: { count: 2, latest: LATEST } },
    entry: { id: 'approval-expired-unseen', text: banners.approvalExpiredUnseen(2, LATEST) },
  },
  {
    on: { approverRegistrationRefused: true },
    entry: { id: 'approver-registration-refused', text: banners.approverRegistrationRefused },
  },
  {
    on: { isDaemonProtocolStale: true },
    entry: { id: 'daemon-protocol-stale', text: banners.daemonProtocolStale },
  },
];

const ALL_IDS: BannerId[] = BANNERS.map((banner) => banner.entry.id);

function allActiveState(): BannerState {
  return BANNERS.reduce<BannerState>((state, banner) => ({ ...state, ...banner.on }), baseState());
}

describe('buildBanners', () => {
  it('returns no banner when nothing is active', () => {
    expect(buildBanners(baseState())).toEqual([]);
  });

  it.each(BANNERS.map((banner) => [banner.entry.id, banner] as const))(
    'shows %s alone, with its text, when only its condition holds',
    (_id, banner) => {
      expect(buildBanners(baseState(banner.on))).toEqual([banner.entry]);
    }
  );

  it('shows every active banner in the fixed display order', () => {
    expect(buildBanners(allActiveState())).toEqual(BANNERS.map((banner) => banner.entry));
  });

  it.each([
    ['operationError'],
    ['detectionWarning'],
    ['keymapNotice'],
    ['boundaryNotice'],
  ] as const)('shows the banner for an empty %s: only null means none', (field) => {
    expect(buildBanners(baseState({ [field]: '' })).map((entry) => entry.text)).toEqual(['']);
  });

  it.each([
    [
      'a dismissed theme-load failure',
      { hasThemeLoadFailure: true, isThemeLoadFailureDismissed: true },
    ],
    ['dismissed theme issues', { themeIssueCount: 3, isThemeIssuesDismissed: true }],
    ['a zero theme-issue count', { themeIssueCount: 0 }],
    ['a negative theme-issue count', { themeIssueCount: -3 }],
  ] as const)('shows nothing for %s', (_label, overrides) => {
    expect(buildBanners(baseState(overrides))).toEqual([]);
  });

  it('shows themes-issues from a count of exactly 1', () => {
    expect(buildBanners(baseState({ themeIssueCount: 1 }))).toEqual([
      { id: 'themes-issues', text: describeThemeIssues(1) },
    ]);
  });
});

describe('dismissBanner', () => {
  it.each(ALL_IDS.map((id) => [id]))(
    'dismissing %s drops only that banner and keeps the rest in order',
    (id) => {
      const state = allActiveState();
      const next = dismissBanner(state, id);

      expect(buildBanners(next).map((entry) => entry.id)).toEqual(
        ALL_IDS.filter((other) => other !== id)
      );
      expect(next.hasThemeLoadFailure).toBe(state.hasThemeLoadFailure);
      expect(next.themeIssueCount).toBe(state.themeIssueCount);
    }
  );
});

describe('recordApprovalExpiredUnseen', () => {
  it('starts the count at 1 with the given summary as latest, from a null state (reason: timeout)', () => {
    const latest = summary();
    const next = recordApprovalExpiredUnseen(baseState(), false, 'timeout', latest);
    expect(next.approvalExpiredUnseen).toEqual({ count: 1, latest });
  });

  it('increments count and overwrites latest on a second unseen expiry (reason: timeout)', () => {
    const first = summary({ requester: 'window one' });
    const second = summary({ requester: 'window two' });
    let state = baseState();
    state = recordApprovalExpiredUnseen(state, false, 'timeout', first);
    state = recordApprovalExpiredUnseen(state, false, 'timeout', second);

    expect(state.approvalExpiredUnseen).toEqual({ count: 2, latest: second });
  });

  it('does not change state when the request had already been shown', () => {
    const state = baseState({ approvalExpiredUnseen: { count: 1, latest: summary() } });
    const next = recordApprovalExpiredUnseen(
      state,
      true,
      'timeout',
      summary({ requester: 'other window' })
    );
    expect(next.approvalExpiredUnseen).toEqual(state.approvalExpiredUnseen);
  });

  it('leaves every other banner-relevant field untouched', () => {
    const state = baseState({ daemonSpawnNotice: DAEMON_SPAWN_NOTICE, operationError: 'boom' });
    const next = recordApprovalExpiredUnseen(state, false, 'timeout', summary());
    expect(next.daemonSpawnNotice).toBe(DAEMON_SPAWN_NOTICE);
    expect(next.operationError).toBe('boom');
  });

  // Only an actual timeout is an "expired unseen" request: a decision made
  // elsewhere, a disconnected requester, or a reason this build does not know is not.
  it.each([['resolved'], ['requester_disconnected'], ['unknown']] as const)(
    'does not count an unseen withdrawal whose reason is "%s"',
    (reason) => {
      const next = recordApprovalExpiredUnseen(baseState(), false, reason, summary());
      expect(next.approvalExpiredUnseen).toBeNull();
    }
  );

  it('does not change state for a non-timeout reason even when a count is already pending', () => {
    const state = baseState({ approvalExpiredUnseen: { count: 1, latest: summary() } });
    const next = recordApprovalExpiredUnseen(
      state,
      false,
      'resolved',
      summary({ requester: 'other window' })
    );
    expect(next.approvalExpiredUnseen).toEqual(state.approvalExpiredUnseen);
  });
});

describe('dismissApprovalExpiredUnseen', () => {
  it('clears the count and leaves every other banner-relevant field untouched', () => {
    const state = baseState({
      boundaryNotice: 'probe: boundary notice',
      approvalExpiredUnseen: { count: 5, latest: summary() },
    });
    const next = dismissApprovalExpiredUnseen(state);
    expect(next.approvalExpiredUnseen).toBeNull();
    expect(next.boundaryNotice).toBe('probe: boundary notice');
  });
});
