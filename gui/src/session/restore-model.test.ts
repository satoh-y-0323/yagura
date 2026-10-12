/**
 * Pure decision functions behind the restore flow: no IPC, no DOM, no svelte
 * store. `applyLayoutSteps` is the sole exception and only calls the injected
 * `LayoutTarget`.
 */
import { describe, it, expect, vi } from 'vitest';

import {
  matchesOpenWindow,
  defaultSelection,
  applySelectionOverrides,
  parseLayoutJson,
  formatRestoreTimestamp,
  planLayoutApplication,
  applyLayoutSteps,
  layoutTargetsForOpened,
  layoutStepsForSnapshot,
  restoreFallbackOf,
  restoreLaunchOf,
  isProfileDisabledRefusal,
  restoreNoticeForDisabled,
  MAX_LAYOUT_JSON_BYTES,
  type LayoutV1,
  type LayoutStep,
  type LayoutTarget,
  type LayoutApplicationTarget,
} from './restore-model';
import { PROFILE_DISABLED_CODE } from '../ipc/session-boundary';
import type { RestoreEntry, WindowInfo, WindowOpenFailure } from '../ipc/session-boundary';

/** Builds a minimal RestoreEntry, overriding only the fields a case cares about. */
function makeEntry(overrides: Partial<RestoreEntry> = {}): RestoreEntry {
  return {
    position: 0,
    command: ['claude'],
    cwd: '/home/user/project',
    name: 'main',
    vendor: null,
    detection: null,
    layout: null,
    displayCommand: 'claude',
    displayCwd: '/home/user/project',
    displayName: 'main',
    displayVendor: null,
    displayDetection: null,
    profile: null,
    model: null,
    effort: null,
    ...overrides,
  };
}

function makeLayout(overrides: Partial<LayoutV1> = {}): LayoutV1 {
  return {
    v: 1,
    pos: { x: 10, y: 20 },
    size: { width: 400, height: 300 },
    z: 1,
    min: false,
    max: false,
    tab: 0,
    ...overrides,
  };
}

describe('matchesOpenWindow', () => {
  it('matches when cwd and name are both byte-identical to an open window', () => {
    const candidate = makeEntry({ cwd: '/a/b', name: 'main' });
    expect(matchesOpenWindow(candidate, [{ cwd: '/a/b', name: 'main' }])).toBe(true);
  });

  it('does not match when only cwd matches (name differs)', () => {
    const candidate = makeEntry({ cwd: '/a/b', name: 'main' });
    expect(matchesOpenWindow(candidate, [{ cwd: '/a/b', name: 'other' }])).toBe(false);
  });

  it('does not match when only name matches (cwd differs)', () => {
    const candidate = makeEntry({ cwd: '/a/b', name: 'main' });
    expect(matchesOpenWindow(candidate, [{ cwd: '/a/other', name: 'main' }])).toBe(false);
  });

  it('does not normalize: a trailing-slash difference in cwd is not a match', () => {
    const candidate = makeEntry({ cwd: '/a/b', name: 'main' });
    expect(matchesOpenWindow(candidate, [{ cwd: '/a/b/', name: 'main' }])).toBe(false);
  });

  it('returns false against an empty window list', () => {
    const candidate = makeEntry();
    expect(matchesOpenWindow(candidate, [])).toBe(false);
  });
});

describe('defaultSelection', () => {
  it('checks every candidate by default when no open window matches', () => {
    const candidates = [
      makeEntry({ position: 0, cwd: '/a', name: 'a' }),
      makeEntry({ position: 1, cwd: '/b', name: 'b' }),
    ];
    expect(defaultSelection(candidates, [])).toEqual(new Set([0, 1]));
  });

  it('unchecks only the candidate matching an open window', () => {
    const candidates = [
      makeEntry({ position: 0, cwd: '/a', name: 'a' }),
      makeEntry({ position: 5, cwd: '/b', name: 'b' }),
    ];
    const result = defaultSelection(candidates, [{ cwd: '/a', name: 'a' }]);
    expect(result).toEqual(new Set([5]));
  });

  it('unchecks every candidate when all match open windows', () => {
    const candidates = [
      makeEntry({ position: 0, cwd: '/a', name: 'a' }),
      makeEntry({ position: 1, cwd: '/b', name: 'b' }),
    ];
    const result = defaultSelection(candidates, [
      { cwd: '/a', name: 'a' },
      { cwd: '/b', name: 'b' },
    ]);
    expect(result).toEqual(new Set());
  });
});

