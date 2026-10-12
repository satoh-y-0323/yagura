/**
 * Each facade function is the matching renderer bound to the resolved locale: calling it
 * gives what the renderer gives for that locale.
 */
import { describe, it, expect } from 'vitest';

import { resolveMessages } from './i18n/messages';
import * as render from './i18n/render';
import * as facade from './ui-messages';
import type { DetectedStatusView } from './ipc/session-boundary';
import type { ConfirmTarget } from './session/confirm-target';

const messages = resolveMessages();

const UNKNOWN_WITH_REASON: DetectedStatusView = { status: 'unknown', reason: 'no_rule_matched' };
const BLOCKED: ConfirmTarget = { allowed: false, requiresConfirmation: false, reason: 'exited' };

const CASES: ReadonlyArray<[string, () => unknown, () => unknown]> = [
  [
    'describeDeliveryState',
    () => facade.describeDeliveryState('confirmed'),
    () => render.describeDeliveryState(messages, 'confirmed'),
  ],
  [
    'describeHaltReason',
    () => facade.describeHaltReason('repetition'),
    () => render.describeHaltReason(messages, 'repetition'),
  ],
  [
    'describeThemeName',
    () => facade.describeThemeName('dark'),
    () => render.describeThemeName(messages, 'dark'),
  ],
  [
    'describeDetectionWarning',
    () => facade.describeDetectionWarning('bad toml'),
    () => render.describeDetectionWarning(messages, 'bad toml'),
  ],
  [
    'describeWindowListTitle',
    () => facade.describeWindowListTitle(7),
    () => render.describeWindowListTitle(messages, 7),
  ],
  ['describeExit', () => facade.describeExit(3), () => render.describeExit(messages, 3)],
  [
    'describeStatus',
    () => facade.describeStatus('running'),
    () => render.describeStatus(messages, 'running'),
  ],
  [
    'describePaneNotice',
    () => facade.describePaneNotice('failed'),
    () => render.describePaneNotice(messages, 'failed'),
  ],
  [
    'describeExitCode',
    () => facade.describeExitCode(5),
    () => render.describeExitCode(messages, 5),
  ],
  [
    'describeDiscardedKeymapEntries',
    () => facade.describeDiscardedKeymapEntries(2),
    () => render.describeDiscardedKeymapEntries(messages, 2),
  ],
  [
    'describeThemeIssues',
    () => facade.describeThemeIssues(2),
    () => render.describeThemeIssues(messages, 2),
  ],
  [
    'describeAttentionBadge',
    () => facade.describeAttentionBadge(4),
    () => render.describeAttentionBadge(messages, 4),
  ],
  [
    'describeRestoreBanner',
    () => facade.describeRestoreBanner(3, '2026-01-02 03:04'),
    () => render.describeRestoreBanner(messages, 3, '2026-01-02 03:04'),
  ],
  [
    'describeRestoreSelectionCount',
    () => facade.describeRestoreSelectionCount(2, 5),
    () => render.describeRestoreSelectionCount(messages, 2, 5),
  ],
  [
    'describeRoomTitle',
    () => facade.describeRoomTitle('main', 2),
    () => render.describeRoomTitle(messages, 'main', 2),
  ],
  [
    'describeUnknownBadge',
    () => facade.describeUnknownBadge(3),
    () => render.describeUnknownBadge(messages, 3),
  ],
  [
    'describeUnknownReasonSuffix',
    () => facade.describeUnknownReasonSuffix(UNKNOWN_WITH_REASON),
    () => render.describeUnknownReasonSuffix(messages, UNKNOWN_WITH_REASON),
  ],
  [
    'describeUnknownExplanation',
    () => facade.describeUnknownExplanation(UNKNOWN_WITH_REASON, 'a warning'),
    () => render.describeUnknownExplanation(messages, UNKNOWN_WITH_REASON, 'a warning'),
  ],
  [
    'describeUndeliveredUnknownReason',
    () =>
      facade.describeUndeliveredUnknownReason({
        state: 'undelivered_unknown',
        reason: 'screen_lost',
      }),
    () =>
      render.describeUndeliveredUnknownReason(messages, {
        state: 'undelivered_unknown',
        reason: 'screen_lost',
      }),
  ],
  [
    'describeInterruptHint',
    () => facade.describeInterruptHint(BLOCKED),
    () => render.describeInterruptHint(messages, BLOCKED),
  ],
];

describe('the ui-messages facade', () => {
  it.each(CASES)(
    '%s renders what the renderer renders for the resolved locale',
    (_name, viaFacade, direct) => {
      const expected = direct();
      expect(expected).not.toBeUndefined();
      expect(viaFacade()).toEqual(expected);
    }
  );
});
