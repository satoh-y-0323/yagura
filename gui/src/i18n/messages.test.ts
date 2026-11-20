/**
 * Locale coverage is compiler-enforced: `Messages` is derived via `typeof`
 * from the English resource, so a locale missing a key is a type error rather
 * than a runtime surprise. The dummy locale is `test-utils/dummy-messages.ts`.
 */
import { describe, it, expect } from 'vitest';

import { resolveMessages } from './messages';

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