/**
 * The pure seam that layers an explicit per-candidate choice on top of the
 * recomputed {@link defaultSelection}, so no caller mutates a `Set` in place.
 */
describe('applySelectionOverrides', () => {
  it('turns on a candidate that defaults off, when its override is true', () => {
    const defaults = new Set<number>();
    const result = applySelectionOverrides(defaults, new Map([[3, true]]));
    expect(result).toEqual(new Set([3]));
  });

  it('turns off a candidate that defaults on, when its override is false', () => {
    const defaults = new Set<number>([1, 2]);
    const result = applySelectionOverrides(defaults, new Map([[1, false]]));
    expect(result).toEqual(new Set([2]));
  });

  it('returns a set equal to defaults when overrides is empty', () => {
    const defaults = new Set<number>([0, 4, 7]);
    const result = applySelectionOverrides(defaults, new Map());
    expect(result).toEqual(new Set([0, 4, 7]));
  });

  it('does not mutate the defaults set or the overrides map it is given', () => {
    const defaults = new Set<number>([1]);
    const overrides = new Map<number, boolean>([
      [1, false],
      [2, true],
    ]);
    applySelectionOverrides(defaults, overrides);
    expect(defaults).toEqual(new Set([1]));
    expect(overrides).toEqual(
      new Map([
        [1, false],
        [2, true],
      ])
    );
  });
});

describe('parseLayoutJson', () => {
  it('parses a well-formed v1 layout', () => {
    const raw = JSON.stringify(makeLayout());
    expect(parseLayoutJson(raw)).toEqual(makeLayout());
  });

  it('returns null for malformed JSON', () => {
    expect(parseLayoutJson('{not json')).toBeNull();
  });

  it('returns null when v is not 1', () => {
    expect(parseLayoutJson(JSON.stringify({ ...makeLayout(), v: 2 }))).toBeNull();
  });

  it('exports MAX_LAYOUT_JSON_BYTES as 16384', () => {
    expect(MAX_LAYOUT_JSON_BYTES).toBe(16_384);
  });

  it('accepts a payload exactly at the 16 KiB byte boundary', () => {
    // The schema has a fixed field set, so the boundary is built by padding an
    // extra property until the encoded payload lands exactly on the cap.
    const base = makeLayout();
    const encoder = new TextEncoder();
    const withoutPad = JSON.stringify({ ...base, pad: '' });
    const padLength = MAX_LAYOUT_JSON_BYTES - encoder.encode(withoutPad).length;
    const raw = JSON.stringify({ ...base, pad: 'x'.repeat(Math.max(0, padLength)) });
    expect(encoder.encode(raw).length).toBeLessThanOrEqual(MAX_LAYOUT_JSON_BYTES);
    expect(parseLayoutJson(raw)).not.toBeNull();
  });

  it('rejects a payload one byte over the 16 KiB boundary', () => {
    const base = makeLayout();
    const encoder = new TextEncoder();
    const atBoundary = JSON.stringify({ ...base, pad: '' });
    const padLength = MAX_LAYOUT_JSON_BYTES - encoder.encode(atBoundary).length + 1;
    const raw = JSON.stringify({ ...base, pad: 'x'.repeat(Math.max(1, padLength)) });
    expect(encoder.encode(raw).length).toBeGreaterThan(MAX_LAYOUT_JSON_BYTES);
    expect(parseLayoutJson(raw)).toBeNull();
  });

  it('accepts pos: null and size: null for a partial layout', () => {
    const raw = JSON.stringify(makeLayout({ pos: null, size: null }));
    const result = parseLayoutJson(raw);
    expect(result).not.toBeNull();
    expect(result?.pos).toBeNull();
    expect(result?.size).toBeNull();
  });
});

describe('formatRestoreTimestamp', () => {
  /** Builds the expected local "YYYY-MM-DD HH:mm" string via Date getters (TZ-independent). */
  function expected(epochS: number): string {
    const date = new Date(epochS * 1000);
    const pad = (n: number): string => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
  }

  it('formats a positive epoch value as local YYYY-MM-DD HH:mm', () => {
    const epochS = 1_735_000_000;
    expect(formatRestoreTimestamp(epochS)).toBe(expected(epochS));
  });

  it('returns null for zero', () => {
    expect(formatRestoreTimestamp(0)).toBeNull();
  });

  it('returns null for a negative value', () => {
    expect(formatRestoreTimestamp(-1)).toBeNull();
  });

  it('returns null for NaN', () => {
    expect(formatRestoreTimestamp(NaN)).toBeNull();
  });

  it('returns null for Infinity', () => {
    expect(formatRestoreTimestamp(Infinity)).toBeNull();
  });
});

