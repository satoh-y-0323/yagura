/**
 * The keymap store: loading a keymap file into the effective keys, saving an edit
 * before it takes effect, and keeping edits made while a save is pending in order.
 */
import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import type { LoadKeymapResult } from '../ipc/session-client';
import { validateBinding } from './binding-rules';
import { defaultKeysFor } from './default-keymap';
import type { KeyboardPlatform } from './keyboard-platform';
import type { ActionId } from './key-binding';
import { createKeymapStore } from './keymap-store';
import { assignChord, bindingsOf, effectiveKeys, overridesOf, type Keys } from './keymap-model';

type SaveFailureReason = 'no_config_root' | 'too_large' | 'write_failed';
type SaveResult = { ok: true } | { ok: false; reason: SaveFailureReason };
type SaveFn = (overrides: Record<string, string>) => Promise<SaveResult>;

const PLATFORM: KeyboardPlatform = 'windows';
const DEFAULTS = defaultKeysFor(PLATFORM);
const CHORD_A = 'Ctrl+Shift+j';
const CHORD_B = 'Ctrl+Shift+u';
const ACTION_A: ActionId = 'toggle-chat';
const ACTION_B: ActionId = 'toggle-plans';

interface PendingSave {
  overrides: Record<string, string>;
  resolve: (result: SaveResult) => void;
  reject: (error: unknown) => void;
}

/** A save whose calls stay pending until the test settles them, in call order. */
function deferredSave(): { save: SaveFn; pending: PendingSave[] } {
  const pending: PendingSave[] = [];
  const save: SaveFn = (overrides) =>
    new Promise<SaveResult>((resolve, reject) => {
      pending.push({ overrides, resolve, reject });
    });
  return { save, pending };
}

function storeWith(save: SaveFn) {
  return createKeymapStore({ platform: PLATFORM, save });
}

function loaded(overrides: Record<string, string> = {}, legacyEntries = 0): LoadKeymapResult {
  return { keys: overrides, legacy_entries: legacyEntries };
}

function give(action: ActionId, chord: string): (keys: Keys) => Keys {
  return (keys) => assignChord(keys, action, chord).keys;
}

