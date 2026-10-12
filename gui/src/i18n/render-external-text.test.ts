/**
 * Text that came from outside the WebView (a daemon diagnosis, a launcher
 * warning) is folded and capped before it is shown, so that one message
 * cannot take over the notice area. That treatment is a single function both
 * display paths go through, rather than a second copy per path: this suite
 * pins what it does, and that `describeOperationFailure` is one of its
 * callers rather than a parallel implementation of the same rules.
 */
import { describe, it, expect } from 'vitest';

import { describeOperationFailure, foldExternalText } from './render';

const SUMMARY = 'Could not close the window';

/**
 * Longest external text kept, in code points: the limit `render.ts` already
 * applies to an operation failure's detail, restated here so a change to
 * either shows up as a mismatch.
 */
const EXTERNAL_TEXT_MAX_CODE_POINTS = 200;

/** True when `str` contains a surrogate code unit without its partner. */
function hasLoneSurrogate(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const code = str.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = str.charCodeAt(i + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return true;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      const prev = i > 0 ? str.charCodeAt(i - 1) : NaN;
      if (!(prev >= 0xd800 && prev <= 0xdbff)) return true;
    }
  }
  return false;
}

/** One folded class, named by the Unicode category it belongs to. */
const FOLDED_CHARACTERS: Array<[string, string]> = [
  ['a line feed (Cc)', '\n'],
  ['a carriage return + line feed (Cc)', '\r\n'],
  ['a tab (Cc)', '\t'],
  ['a NUL (Cc)', '\u0000'],
  ['an escape, the lead-in of an ANSI sequence (Cc)', '\u001b'],
  ['a delete (Cc)', '\u007f'],
  ['a zero-width space (Cf)', '​'],
  ['a line separator (Zl)', ' '],
  ['a paragraph separator (Zp)', ' '],
  ['a no-break space (Zs)', ' '],
  ['an ideographic space (Zs)', '　'],
];

describe('foldExternalText: folds control characters, breaks and separators into single spaces', () => {
  it.each(FOLDED_CHARACTERS)('folds %s into one space', (_label, character) => {
    expect(foldExternalText(`left${character}right`)).toBe('left right');
  });

  it('collapses a run of mixed folded characters into exactly one space', () => {
    expect(foldExternalText('left \r\n\t  right')).toBe('left right');
  });

  it('drops folded characters at both ends', () => {
    expect(foldExternalText('\n  padded  ')).toBe('padded');
  });

  it('returns the empty string for text made only of folded characters', () => {
    expect(foldExternalText('\n\r\t  ')).toBe('');
  });

  it('returns the empty string unchanged', () => {
    expect(foldExternalText('')).toBe('');
  });

  it('leaves text that needs no folding exactly as it was', () => {
    expect(foldExternalText('approval denied by policy')).toBe('approval denied by policy');
  });
});

describe('foldExternalText: caps by code point', () => {
  it('keeps text of exactly the limit unchanged', () => {
    const input = 'x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS);
    expect(foldExternalText(input)).toBe(input);
  });

  it('cuts text one code point over the limit and marks the cut with an ellipsis', () => {
    const input = 'x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS + 1);
    expect(foldExternalText(input)).toBe(`${'x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS)}…`);
  });

  it('counts code points, not UTF-16 units: astral characters are not halved', () => {
    const astral = '\u{1f600}';
    const result = foldExternalText(astral.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS + 1));
    expect(Array.from(result)).toHaveLength(EXTERNAL_TEXT_MAX_CODE_POINTS + 1);
    expect(result.endsWith('…')).toBe(true);
    expect(hasLoneSurrogate(result)).toBe(false);
  });

  it('applies the cap after folding, so folded runs do not consume the budget', () => {
    const input = `${'x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS)}${'\n'.repeat(50)}`;
    expect(foldExternalText(input)).toBe('x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS));
  });
});

describe('describeOperationFailure: goes through the same treatment rather than a copy of it', () => {
  const DETAILS = [
    'approval denied by policy',
    'left\nright',
    'left  right',
    '  \u0000 padded \u001b ',
    '\n\r\t ',
    '',
    'x'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS + 40),
    '\u{1f600}'.repeat(EXTERNAL_TEXT_MAX_CODE_POINTS + 5),
  ];

  it.each(DETAILS)(
    'renders the summary followed by the folded detail, or the summary alone when nothing is left, for %j',
    (detail) => {
      const folded = foldExternalText(detail);
      const expected = folded === '' ? SUMMARY : `${SUMMARY}: ${folded}`;
      expect(describeOperationFailure(SUMMARY, detail)).toBe(expected);
    }
  );
});