describe('planLayoutApplication', () => {
  it('returns an empty list for zero targets', () => {
    expect(planLayoutApplication([], [])).toEqual([]);
  });

  it('applies position, size, minimized, front, and tab steps for one non-maximized target', () => {
    const layout = makeLayout({ min: false, max: false, tab: 0 });
    const steps = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']);
    expect(steps).toEqual<LayoutStep[]>([
      { kind: 'position', windowId: 'w1', position: layout.pos as { x: number; y: number } },
      { kind: 'size', windowId: 'w1', size: layout.size as { width: number; height: number } },
      { kind: 'minimized', windowId: 'w1', minimized: false },
      { kind: 'front', windowId: 'w1' },
      { kind: 'tab', windowId: 'w1', targetIndex: 0 },
    ]);
  });

  it('sorts targets by position ascending before applying steps', () => {
    const layoutA = makeLayout({ pos: { x: 1, y: 1 }, tab: 0 });
    const layoutB = makeLayout({ pos: { x: 2, y: 2 }, tab: 1 });
    const steps = planLayoutApplication(
      [
        { windowId: 'second', position: 5, layout: layoutB },
        { windowId: 'first', position: 1, layout: layoutA },
      ],
      ['first', 'second']
    );
    const positionSteps = steps.filter((step) => step.kind === 'position');
    expect(positionSteps.map((step) => step.windowId)).toEqual(['first', 'second']);
  });

  it('applies maximize then front as a connect for a valid maximize candidate (max=true, pos and size non-null)', () => {
    const layout = makeLayout({ max: true, tab: 0 });
    const steps = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']);
    const kinds = steps.map((step) => step.kind);
    const maximizeIndex = kinds.indexOf('maximize');
    const frontIndex = kinds.indexOf('front');
    expect(maximizeIndex).toBeGreaterThanOrEqual(0);
    expect(frontIndex).toBe(maximizeIndex + 1);
  });

  it('max=true with pos null (only) is treated as non-maximized and is a front target', () => {
    const layout = makeLayout({ max: true, pos: null, size: { width: 1, height: 1 }, tab: 0 });
    const steps = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']);
    expect(steps.some((step) => step.kind === 'maximize')).toBe(false);
    expect(steps).toContainEqual({ kind: 'front', windowId: 'w1' });
    // pos is null, so no position step is emitted for this target.
    expect(steps.some((step) => step.kind === 'position')).toBe(false);
    expect(steps).toContainEqual({
      kind: 'size',
      windowId: 'w1',
      size: { width: 1, height: 1 },
    });
  });

  it('with two max=true targets where the sorted-last one has pos null, the earlier one is the maximize candidate', () => {
    const earlierLayout = makeLayout({ max: true, tab: 0 });
    const laterLayout = makeLayout({ max: true, pos: null, tab: 1 });
    const steps = planLayoutApplication(
      [
        { windowId: 'earlier', position: 0, layout: earlierLayout },
        { windowId: 'later', position: 1, layout: laterLayout },
      ],
      ['earlier', 'later']
    );
    const maximizeSteps = steps.filter((step) => step.kind === 'maximize');
    expect(maximizeSteps).toEqual([{ kind: 'maximize', windowId: 'earlier' }]);
    // 'later' is not a maximize candidate (pos is null), so it is a front target instead.
    expect(steps).toContainEqual({ kind: 'front', windowId: 'later' });
    // 'earlier' is excluded from the plain front set (it is the maximize candidate).
    const frontSteps = steps.filter((step) => step.kind === 'front');
    expect(frontSteps.map((step) => step.windowId)).not.toContain('earlier');
  });

  it('moves every target tab to the end of the saved tab order, ascending by saved tab index', () => {
    const layoutFirst = makeLayout({ tab: 2 });
    const layoutSecond = makeLayout({ tab: 0 });
    const steps = planLayoutApplication(
      [
        { windowId: 'w-tab2', position: 0, layout: layoutFirst },
        { windowId: 'w-tab0', position: 1, layout: layoutSecond },
      ],
      ['w-tab0', 'w-tab2', 'w-other']
    );
    const tabSteps = steps.filter((step) => step.kind === 'tab');
    expect(tabSteps.map((step) => step.windowId)).toEqual(['w-tab0', 'w-tab2']);
    for (const step of tabSteps) {
      expect(step).toMatchObject({ targetIndex: 2 });
    }
  });
});

