/**
 * Verifies `describeBoundaryNotice`, a pure function mapping a
 * `BoundaryOperation` and its detail to the display text shown for a
 * boundary-crossing failure (fixed per-operation summary, detail
 * sanitization/cap, and the `profilesIncomplete` multi-warning assembly).
 */
import { describe, it, expect } from 'vitest';

import { describeBoundaryNotice, type BoundaryOperation } from './boundary-notice-model';

/**
 * The nine fixed summaries, copied
 * here rather than read back from `messages-en.ts`: comparing a value against
 * itself would be vacuous. `Record<BoundaryOperation, string>` also makes
 * this fixture exhaustive at the type level -- a tenth operation added to the
 * union without a matching entry here fails type-checking.
 */
const SUMMARIES: Record<BoundaryOperation, string> = {
  sessionEvent: 'Could not apply a session event, so the view may be behind',
  sessionEvents: "Could not subscribe to the daemon's events",
  windowList: 'Could not refresh the window list, so the view may be behind',
  chatChanged: 'Could not apply a chat update, so the view may be behind',
  themes: 'Could not load the theme settings',
  detectionsRefresh: 'Could not refresh the detection definitions',
  detectionsList: 'Could not list the detection definitions',
  profilesIncomplete: 'Not all launch profiles could be read',
  profilesList: 'Could not list the launch profiles',
};

const ALL_OPERATIONS = Object.keys(SUMMARIES) as BoundaryOperation[];

describe('describeBoundaryNotice: operation -> summary mapping covers every BoundaryOperation', () => {
  it.each(ALL_OPERATIONS)(
    'renders the fixed summary alone for operation "%s" when detail is empty',
    (operation) => {
      expect(describeBoundaryNotice({ operation, detail: '' })).toBe(SUMMARIES[operation]);
    }
  );
});

describe('describeBoundaryNotice: non-empty detail always reaches the display text', () => {
  it.each(ALL_OPERATIONS.filter((operation) => operation !== 'profilesIncomplete'))(
    'includes a plain-string detail in the display text for operation "%s"',
    (operation) => {
      const result = describeBoundaryNotice({ operation, detail: 'a specific daemon reason' });
      expect(result).toContain('a specific daemon reason');
    }
  );

  it('includes the sole warning for profilesIncomplete when detail has exactly one entry', () => {
    const result = describeBoundaryNotice({
      operation: 'profilesIncomplete',
      detail: ['profile file "a.toml" could not be parsed'],
    });
    expect(result).toContain('profile file "a.toml" could not be parsed');
  });
});

describe('describeBoundaryNotice: blank detail falls back to the summary alone (existing rule preserved)', () => {
  it.each(['', '   ', '\n\t'])(
    'returns the summary alone for a blank detail (case: %j)',
    (detail) => {
      expect(describeBoundaryNotice({ operation: 'themes', detail })).toBe(SUMMARIES.themes);
    }
  );
});

describe('describeBoundaryNotice: sanitizes and caps detail exactly like describeOperationFailure', () => {
  it('folds a control character in the detail into a single space', () => {
    const result = describeBoundaryNotice({ operation: 'sessionEvent', detail: 'line1\nline2' });
    expect(result).toBe(`${SUMMARIES.sessionEvent}: line1 line2`);
  });

  it('caps an overlong detail with an ellipsis at the 200 code point limit', () => {
    const long = 'a'.repeat(250);
    const result = describeBoundaryNotice({ operation: 'sessionEvent', detail: long });
    expect(result).toBe(`${SUMMARIES.sessionEvent}: ${'a'.repeat(200)}…`);
  });

  it('never lets the display text carry a raw newline or tab', () => {
    const result = describeBoundaryNotice({ operation: 'windowList', detail: 'a\nb\tc' });
    expect(result).not.toMatch(/[\n\t]/);
  });
});

describe('describeBoundaryNotice: profilesIncomplete assembles a multi-warning detail with the count in front', () => {
  it('returns the summary alone when detail is an empty array', () => {
    expect(describeBoundaryNotice({ operation: 'profilesIncomplete', detail: [] })).toBe(
      SUMMARIES.profilesIncomplete
    );
  });

  it('uses the sole entry verbatim when detail has exactly one warning', () => {
    const detail = ['profile file "a.toml" could not be parsed'];
    expect(describeBoundaryNotice({ operation: 'profilesIncomplete', detail })).toBe(
      `${SUMMARIES.profilesIncomplete}: ${detail[0]}`
    );
  });

  it('places the count before the first warning when detail has two entries', () => {
    const detail = [
      'profile file "a.toml" could not be parsed',
      'profile file "b.toml" could not be parsed',
    ];
    const result = describeBoundaryNotice({ operation: 'profilesIncomplete', detail });
    expect(result).toBe(`${SUMMARIES.profilesIncomplete}: 2 problems, first: ${detail[0]}`);
  });

  it('places the count before the first warning when detail has three entries', () => {
    const detail = [
      'profile file "a.toml" could not be parsed',
      'profile file "b.toml" could not be parsed',
      'profile file "c.toml" could not be parsed',
    ];
    const result = describeBoundaryNotice({ operation: 'profilesIncomplete', detail });
    expect(result).toBe(`${SUMMARIES.profilesIncomplete}: 3 problems, first: ${detail[0]}`);
  });

  it('does not put the count at the end (a trailing "(and N more)" form is rejected)', () => {
    const detail = [
      'profile file "a.toml" could not be parsed',
      'profile file "b.toml" could not be parsed',
      'profile file "c.toml" could not be parsed',
    ] as const;
    const result = describeBoundaryNotice({ operation: 'profilesIncomplete', detail });
    expect(result).not.toMatch(/and \d+ more\)?\s*$/);
    expect(result.indexOf('3')).toBeGreaterThanOrEqual(0);
    expect(result.indexOf('3')).toBeLessThan(result.indexOf(detail[0]));
  });

  it('keeps the count prefix even when the combined text is long enough to be truncated (the count must survive the 200 code point cap)', () => {
    const long = 'x'.repeat(250);
    const detail = [long, 'profile file "b.toml" could not be parsed'];
    const result = describeBoundaryNotice({ operation: 'profilesIncomplete', detail });
    expect(result.startsWith(`${SUMMARIES.profilesIncomplete}: 2 problems, first: `)).toBe(true);
    expect(result.endsWith('…')).toBe(true);
  });
});

describe('describeBoundaryNotice: a detail that is a list but not of strings', () => {
  it('shows it as the list it is, not as a collapsed first warning', () => {
    const result = describeBoundaryNotice({ operation: 'profilesList', detail: ['first', 2] });

    expect(result).toBe(`${SUMMARIES.profilesList}: ["first",2]`);
  });
});
