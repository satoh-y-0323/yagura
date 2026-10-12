/**
 * The pure model behind the Settings panel's Keyboard tab: the rows, what a
 * recorded chord does, when editing is refused, the notices a row shows, and
 * the controller that turns a press on Change / Reset / Remove into a save.
 * The component only renders these values.
 */
import { writable, type Readable } from 'svelte/store';

import type { LoadKeymapResult } from '../ipc/session-client';
import { validateBinding, type BindingRejection } from '../keymap/binding-rules';
import { defaultKeysFor } from '../keymap/default-keymap';
import type { KeyboardPlatform } from '../keymap/keyboard-platform';
import { ACTION_IDS, type ActionId } from '../keymap/key-binding';
import { assignChord, removeAction, resetAction, type Keys } from '../keymap/keymap-model';

/** The section of the tab a row is listed under. */
export type KeyboardGroup = 'windows' | 'focus-and-panels';

/** The group of every action. */
const GROUP_OF: Readonly<Record<ActionId, KeyboardGroup>> = {
  'focus-next-window': 'windows',
  'focus-previous-window': 'windows',
  'focus-next-attention': 'windows',
  'open-window-form': 'windows',
  'close-window': 'windows',
  'interrupt-window': 'windows',
  'minimize-window': 'windows',
  'toggle-maximize-window': 'windows',
  'toggle-sidebar-focus': 'focus-and-panels',
  'toggle-chat': 'focus-and-panels',
  'toggle-plans': 'focus-and-panels',
  'toggle-settings': 'focus-and-panels',
};

/** One row of the tab: an action, its chord and what may be done to it. */
export interface KeyboardRow {
  action: ActionId;
  group: KeyboardGroup;
  /** The chord the action has now, or `null` when it has none. */
  chord: string | null;
  /** The chord differs from the default (including having none). */
  isChanged: boolean;
  /** Reset would change something. */
  canReset: boolean;
  /** Remove would change something. */
  canRemove: boolean;
}

/** The rows of the tab, one per action in `ACTION_IDS` order, judged against `defaults`. */
export function keyboardRows(
  keys: Keys,
  defaults: Readonly<Record<ActionId, string>>
): KeyboardRow[] {
  return ACTION_IDS.map((action) => {
    const chord = keys[action];
    const isChanged = chord !== defaults[action];
    return {
      action,
      group: GROUP_OF[action],
      chord,
      isChanged,
      canReset: isChanged,
      canRemove: chord !== null,
    };
  });
}

/** What a recorded chord came to: refused with a reason, or given to the action. */
export type RecordOutcome =
  | { kind: 'rejected'; chord: string; reason: BindingRejection }
  | { kind: 'assigned'; chord: string; keys: Keys; movedFrom: ActionId | null };

/**
 * Judges `chord` for `platform` and, when it is allowed, gives it to `action`.
 * A chord another action holds is taken from it (`movedFrom`).
 */
export function applyRecordedChord(
  keys: Keys,
  action: ActionId,
  chord: string,
  platform: KeyboardPlatform
): RecordOutcome {
  const reason = validateBinding(chord, platform);
  if (reason !== null) return { kind: 'rejected', chord, reason };
  const assigned = assignChord(keys, action, chord);
  return { kind: 'assigned', chord, keys: assigned.keys, movedFrom: assigned.movedFrom };
}

/** How the keymap load ended, as the shell saw it. */
export type KeymapLoadStatus =
  { kind: 'not-loaded' } | { kind: 'rejected' } | { kind: 'loaded'; result: LoadKeymapResult };

/** Why the tab refuses every edit. */
export type KeymapEditBlock =
  'not_loaded' | 'rejected' | 'read_failed' | 'invalid_toml' | 'too_large';

/**
 * Why editing must stop, or `null` when it may go on. A missing file is the
 * normal way of running on the defaults; any other unread file would be
 * overwritten by the first save, so it stops editing.
 */
export function keymapEditBlock(status: KeymapLoadStatus): KeymapEditBlock | null {
  if (status.kind === 'not-loaded') return 'not_loaded';
  if (status.kind === 'rejected') return 'rejected';
  const failure = status.result.failure;
  return failure === undefined || failure === 'not_found' ? null : failure;
}

/** Where keymap.toml is, per platform, when the app has not reported its own path. */
const DOCUMENTED_KEYMAP_LOCATION: Readonly<Record<KeyboardPlatform, string>> = {
  windows: '%LOCALAPPDATA%\\io.github.satoh-y-0323.yagura\\keymap.toml',
  mac: '~/Library/Application Support/yagura/keymap.toml',
  linux: '~/.config/yagura/keymap.toml',
};

/**
 * The location of keymap.toml to show: the path the app reads when it has
 * reported one, else the documented default location of `platform`.
 */
export function keymapFileLocation(
  platform: KeyboardPlatform,
  reportedPath: string | null = null
): string {
  return reportedPath ?? DOCUMENTED_KEYMAP_LOCATION[platform];
}

/** A line under a row: why a chord was refused, or that a chord moved between two rows. */
export type RowNotice =
  | { kind: 'rejected'; chord: string; reason: BindingRejection }
  | { kind: 'took'; chord: string; from: ActionId }
  | { kind: 'lost'; chord: string; to: ActionId };

