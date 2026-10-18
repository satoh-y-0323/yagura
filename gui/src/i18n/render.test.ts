/**
 * Message assembly is resource-driven: passing the dummy locale must replace
 * the English wording in every output. `describeOperationFailure` is out of
 * scope, since it takes its summary from the caller and reads no locale.
 * Plural forms and unknown-key fallback are checked against the English
 * locale instead, being properties of `render.ts` rather than of a locale.
 */
import { describe, it, expect } from 'vitest';

import { resolveMessages } from './messages';
import { dummyMessages } from './messages.test';
import type {
  ChatDeliveryOutcome,
  DetectedStatusView,
  UnknownReason,
} from '../ipc/session-boundary';
import type { ConfirmTarget } from '../session/confirm-target';
import {
  describeDeliveryState,
  describeStatus,
  describeExit,
  describeExitCode,
  describeWindowListTitle,
  describePaneNotice,
  describeDetectionWarning,
  describeDiscardedKeymapEntries,
  describeThemeIssues,
  describeThemeName,
  describeAttentionBadge,
  describeRestoreBanner,
  describeRestoreSelectionCount,
  // A normal value import is safe here
  // (unlike Rust, an unresolved named export only fails the `it` that calls
  // it, not collection of this whole file), so it stays alongside the rest
  // instead of a separate glob/namespace import.
  describeRoomTitle,
  describeUnknownBadge,
  describeUnknownReasonSuffix,
  describeUnknownExplanation,
  describeUndeliveredUnknownReason,
  // Same import shape as the room additions above.
  describeInterruptHint,
} from './render';

const english = resolveMessages();

describe('render.ts: output is driven by the given locale (dummy-locale swap)', () => {
  it('describeStatus: renders the dummy status label instead of the English one', () => {
    const result = describeStatus(dummyMessages, 'running');
    expect(result).toBe(dummyMessages.statusLabels.running);
    expect(result).not.toBe(english.statusLabels.running);
  });

  it('describeExit: renders the dummy session-phase label when there is no exit code', () => {
    const result = describeExit(dummyMessages, null);
    expect(result).toBe(dummyMessages.sessionPhaseLabels.exited);
    expect(result).not.toBe(english.sessionPhaseLabels.exited);
  });

  it('describeExitCode: renders from the dummy formats.exitCode template', () => {
    const result = describeExitCode(dummyMessages, 3);
    expect(result).toBe(dummyMessages.formats.exitCode(3));
    expect(result).not.toBe(english.formats.exitCode(3));
  });

  it('describeWindowListTitle: renders from the dummy hints.windowList label', () => {
    const result = describeWindowListTitle(dummyMessages, 2);
    expect(result).toBe(dummyMessages.formats.windowListTitle(dummyMessages.hints.windowList, 2));
    expect(result).not.toContain(english.hints.windowList);
  });

  it('describePaneNotice: renders the dummy banner text for a failed phase', () => {
    const result = describePaneNotice(dummyMessages, 'failed');
    expect(result).toBe(dummyMessages.banners.connectionFailed);
    expect(result).not.toBe(english.banners.connectionFailed);
  });

  it('describePaneNotice: renders the dummy banner text for a superseded phase', () => {
    const result = describePaneNotice(dummyMessages, 'superseded');
    expect(result).toBe(dummyMessages.banners.windowSuperseded);
    expect(result).not.toBe(english.banners.windowSuperseded);
  });

  it('describeDetectionWarning: joins the dummy errors.detectionLoadFailed summary with the detail', () => {
    const result = describeDetectionWarning(dummyMessages, 'daemon detail text');
    expect(result).toBe(`${dummyMessages.errors.detectionLoadFailed}: daemon detail text`);
    expect(result).not.toContain(english.errors.detectionLoadFailed);
  });

  it('describeDiscardedKeymapEntries: renders from the dummy banners.discardedKeymapEntries template', () => {
    const result = describeDiscardedKeymapEntries(dummyMessages, 3);
    expect(result).toBe(dummyMessages.banners.discardedKeymapEntries(3));
    expect(result).not.toBe(english.banners.discardedKeymapEntries(3));
  });

  it('describeThemeIssues: renders from the dummy banners.themeIssues template', () => {
    const result = describeThemeIssues(dummyMessages, 3);
    expect(result).toBe(dummyMessages.banners.themeIssues(3));
    expect(result).not.toBe(english.banners.themeIssues(3));
  });

  it('describeThemeName: renders the dummy built-in theme label for a known theme', () => {
    const result = describeThemeName(dummyMessages, 'dark');
    expect(result).toBe(dummyMessages.builtInThemeLabels.dark);
    expect(result).not.toBe(english.builtInThemeLabels.dark);
  });

  it('describeAttentionBadge: renders from the dummy formats.attentionBadge template', () => {
    const result = describeAttentionBadge(dummyMessages, 4);
    expect(result).toBe(dummyMessages.formats.attentionBadge(4));
    expect(result).not.toBe(english.formats.attentionBadge(4));
  });

  it('describeRestoreBanner: renders from the dummy banners.restoreAvailable template', () => {
    const result = describeRestoreBanner(dummyMessages, 2, null);
    expect(result).toBe(dummyMessages.banners.restoreAvailable(2, null));
    expect(result).not.toBe(english.banners.restoreAvailable(2, null));
  });

  it('describeRestoreSelectionCount: renders from the dummy formats.restoreSelectionCount template', () => {
    const result = describeRestoreSelectionCount(dummyMessages, 2, 5);
    expect(result).toBe(dummyMessages.formats.restoreSelectionCount(2, 5));
    expect(result).not.toBe(english.formats.restoreSelectionCount(2, 5));
  });

  it('describeRoomTitle: renders through the dummy formats.windowListTitle(name, count) template', () => {
    const result = describeRoomTitle(dummyMessages, 'yagura', 2);
    expect(result).toBe(dummyMessages.formats.windowListTitle('yagura', 2));
    expect(result).not.toBe(english.formats.windowListTitle('yagura', 2));
  });

  it('describeUnknownBadge: renders from the dummy formats.unknownBadge template', () => {
    const result = describeUnknownBadge(dummyMessages, 3);
    expect(result).toBe(dummyMessages.formats.unknownBadge(3));
    expect(result).not.toBe(english.formats.unknownBadge(3));
  });
});

