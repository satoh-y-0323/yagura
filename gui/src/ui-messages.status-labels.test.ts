import { describe, it, expect } from 'vitest';

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
});
