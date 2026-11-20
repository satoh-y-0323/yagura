import { describe, it, expect } from 'vitest';

import {
  buildNotices,
  classifyNotice,
  dismissNotice,
  reopenOffer,
  applyRecoveryUpdate,
  applyClientConfigFailure,
  recordApprovalExpiredUnseen,
  dismissApprovalExpiredUnseen,
  profilesNeedingNativeProgram,
} from './notice-model';
import type {
  ApprovalExpiredSummary,
  NoticeAction,
  NoticeClass,
  NoticeDraft,
  NoticeFacts,
  NoticeKey,
  NoticeSourceState,
} from './notice-model';
import { banners, describeReopenNotice, describeThemeIssues } from '../ui-messages';

const DAEMON_SPAWN_NOTICE = 'spawn-probe: started without the strongest detachment mode';
const DAEMON_HEALTH_NOTICE = 'health-probe: windows cannot be opened';

function baseState(overrides: Partial<NoticeSourceState> = {}): NoticeSourceState {
  return {
    daemonSpawnNotice: null,
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
    clientConfigFailure: null,
    isDaemonProtocolStale: false,
    startupEnvFailure: null,
    profilesNeedingNativeProgram: [],
    isProfileUnavailableDismissed: false,
    daemonHealthNotices: [],
    lastSession: null,
    isReopenOfferDismissed: false,
    hasOpenWindows: false,
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
const LAST_SESSION = { count: 3, timestamp: '2026-11-20 09:15' };

/** The sentence that states the fact: the detail under a blocking title, else the title itself. */
function sentenceOf(draft: NoticeDraft): string {
  return draft.detail ?? draft.title;
}

function shown(drafts: readonly NoticeDraft[]) {
  return drafts.map((draft) => ({
    key: draft.key,
    sentence: sentenceOf(draft),
    action: draft.action,
  }));
}

type ShownNotice = { key: NoticeKey; sentence: string; action: NoticeAction | null };

/** Every state notice: the state that turns it on, and what it then shows. */
const NOTICES: { label: string; on: Partial<NoticeSourceState>; notice: ShownNotice }[] = [
  {
    label: 'startup-env-failure (invalid_instance_id)',
    on: { startupEnvFailure: 'invalid_instance_id' },
    notice: { key: 'startup-env-failure', sentence: banners.invalidInstanceId, action: null },
  },
  {
    label: 'startup-env-failure (store_override_with_named_instance)',
    on: { startupEnvFailure: 'store_override_with_named_instance' },
    notice: {
      key: 'startup-env-failure',
      sentence: banners.storeOverrideWithNamedInstance,
      action: null,
    },
  },
  {
    label: 'daemon-recovery (retrying)',
    on: { daemonRecovery: 'retrying' },
    notice: { key: 'daemon-recovery', sentence: banners.daemonReconnecting, action: null },
  },
  {
    label: 'daemon-recovery (exhausted)',
    on: { daemonRecovery: 'exhausted' },
    notice: {
      key: 'daemon-recovery',
      sentence: banners.daemonReconnectFailed,
      action: { labelKey: 'reconnect' },
    },
  },
  {
    label: 'daemon-health',
    on: { daemonHealthNotices: [DAEMON_HEALTH_NOTICE] },
    notice: { key: 'daemon-health', sentence: DAEMON_HEALTH_NOTICE, action: null },
  },
  {
    label: 'approver-registration-refused',
    on: { approverRegistrationRefused: true },
    notice: {
      key: 'approver-registration-refused',
      sentence: banners.approverRegistrationRefused,
      action: null,
    },
  },
  {
    label: 'daemon-protocol-stale',
    on: { isDaemonProtocolStale: true },
    notice: { key: 'daemon-protocol-stale', sentence: banners.daemonProtocolStale, action: null },
  },
  {
    label: 'daemon-spawn',
    on: { daemonSpawnNotice: DAEMON_SPAWN_NOTICE },
    notice: { key: 'daemon-spawn', sentence: DAEMON_SPAWN_NOTICE, action: null },
  },
  {
    label: 'client-config-failure',
    on: { clientConfigFailure: 'read_failed' },
    notice: {
      key: 'client-config-failure',
      sentence: banners.clientConfigLoadFailed,
      action: null,
    },
  },
  {
    label: 'detection-warning',
    on: { detectionWarning: 'probe: detection warning' },
    notice: { key: 'detection-warning', sentence: 'probe: detection warning', action: null },
  },
  {
    label: 'keymap-notice',
    on: { keymapNotice: 'probe: keymap notice' },
    notice: { key: 'keymap-notice', sentence: 'probe: keymap notice', action: null },
  },
  {
    label: 'boundary-error',
    on: { boundaryNotice: 'probe: boundary notice' },
    notice: { key: 'boundary-error', sentence: 'probe: boundary notice', action: null },
  },
  {
    label: 'themes-load-failure',
    on: { hasThemeLoadFailure: true },
    notice: { key: 'themes-load-failure', sentence: banners.themesLoadFailed, action: null },
  },
  {
    label: 'themes-issues',
    on: { themeIssueCount: 4 },
    notice: { key: 'themes-issues', sentence: describeThemeIssues(4), action: null },
  },
  {
    label: 'approval-expired-unseen',
    on: { approvalExpiredUnseen: { count: 2, latest: LATEST } },
    notice: {
      key: 'approval-expired-unseen',
      sentence: banners.approvalExpiredUnseen(2, LATEST),
      action: null,
    },
  },
  {
    label: 'profile-unavailable',
    on: { profilesNeedingNativeProgram: [{ id: 'codex-cli', displayName: 'Codex CLI' }] },
    notice: {
      key: 'profile-unavailable',
      sentence: banners.profileNeedsNativeProgram(['Codex CLI']),
      action: { labelKey: 'howToInstall', profileId: 'codex-cli' },
    },
  },
  {
    label: 'reopen-offer',
    on: { lastSession: LAST_SESSION, hasOpenWindows: true },
    notice: {
      key: 'reopen-offer',
      sentence: describeReopenNotice(LAST_SESSION.count, LAST_SESSION.timestamp),
      action: { labelKey: 'chooseWindows' },
    },
  },
];

const BLOCKING_LABELS = new Set([
  'startup-env-failure (invalid_instance_id)',
  'startup-env-failure (store_override_with_named_instance)',
  'daemon-recovery (retrying)',
  'daemon-recovery (exhausted)',
  'daemon-health',
  'approver-registration-refused',
  'daemon-protocol-stale',
]);

/** One row per state notice key, so every key is switched on together. */
function allActiveState(): NoticeSourceState {
  const once = new Set(['startup-env-failure (store_override_with_named_instance)']);
  return NOTICES.filter(
    (row) => !once.has(row.label) && row.label !== 'daemon-recovery (exhausted)'
  ).reduce<NoticeSourceState>((state, row) => ({ ...state, ...row.on }), baseState());
}

const NOT_BLOCKING_SETUP: NoticeClass = {
  blocking: false,
  tone: 'warning',
  dismissible: true,
  category: 'setup-problems',
};

/** Every key and the facts it is classified under, with what it is. */
const CLASSES: Record<NoticeKey, readonly { facts: NoticeFacts; expected: NoticeClass }[]> = {
  'startup-env-failure': [
    {
      facts: {},
      expected: { blocking: true, tone: 'error', dismissible: false, category: null },
    },
  ],
  'daemon-recovery': [
    {
      facts: { daemonRecovery: 'exhausted' },
      expected: { blocking: true, tone: 'error', dismissible: false, category: null },
    },
    {
      facts: { daemonRecovery: 'retrying' },
      expected: { blocking: true, tone: 'info', dismissible: false, category: null },
    },
  ],
  'daemon-health': [
    {
      facts: {},
      expected: { blocking: true, tone: 'error', dismissible: false, category: null },
    },
  ],
  'approver-registration-refused': [
    {
      facts: {},
      expected: { blocking: true, tone: 'warning', dismissible: false, category: null },
    },
  ],
  'daemon-protocol-stale': [
    {
      facts: {},
      expected: { blocking: true, tone: 'warning', dismissible: false, category: null },
    },
  ],
  'daemon-spawn': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'client-config-failure': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'detection-warning': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'keymap-notice': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'themes-load-failure': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'themes-issues': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'profile-unavailable': [{ facts: {}, expected: NOT_BLOCKING_SETUP }],
  'boundary-error': [
    {
      facts: {},
      expected: { blocking: false, tone: 'error', dismissible: true, category: 'failed-actions' },
    },
  ],
  // Expired permission requests are a safety gate the table cannot switch off.
  'approval-expired-unseen': [
    {
      facts: {},
      expected: { blocking: false, tone: 'warning', dismissible: true, category: null },
    },
  ],
  'reopen-offer': [
    {
      facts: {},
      expected: { blocking: false, tone: 'info', dismissible: true, category: 'last-session' },
    },
  ],
  'daemon-reconnected': [
    {
      facts: {},
      expected: { blocking: false, tone: 'info', dismissible: true, category: 'reconnected' },
    },
  ],
  'operation-failure': [
    {
      facts: {},
      expected: { blocking: false, tone: 'error', dismissible: true, category: 'failed-actions' },
    },
  ],
  'window-exit': [
    {
      facts: { exitCode: 0 },
      expected: { blocking: false, tone: 'info', dismissible: true, category: 'window-exits' },
    },
    {
      facts: { exitCode: 1 },
      expected: { blocking: false, tone: 'warning', dismissible: true, category: 'window-exits' },
    },
    {
      facts: { exitCode: null },
      expected: { blocking: false, tone: 'warning', dismissible: true, category: 'window-exits' },
    },
  ],
  'restore-load-failed': [
    {
      facts: {},
      expected: { blocking: false, tone: 'warning', dismissible: true, category: 'failed-actions' },
    },
  ],
  'restore-not-restored': [
    {
      facts: {},
      expected: { blocking: false, tone: 'warning', dismissible: true, category: 'failed-actions' },
    },
  ],
};

describe('classifyNotice', () => {
  it.each(
    (Object.entries(CLASSES) as [NoticeKey, (typeof CLASSES)[NoticeKey]][]).flatMap(([key, rows]) =>
      rows.map((row) => [key, JSON.stringify(row.facts), row] as const)
    )
  )('classifies %s under %s', (key, _facts, row) => {
    expect(classifyNotice(key, row.facts)).toEqual(row.expected);
  });
});

describe('buildNotices', () => {
  it('returns no notice when nothing is active', () => {
    expect(buildNotices(baseState())).toEqual([]);
  });

  it.each(NOTICES.map((row) => [row.label, row] as const))(
    'shows %s alone, with its sentence and action, when only its condition holds',
    (label, row) => {
      const drafts = buildNotices(baseState(row.on));
      expect(shown(drafts)).toEqual([row.notice]);

      // A blocking notice carries a one-line title of its own above the sentence;
      // any other notice is its sentence alone.
      const [draft] = drafts;
      if (BLOCKING_LABELS.has(label)) {
        expect(draft?.detail).toBe(row.notice.sentence);
        expect(draft?.title).not.toBe('');
        expect(draft?.title).not.toBe(draft?.detail);
      } else {
        expect(draft?.detail).toBeNull();
      }
    }
  );

  it.each([['retrying'], ['exhausted']] as const)(
    'orders the blocking notices by cause, with recovery %s',
    (daemonRecovery) => {
      const state = baseState({
        startupEnvFailure: 'invalid_instance_id',
        daemonRecovery,
        daemonHealthNotices: ['health-probe: first', 'health-probe: second'],
        approverRegistrationRefused: true,
        isDaemonProtocolStale: true,
      });

      expect(buildNotices(state).map((draft) => draft.key)).toEqual([
        'startup-env-failure',
        'daemon-recovery',
        'daemon-health',
        'daemon-health',
        'approver-registration-refused',
        'daemon-protocol-stale',
      ]);
    }
  );

  it.each([['detectionWarning'], ['keymapNotice'], ['boundaryNotice']] as const)(
    'shows the notice for an empty %s: only null means none',
    (field) => {
      expect(buildNotices(baseState({ [field]: '' })).map(sentenceOf)).toEqual(['']);
    }
  );

  it.each([
    [
      'a dismissed theme-load failure',
      { hasThemeLoadFailure: true, isThemeLoadFailureDismissed: true },
    ],
    ['dismissed theme issues', { themeIssueCount: 3, isThemeIssuesDismissed: true }],
    ['a zero theme-issue count', { themeIssueCount: 0 }],
    ['a negative theme-issue count', { themeIssueCount: -3 }],
  ] as const)('shows nothing for %s', (_label, overrides) => {
    expect(buildNotices(baseState(overrides))).toEqual([]);
  });

  it('shows themes-issues from a count of exactly 1', () => {
    expect(shown(buildNotices(baseState({ themeIssueCount: 1 })))).toEqual([
      { key: 'themes-issues', sentence: describeThemeIssues(1), action: null },
    ]);
  });

  // An absent settings file is the ordinary state of a fresh install.
  it('shows no client-config-failure for a settings file that is not there', () => {
    expect(buildNotices(baseState({ clientConfigFailure: 'not_found' }))).toEqual([]);
  });

  it('names every profile needing a native program and points the action at the first', () => {
    const drafts = buildNotices(
      baseState({
        profilesNeedingNativeProgram: [
          { id: 'codex-cli', displayName: 'Codex CLI' },
          { id: 'other-cli', displayName: 'Other CLI' },
        ],
      })
    );

    expect(shown(drafts)).toEqual([
      {
        key: 'profile-unavailable',
        sentence: banners.profileNeedsNativeProgram(['Codex CLI', 'Other CLI']),
        action: { labelKey: 'howToInstall', profileId: 'codex-cli' },
      },
    ]);
  });

  // With no window open the empty work area makes the offer, so the notice does not.
  it('leaves the last-session offer to the empty work area while no window is open', () => {
    const state = baseState({ lastSession: LAST_SESSION, hasOpenWindows: false });

    expect(buildNotices(state)).toEqual([]);
    expect(reopenOffer(state)).toEqual(LAST_SESSION);
  });
});

describe('dismissNotice', () => {
  const dismissible = NOTICES.filter((row) => !BLOCKING_LABELS.has(row.label));
  const blocking = NOTICES.filter(
    (row) => BLOCKING_LABELS.has(row.label) && row.label !== 'daemon-recovery (exhausted)'
  ).filter((row) => row.label !== 'startup-env-failure (store_override_with_named_instance)');

  it.each(dismissible.map((row) => [row.notice.key] as const))(
    'dismissing %s drops only that notice and keeps the rest in order',
    (key) => {
      const state = allActiveState();
      const before = buildNotices(state).map((draft) => draft.key);
      const next = dismissNotice(state, key as Parameters<typeof dismissNotice>[1]);

      expect(buildNotices(next).map((draft) => draft.key)).toEqual(
        before.filter((other) => other !== key)
      );
      expect(next.hasThemeLoadFailure).toBe(state.hasThemeLoadFailure);
      expect(next.themeIssueCount).toBe(state.themeIssueCount);
    }
  );

  it.each(blocking.map((row) => [row.notice.key] as const))(
    'dismissing the blocking %s leaves every notice in place',
    (key) => {
      const state = allActiveState();
      const next = dismissNotice(state, key as Parameters<typeof dismissNotice>[1]);

      expect(buildNotices(next)).toEqual(buildNotices(state));
    }
  );

  it('dismissing reopen-offer withdraws the offer from the empty work area too', () => {
    const state = baseState({ lastSession: LAST_SESSION, hasOpenWindows: true });
    expect(reopenOffer(state)).toEqual(LAST_SESSION);

    const next = dismissNotice(state, 'reopen-offer');

    expect(reopenOffer(next)).toBeNull();
    expect(reopenOffer({ ...next, hasOpenWindows: false })).toBeNull();
  });
});

describe('recordApprovalExpiredUnseen', () => {
  it('starts the count at 1 with the given summary as latest, from a null state (reason: timeout), and leaves every other field untouched', () => {
    const latest = summary();
    const state = baseState({
      daemonSpawnNotice: DAEMON_SPAWN_NOTICE,
      boundaryNotice: 'probe: boundary notice',
    });
    const next = recordApprovalExpiredUnseen(state, false, 'timeout', latest);
    expect(next).toEqual({ ...state, approvalExpiredUnseen: { count: 1, latest } });
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
  it('clears the count and leaves every other notice-relevant field untouched', () => {
    const state = baseState({
      boundaryNotice: 'probe: boundary notice',
      approvalExpiredUnseen: { count: 5, latest: summary() },
    });
    const next = dismissApprovalExpiredUnseen(state);
    expect(next.approvalExpiredUnseen).toBeNull();
    expect(next.boundaryNotice).toBe('probe: boundary notice');
  });
});

describe('profilesNeedingNativeProgram', () => {
  function entry(id: string, unavailableKind: string | null, enabled = true) {
    return { id, displayName: id.toUpperCase(), enabled, unavailableKind };
  }

  it('keeps only the enabled profiles whose reason is a .cmd / .bat program, in list order', () => {
    expect(
      profilesNeedingNativeProgram([
        entry('a', 'needs_native_program'),
        entry('b', 'not_found'),
        entry('c', null),
        entry('d', 'needs_native_program', false),
        entry('e', 'needs_native_program'),
      ])
    ).toEqual([
      { id: 'a', displayName: 'A' },
      { id: 'e', displayName: 'E' },
    ]);
  });
});

describe('applyRecoveryUpdate: generation-ordered adoption', () => {
  it('does not change state when the incoming generation equals the current one', () => {
    const state = baseState({ daemonRecovery: 'retrying', daemonRecoveryRev: 3 });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 3 });
    expect(next).toEqual(state);
  });

  it('does not change state when the incoming generation is less than the current one', () => {
    const state = baseState({ daemonRecovery: 'retrying', daemonRecoveryRev: 3 });
    const next = applyRecoveryUpdate(state, { status: 'exhausted', rev: 2 });
    expect(next).toEqual(state);
  });

  it('does not go from idle to idle: the first adopted generation is 1, applied from the initial idle/0 state, and every other field stays untouched', () => {
    const state = baseState({
      daemonSpawnNotice: DAEMON_SPAWN_NOTICE,
      detectionWarning: 'probe: unrelated warning',
      daemonRecovery: 'idle',
      daemonRecoveryRev: 0,
    });
    const next = applyRecoveryUpdate(state, { status: 'retrying', rev: 1 });
    expect(next).toEqual({ ...state, daemonRecovery: 'retrying', daemonRecoveryRev: 1 });
  });
});

describe('applyClientConfigFailure', () => {
  // Every reason is kept, including the one no notice is shown for, so an
  // absent file stays distinguishable from an unusable one.
  it('keeps the reason given, including "not_found" which raises no notice, and leaves every other field untouched', () => {
    const state = baseState({ boundaryNotice: 'wiring-probe: boundary notice' });
    const shownFailure = applyClientConfigFailure(state, 'read_failed');
    expect(shownFailure).toEqual({ ...state, clientConfigFailure: 'read_failed' });
    const next = applyClientConfigFailure(shownFailure, 'not_found');
    expect(next).toEqual({ ...state, clientConfigFailure: 'not_found' });
  });

  it('clears the failure when passed null', () => {
    const state = baseState({ clientConfigFailure: 'invalid_toml' });
    const next = applyClientConfigFailure(state, null);
    expect(next.clientConfigFailure).toBeNull();
  });
});