describe('applyLayoutSteps', () => {
  function makeMockTarget(): LayoutTarget {
    return {
      setWindowPosition: vi.fn(),
      setWindowSize: vi.fn(),
      setWindowMinimized: vi.fn(),
      bringWindowToFront: vi.fn(),
      setWindowMaximized: vi.fn(),
      moveTab: vi.fn(),
    };
  }

  it('calls exactly one target method per step, in step order', () => {
    const target = makeMockTarget();
    const steps: LayoutStep[] = [
      { kind: 'position', windowId: 'w1', position: { x: 1, y: 2 } },
      { kind: 'size', windowId: 'w1', size: { width: 3, height: 4 } },
      { kind: 'minimized', windowId: 'w1', minimized: true },
      { kind: 'front', windowId: 'w1' },
      { kind: 'maximize', windowId: 'w1' },
      { kind: 'tab', windowId: 'w1', targetIndex: 0 },
    ];

    applyLayoutSteps(target, steps);

    expect(target.setWindowPosition).toHaveBeenCalledTimes(1);
    expect(target.setWindowPosition).toHaveBeenCalledWith('w1', { x: 1, y: 2 });
    expect(target.setWindowSize).toHaveBeenCalledTimes(1);
    expect(target.setWindowSize).toHaveBeenCalledWith('w1', { width: 3, height: 4 });
    expect(target.setWindowMinimized).toHaveBeenCalledTimes(1);
    expect(target.setWindowMinimized).toHaveBeenCalledWith('w1', true);
    expect(target.bringWindowToFront).toHaveBeenCalledTimes(1);
    expect(target.bringWindowToFront).toHaveBeenCalledWith('w1');
    expect(target.setWindowMaximized).toHaveBeenCalledTimes(1);
    expect(target.setWindowMaximized).toHaveBeenCalledWith('w1', true);
    expect(target.moveTab).toHaveBeenCalledTimes(1);
    expect(target.moveTab).toHaveBeenCalledWith('w1', 0);
  });
});

/** Builds a minimal WindowInfo, omitting layout_json when layoutJson is undefined. */
function makeWindowInfo(windowId: string, layoutJson?: string): WindowInfo {
  const base: WindowInfo = {
    window_id: windowId,
    name: windowId,
    vendor: null,
    cwd: '/home/user/project',
    detected: { status: 'running' },
    exit_code: null,
  };
  return layoutJson === undefined ? base : { ...base, layout_json: layoutJson };
}

describe('layoutTargetsForOpened', () => {
  it('excludes a window that is not in opened', () => {
    const windows = [makeWindowInfo('w-a', '{}'), makeWindowInfo('w-b', '{}')];
    const result = layoutTargetsForOpened(windows, ['w-a']);
    expect(result.map((target) => target.windowId)).toEqual(['w-a']);
  });

  it('excludes a window whose layout_json is undefined', () => {
    const windows = [makeWindowInfo('w-a'), makeWindowInfo('w-b', '{}')];
    const result = layoutTargetsForOpened(windows, ['w-a', 'w-b']);
    expect(result.map((target) => target.windowId)).toEqual(['w-b']);
  });

  it('uses the windows array index as position', () => {
    const windows = [
      makeWindowInfo('w-a', '{}'),
      makeWindowInfo('w-b', '{}'),
      makeWindowInfo('w-c', '{}'),
    ];
    const result = layoutTargetsForOpened(windows, ['w-b', 'w-c']);
    expect(result).toEqual([
      { windowId: 'w-b', position: 1, layout: null },
      { windowId: 'w-c', position: 2, layout: null },
    ]);
  });

  it('keeps layout: null for a window whose layout_json fails to parse', () => {
    const windows = [makeWindowInfo('w-a', '{not json')];
    const result = layoutTargetsForOpened(windows, ['w-a']);
    expect(result).toEqual([{ windowId: 'w-a', position: 0, layout: null }]);
  });

  it('parses a well-formed layout_json into a non-null layout via parseLayoutJson', () => {
    const layout = makeLayout();
    const windows = [makeWindowInfo('w-a', JSON.stringify(layout))];
    const result = layoutTargetsForOpened(windows, ['w-a']);
    expect(result).toEqual([{ windowId: 'w-a', position: 0, layout }]);
  });

  it('follows the windows array order, not the opened array order', () => {
    const windows = [
      makeWindowInfo('w-c', '{}'),
      makeWindowInfo('w-a', '{}'),
      makeWindowInfo('w-b', '{}'),
    ];
    const result = layoutTargetsForOpened(windows, ['w-a', 'w-b', 'w-c']);
    expect(result.map((target) => target.windowId)).toEqual(['w-c', 'w-a', 'w-b']);
  });

  it('returns an empty array for empty inputs', () => {
    expect(layoutTargetsForOpened([], [])).toEqual([]);
  });
});

