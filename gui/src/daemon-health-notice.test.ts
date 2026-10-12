import { describe, it, expect } from 'vitest';

import { daemonProblemNotices } from './daemon-health-notice';

describe('daemonProblemNotices', () => {
  it('shows no notice when the daemon reports no problem', () => {
    expect(daemonProblemNotices([])).toEqual([]);
  });

  it('names the bundled console host and does not echo the daemon detail for windows_unavailable', () => {
    expect(
      daemonProblemNotices([{ kind: 'windows_unavailable', detail: 'a-private-path/conpty.dll' }])
    ).toEqual([
      'Windows cannot be opened: the console host bundled with yagura (conpty.dll, OpenConsole.exe) is missing or damaged. Reinstall yagura.',
    ]);
  });

  it('carries the daemon detail for initial_window_failed', () => {
    expect(
      daemonProblemNotices([{ kind: 'initial_window_failed', detail: 'spawn refused' }])
    ).toEqual(['The first window could not be opened: spawn refused']);
  });
});