describe('render.ts: plural forms (contract (3), English locale)', () => {
  it.each([
    [0, 'entries'],
    [2, 'entries'],
  ])('describeDiscardedKeymapEntries: uses the plural form for count %i', (count) => {
    const result = describeDiscardedKeymapEntries(english, count);
    expect(result).toBe(
      `Discarded ${count} invalid keymap entries. Those keys fall back to their default bindings.`
    );
  });

  it('describeDiscardedKeymapEntries: uses the singular form for count 1', () => {
    const result = describeDiscardedKeymapEntries(english, 1);
    expect(result).toBe(
      'Discarded 1 invalid keymap entry. That key falls back to its default binding.'
    );
  });

  it.each([
    [0, 'entries'],
    [2, 'entries'],
  ])('describeThemeIssues: uses the plural form for count %i', (count) => {
    const result = describeThemeIssues(english, count);
    expect(result).toBe(
      `Excluded ${count} invalid theme entries. Those themes cannot be selected.`
    );
  });

  it('describeThemeIssues: uses the singular form for count 1', () => {
    const result = describeThemeIssues(english, 1);
    expect(result).toBe('Excluded 1 invalid theme entry. That theme cannot be selected.');
  });

  it('describeRestoreBanner: uses the singular form ("1 window") for a count of 1', () => {
    const result = describeRestoreBanner(english, 1, null);
    expect(result).toContain('1 window');
    expect(result).not.toContain('1 windows');
  });

  it.each([2, 5])('describeRestoreBanner: uses the plural form for count %i', (count) => {
    const result = describeRestoreBanner(english, count, null);
    expect(result).toContain(`${count} windows`);
  });

  it('describeRestoreBanner: includes the given timestamp text when provided', () => {
    const result = describeRestoreBanner(english, 2, '2026-08-29 10:15');
    expect(result).toContain('2026-08-29 10:15');
  });

  it('describeRestoreBanner: omits timestamp text when the timestamp is null', () => {
    const withTimestamp = describeRestoreBanner(english, 2, '2026-08-29 10:15');
    const withoutTimestamp = describeRestoreBanner(english, 2, null);
    expect(withoutTimestamp).not.toContain('2026-08-29 10:15');
    expect(withoutTimestamp).not.toBe(withTimestamp);
  });

  it('describeUnknownBadge: uses the singular form for a count of 1', () => {
    expect(describeUnknownBadge(english, 1)).toBe('1 window in an unknown state');
  });

  it.each([2, 5])('describeUnknownBadge: uses the plural form for count %i', (count) => {
    expect(describeUnknownBadge(english, count)).toBe(`${count} windows in an unknown state`);
  });
});

