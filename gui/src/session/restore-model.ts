/**
 * Pure decision functions behind the restore flow. Every function here is pure
 * -- no IPC, no DOM, no svelte store -- except {@link applyLayoutSteps}, which
 * only calls the injected {@link LayoutTarget}.
 */
import {
  PROFILE_DISABLED_CODE,
  type RestoreEntry,
  type WindowInfo,
  type WindowOpenFailure,
} from '../ipc/session-boundary';
import { banners } from '../ui-messages';

/** Byte cap of a `layout_json` payload (mirrors `yagura_protocol::MAX_LAYOUT_JSON_BYTES`). */
export const MAX_LAYOUT_JSON_BYTES = 16_384;

/** The GUI-owned layout schema, version 1. */
export interface LayoutV1 {
  v: 1;
  pos: { x: number; y: number } | null;
  size: { width: number; height: number } | null;
  z: number;
  min: boolean;
  max: boolean;
  tab: number;
}

/**
 * True iff some open window has byte-identical `cwd` AND byte-identical
 * `name`; `command` is not part of `WindowInfo`, so those two are the whole
 * key. No normalization: both cwds come from the same daemon-side resolution,
 * and either a miss or a false match only flips the default checkbox, never
 * breaking restore.
 */
export function matchesOpenWindow(
  candidate: RestoreEntry,
  windows: readonly { cwd: string; name: string }[]
): boolean {
  return windows.some((window) => window.cwd === candidate.cwd && window.name === candidate.name);
}

/** Positions checked by default: every candidate EXCEPT a {@link matchesOpenWindow} hit. */
export function defaultSelection(
  candidates: readonly RestoreEntry[],
  windows: readonly { cwd: string; name: string }[]
): Set<number> {
  const selected = new Set<number>();
  for (const candidate of candidates) {
    if (!matchesOpenWindow(candidate, windows)) {
      selected.add(candidate.position);
    }
  }
  return selected;
}

/**
 * Layers explicit per-candidate choices over a computed {@link
 * defaultSelection}: an override of `true` checks that position, `false`
 * unchecks it, and a position with no override keeps whatever `defaults`
 * says. Neither input is mutated -- the defaults are recomputed on every
 * dialog open while the overrides outlive the dialog.
 */
export function applySelectionOverrides(
  defaults: Set<number>,
  overrides: ReadonlyMap<number, boolean>
): Set<number> {
  const selected = new Set(defaults);
  for (const [position, isChecked] of overrides) {
    if (isChecked) {
      selected.add(position);
    } else {
      selected.delete(position);
    }
  }
  return selected;
}

/** The point-shaped part of a {@link LayoutV1} field (`pos` or `size`). */
function readOptionalPoint(
  value: unknown,
  keyA: string,
  keyB: string
): Record<string, number> | null | undefined {
  if (value === null) {
    return null;
  }
  if (typeof value !== 'object') {
    return undefined;
  }
  const record = value as Record<string, unknown>;
  const a = record[keyA];
  const b = record[keyB];
  if (
    typeof a !== 'number' ||
    !Number.isFinite(a) ||
    typeof b !== 'number' ||
    !Number.isFinite(b)
  ) {
    return undefined;
  }
  return { [keyA]: a, [keyB]: b };
}

/**
 * Parses and validates a `layout_json` payload: `v !== 1`, a type mismatch, a
 * non-finite number, or a payload exceeding {@link MAX_LAYOUT_JSON_BYTES}
 * (measured in UTF-8 bytes, not code points) discards the whole value. Extra
 * keys (e.g. a future `v: 2` field carried ahead of a schema bump) are ignored
 * rather than rejected.
 */
export function parseLayoutJson(raw: string): LayoutV1 | null {
  if (new TextEncoder().encode(raw).length > MAX_LAYOUT_JSON_BYTES) {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return null;
  }
  const record = parsed as Record<string, unknown>;
  if (record['v'] !== 1) {
    return null;
  }
  const pos = readOptionalPoint(record['pos'], 'x', 'y');
  if (pos === undefined) {
    return null;
  }
  const size = readOptionalPoint(record['size'], 'width', 'height');
  if (size === undefined) {
    return null;
  }
  const z = record['z'];
  const min = record['min'];
  const max = record['max'];
  const tab = record['tab'];
  if (
    typeof z !== 'number' ||
    !Number.isFinite(z) ||
    typeof min !== 'boolean' ||
    typeof max !== 'boolean' ||
    typeof tab !== 'number' ||
    !Number.isFinite(tab)
  ) {
    return null;
  }
  return {
    v: 1,
    pos: pos as { x: number; y: number } | null,
    size: size as { width: number; height: number } | null,
    z,
    min,
    max,
    tab,
  };
}

