import { describe, it, expect } from 'vitest';

import { BoundaryError, narrowDaemonHealth } from './session-boundary';

describe('narrowDaemonHealth', () => {
  it('accepts an empty problem list', () => {
    expect(narrowDaemonHealth({ problems: [] })).toEqual([]);
  });

  it('keeps both known kinds in order', () => {
    expect(
      narrowDaemonHealth({
        problems: [
          { kind: 'windows_unavailable', detail: 'a' },
          { kind: 'initial_window_failed', detail: 'b' },
        ],
      })
    ).toEqual([
      { kind: 'windows_unavailable', detail: 'a' },
      { kind: 'initial_window_failed', detail: 'b' },
    ]);
  });

  it('drops a kind this build does not know instead of failing the reply', () => {
    expect(
      narrowDaemonHealth({
        problems: [
          { kind: 'from_the_future', detail: 'x' },
          { kind: 'initial_window_failed', detail: 'b' },
        ],
      })
    ).toEqual([{ kind: 'initial_window_failed', detail: 'b' }]);
  });

  it('strips control characters from the detail', () => {
    const [problem] = narrowDaemonHealth({
      problems: [{ kind: 'initial_window_failed', detail: 'bad\u001b[31mred' }],
    });
    expect(problem?.detail).not.toContain('\u001b');
  });

  it.each([null, 'x', { problems: 'no' }, { problems: [null] }, { problems: [{ kind: 'a' }] }])(
    'rejects a malformed reply %j',
    (value) => {
      expect(() => narrowDaemonHealth(value)).toThrow(BoundaryError);
    }
  );
});