async function flush(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

function defaultKeys(): Keys {
  return effectiveKeys(DEFAULTS, {}, PLATFORM).keys;
}

describe('fixtures', () => {
  it('uses chords that the binding rules accept and that no default holds', () => {
    for (const chord of [CHORD_A, CHORD_B]) {
      expect(validateBinding(chord, PLATFORM)).toBeNull();
      expect(Object.values(DEFAULTS)).not.toContain(chord);
    }
  });
});

describe('load', () => {
  it.each(['windows', 'mac', 'linux'] as const)(
    'makes the keys and bindings the effective combination of the defaults and the file on %s',
    (platform) => {
      const defaults = defaultKeysFor(platform);
      const overrides = { 'toggle-chat': 'Ctrl+Shift+j', 'close-window': '' };
      const store = createKeymapStore({ platform, save: () => Promise.resolve({ ok: true }) });

      store.load(loaded(overrides));

      const expected = effectiveKeys(defaults, overrides, platform).keys;
      expect(get(store).keys).toEqual(expected);
      expect(get(store).bindings).toEqual(bindingsOf(expected));
    }
  );

  it('returns the discarded overrides plus the legacy entries as one count', () => {
    const store = storeWith(() => Promise.resolve({ ok: true }));
    const overrides = { 'no-such-action': 'Ctrl+Shift+j', 'toggle-chat': 'Ctrl+Shift+u' };
    const discarded = effectiveKeys(DEFAULTS, overrides, PLATFORM).discarded;
    expect(discarded).toBeGreaterThan(0);

    const result = store.load(loaded(overrides, 2));

    expect(result).toEqual({ discarded: discarded + 2 });
  });

  it('returns zero for a clean file with no legacy entries', () => {
    const store = storeWith(() => Promise.resolve({ ok: true }));

    expect(store.load(loaded({ 'toggle-chat': CHORD_A }))).toEqual({ discarded: 0 });
  });

  it.each([['not_found'], ['read_failed'], ['invalid_toml'], ['too_large']] as const)(
    'runs on the defaults and counts nothing when the load failed with %s',
    (failure) => {
      const store = storeWith(() => Promise.resolve({ ok: true }));

      const result = store.load({ keys: { 'toggle-chat': CHORD_A }, legacy_entries: 0, failure });

      expect(result).toEqual({ discarded: 0 });
      expect(get(store).keys).toEqual(defaultKeys());
      expect(get(store).bindings).toEqual(bindingsOf(defaultKeys()));
    }
  );

  it('starts without a save failure', () => {
    const store = storeWith(() => Promise.resolve({ ok: true }));

    expect(get(store).saveFailure).toBeNull();
  });
});

describe('commit', () => {
  it('saves only the overrides of the edited table', async () => {
    const save = vi.fn<SaveFn>(() => Promise.resolve({ ok: true }));
    const store = storeWith(save);
    store.load(loaded());

    await store.commit(give(ACTION_A, CHORD_A));

    const next = give(ACTION_A, CHORD_A)(defaultKeys());
    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith(overridesOf(DEFAULTS, next));
    expect(Object.keys(overridesOf(DEFAULTS, next))).toContain(ACTION_A);
  });

  it('hands the edit the table in effect and applies it once the save succeeded', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded({ [ACTION_B]: CHORD_B }));
    const before = get(store).keys;
    const edit = vi.fn(give(ACTION_A, CHORD_A));

    const done = store.commit(edit);
    await flush();

    expect(pending).toHaveLength(1);
    expect(get(store).keys).toEqual(before);
    expect(edit).toHaveBeenLastCalledWith(before);

    pending[0]?.resolve({ ok: true });
    const ok = await done;

    const next = give(ACTION_A, CHORD_A)(before);
    expect(ok).toBe(true);
    expect(get(store).keys).toEqual(next);
    expect(get(store).bindings).toEqual(bindingsOf(next));
    expect(get(store).saveFailure).toBeNull();
  });

  it.each([['no_config_root'], ['too_large'], ['write_failed']] as const)(
    'leaves the keys and bindings alone and records "%s" when the save is refused',
    async (reason) => {
      const store = storeWith(() => Promise.resolve({ ok: false, reason }));
      store.load(loaded({ [ACTION_B]: CHORD_B }));
      const before = get(store);

      const ok = await store.commit(give(ACTION_A, CHORD_A));

      expect(ok).toBe(false);
      expect(get(store).keys).toEqual(before.keys);
      expect(get(store).bindings).toEqual(before.bindings);
      expect(get(store).saveFailure).toBe(reason);
    }
  );

  it('leaves the keys alone and records an unexpected failure when the save rejects', async () => {
    const store = storeWith(() => Promise.reject(new Error('boundary')));
    store.load(loaded());
    const before = get(store).keys;

    const ok = await store.commit(give(ACTION_A, CHORD_A));

    expect(ok).toBe(false);
    expect(get(store).keys).toEqual(before);
    expect(get(store).saveFailure).toBe('unexpected');
  });

  it('clears the save failure at the next success and applies that edit', async () => {
    const results: SaveResult[] = [{ ok: false, reason: 'write_failed' }, { ok: true }];
    const store = storeWith(() => Promise.resolve(results.shift() ?? { ok: true }));
    store.load(loaded());

    expect(await store.commit(give(ACTION_A, CHORD_A))).toBe(false);
    expect(get(store).saveFailure).toBe('write_failed');

    expect(await store.commit(give(ACTION_B, CHORD_B))).toBe(true);

    expect(get(store).saveFailure).toBeNull();
    expect(get(store).keys).toEqual(give(ACTION_B, CHORD_B)(defaultKeys()));
  });
});