/**
 * Formats an epoch-seconds timestamp as local `YYYY-MM-DD HH:mm`. `null` for
 * a non-positive or non-finite value (no plausible recording time).
 */
export function formatRestoreTimestamp(epochS: number): string | null {
  if (!Number.isFinite(epochS) || epochS <= 0) {
    return null;
  }
  const date = new Date(epochS * 1000);
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** One step of a layout application plan, applied in array order. */
export type LayoutStep =
  | { kind: 'position'; windowId: string; position: { x: number; y: number } }
  | { kind: 'size'; windowId: string; size: { width: number; height: number } }
  | { kind: 'minimized'; windowId: string; minimized: boolean }
  | { kind: 'front'; windowId: string }
  | { kind: 'maximize'; windowId: string }
  | { kind: 'tab'; windowId: string; targetIndex: number };

/** One window queued for layout application. */
export interface LayoutApplicationTarget {
  windowId: string;
  /** Stable-sort key: the stored/discovered order. */
  position: number;
  layout: LayoutV1 | null;
}

/**
 * Builds the layout targets for windows that were just opened in `windows`:
 * `position` is the window's own index in `windows`, so a step referring to
 * it lines up with the daemon's own order rather than a filtered one. A
 * window outside `opened`, or carrying no `layout_json`, has nothing to
 * apply and is left out.
 */
export function layoutTargetsForOpened(
  windows: readonly WindowInfo[],
  opened: readonly string[]
): LayoutApplicationTarget[] {
  const openedIds = new Set(opened);
  const targets: LayoutApplicationTarget[] = [];
  windows.forEach((info, position) => {
    if (!openedIds.has(info.window_id) || info.layout_json === undefined) {
      return;
    }
    targets.push({
      windowId: info.window_id,
      position,
      layout: parseLayoutJson(info.layout_json),
    });
  });
  return targets;
}

/**
 * Builds the ordered application plan for a set of targets. `targets` is
 * stable-sorted by `position` ascending first; every later step (front /
 * maximize / tab) is derived from that order.
 *
 * "Maximize candidate": a target with `max: true` AND both `pos`
 * and `size` non-null. A `max: true` target with either null is *not* a
 * maximize candidate -- it is an ordinary front target instead. Multiple valid
 * candidates (a tampered store) resolve to the position-sorted last one; the
 * rest fall back to being front targets.
 */
export function planLayoutApplication(
  targets: LayoutApplicationTarget[],
  currentTabOrder: readonly string[]
): LayoutStep[] {
  const sorted = [...targets].sort((a, b) => a.position - b.position);
  const steps: LayoutStep[] = [];

  for (const target of sorted) {
    const layout = target.layout;
    if (layout !== null && layout.pos !== null) {
      steps.push({ kind: 'position', windowId: target.windowId, position: layout.pos });
    }
    if (layout !== null && layout.size !== null) {
      steps.push({ kind: 'size', windowId: target.windowId, size: layout.size });
    }
    steps.push({ kind: 'minimized', windowId: target.windowId, minimized: layout?.min ?? false });
  }

  const maximizeCandidates = sorted.filter(
    (target) =>
      target.layout !== null &&
      target.layout.max === true &&
      target.layout.pos !== null &&
      target.layout.size !== null
  );
  const maximizeCandidate = maximizeCandidates[maximizeCandidates.length - 1] ?? null;

  const frontTargets = sorted.filter((target) => target !== maximizeCandidate);
  const frontOrder = [...frontTargets].sort((a, b) => (a.layout?.z ?? 0) - (b.layout?.z ?? 0));
  for (const target of frontOrder) {
    steps.push({ kind: 'front', windowId: target.windowId });
  }

  if (maximizeCandidate !== null) {
    steps.push({ kind: 'maximize', windowId: maximizeCandidate.windowId });
    // The trailing `front` is emitted only when the candidate is the sole
    // target: a maximized window renders on top regardless of z-order, so
    // fronting it after other targets were fronted would be redundant.
    if (frontOrder.length === 0) {
      steps.push({ kind: 'front', windowId: maximizeCandidate.windowId });
    }
  }

  const tabOrder = [...sorted].sort((a, b) => (a.layout?.tab ?? 0) - (b.layout?.tab ?? 0));
  for (const target of tabOrder) {
    steps.push({
      kind: 'tab',
      windowId: target.windowId,
      targetIndex: currentTabOrder.length - 1,
    });
  }

  return steps;
}

/**
 * Narrows a {@link planLayoutApplication} plan to what a live snapshot may
 * apply. `workspaceWasEmpty` restores a whole workspace (every step); with
 * live panes already on screen, only geometry (position/size/minimized) is
 * applied, so a newly opened window's layout never steals focus, the tab
 * strip, or maximized state from what the user is already doing.
 */
export function layoutStepsForSnapshot(
  targets: LayoutApplicationTarget[],
  currentTabOrder: readonly string[],
  workspaceWasEmpty: boolean
): LayoutStep[] {
  const steps = planLayoutApplication(targets, currentTabOrder);
  if (workspaceWasEmpty) {
    return steps;
  }
  return steps.filter(
    (step) => step.kind === 'position' || step.kind === 'size' || step.kind === 'minimized'
  );
}

/** The registry operations {@link applyLayoutSteps} drives. */
export interface LayoutTarget {
  setWindowPosition: (windowId: string, position: { x: number; y: number }) => void;
  setWindowSize: (windowId: string, size: { width: number; height: number }) => void;
  setWindowMinimized: (windowId: string, minimized: boolean) => void;
  bringWindowToFront: (windowId: string) => void;
  setWindowMaximized: (windowId: string, maximized: boolean) => void;
  moveTab: (windowId: string, targetIndex: number) => void;
}

/** How to open one restore candidate: from a launch profile, or from a saved argv. */
export type RestoreLaunch =
  | {
      kind: 'profile';
      profile: string;
      cwd: string;
      name: string;
      model: string | null;
      effort: string | null;
    }
  | {
      kind: 'argv';
      command: string[];
      cwd: string;
      name: string;
      detection: string | null;
      vendor: string | null;
    };

/**
 * How to open one restore candidate: from the launch profile it recorded,
 * when it recorded one, else from its saved argv.
 */
export function restoreLaunchOf(candidate: RestoreEntry): RestoreLaunch {
  if (candidate.profile !== null) {
    return {
      kind: 'profile',
      profile: candidate.profile,
      cwd: candidate.cwd,
      name: candidate.name,
      model: candidate.model,
      effort: candidate.effort,
    };
  }
  return argvLaunchOf(candidate);
}

function argvLaunchOf(candidate: RestoreEntry): RestoreLaunch {
  return {
    kind: 'argv',
    command: candidate.command,
    cwd: candidate.cwd,
    name: candidate.name,
    detection: candidate.detection,
    vendor: candidate.vendor,
  };
}

/**
 * The argv launch to retry with after a profile launch failed, `null` when no
 * retry is warranted.
 *
 * Only a daemon rejection of the profile itself (`failure.kind ===
 * 'rejected'`) falls back: `local` / `unsupported` / `bare` are not the
 * daemon refusing the profile, so retrying under a different launch would
 * not address them. A profile the user switched off is not retried either:
 * the saved argv would start the very CLI they turned off. A candidate that
 * named no profile has no second form to retry with, so it is left alone
 * regardless of the failure kind.
 */
export function restoreFallbackOf(
  candidate: RestoreEntry,
  failure: WindowOpenFailure
): RestoreLaunch | null {
  if (
    candidate.profile === null ||
    failure.kind !== 'rejected' ||
    isProfileDisabledRefusal(failure)
  ) {
    return null;
  }
  return argvLaunchOf(candidate);
}

/** Whether the daemon refused a launch because its profile is switched off. */
export function isProfileDisabledRefusal(failure: WindowOpenFailure): boolean {
  return failure.kind === 'rejected' && failure.code === PROFILE_DISABLED_CODE;
}

/**
 * The one-line notice naming the windows a restore held back because their
 * profile is switched off, each name once in first-seen order; `null` when
 * none was held back.
 */
export function restoreNoticeForDisabled(names: readonly string[]): string | null {
  const unique = [...new Set(names)];
  return unique.length === 0 ? null : banners.restoreSkippedDisabledProfiles(unique);
}

/** Applies a layout plan, one target method call per step, in step order. */
export function applyLayoutSteps(target: LayoutTarget, steps: readonly LayoutStep[]): void {
  for (const step of steps) {
    switch (step.kind) {
      case 'position':
        target.setWindowPosition(step.windowId, step.position);
        break;
      case 'size':
        target.setWindowSize(step.windowId, step.size);
        break;
      case 'minimized':
        target.setWindowMinimized(step.windowId, step.minimized);
        break;
      case 'front':
        target.bringWindowToFront(step.windowId);
        break;
      case 'maximize':
        target.setWindowMaximized(step.windowId, true);
        break;
      case 'tab':
        target.moveTab(step.windowId, step.targetIndex);
        break;
    }
  }
}