describe('layoutStepsForSnapshot', () => {
  it('returns the same steps as planLayoutApplication when workspaceWasEmpty is true', () => {
    const layout = makeLayout({ max: true, tab: 0 });
    const targets: LayoutApplicationTarget[] = [{ windowId: 'w1', position: 0, layout }];
    const tabOrder = ['w1'];

    const steps = layoutStepsForSnapshot(targets, tabOrder, true);

    expect(steps).toEqual(planLayoutApplication(targets, tabOrder));
    expect(steps.some((step) => step.kind === 'maximize')).toBe(true);
  });

  it('returns only position/size/minimized steps, in that order, when workspaceWasEmpty is false', () => {
    const layout = makeLayout({ max: true, tab: 0 });
    const targets: LayoutApplicationTarget[] = [{ windowId: 'w1', position: 0, layout }];
    const tabOrder = ['w1'];

    const steps = layoutStepsForSnapshot(targets, tabOrder, false);

    expect(steps.map((step) => step.kind)).toEqual(['position', 'size', 'minimized']);
  });

  it('drops front/maximize/tab steps but keeps geometry for a multi-target plan', () => {
    const layoutA = makeLayout({ pos: { x: 1, y: 1 }, tab: 0 });
    const layoutB = makeLayout({ pos: { x: 2, y: 2 }, tab: 1 });
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'w-a', position: 0, layout: layoutA },
      { windowId: 'w-b', position: 1, layout: layoutB },
    ];
    const tabOrder = ['w-a', 'w-b'];

    const steps = layoutStepsForSnapshot(targets, tabOrder, false);

    const kinds = new Set(steps.map((step) => step.kind));
    expect(kinds.has('front')).toBe(false);
    expect(kinds.has('maximize')).toBe(false);
    expect(kinds.has('tab')).toBe(false);
    expect(steps.some((step) => step.kind === 'position')).toBe(true);
  });
});

describe('restore synthesis: layoutTargetsForOpened + layoutStepsForSnapshot', () => {
  it('produces geometry steps only for the newly opened window, none for the other live windows', () => {
    const windows = [
      makeWindowInfo('w-a', JSON.stringify(makeLayout({ pos: { x: 1, y: 1 } }))),
      makeWindowInfo('w-b', JSON.stringify(makeLayout({ pos: { x: 2, y: 2 } }))),
      makeWindowInfo('w-c', JSON.stringify(makeLayout({ pos: { x: 3, y: 3 } }))),
    ];
    const opened = ['w-b'];
    const tabOrder = ['w-a', 'w-b', 'w-c'];

    const targets = layoutTargetsForOpened(windows, opened);
    const steps = layoutStepsForSnapshot(targets, tabOrder, false);

    expect(steps.length).toBeGreaterThan(0);
    for (const step of steps) {
      expect(step.windowId).toBe('w-b');
    }
  });
});