describe('render.ts: unknown external keys fall back safely (contract (10), English locale)', () => {
  it('describeStatus: falls back to the unknown label for an unrecognized status', () => {
    expect(describeStatus(english, 'bogus-status')).toBe(english.statusLabels.unknown);
  });

  it('describePaneNotice: returns null for a session phase with no notice', () => {
    expect(describePaneNotice(english, 'bogus-phase')).toBeNull();
    expect(describePaneNotice(english, 'attached')).toBeNull();
  });

  it('describeThemeName: returns the name unchanged for an unrecognized (user-defined) theme', () => {
    expect(describeThemeName(english, 'my-custom-theme')).toBe('my-custom-theme');
  });
});

// ---------------------------------------------------------------------------
// The reason a window's status is Unknown, and the reason a message was not
// delivered, both reach these renderers as one value rather than as a loose
// reason: there is no call these functions can be given that names a reason
// for a status that is not Unknown. `describeUnknownReasonSuffix` is named
// without `describeStatus` as a substring on purpose: the sidebar's verbatim
// scan rejects any occurrence of that literal substring.
// ---------------------------------------------------------------------------

/** The six wire values `UnknownReason` narrows. */
const UNKNOWN_REASONS = [
  'no_definition',
  'no_rule_matched',
  'screen_lost',
  'detector_stopped',
  'not_yet_judged',
  'declared_unknown',
] as const;

/** The unknown arm of a status pair, with the reason it carries. */
function unknownPair(reason: UnknownReason | null): DetectedStatusView {
  return { status: 'unknown', reason };
}

/** The outcome arm that can carry a reason, with the reason it carries. */
function undeliveredUnknown(reason: UnknownReason | null): ChatDeliveryOutcome {
  return { state: 'undelivered_unknown', reason };
}

describe('describeUnknownReasonSuffix', () => {
  it('returns the empty string for an unknown status carrying no reason (no reason to show)', () => {
    expect(describeUnknownReasonSuffix(english, unknownPair(null))).toBe('');
  });

  it('returns the empty string for every arm that is not the unknown one', () => {
    expect(describeUnknownReasonSuffix(english, { status: 'running' })).toBe('');
    expect(describeUnknownReasonSuffix(english, { status: 'waiting_approval' })).toBe('');
    expect(describeUnknownReasonSuffix(english, { status: 'exited' })).toBe('');
  });

  it.each(UNKNOWN_REASONS)('returns a non-empty suffix for reason %s', (reason) => {
    expect(describeUnknownReasonSuffix(english, unknownPair(reason)).length).toBeGreaterThan(0);
  });

  it('renders the six reasons as six distinct suffixes', () => {
    const rendered = UNKNOWN_REASONS.map((reason) =>
      describeUnknownReasonSuffix(english, unknownPair(reason))
    );
    expect(new Set(rendered).size).toBe(UNKNOWN_REASONS.length);
  });

  it('joins the short label with " · " (the connector the pill uses)', () => {
    expect(describeUnknownReasonSuffix(english, unknownPair('no_definition'))).toBe(
      ` · ${english.unknownReasons.short.no_definition}`
    );
  });

  it('pins the six short labels verbatim (English locale)', () => {
    expect(english.unknownReasons.short.no_definition).toBe('No definition');
    expect(english.unknownReasons.short.no_rule_matched).toBe('No rule matched');
    expect(english.unknownReasons.short.screen_lost).toBe('Screen lost');
    expect(english.unknownReasons.short.detector_stopped).toBe('Detector stopped');
    expect(english.unknownReasons.short.not_yet_judged).toBe('Not judged yet');
    expect(english.unknownReasons.short.declared_unknown).toBe('Reported unknown');
  });

  it('renders through the dummy locale, not the English one (locale-driven, not hardcoded)', () => {
    const result = describeUnknownReasonSuffix(dummyMessages, unknownPair('screen_lost'));
    expect(result).toBe(` · ${dummyMessages.unknownReasons.short.screen_lost}`);
    expect(result).not.toContain(english.unknownReasons.short.screen_lost);
  });
});

