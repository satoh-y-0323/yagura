/**
 * The keymap store: the effective keys of the running app. A keymap file is
 * loaded into it, and an edit takes effect only after it has been saved.
 */
import { writable, type Readable } from 'svelte/store';

import type { LoadKeymapResult, SaveKeymapResult } from '../ipc/session-client';
import { defaultKeysFor } from './default-keymap';
import type { KeyboardPlatform } from './keyboard-platform';
import type { KeyMap } from './key-binding';
import { bindingsOf, effectiveKeys, overridesOf, type Keys } from './keymap-model';

/** Why the last edit was not kept: a refusal the core named, or a failure nobody named. */
export type KeymapSaveFailureKind = 'no_config_root' | 'too_large' | 'write_failed' | 'unexpected';

/** What the store holds. `bindings` is always derived from `keys`. */
export interface KeymapState {
  keys: Keys;
  bindings: KeyMap;
  saveFailure: KeymapSaveFailureKind | null;
}

/** A keymap store: readable state plus the two ways to change it. */
export interface KeymapStore extends Readable<KeymapState> {
  /**
   * Takes the keys of a loaded keymap file, or the defaults when the load
   * failed.
   *
   * @returns The number of entries that do not take effect: discarded overrides
   * plus the legacy entries of the file.
   */
  load: (result: LoadKeymapResult) => { discarded: number };
  /**
   * Applies `edit` to the table the previous save confirmed, saves the result,
   * and adopts it only if the save succeeded. Commits are saved one at a time,
   * in call order.
   *
   * @returns Whether this edit was saved and adopted. Never rejects.
   */
  commit: (edit: (keys: Keys) => Keys) => Promise<boolean>;
}

/** Dependencies of `createKeymapStore`. `save` receives the overrides to write. */
export interface KeymapStoreDeps {
  platform: KeyboardPlatform;
  save: (overrides: Record<string, string>) => Promise<SaveKeymapResult>;
}

/** Creates a keymap store that starts on the defaults of `platform`. */
export function createKeymapStore(deps: KeymapStoreDeps): KeymapStore {
  const { platform, save } = deps;
  const defaults = defaultKeysFor(platform);

  function stateOf(keys: Keys, saveFailure: KeymapSaveFailureKind | null): KeymapState {
    return { keys, bindings: bindingsOf(keys), saveFailure };
  }

  let current = stateOf(effectiveKeys(defaults, {}, platform).keys, null);
  const store = writable<KeymapState>(current);
  const setState = (next: KeymapState): void => {
    current = next;
    store.set(next);
  };
  let queue: Promise<unknown> = Promise.resolve();

  function load(result: LoadKeymapResult): { discarded: number } {
    // A failed load leaves whatever `keys` holds untrusted, so it is not read.
    if (result.failure !== undefined) {
      setState(stateOf(effectiveKeys(defaults, {}, platform).keys, current.saveFailure));
      return { discarded: 0 };
    }
    const combined = effectiveKeys(defaults, result.keys, platform);
    setState(stateOf(combined.keys, current.saveFailure));
    return { discarded: combined.discarded + result.legacy_entries };
  }

  async function saveEdit(edit: (keys: Keys) => Keys): Promise<boolean> {
    const next = edit(current.keys);
    let outcome: SaveKeymapResult | null;
    try {
      outcome = await save(overridesOf(defaults, next));
    } catch {
      outcome = null;
    }
    if (outcome?.ok === true) {
      setState(stateOf(next, null));
      return true;
    }
    setState({ ...current, saveFailure: outcome === null ? 'unexpected' : outcome.reason });
    return false;
  }

  function commit(edit: (keys: Keys) => Keys): Promise<boolean> {
    const done = queue.then(() => saveEdit(edit));
    // A rejected edit must not stall the commits queued behind it.
    queue = done.then(
      () => undefined,
      () => undefined
    );
    return done;
  }

  return { subscribe: store.subscribe, load, commit };
}