describe('commit while a save is pending', () => {
  it('starts the second save only after the first one finished', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(give(ACTION_B, CHORD_B));
    await flush();

    expect(pending).toHaveLength(1);

    pending[0]?.resolve({ ok: true });
    await flush();

    expect(pending).toHaveLength(2);
    pending[1]?.resolve({ ok: true });
    expect(await Promise.all([first, second])).toEqual([true, true]);
  });

  it('keeps both edits: the second save carries the first edit and the store ends with both', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(give(ACTION_B, CHORD_B));
    await flush();
    pending[0]?.resolve({ ok: true });
    await flush();
    pending[1]?.resolve({ ok: true });
    await Promise.all([first, second]);

    const both = give(ACTION_B, CHORD_B)(give(ACTION_A, CHORD_A)(defaultKeys()));
    expect(pending[0]?.overrides).toEqual(
      overridesOf(DEFAULTS, give(ACTION_A, CHORD_A)(defaultKeys()))
    );
    expect(pending[1]?.overrides).toEqual(overridesOf(DEFAULTS, both));
    expect(get(store).keys).toEqual(both);
    expect(get(store).bindings).toEqual(bindingsOf(both));
  });

  it('applies the second edit to the table the first save confirmed', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());
    const secondEdit = vi.fn(give(ACTION_B, CHORD_B));

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(secondEdit);
    await flush();
    pending[0]?.resolve({ ok: true });
    await flush();
    pending[1]?.resolve({ ok: true });
    await Promise.all([first, second]);

    expect(secondEdit).toHaveBeenLastCalledWith(give(ACTION_A, CHORD_A)(defaultKeys()));
  });

  it('drops a refused first edit and re-applies the second to the confirmed table', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(give(ACTION_B, CHORD_B));
    await flush();
    pending[0]?.resolve({ ok: false, reason: 'write_failed' });
    await flush();

    expect(pending).toHaveLength(2);
    pending[1]?.resolve({ ok: true });
    const results = await Promise.all([first, second]);

    const onlySecond = give(ACTION_B, CHORD_B)(defaultKeys());
    expect(results).toEqual([false, true]);
    expect(pending[1]?.overrides).toEqual(overridesOf(DEFAULTS, onlySecond));
    expect(get(store).keys).toEqual(onlySecond);
    expect(get(store).saveFailure).toBeNull();
  });

  it('keeps the first edit when only the second save is refused', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(give(ACTION_B, CHORD_B));
    await flush();
    pending[0]?.resolve({ ok: true });
    await flush();
    pending[1]?.resolve({ ok: false, reason: 'too_large' });
    const results = await Promise.all([first, second]);

    expect(results).toEqual([true, false]);
    expect(get(store).keys).toEqual(give(ACTION_A, CHORD_A)(defaultKeys()));
    expect(get(store).saveFailure).toBe('too_large');
  });

  it('continues the queue after a save rejects', async () => {
    const { save, pending } = deferredSave();
    const store = storeWith(save);
    store.load(loaded());

    const first = store.commit(give(ACTION_A, CHORD_A));
    const second = store.commit(give(ACTION_B, CHORD_B));
    await flush();
    pending[0]?.reject(new Error('boundary'));
    await flush();

    expect(pending).toHaveLength(2);
    pending[1]?.resolve({ ok: true });
    expect(await Promise.all([first, second])).toEqual([false, true]);
    expect(get(store).keys).toEqual(give(ACTION_B, CHORD_B)(defaultKeys()));
  });
});

describe('load after an earlier load', () => {
  it('goes back to the defaults when the file can no longer be read', () => {
    const store = storeWith(() => Promise.resolve({ ok: true }));
    store.load(loaded({ 'toggle-chat': CHORD_A }));
    expect(get(store).keys).not.toEqual(defaultKeys());

    store.load({ keys: {}, legacy_entries: 0, failure: 'read_failed' });

    expect(get(store).keys).toEqual(defaultKeys());
    expect(get(store).bindings).toEqual(bindingsOf(defaultKeys()));
  });

  it('keeps the save failure of an earlier edit across a load', async () => {
    const store = storeWith(() => Promise.resolve({ ok: false, reason: 'write_failed' }));
    await store.commit(give(ACTION_A, CHORD_A));

    store.load(loaded());
    expect(get(store).saveFailure).toBe('write_failed');

    store.load({ keys: {}, legacy_entries: 0, failure: 'invalid_toml' });
    expect(get(store).saveFailure).toBe('write_failed');
  });
});
