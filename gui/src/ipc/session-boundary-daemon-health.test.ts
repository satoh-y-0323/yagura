import { describe, it, expect } from 'vitest';

import { BoundaryError, narrowDaemonHealth } from './session-boundary';

describe('narrowDaemonHealth', () => {
  it('accepts an empty problem list', () => {
    expect(narrowDaemonHealth({ problems: [] })).toEqual([]);
  });

  it('keeps the known kind', () => {
    expect(
      narrowDaemonHealth({
        problems: [{ kind: 'windows_unavailable', detail: 'a' }],
      })
    ).toEqual([{ kind: 'windows_unavailable', detail: 'a' }]);
  });

  it('drops a kind this build does not know instead of failing the reply', () => {
    expect(
      narrowDaemonHealth({
        problems: [
          { kind: 'from_the_future', detail: 'x' },
          { kind: 'initial_window_failed', detail: 'y' },
          { kind: 'windows_unavailable', detail: 'b' },
        ],
      })
    ).toEqual([{ kind: 'windows_unavailable', detail: 'b' }]);
  });

  it('strips control characters from the detail', () => {
    const [problem] = narrowDaemonHealth({
      problems: [{ kind: 'windows_unavailable', detail: 'bad\u001b[31mred' }],
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
