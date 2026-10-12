/**
 * `detail` is external text from the daemon, so it may carry control characters,
 * non-breaking whitespace, or shapes that are not JSON-serializable; this suite pins
 * the folding and capping that makes it safe to show.
 */
import { describe, it, expect } from 'vitest';

import {
  describeDetectionWarning,
  describeOperationFailure,
  describeUnknownReasonSuffix,
  describeUnknownExplanation,
} from './ui-messages';

const SUMMARY = 'Could not close the window';

/**
 * The `errors.detectionLoadFailed` value of `messages-en.ts`, restated here so that a
 * change to either wording shows up as a mismatch.
 */
const DETECTION_WARNING_SUMMARY = 'Could not load the detection definition';

/** Longest daemon-supplied detail kept in an operation failure message (ui-messages.ts). */
const OPERATION_DETAIL_MAX_LENGTH = 200;

/**
 * All Unicode Zs (Space_Separator) code points, derived by scanning rather than
 * hand-typed. Zs has no code points outside the BMP, so the scan is exhaustive.
 */
const ZS_CODE_POINTS: number[] = (() => {
  const points: number[] = [];
  for (let cp = 0x0000; cp <= 0xffff; cp++) {
    if (/\p{Zs}/u.test(String.fromCodePoint(cp))) {
      points.push(cp);
    }
  }
  return points;
})();

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

describe('describeOperationFailure: stringification, control-character folding and capping', () => {
  it('joins summary and detail when detail is a plain string', () => {
    expect(describeOperationFailure(SUMMARY, 'approval denied by policy')).toBe(
      `${SUMMARY}: approval denied by policy`
    );
  });

  it('uses the Error message when detail is an Error instance', () => {
    expect(describeOperationFailure(SUMMARY, new Error('daemon unreachable'))).toBe(
      `${SUMMARY}: daemon unreachable`
    );
  });

  it('renders null detail as the string "null" (JSON.stringify(null))', () => {
    expect(describeOperationFailure(SUMMARY, null)).toBe(`${SUMMARY}: null`);
  });

  it('falls back to summary alone when detail is undefined (JSON.stringify(undefined) is undefined)', () => {
    expect(describeOperationFailure(SUMMARY, undefined)).toBe(SUMMARY);
  });

  it('stringifies a numeric detail', () => {
    expect(describeOperationFailure(SUMMARY, 42)).toBe(`${SUMMARY}: 42`);
  });

  it('stringifies a plain-object detail as JSON', () => {
    expect(describeOperationFailure(SUMMARY, { code: 'EPERM' })).toBe(
      `${SUMMARY}: {"code":"EPERM"}`
    );
  });

  it.each([
    ['\n', 'line1\nline2', 'line1 line2'],
    ['\\r\\n', 'line1\r\nline2', 'line1 line2'],
    ['\\t', 'col\ttab', 'col tab'],
  ])('folds control character %s into a single space', (_label, input, expected) => {
    expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: ${expected}`);
  });

  it('folds an ANSI escape (ESC control byte) into a space, leaving the CSI bytes as text', () => {
    // \x1b is Cc and gets folded; "[31m"/"[0m" are plain printable characters
    // the current regex does not target, so they survive as literal text.
    const input = 'before\x1b[31mred\x1b[0mafter';
    expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: before [31mred [0mafter`);
  });

  it.each([
    ['empty string', ''],
    ['whitespace only', '   '],
    ['control characters only', '\n\t\r\n'],
  ])('returns summary alone when detail folds down to nothing (%s)', (_label, input) => {
    expect(describeOperationFailure(SUMMARY, input)).toBe(SUMMARY);
  });

  it('does not cap a detail exactly at OPERATION_DETAIL_MAX_LENGTH code points', () => {
    const detail = 'a'.repeat(OPERATION_DETAIL_MAX_LENGTH);
    const result = describeOperationFailure(SUMMARY, detail);
    expect(result).toBe(`${SUMMARY}: ${detail}`);
    expect(result.endsWith('…')).toBe(false);
  });

  it('caps a detail one code point past OPERATION_DETAIL_MAX_LENGTH and appends an ellipsis', () => {
    const detail = 'a'.repeat(OPERATION_DETAIL_MAX_LENGTH + 1);
    const result = describeOperationFailure(SUMMARY, detail);
    expect(result).toBe(`${SUMMARY}: ${'a'.repeat(OPERATION_DETAIL_MAX_LENGTH)}…`);
  });

  it('keeps a surrogate pair intact when it lands exactly at OPERATION_DETAIL_MAX_LENGTH code points (no cap)', () => {
    const detail = 'a'.repeat(OPERATION_DETAIL_MAX_LENGTH - 1) + '\u{1F600}';
    expect(Array.from(detail)).toHaveLength(OPERATION_DETAIL_MAX_LENGTH);
    const result = describeOperationFailure(SUMMARY, detail);
    expect(result).toBe(`${SUMMARY}: ${detail}`);
    expect(hasLoneSurrogate(result)).toBe(false);
  });

  it('keeps a surrogate pair intact and adds an ellipsis when it lands exactly at the cut boundary', () => {
    // One code point over the cap; the surrogate pair sits at the last kept position.
    const detail = 'a'.repeat(OPERATION_DETAIL_MAX_LENGTH - 1) + '\u{1F600}' + 'a';
    expect(Array.from(detail)).toHaveLength(OPERATION_DETAIL_MAX_LENGTH + 1);
    const result = describeOperationFailure(SUMMARY, detail);
    expect(result).toBe(`${SUMMARY}: ${'a'.repeat(OPERATION_DETAIL_MAX_LENGTH - 1)}\u{1F600}…`);
    expect(hasLoneSurrogate(result)).toBe(false);
  });
});