/** What `rowNotices` reads of an outcome: the table is not needed. */
type NoticeSource =
  | { kind: 'rejected'; chord: string; reason: BindingRejection }
  | { kind: 'assigned'; chord: string; movedFrom: ActionId | null };

/**
 * The notices an outcome puts under rows: a refusal under the row that was
 * being changed; a move under both the row that received the chord and the one
 * that lost it; nothing for an assignment that took no chord from anyone.
 */
export function rowNotices(
  action: ActionId,
  outcome: NoticeSource
): Partial<Record<ActionId, RowNotice>> {
  if (outcome.kind === 'rejected') {
    return { [action]: { kind: 'rejected', chord: outcome.chord, reason: outcome.reason } };
  }
  if (outcome.movedFrom === null) return {};
  return {
    [action]: { kind: 'took', chord: outcome.chord, from: outcome.movedFrom },
    [outcome.movedFrom]: { kind: 'lost', chord: outcome.chord, to: action },
  };
}

/** What the tab shows that is not derived from the keys. */
export interface KeyboardTabState {
  /** The row waiting for a chord, or `null`. */
  recording: ActionId | null;
  notices: Partial<Record<ActionId, RowNotice>>;
}

/** What the controller needs from the shell. */
export interface KeyboardTabDeps {
  platform: KeyboardPlatform;
  /** The keys now in force, read when a press happens. */
  keys: () => Keys;
  /** Why editing is refused now, or `null`; read when a press happens. */
  editBlock: () => KeymapEditBlock | null;
  /** Makes the next chord press a recording; `onResult` gets the chord, or `null` when it was cancelled. */
  beginRecording: (onResult: (chord: string | null) => void) => void;
  cancelRecording: () => void;
  /** Applies `edit` to the saved table and saves it; resolves whether the save was kept. */
  commit: (edit: (keys: Keys) => Keys) => Promise<boolean>;
}

/** The tab's state plus the three presses a row has and the tab's teardown. */
export interface KeyboardTabController extends Readable<KeyboardTabState> {
  /** Starts recording a chord for `action`; the row pressed last wins. */
  change: (action: ActionId) => void;
  /** Gives `action` its default chord back. */
  reset: (action: ActionId) => Promise<void>;
  /** Leaves `action` without a chord. */
  remove: (action: ActionId) => Promise<void>;
  /** Ends a recording that is under way; call when the tab goes away. */
  dispose: () => void;
}

/**
 * Creates the controller of the Keyboard tab. Every press is ignored while
 * `editBlock` names a reason. A recording ends the moment it reports, before
 * its save finishes, and a move is shown only once the save was kept.
 */
export function createKeyboardTabController(deps: KeyboardTabDeps): KeyboardTabController {
  const { platform } = deps;
  const defaults = defaultKeysFor(platform);
  let current: KeyboardTabState = { recording: null, notices: {} };
  const state = writable<KeyboardTabState>(current);
  // Identifies the recording that is under way, so a stale one reports into the void.
  let recordingId = 0;

  function update(next: Partial<KeyboardTabState>): void {
    current = { ...current, ...next };
    state.set(current);
  }

  function withoutNotice(action: ActionId): Partial<Record<ActionId, RowNotice>> {
    const rest = { ...current.notices };
    delete rest[action];
    return rest;
  }

  function endRecording(): void {
    if (current.recording === null) return;
    recordingId += 1;
    deps.cancelRecording();
    update({ recording: null });
  }

  async function save(
    action: ActionId,
    chord: string,
    movedFrom: ActionId | null,
    edit: (keys: Keys) => Keys
  ): Promise<void> {
    const kept = await deps.commit(edit);
    if (kept) {
      update({
        notices: {
          ...current.notices,
          ...rowNotices(action, { kind: 'assigned', chord, movedFrom }),
        },
      });
    }
  }

  function report(id: number, action: ActionId, chord: string | null): void {
    if (id !== recordingId) return;
    update({ recording: null });
    if (chord === null) return;
    const outcome = applyRecordedChord(deps.keys(), action, chord, platform);
    if (outcome.kind === 'rejected') {
      update({ notices: { ...current.notices, ...rowNotices(action, outcome) } });
      return;
    }
    void save(action, chord, outcome.movedFrom, (keys) => assignChord(keys, action, chord).keys);
  }

  function change(action: ActionId): void {
    if (deps.editBlock() !== null) return;
    recordingId += 1;
    const id = recordingId;
    update({ recording: action, notices: withoutNotice(action) });
    deps.beginRecording((chord) => report(id, action, chord));
  }

  async function reset(action: ActionId): Promise<void> {
    if (deps.editBlock() !== null) return;
    endRecording();
    update({ notices: withoutNotice(action) });
    const keys = deps.keys();
    if (keys[action] === defaults[action]) return;
    const movedFrom = resetAction(keys, defaults, action).movedFrom;
    await save(
      action,
      defaults[action],
      movedFrom,
      (table) => resetAction(table, defaults, action).keys
    );
  }

  async function remove(action: ActionId): Promise<void> {
    if (deps.editBlock() !== null) return;
    endRecording();
    update({ notices: withoutNotice(action) });
    if (deps.keys()[action] === null) return;
    await deps.commit((table) => removeAction(table, action));
  }

  return {
    subscribe: state.subscribe,
    change,
    reset,
    remove,
    dispose: endRecording,
  };
}