describe('restoreLaunchOf', () => {
  it('opens a candidate that names a profile from that profile', () => {
    const candidate = makeEntry({
      command: ['claude', '--model', 'opus'],
      cwd: '/home/user/project',
      name: 'main',
      vendor: 'claude-code',
      detection: 'claude-code',
      profile: 'claude-code',
      model: 'opus',
      effort: 'high',
    });

    expect(restoreLaunchOf(candidate)).toEqual({
      kind: 'profile',
      profile: 'claude-code',
      cwd: '/home/user/project',
      name: 'main',
      model: 'opus',
      effort: 'high',
    });
  });

  it('leaves model and effort null when the candidate recorded none', () => {
    const candidate = makeEntry({ profile: 'codex-cli' });

    expect(restoreLaunchOf(candidate)).toEqual({
      kind: 'profile',
      profile: 'codex-cli',
      cwd: '/home/user/project',
      name: 'main',
      model: null,
      effort: null,
    });
  });

  it('opens a candidate without a profile from its saved argv', () => {
    const candidate = makeEntry({
      command: ['bash', '-l'],
      cwd: '/srv',
      name: 'shell',
      vendor: 'generic',
      detection: 'shell',
    });

    expect(restoreLaunchOf(candidate)).toEqual({
      kind: 'argv',
      command: ['bash', '-l'],
      cwd: '/srv',
      name: 'shell',
      detection: 'shell',
      vendor: 'generic',
    });
  });
});

describe('restoreFallbackOf', () => {
  const profileCandidate = makeEntry({
    command: ['claude', '--model', 'opus'],
    cwd: '/home/user/project',
    name: 'main',
    vendor: 'claude-code',
    detection: 'claude-code',
    profile: 'claude-code',
    model: 'opus',
    effort: null,
  });

  const rejected: WindowOpenFailure = {
    kind: 'rejected',
    code: 'invalid_argument',
    message: 'unknown profile',
    rejectedArgument: 'profile',
  };

  it('falls back to the saved argv when the daemon rejected the profile launch', () => {
    expect(restoreFallbackOf(profileCandidate, rejected)).toEqual({
      kind: 'argv',
      command: ['claude', '--model', 'opus'],
      cwd: '/home/user/project',
      name: 'main',
      detection: 'claude-code',
      vendor: 'claude-code',
    });
  });

  it.each<{ label: string; failure: WindowOpenFailure }>([
    { label: 'bare', failure: { kind: 'bare', message: 'bridge failed' } },
    { label: 'local', failure: { kind: 'local', message: 'daemon unreachable' } },
    { label: 'unsupported', failure: { kind: 'unsupported', requestType: 'open_window' } },
  ])('does not fall back after a $label failure', ({ failure }) => {
    expect(restoreFallbackOf(profileCandidate, failure)).toBeNull();
  });

  it('does not retry a candidate that was already opened from its argv', () => {
    const argvCandidate = makeEntry({ command: ['bash'], profile: null });

    expect(restoreFallbackOf(argvCandidate, rejected)).toBeNull();
  });

  it('does not reopen from the saved argv when the daemon refused the profile as disabled', () => {
    const disabled: WindowOpenFailure = {
      kind: 'rejected',
      code: 'profile_disabled',
      message: 'profile is disabled',
      rejectedArgument: 'profile',
    };

    expect(restoreFallbackOf(profileCandidate, disabled)).toBeNull();
  });
});

describe('isProfileDisabledRefusal', () => {
  function rejectedWith(code: string): WindowOpenFailure {
    return { kind: 'rejected', code, message: 'refused', rejectedArgument: 'profile' };
  }

  it('names the wire code the daemon uses for a disabled profile', () => {
    expect(PROFILE_DISABLED_CODE).toBe('profile_disabled');
  });

  it('is true for a daemon rejection carrying the disabled-profile code', () => {
    expect(isProfileDisabledRefusal(rejectedWith('profile_disabled'))).toBe(true);
  });

  it.each(['invalid_argument', 'not_found', 'PROFILE_DISABLED', ''])(
    'is false for a daemon rejection with code %j',
    (code) => {
      expect(isProfileDisabledRefusal(rejectedWith(code))).toBe(false);
    }
  );

  it.each<{ label: string; failure: WindowOpenFailure }>([
    { label: 'bare', failure: { kind: 'bare', message: 'profile_disabled' } },
    { label: 'local', failure: { kind: 'local', message: 'profile_disabled' } },
    { label: 'unsupported', failure: { kind: 'unsupported', requestType: 'profile_disabled' } },
  ])('is false for a $label failure even when its text reads like the code', ({ failure }) => {
    expect(isProfileDisabledRefusal(failure)).toBe(false);
  });
});