describe('describeOperationFailure: Zs (Space_Separator) folding and trimming', () => {
  describe.each(ZS_CODE_POINTS.map((cp) => [cp, String.fromCodePoint(cp)] as const))(
    'Zs code point U+%s',
    (cp, char) => {
      it(`folds a single mid-string occurrence into a plain space`, () => {
        const input = `left${char}right`;
        expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: left right`);
      });
    }
  );

  // Every Zs code point takes the same fold branch, proven one by one above, so the
  // cases below exercise the remaining axes instead of every Zs pair: runs of
  // distinct Zs, a Zs next to an already-covered class, and Zs at the edges.
  it('folds a run of distinct Zs code points into a single space', () => {
    // NBSP + IDEOGRAPHIC SPACE + EM SPACE, three different Zs code points back to back.
    const input = 'left 　 right';
    expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: left right`);
  });

  it.each([
    ['Zs before Cc', 'left \nright'],
    ['Cc before Zs', 'left\n right'],
  ])(
    'folds a Zs code point adjacent to a Cc control character into a single space (%s)',
    (_label, input) => {
      expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: left right`);
    }
  );

  it.each([
    ['leading', '  left right'],
    ['trailing', 'left right　　'],
  ])('trims %s Zs code points from the edges', (_label, input) => {
    expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: left right`);
  });

  it('leaves a single regular space (U+0020) between words unchanged (no-op case within the same fold rule)', () => {
    const input = 'left right';
    expect(describeOperationFailure(SUMMARY, input)).toBe(`${SUMMARY}: left right`);
  });

  it('does not throw for a detail object with a circular reference and falls back to summary alone', () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(() => describeOperationFailure(SUMMARY, circular)).not.toThrow();
    expect(describeOperationFailure(SUMMARY, circular)).toBe(SUMMARY);
  });

  it('does not throw for a detail object containing a BigInt and falls back to summary alone', () => {
    const withBigInt = { count: 10n };
    expect(() => describeOperationFailure(SUMMARY, withBigInt)).not.toThrow();
    expect(describeOperationFailure(SUMMARY, withBigInt)).toBe(SUMMARY);
  });
});

/**
 * The daemon's `detection_warning` is external text folded and capped exactly as an
 * operation-failure detail is, so only the summary and a representative slice of the
 * shared fold/cap behaviour are pinned here.
 */
