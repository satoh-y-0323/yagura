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
const HEADER_FACTS: render.WindowHeaderFacts = {
  vendor: 'claude-code',
  cwd: '/work/yagura',
  showsMcpMark: true,
  showsStatusReportMark: false,
  queuedCount: 2,
};

const CASES: ReadonlyArray<[string, () => unknown, () => unknown]> = [
  [
    'describeDeliveryState',
    () => facade.describeDeliveryState('taken'),
    () => render.describeDeliveryState(messages, 'taken'),
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
    'describeWindowRow',
    () => facade.describeWindowRow('reviewer', 'waiting_input', true),
    () => render.describeWindowRow(messages, 'reviewer', 'waiting_input', true),
  ],
  [
    'describeWindowHeader',
    () => facade.describeWindowHeader(HEADER_FACTS),
    () => render.describeWindowHeader(messages, HEADER_FACTS),
  ],
  [
    'describeWindowLocation',
    () => facade.describeWindowLocation(HEADER_FACTS),
    () => render.describeWindowLocation(messages, HEADER_FACTS),
  ],
  [
    'describeExitedWindow',
    () => facade.describeExitedWindow('reviewer', 1),
    () => render.describeExitedWindow(messages, 'reviewer', 1),
  ],
  [
    'describeGoToWindow',
    () => facade.describeGoToWindow('implementer'),
    () => render.describeGoToWindow(messages, 'implementer'),
  ],
  [
    'describeWindowCall',
    () => facade.describeWindowCall('reviewer', 'waiting_approval'),
    () => render.describeWindowCall(messages, 'reviewer', 'waiting_approval'),
  ],
  [
    'describeWindowCallParts',
    () => facade.describeWindowCallParts('reviewer', 'waiting_input'),
    () => render.describeWindowCallParts(messages, 'reviewer', 'waiting_input'),
  ],
  [
    'describeExitedWindowParts',
    () => facade.describeExitedWindowParts('reviewer', 1),
    () => render.describeExitedWindowParts(messages, 'reviewer', 1),
  ],
  [
    'describeNoticeAndMore',
    () => facade.describeNoticeAndMore('builder exited', 3),
    () => render.describeNoticeAndMore(messages, 'builder exited', 3),
  ],
  [
    'describeEventTime',
    () => facade.describeEventTime(new Date(2026, 0, 2, 0, 5)),
    () => render.describeEventTime(messages, new Date(2026, 0, 2, 0, 5)),
  ],
  [
    'describeAnnouncement',
    () => facade.describeAnnouncement(['one', 'two']),
    () => render.describeAnnouncement(messages, ['one', 'two']),
  ],
  [
    'describeReopenOffer',
    () => facade.describeReopenOffer(6, '2026-01-02 03:04'),
    () => render.describeReopenOffer(messages, 6, '2026-01-02 03:04'),
  ],
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
    'describeReopenNotice',
    () => facade.describeReopenNotice(3, '2026-01-02 03:04'),
    () => render.describeReopenNotice(messages, 3, '2026-01-02 03:04'),
  ],
  [
    'describeRestoreSelectionCount',
    () => facade.describeRestoreSelectionCount(2, 5),
    () => render.describeRestoreSelectionCount(messages, 2, 5),
  ],
  [
    'describeRestoreConfirm',
    () => facade.describeRestoreConfirm(2),
    () => render.describeRestoreConfirm(messages, 2),
  ],
  [
    'describeRoomTitle',
    () => facade.describeRoomTitle('main', 2),
    () => render.describeRoomTitle(messages, 'main', 2),
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
