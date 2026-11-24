/**
 * Locale coverage is compiler-enforced: `Messages` is derived via `typeof`
 * from the English resource, so a locale missing a key is a type error rather
 * than a runtime surprise. The dummy locale is `test-utils/dummy-messages.ts`.
 */
import { describe, it, expect } from 'vitest';

import { sanitizeForDisplay } from '../display/untrusted-text';
import { resolveMessages } from './messages';
import {
  describeGoToCallingWindowParts,
  describeWindowCall,
  describeWindowCallParts,
} from './render';

describe('unknownReasons', () => {
  it('unknownReasons.short/explanation/chatWord each cover exactly the six UnknownReason wire values', () => {
    const expected = [
      'no_definition',
      'no_rule_matched',
      'screen_lost',
      'detector_stopped',
      'not_yet_judged',
      'declared_unknown',
    ].sort();
    expect(Object.keys(resolveMessages().unknownReasons.short).sort()).toEqual(expected);
    expect(Object.keys(resolveMessages().unknownReasons.explanation).sort()).toEqual(expected);
    expect(Object.keys(resolveMessages().unknownReasons.chatWord).sort()).toEqual(expected);
  });
});

// A lane card cuts only the name short, so each sentence it draws is the parts put together.
it('builds the window call titles and the card’s go-to sentence from their parts, and leaves the exited card’s go-to sentence as it is', () => {
  const english = resolveMessages();
  const { workArea } = english;
  const joined = (parts: { before: string; name: string; after: string }): string =>
    `${parts.before}${parts.name}${parts.after}`;
  const reviewer = sanitizeForDisplay('reviewer');

  for (const [status, sentence] of [
    ['waiting_approval', '“reviewer” needs approval'],
    ['waiting_input', '“reviewer” needs input'],
  ] as const) {
    const parts = describeWindowCallParts(english, reviewer, status);
    expect(parts.name).toBe('reviewer');
    expect(joined(parts)).toBe(sentence);
    expect(parts.sentence).toBe(`${parts.before}${parts.name}${parts.after}`);
    expect(parts.sentence).toBe(sentence);
    expect(describeWindowCall(english, reviewer, status)).toBe(sentence);
  }

  const goTo = describeGoToCallingWindowParts(english, reviewer);
  expect(goTo.name).toBe('reviewer');
  expect(joined(goTo)).toBe('Go to “reviewer”');
  expect(goTo.sentence).toBe(`${goTo.before}${goTo.name}${goTo.after}`);
  expect(goTo.sentence).toBe('Go to “reviewer”');
  expect(workArea.goToWindow(reviewer)).toBe('Go to reviewer');
});