describe('restoreNoticeForDisabled', () => {
  function occurrences(text: string, needle: string): number {
    return text.split(needle).length - 1;
  }

  it('is null when no window was held back', () => {
    expect(restoreNoticeForDisabled([])).toBeNull();
  });

  it('is one line naming every held-back window in the order given', () => {
    const notice = restoreNoticeForDisabled(['review-pane', 'build-runner', 'docs-writer']);

    expect(notice).not.toBeNull();
    const text = notice ?? '';
    expect(text).not.toContain('\n');
    const positions = ['review-pane', 'build-runner', 'docs-writer'].map((name) =>
      text.indexOf(name)
    );
    expect(positions.every((position) => position >= 0)).toBe(true);
    expect(positions).toEqual([...positions].sort((a, b) => a - b));
  });

  it('says more than the bare names', () => {
    const notice = restoreNoticeForDisabled(['review-pane']) ?? '';

    expect(notice.length).toBeGreaterThan('review-pane'.length);
  });

  it('names a window that appears twice only once', () => {
    const notice = restoreNoticeForDisabled(['review-pane', 'build-runner', 'review-pane']) ?? '';

    expect(occurrences(notice, 'review-pane')).toBe(1);
    expect(occurrences(notice, 'build-runner')).toBe(1);
  });
});

describe('parseLayoutJson rejects any field of the wrong shape', () => {
  const valid = makeLayout();
  const nonFinite = (key: string, value: Record<string, unknown>): string =>
    JSON.stringify(value).replace(`"${key}":0`, `"${key}":1e999`);

  it.each<[string, string]>([
    ['a JSON string', '"layout"'],
    ['a JSON number', '1'],
    ['JSON null', 'null'],
    ['a JSON array', '[1]'],
    ['pos given as a string', JSON.stringify({ ...valid, pos: 'top-left' })],
    ['pos given as a number', JSON.stringify({ ...valid, pos: 5 })],
    ['pos without y', JSON.stringify({ ...valid, pos: { x: 1 } })],
    ['pos without x', JSON.stringify({ ...valid, pos: { y: 1 } })],
    ['pos with a string x', JSON.stringify({ ...valid, pos: { x: '1', y: 2 } })],
    ['pos with a string y', JSON.stringify({ ...valid, pos: { x: 1, y: '2' } })],
    ['pos with a non-finite x', nonFinite('x', { ...valid, pos: { x: 0, y: 2 } })],
    ['pos with a non-finite y', nonFinite('y', { ...valid, pos: { x: 1, y: 0 } })],
    ['size given as a string', JSON.stringify({ ...valid, size: 'big' })],
    ['size without height', JSON.stringify({ ...valid, size: { width: 1 } })],
    ['size with a string width', JSON.stringify({ ...valid, size: { width: '1', height: 2 } })],
    ['size with a string height', JSON.stringify({ ...valid, size: { width: 1, height: '2' } })],
    ['a missing pos', JSON.stringify({ ...valid, pos: undefined })],
    ['a missing size', JSON.stringify({ ...valid, size: undefined })],
    ['a string z', JSON.stringify({ ...valid, z: '1' })],
    ['a non-finite z', nonFinite('z', { ...valid, z: 0 })],
    ['a numeric min', JSON.stringify({ ...valid, min: 0 })],
    ['a numeric max', JSON.stringify({ ...valid, max: 1 })],
    ['a string tab', JSON.stringify({ ...valid, tab: '0' })],
    ['a non-finite tab', nonFinite('tab', { ...valid, tab: 0 })],
  ])('returns null for %s', (_label, raw) => {
    expect(parseLayoutJson(raw)).toBeNull();
  });

  it('ignores keys it does not know', () => {
    expect(parseLayoutJson(JSON.stringify({ ...valid, future: 1 }))).toEqual(valid);
  });

  it('keeps only the known keys of pos and size', () => {
    const raw = JSON.stringify({
      ...valid,
      pos: { x: 1, y: 2, extra: 3 },
      size: { width: 4, height: 5, extra: 6 },
    });
    expect(parseLayoutJson(raw)).toEqual({
      ...valid,
      pos: { x: 1, y: 2 },
      size: { width: 4, height: 5 },
    });
  });

  it('measures the size cap in UTF-8 bytes rather than characters', () => {
    const multiByte = JSON.stringify({ ...valid, pad: 'あ'.repeat(5500) });
    expect(multiByte.length).toBeLessThan(MAX_LAYOUT_JSON_BYTES);
    expect(parseLayoutJson(multiByte)).toBeNull();
  });
});