describe('describeUnknownExplanation', () => {
  it('pins the six explanations verbatim (English locale)', () => {
    expect(english.unknownReasons.explanation.no_definition).toBe(
      "No usable detection definition is assigned, so the status can never be detected. Assign one with open_window's detection argument or fix the profile."
    );
    expect(english.unknownReasons.explanation.no_rule_matched).toBe(
      'The screen matches no rule of the detection definition. Look at the window; messages stay refused until a rule matches or the definition settles this window on time alone.'
    );
    expect(english.unknownReasons.explanation.screen_lost).toBe(
      'The screen model was lost and has not been trusted again. Look at the window; the status recovers once the window settles on a state that accepts input.'
    );
    expect(english.unknownReasons.explanation.detector_stopped).toBe(
      'Detection stopped reporting for this window. Close the window and open it again.'
    );
    expect(english.unknownReasons.explanation.not_yet_judged).toBe(
      'The window just opened and nothing has been judged yet. Wait for output.'
    );
    expect(english.unknownReasons.explanation.declared_unknown).toBe(
      'A detection rule or a status event reported the status as unknown. Look at the window.'
    );
  });

  it('returns the empty string for an unknown status carrying no reason, the same fail-safe shape describeUnknownReasonSuffix takes', () => {
    expect(describeUnknownExplanation(english, unknownPair(null), null)).toBe('');
    expect(describeUnknownExplanation(english, unknownPair(null), 'a warning with no reason')).toBe(
      ''
    );
  });

  it('returns the empty string for every arm that is not the unknown one (nothing to explain)', () => {
    expect(describeUnknownExplanation(english, { status: 'running' }, null)).toBe('');
    expect(
      describeUnknownExplanation(english, { status: 'idle' }, 'a warning with no reason')
    ).toBe('');
  });

  it('renders the explanation alone when there is no warning (null)', () => {
    expect(describeUnknownExplanation(english, unknownPair('no_definition'), null)).toBe(
      english.unknownReasons.explanation.no_definition
    );
  });

  it('renders the explanation alone when the warning is an empty string', () => {
    expect(describeUnknownExplanation(english, unknownPair('no_definition'), '')).toBe(
      english.unknownReasons.explanation.no_definition
    );
  });

  it('renders the explanation alone when the warning is whitespace only', () => {
    expect(describeUnknownExplanation(english, unknownPair('no_definition'), '   ')).toBe(
      english.unknownReasons.explanation.no_definition
    );
  });

  it('appends the warning in parentheses when it is non-blank', () => {
    expect(
      describeUnknownExplanation(
        english,
        unknownPair('no_definition'),
        'profile.toml has no [detection]'
      )
    ).toBe(`${english.unknownReasons.explanation.no_definition} (profile.toml has no [detection])`);
  });

  it('renders through the dummy locale, not the English one (locale-driven, not hardcoded)', () => {
    const result = describeUnknownExplanation(dummyMessages, unknownPair('declared_unknown'), null);
    expect(result).toBe(dummyMessages.unknownReasons.explanation.declared_unknown);
    expect(result).not.toBe(english.unknownReasons.explanation.declared_unknown);
  });
});

