import { describe, it, expect } from 'vitest';

import { resolveMessages } from './i18n/messages';
import { describeStatus } from './ui-messages';

const EXPECTED_LABELS: Record<string, string> = {
  running: 'Running',
  waiting_input: 'Needs input',
  waiting_approval: 'Needs approval',
  interrupted: 'Interrupted',
  idle: 'Idle',
  exited: 'Exited',
  unknown: 'Unknown',
};

describe('describeStatus', () => {
  for (const [status, label] of Object.entries(EXPECTED_LABELS)) {
    it(`maps ${status} to ${label}`, () => {
      expect(describeStatus(status)).toBe(label);
    });
  }

  it('falls back to the unknown label for an unrecognized status', () => {
    expect(describeStatus('bogus')).toBe('Unknown');
  });
});

describe('status words', () => {
  it('exist as one set: the window list keeps no second set of status words', () => {
    const messages = resolveMessages() as unknown as Record<string, Record<string, unknown>>;
    expect(messages['windowList']?.['shortStatus']).toBeUndefined();
  });
});
