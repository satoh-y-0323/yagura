/**
 * The single keydown entry point for shortcuts. It decides, for every key the
 * app sees, whether the key is a shortcut (stopped and run) or input for the
 * terminal and the text fields (left untouched), and records a new chord for
 * the Keyboard settings.
 */

import type { ActionId, KeyMap } from './key-binding';
import { chordOfEvent, isImeKey, type KeyEventLike } from './key-chord';
import type { KeyboardPlatform } from './keyboard-platform';
import { isReloadKey } from './reload-keys';

/** The parts of a `KeyboardEvent` the dispatcher reads and acts on. */
export interface KeydownLike extends KeyEventLike {
  isComposing: boolean;
  keyCode: number;
  repeat: boolean;
  preventDefault(): void;
  stopPropagation(): void;
}

/** What the dispatcher reads on every keydown and what it runs. */
export interface ShortcutDispatcherDeps {
  /** The current bindings; read on every keydown so a change applies at once. */
  bindings: () => KeyMap;
  /** Whether a modal dialog is open; the dialog then owns every key. */
  isModalOpen: () => boolean;
  /** Runs a matched action. */
  run: (action: ActionId) => void;
  /** The platform whose reload keys are cancelled. */
  platform: KeyboardPlatform;
}

/** The dispatcher, bound to its dependencies. */
export interface ShortcutDispatcher {
  /** Handles one keydown; meant for a window-level listener in the capture phase. */
  handleKeydown: (event: KeydownLike) => void;
  /**
   * Makes the next complete chord a recording instead of a shortcut.
   * `onResult` is called once: with the chord, or with null when a plain
   * Escape or an opening modal dialog cancels the recording.
   */
  beginRecording: (onResult: (chord: string | null) => void) => void;
  /** Ends a recording without reporting anything. */
  cancelRecording: () => void;
  /** Whether a recording is in progress. */
  isRecording: () => boolean;
}

function stop(event: KeydownLike): void {
  event.preventDefault();
  event.stopPropagation();
}

/**
 * Builds the shortcut dispatcher. A matched keydown is stopped and runs its
 * action once (a held-down repeat is stopped but not run again); anything
 * else, a composing key, or any key while a modal dialog is open is left for
 * the terminal and the inputs. A reload key only has its default cancelled,
 * whatever the state, and still reaches the terminal.
 */
export function createShortcutDispatcher(deps: ShortcutDispatcherDeps): ShortcutDispatcher {
  let recording: ((chord: string | null) => void) | null = null;

  function record(event: KeydownLike, report: (chord: string | null) => void): void {
    const finish = (chord: string | null): void => {
      recording = null;
      report(chord);
    };
    // The dialog takes over the keyboard, so the recording gives way to it.
    if (deps.isModalOpen()) {
      finish(null);
      return;
    }
    // Stopping a composing key would break the input method's conversion.
    if (isImeKey(event)) {
      return;
    }
    stop(event);
    const chord = chordOfEvent(event);
    if (chord === null) {
      return;
    }
    finish(chord === 'Escape' ? null : chord);
  }

  function dispatch(event: KeydownLike): void {
    if (isImeKey(event) || deps.isModalOpen()) {
      return;
    }
    const chord = chordOfEvent(event);
    const action = chord === null ? undefined : deps.bindings().get(chord);
    if (action === undefined) {
      return;
    }
    stop(event);
    if (!event.repeat) {
      deps.run(action);
    }
  }

  return {
    handleKeydown: (event) => {
      const reload = isReloadKey(event, deps.platform);
      // Not stopped: the terminal ignores defaultPrevented, so Ctrl+R still reaches the CLI.
      if (reload) {
        event.preventDefault();
      }
      if (recording !== null) {
        record(event, recording);
      } else if (!reload) {
        dispatch(event);
      }
    },
    beginRecording: (onResult) => {
      recording = onResult;
    },
    cancelRecording: () => {
      recording = null;
    },
    isRecording: () => recording !== null,
  };
}
