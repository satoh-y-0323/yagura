/**
 * Each facade function is the matching renderer bound to the resolved locale: calling it
 * gives what the renderer gives for that locale.
 */
import { describe, it, expect } from 'vitest';

import { resolveMessages } from './i18n/messages';
import * as render from './i18n/render';
import * as facade from './ui-messages';
import { formatPathForDisplay, sanitizeForDisplay } from './display/untrusted-text';
import type { DetectedStatusView } from './ipc/session-boundary';
import type { ConfirmTarget } from './session/confirm-target';

const messages = resolveMessages();

const UNKNOWN_WITH_REASON: DetectedStatusView = { status: 'unknown', reason: 'no_rule_matched' };
const BLOCKED: ConfirmTarget = { allowed: false, requiresConfirmation: false, reason: 'exited' };
const REVIEWER = sanitizeForDisplay('reviewer');
const IMPLEMENTER = sanitizeForDisplay('implementer');
const BUILDER_EXITED = sanitizeForDisplay('builder exited');
const ANNOUNCED = [sanitizeForDisplay('one'), sanitizeForDisplay('two')];
const WARNING = sanitizeForDisplay('a warning');
const SAVED_AT_EPOCH_S = 1_767_315_840;
const HEADER_FACTS: render.WindowHeaderFacts = {
  vendor: sanitizeForDisplay('claude-code'),
  cwd: formatPathForDisplay('/work/yagura'),
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
    () => facade.describeWindowRow(REVIEWER, 'waiting_input', true),
    () => render.describeWindowRow(messages, REVIEWER, 'waiting_input', true),
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
    () => facade.describeExitedWindow(REVIEWER, 1),
    () => render.describeExitedWindow(messages, REVIEWER, 1),
  ],
  [
    'describeGoToWindow',
    () => facade.describeGoToWindow(IMPLEMENTER),
    () => render.describeGoToWindow(messages, IMPLEMENTER),
  ],
  [
    'describeWindowCall',
    () => facade.describeWindowCall(REVIEWER, 'waiting_approval'),
    () => render.describeWindowCall(messages, REVIEWER, 'waiting_approval'),
  ],
  [
    'describeWindowCallParts',
    () => facade.describeWindowCallParts(REVIEWER, 'waiting_input'),
    () => render.describeWindowCallParts(messages, REVIEWER, 'waiting_input'),
  ],
  [
    'describeExitedWindowParts',
    () => facade.describeExitedWindowParts(REVIEWER, 1),
    () => render.describeExitedWindowParts(messages, REVIEWER, 1),
  ],
  [
    'describeGoToCallingWindowParts',
    () => facade.describeGoToCallingWindowParts(REVIEWER),
    () => render.describeGoToCallingWindowParts(messages, REVIEWER),
  ],
  [
    'describeApprovalHeadline',
    () =>
      facade.describeApprovalHeadline(
        'send_message',
        null,
        { kind: 'name', name: IMPLEMENTER },
        REVIEWER
      ),
    () =>
      render.describeApprovalHeadline(
        messages,
        'send_message',
        null,
        { kind: 'name', name: IMPLEMENTER },
        REVIEWER
      ),
  ],
  [
    'describeNoticeAndMore',
    () => facade.describeNoticeAndMore(BUILDER_EXITED, 3),
    () => render.describeNoticeAndMore(messages, BUILDER_EXITED, 3),
  ],
  [
    'describeEventTime',
    () => facade.describeEventTime(new Date(2026, 0, 2, 0, 5)),
    () => render.describeEventTime(messages, new Date(2026, 0, 2, 0, 5)),
  ],
  [
    'describeAnnouncement',
    () => facade.describeAnnouncement(ANNOUNCED),
    () => render.describeAnnouncement(messages, ANNOUNCED),
  ],
  [
    'describeReopenOffer',
    () => facade.describeReopenOffer(6, SAVED_AT_EPOCH_S),
    () => render.describeReopenOffer(messages, 6, SAVED_AT_EPOCH_S),
  ],
  [
    'describeRecordedTime',
    () => facade.describeRecordedTime(SAVED_AT_EPOCH_S),
    () => render.describeRecordedTime(messages, SAVED_AT_EPOCH_S),
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
    () => facade.describeReopenNotice(3, SAVED_AT_EPOCH_S),
    () => render.describeReopenNotice(messages, 3, SAVED_AT_EPOCH_S),
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
    () => facade.describeRoomTitle(formatPathForDisplay('main'), 2),
    () => render.describeRoomTitle(messages, formatPathForDisplay('main'), 2),
  ],
  [
    'describeUnknownReasonSuffix',
    () => facade.describeUnknownReasonSuffix(UNKNOWN_WITH_REASON),
    () => render.describeUnknownReasonSuffix(messages, UNKNOWN_WITH_REASON),
  ],
  [
    'describeUnknownExplanation',
    () => facade.describeUnknownExplanation(UNKNOWN_WITH_REASON, WARNING),
    () => render.describeUnknownExplanation(messages, UNKNOWN_WITH_REASON, WARNING),
  ],
  [
    'describeStatusLabel',
    () => facade.describeStatusLabel('unknown', messages.unknownReasons.short.screen_lost),
    () =>
      render.describeStatusLabel(messages, 'unknown', messages.unknownReasons.short.screen_lost),
  ],
  [
    'describeRestoreOrigin',
    () => facade.describeRestoreOrigin(REVIEWER, sanitizeForDisplay('claude-code')),
    () => render.describeRestoreOrigin(messages, REVIEWER, sanitizeForDisplay('claude-code')),
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