describe('describeUndeliveredUnknownReason', () => {
  it('falls back to the existing reason-less wording when the outcome carries no reason (the reason-less facade stays untouched)', () => {
    expect(describeUndeliveredUnknownReason(english, undeliveredUnknown(null))).toBe(
      english.chat.deliveryUndeliveredUnknown
    );
  });

  it('renders every other outcome exactly as the reason-less facade does, reason or not', () => {
    expect(describeUndeliveredUnknownReason(english, { state: 'taken' })).toBe(
      describeDeliveryState(english, 'taken')
    );
    expect(describeUndeliveredUnknownReason(english, { state: 'undelivered_halted' })).toBe(
      describeDeliveryState(english, 'undelivered_halted')
    );
    expect(describeUndeliveredUnknownReason(english, { state: 'undelivered_restart' })).toBe(
      describeDeliveryState(english, 'undelivered_restart')
    );
  });

  it('pins the six chat words verbatim (English locale)', () => {
    expect(english.unknownReasons.chatWord.no_definition).toBe('no definition');
    expect(english.unknownReasons.chatWord.no_rule_matched).toBe('no rule matched');
    expect(english.unknownReasons.chatWord.screen_lost).toBe('screen lost');
    expect(english.unknownReasons.chatWord.detector_stopped).toBe('detector stopped');
    expect(english.unknownReasons.chatWord.not_yet_judged).toBe('not judged yet');
    expect(english.unknownReasons.chatWord.declared_unknown).toBe('reported unknown');
  });

  it('renders "Not sent (status unknown: <word>)" for a given reason', () => {
    expect(describeUndeliveredUnknownReason(english, undeliveredUnknown('screen_lost'))).toBe(
      'Not sent (status unknown: screen lost)'
    );
  });

  it.each(UNKNOWN_REASONS)('renders a non-empty, reason-specific string for %s', (reason) => {
    const result = describeUndeliveredUnknownReason(english, undeliveredUnknown(reason));
    expect(result.length).toBeGreaterThan(0);
    expect(result).not.toBe(english.chat.deliveryUndeliveredUnknown);
  });

  it('renders the six reasons as six distinct strings, all distinct from the reason-less fallback', () => {
    const rendered = UNKNOWN_REASONS.map((reason) =>
      describeUndeliveredUnknownReason(english, undeliveredUnknown(reason))
    );
    expect(new Set(rendered).size).toBe(UNKNOWN_REASONS.length);
    expect(rendered).not.toContain(english.chat.deliveryUndeliveredUnknown);
  });

  it('renders through the dummy locale, not the English one (locale-driven, not hardcoded)', () => {
    const result = describeUndeliveredUnknownReason(
      dummyMessages,
      undeliveredUnknown('detector_stopped')
    );
    expect(result).toBe(
      dummyMessages.chat.deliveryUndeliveredUnknownReason(
        dummyMessages.unknownReasons.chatWord.detector_stopped
      )
    );
    expect(result).not.toBe(english.chat.deliveryUndeliveredUnknown);
  });
});

// ---------------------------------------------------------------------------
// Why the ⏹ button is disabled. `describeInterruptHint` takes the
// gate's own verdict (a `ConfirmTarget`), not the raw status, so the reason
// is never derived a second time by the caller. The two blocked reasons must
// read as different sentences; the wording itself is
// fixed by the implementation task and is not asserted here verbatim.
// ---------------------------------------------------------------------------

/** The blocked arm of `ConfirmTarget`, naming `reason`. */
const blockedTarget = (reason: 'unknown' | 'exited'): ConfirmTarget => ({
  allowed: false,
  requiresConfirmation: false,
  reason,
});

/** The allowed arm of `ConfirmTarget`, carrying no reason. */
const permittedTarget: ConfirmTarget = { allowed: true, requiresConfirmation: true };

describe('describeInterruptHint', () => {
  it('returns a non-empty string when interrupt is blocked because the status is unknown', () => {
    expect(describeInterruptHint(english, blockedTarget('unknown')).length).toBeGreaterThan(0);
  });

  it('returns a non-empty string when interrupt is blocked because the process exited', () => {
    expect(describeInterruptHint(english, blockedTarget('exited')).length).toBeGreaterThan(0);
  });

  it('returns a different sentence for the unknown reason than for the exited reason', () => {
    const unknownText = describeInterruptHint(english, blockedTarget('unknown'));
    const exitedText = describeInterruptHint(english, blockedTarget('exited'));
    expect(unknownText).not.toBe(exitedText);
  });

  it('returns the enabled hint (hints.interruptWindow) when the target is allowed', () => {
    expect(describeInterruptHint(english, permittedTarget)).toBe(english.hints.interruptWindow);
  });

  it('renders through the given locale rather than always the English one', () => {
    const result = describeInterruptHint(dummyMessages, blockedTarget('unknown'));
    expect(result).not.toBe(describeInterruptHint(english, blockedTarget('unknown')));
  });
});

describe('describeInterruptHint: which message each reason picks', () => {
  it('picks the exited message for an exited process and the unknown message for an unknown status', () => {
    expect(describeInterruptHint(english, blockedTarget('exited'))).toBe(
      english.hints.interruptUnavailableExited
    );
    expect(describeInterruptHint(english, blockedTarget('unknown'))).toBe(
      english.hints.interruptUnavailableUnknown
    );
  });
});

describe('describeExit: with an exit code', () => {
  it('appends the formatted code to the session-phase label, separated by a space', () => {
    expect(describeExit(english, 3)).toBe(
      `${english.sessionPhaseLabels.exited} ${english.formats.exitCode(3)}`
    );
    expect(describeExit(english, 0)).toBe(
      `${english.sessionPhaseLabels.exited} ${english.formats.exitCode(0)}`
    );
  });

  it('is the bare label without an exit code', () => {
    expect(describeExit(english, null)).toBe(english.sessionPhaseLabels.exited);
  });
});