describe('describeDetectionWarning: Japanese summary, folding and capping', () => {
  const DETECTION_MAX_LENGTH = 200;

  it('joins the fixed Japanese summary and the daemon detail with ": "', () => {
    const detail = 'detection definition file not found (os error 2); status stays unknown';
    expect(describeDetectionWarning(detail)).toBe(`${DETECTION_WARNING_SUMMARY}: ${detail}`);
  });

  it('falls back to the summary alone when the detail is an empty string', () => {
    expect(describeDetectionWarning('')).toBe(DETECTION_WARNING_SUMMARY);
  });

  it.each([
    ['\n', 'line1\nline2', 'line1 line2'],
    ['\\t', 'col\ttab', 'col tab'],
  ])('folds control character %s into a single space', (_label, input, expected) => {
    expect(describeDetectionWarning(input)).toBe(`${DETECTION_WARNING_SUMMARY}: ${expected}`);
  });

  it('does not cap a detail exactly at the 200 code point limit', () => {
    const detail = 'a'.repeat(DETECTION_MAX_LENGTH);
    const result = describeDetectionWarning(detail);
    expect(result).toBe(`${DETECTION_WARNING_SUMMARY}: ${detail}`);
    expect(result.endsWith('…')).toBe(false);
  });

  it('caps a detail one code point past the 200 code point limit and appends an ellipsis', () => {
    const detail = 'a'.repeat(DETECTION_MAX_LENGTH + 1);
    const result = describeDetectionWarning(detail);
    expect(result).toBe(`${DETECTION_WARNING_SUMMARY}: ${'a'.repeat(DETECTION_MAX_LENGTH)}…`);
  });

  it('matches describeOperationFailure(DETECTION_WARNING_SUMMARY, detail) for a representative detail (shared fold/cap implementation)', () => {
    const detail =
      'the definition path resolves into a build artifact directory\r\n(or cannot be resolved)';
    expect(describeDetectionWarning(detail)).toBe(
      describeOperationFailure(DETECTION_WARNING_SUMMARY, detail)
    );
  });
});

/**
 * These two option strings were shortened after the window-side theme select
 * truncated them on a real device; pinned so a future lengthening fails here.
 */
/**
 * Every other file is forbidden to import `i18n/` directly, so a missing re-export
 * here would leave every consumer unable to read these strings at all.
 */
/**
 * `banners.approvalExpiredUnseen` renders the "N unseen expired approval
 * requests" notice (count + a one-item summary of the
 * latest). It is re-exported directly from `banners` (unlike
 * `describeThemeIssues`, it has no dedicated `describe*` wrapper -- there is
 * nothing render.ts-side to test in render.test.ts, which this task does not
 * touch).
 */
/**
 * The facade forwards to render.ts's `describeUnknownReasonSuffix` /
 * `describeUnknownExplanation` without the `messages` parameter, the same
 * shape `describeStatus`/`describeAttentionBadge` already take above. Both now
 * take the status and its reason as one value, so no caller of the facade can
 * name a reason for a status that is not Unknown. The per-reason wording
 * itself is pinned in render.test.ts (against the resolved locale, which this
 * facade also resolves); only the forwarding is measured here.
 */
describe('facade re-exports the unknown-reason renderings', () => {
  it('describeUnknownReasonSuffix is the empty string for an unknown status with no reason', () => {
    expect(describeUnknownReasonSuffix({ status: 'unknown', reason: null })).toBe('');
  });

  it('describeUnknownReasonSuffix is the empty string for an arm that is not the unknown one', () => {
    expect(describeUnknownReasonSuffix({ status: 'running' })).toBe('');
  });

  it('describeUnknownReasonSuffix renders a non-empty suffix for a reason', () => {
    expect(
      describeUnknownReasonSuffix({ status: 'unknown', reason: 'no_definition' }).length
    ).toBeGreaterThan(0);
  });

  it('describeUnknownExplanation is the empty string for an unknown status with no reason', () => {
    expect(describeUnknownExplanation({ status: 'unknown', reason: null }, null)).toBe('');
    expect(
      describeUnknownExplanation({ status: 'unknown', reason: null }, 'a warning with no reason')
    ).toBe('');
  });

  it('describeUnknownExplanation is the empty string for an arm that is not the unknown one', () => {
    expect(describeUnknownExplanation({ status: 'idle' }, null)).toBe('');
    expect(describeUnknownExplanation({ status: 'idle' }, 'a warning with no reason')).toBe('');
  });

  it('describeUnknownExplanation renders the explanation alone for a blank warning', () => {
    const pair = { status: 'unknown', reason: 'no_definition' } as const;
    const alone = describeUnknownExplanation(pair, null);
    expect(alone.length).toBeGreaterThan(0);
    expect(describeUnknownExplanation(pair, '')).toBe(alone);
  });

  it('describeUnknownExplanation appends a non-blank warning in parentheses', () => {
    const pair = { status: 'unknown', reason: 'no_definition' } as const;
    const alone = describeUnknownExplanation(pair, null);
    const withWarning = describeUnknownExplanation(pair, 'detail text');
    expect(withWarning).toBe(`${alone} (detail text)`);
  });
});

/**
 * The fixed line the add-window form shows when the
 * connected daemon has not settled a wire revision the launch options need.
 * `describeWindowOpenFailure` (window-open-failure-model.ts) is the only
 * caller; this only checks the facade re-export resolves to real text.
 */