describe('planLayoutApplication ordering and partial layouts', () => {
  it('gives a window whose layout could not be parsed an unminimized front and tab step only', () => {
    const steps = planLayoutApplication([{ windowId: 'w1', position: 0, layout: null }], ['w1']);
    expect(steps).toEqual<LayoutStep[]>([
      { kind: 'minimized', windowId: 'w1', minimized: false },
      { kind: 'front', windowId: 'w1' },
      { kind: 'tab', windowId: 'w1', targetIndex: 0 },
    ]);
  });

  it('carries the saved minimized flag into the step', () => {
    const steps = planLayoutApplication(
      [{ windowId: 'w1', position: 0, layout: makeLayout({ min: true }) }],
      ['w1']
    );
    expect(steps).toContainEqual({ kind: 'minimized', windowId: 'w1', minimized: true });
  });

  it('emits a position step without a size step when only the position was saved', () => {
    const layout = makeLayout({ size: null });
    const kinds = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']).map(
      (step) => step.kind
    );
    expect(kinds).toEqual(['position', 'minimized', 'front', 'tab']);
  });

  it('emits a size step without a position step when only the size was saved', () => {
    const layout = makeLayout({ pos: null });
    const kinds = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']).map(
      (step) => step.kind
    );
    expect(kinds).toEqual(['size', 'minimized', 'front', 'tab']);
  });

  it('fronts the windows from the lowest saved z to the highest, with an unparsed layout counting as 0', () => {
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'high', position: 0, layout: makeLayout({ z: 5 }) },
      { windowId: 'low', position: 1, layout: makeLayout({ z: 1 }) },
      { windowId: 'none', position: 2, layout: null },
    ];
    const fronts = planLayoutApplication(targets, ['high', 'low', 'none'])
      .filter((step) => step.kind === 'front')
      .map((step) => step.windowId);
    expect(fronts).toEqual(['none', 'low', 'high']);
  });

  it('orders the tab moves by the saved tab index, with an unparsed layout counting as 0', () => {
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'third', position: 0, layout: makeLayout({ tab: 7 }) },
      { windowId: 'second', position: 1, layout: makeLayout({ tab: 2 }) },
      { windowId: 'first', position: 2, layout: null },
    ];
    const tabs = planLayoutApplication(targets, ['a', 'b'])
      .filter((step) => step.kind === 'tab')
      .map((step) => step.windowId);
    expect(tabs).toEqual(['first', 'second', 'third']);
  });

  it('plans the fronts and tabs when the windows without a parsed layout come first', () => {
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'none', position: 0, layout: null },
      { windowId: 'saved', position: 1, layout: makeLayout({ z: 3, tab: 4 }) },
    ];
    const steps = planLayoutApplication(targets, ['none', 'saved']);
    const windowsOf = (kind: LayoutStep['kind']): string[] =>
      steps.filter((step) => step.kind === kind).map((step) => step.windowId);
    expect(windowsOf('front')).toEqual(['none', 'saved']);
    expect(windowsOf('tab')).toEqual(['none', 'saved']);
  });

  it('does not maximize a max window that has a position but no size', () => {
    const layout = makeLayout({ max: true, size: null });
    const steps = planLayoutApplication([{ windowId: 'w1', position: 0, layout }], ['w1']);
    expect(steps.some((step) => step.kind === 'maximize')).toBe(false);
    expect(steps).toContainEqual({ kind: 'front', windowId: 'w1' });
  });

  it('maximizes the last of several candidates and leaves the others as ordinary front targets', () => {
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'first', position: 0, layout: makeLayout({ max: true, z: 1 }) },
      { windowId: 'last', position: 1, layout: makeLayout({ max: true, z: 2 }) },
    ];
    const steps = planLayoutApplication(targets, ['first', 'last']);
    expect(steps.filter((step) => step.kind === 'maximize')).toEqual([
      { kind: 'maximize', windowId: 'last' },
    ]);
    expect(steps.filter((step) => step.kind === 'front')).toEqual([
      { kind: 'front', windowId: 'first' },
    ]);
  });

  it('does not front a maximized window again when other windows were fronted', () => {
    const targets: LayoutApplicationTarget[] = [
      { windowId: 'big', position: 0, layout: makeLayout({ max: true }) },
      { windowId: 'other', position: 1, layout: makeLayout({}) },
    ];
    const steps = planLayoutApplication(targets, ['big', 'other']);
    expect(steps.filter((step) => step.kind === 'front')).toEqual([
      { kind: 'front', windowId: 'other' },
    ]);
    expect(steps).toContainEqual({ kind: 'maximize', windowId: 'big' });
  });
});
