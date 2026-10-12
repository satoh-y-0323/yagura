import { describe, it, expect } from 'vitest';

import { describeStatus } from './ui-messages';

const EXPECTED_LABELS: Record<string, string> = {
  running: 'Running',
  waiting_input: 'Waiting for input',
  waiting_approval: 'Waiting for approval',
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
