/**
 * Contract for the launch-profile memory: the last profile id and working
 * directory used at this terminal, degrading the same way `theme-store.ts`
 * does when storage is unavailable (mirrors `createBrowserThemeDeps` /
 * `createThemeStore`).
 */
import { describe, it, expect, vi } from 'vitest';

import {
  createBrowserProfileMemoryDeps,
  createProfileMemory,
  withRememberedOption,
  type ProfileMemoryDeps,
  type RememberedProfile,
} from './profile-store';

interface FakeDeps {
  deps: ProfileMemoryDeps;
  storageReadMock: ReturnType<typeof vi.fn<() => string | null>>;
  storageWriteMock: ReturnType<typeof vi.fn<(value: string) => void>>;
}

function makeDeps(stored: string | null = null): FakeDeps {
  const storageReadMock = vi.fn<() => string | null>(() => stored);
  const storageWriteMock = vi.fn<(value: string) => void>();
  return {
    deps: { storageRead: storageReadMock, storageWrite: storageWriteMock },
    storageReadMock,
    storageWriteMock,
  };
}

const DEFAULT_ENTRY = { profileId: null, cwd: null, options: {} };

describe('createProfileMemory - read', () => {
  it('returns { profileId: null, cwd: null } when nothing is stored', () => {
    const { deps } = makeDeps(null);
    expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
  });

  it('restores a previously written entry', () => {
    const { deps } = makeDeps(JSON.stringify({ profileId: 'codex-cli', cwd: 'C:\\work' }));
    expect(createProfileMemory(deps).read()).toEqual({
      profileId: 'codex-cli',
      cwd: 'C:\\work',
      options: {},
    });
  });

  it('restores an entry whose cwd was never set (profileId only)', () => {
    const { deps } = makeDeps(JSON.stringify({ profileId: 'shell', cwd: null }));
    expect(createProfileMemory(deps).read()).toEqual({
      profileId: 'shell',
      cwd: null,
      options: {},
    });
  });

  it('falls back to the default entry on malformed JSON, without throwing', () => {
    const { deps } = makeDeps('{not json');
    expect(() => createProfileMemory(deps).read()).not.toThrow();
    expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
  });

  it('falls back to the default entry when the stored value is a JSON array', () => {
    const { deps } = makeDeps('["codex-cli","C:\\\\work"]');
    expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
  });

  it.each([
    ['profileId is a number', { profileId: 42, cwd: null }],
    ['profileId is an object', { profileId: {}, cwd: null }],
  ])('narrows profileId to null when %s', (_label, raw) => {
    const { deps } = makeDeps(JSON.stringify(raw));
    expect(createProfileMemory(deps).read().profileId).toBeNull();
  });

  it.each([
    ['cwd is a number', { profileId: null, cwd: 7 }],
    ['cwd is an object', { profileId: null, cwd: {} }],
  ])('narrows cwd to null when %s', (_label, raw) => {
    const { deps } = makeDeps(JSON.stringify(raw));
    expect(createProfileMemory(deps).read().cwd).toBeNull();
  });

  it.each(['null', '42', '"codex-cli"', 'true'])(
    'falls back to the default entry when the stored JSON value is %s',
    (stored) => {
      const { deps } = makeDeps(stored);
      expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
    }
  );

  it('falls back to the default entry when storageRead throws', () => {
    const storageReadMock = vi.fn<() => string | null>(() => {
      throw new Error('denied');
    });
    const deps: ProfileMemoryDeps = { storageRead: storageReadMock, storageWrite: vi.fn() };
    expect(() => createProfileMemory(deps).read()).not.toThrow();
    expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
  });
});

describe('createProfileMemory - write', () => {
  it('persists the entry as JSON under one call to storageWrite', () => {
    const { deps, storageWriteMock } = makeDeps(null);
    createProfileMemory(deps).write({
      profileId: 'codex-cli',
      cwd: 'C:\\work',
      options: {},
    });

    expect(storageWriteMock).toHaveBeenCalledTimes(1);
    const [value] = storageWriteMock.mock.calls[0] as [string];
    expect(JSON.parse(value)).toEqual({
      profileId: 'codex-cli',
      cwd: 'C:\\work',
      options: {},
    });
  });

  it('a value written can be read back by a second memory over the same storage', () => {
    let stored: string | null = null;
    const deps: ProfileMemoryDeps = {
      storageRead: () => stored,
      storageWrite: (value) => {
        stored = value;
      },
    };
    createProfileMemory(deps).write({ profileId: 'shell', cwd: null });
    expect(createProfileMemory(deps).read()).toEqual({
      profileId: 'shell',
      cwd: null,
      options: {},
    });
  });

  it('does not throw when storageWrite fails (best effort, matching theme-store.select)', () => {
    const storageWriteMock = vi.fn<(value: string) => void>(() => {
      throw new Error('denied');
    });
    const deps: ProfileMemoryDeps = { storageRead: () => null, storageWrite: storageWriteMock };
    expect(() =>
      createProfileMemory(deps).write({ profileId: 'codex-cli', cwd: null })
    ).not.toThrow();
  });
});

/**
 * `RememberedProfile.options` is a per-profile-id
 * dictionary of the last model/effort used, narrowed defensively because a
 * browser origin's storage is shared with whatever else uses it.
 */
describe('createProfileMemory - read: options', () => {
  it('narrows a stored entry with no options field (the shape stored before launch options existed) to an empty dictionary', () => {
    const { deps } = makeDeps(JSON.stringify({ profileId: 'codex-cli', cwd: null }));
    expect(createProfileMemory(deps).read().options).toEqual({});
  });

  it.each([
    ['options is null', { profileId: 'codex-cli', cwd: '/w', options: null }],
    ['options is an array', { profileId: 'codex-cli', cwd: '/w', options: [] }],
    ['options is a string', { profileId: 'codex-cli', cwd: '/w', options: 'x' }],
  ])('narrows to an empty dictionary when %s, keeping the profile and directory', (_label, raw) => {
    const { deps } = makeDeps(JSON.stringify(raw));
    expect(createProfileMemory(deps).read()).toEqual({
      profileId: 'codex-cli',
      cwd: '/w',
      options: {},
    });
  });

  it('reads a per-profile model/effort record back unchanged', () => {
    const { deps } = makeDeps(
      JSON.stringify({
        profileId: null,
        cwd: null,
        options: { 'codex-cli': { model: 'strong', effort: 'high' } },
      })
    );
    expect(createProfileMemory(deps).read().options).toEqual({
      'codex-cli': { model: 'strong', effort: 'high' },
    });
  });

  it.each([
    ['the record is a string', { p: 'garbage' }],
    ['the record is a number', { p: 3 }],
    ['the record is null', { p: null }],
  ])('does not adopt a profile key whose record is not an object (%s)', (_label, options) => {
    const { deps } = makeDeps(JSON.stringify({ profileId: null, cwd: null, options }));
    const result = createProfileMemory(deps).read();
    expect(result.options['p']).toBeUndefined();
  });

  it.each([
    ['model is a number', { model: 1, effort: 'high' }, { model: null, effort: 'high' }],
    ['effort is an object', { model: 'strong', effort: {} }, { model: 'strong', effort: null }],
  ])(
    'narrows a non-string model/effort to null while keeping the other field (%s)',
    (_label, storedRecord, expectedRecord) => {
      const { deps } = makeDeps(
        JSON.stringify({ profileId: null, cwd: null, options: { p: storedRecord } })
      );
      expect(createProfileMemory(deps).read().options['p']).toEqual(expectedRecord);
    }
  );

  it('has a null prototype', () => {
    const { deps } = makeDeps(null);
    expect(Object.getPrototypeOf(createProfileMemory(deps).read().options)).toBeNull();
  });

  it('does not adopt __proto__ or constructor keys from a stored entry, while adopting an ordinary sibling key', () => {
    // Built from a JSON *string* (not an object literal), so `__proto__`
    // parses as an ordinary own key rather than mutating the prototype.
    const stored =
      '{"profileId":null,"cwd":null,"options":' +
      '{"__proto__":{"model":"strong","effort":"high"},' +
      '"constructor":{"model":"strong","effort":"high"},' +
      '"p":{"model":"strong","effort":"high"}}}';
    const { deps } = makeDeps(stored);
    const { options } = createProfileMemory(deps).read();
    expect(options['__proto__']).toBeUndefined();
    expect(options['constructor']).toBeUndefined();
    expect(options['p']).toEqual({ model: 'strong', effort: 'high' });
  });

  it('returns a fresh options dictionary on every call, even when empty', () => {
    const { deps } = makeDeps(null);
    const memory = createProfileMemory(deps);
    expect(memory.read().options).not.toBe(memory.read().options);
  });

  it('persists and restores a per-profile options record across write and read', () => {
    let stored: string | null = null;
    const deps: ProfileMemoryDeps = {
      storageRead: () => stored,
      storageWrite: (value) => {
        stored = value;
      },
    };
    const memory = createProfileMemory(deps);
    memory.write({
      profileId: 'codex-cli',
      cwd: null,
      options: { 'codex-cli': { model: 'strong', effort: 'high' } },
    });
    expect(memory.read().options).toEqual({
      'codex-cli': { model: 'strong', effort: 'high' },
    });
  });
});

/**
 * `withRememberedOption` is the pure function
 * `App.svelte` hands its just-launched profile id, cwd and the model/effort
 * it just used to, rather than assembling the merged dictionary itself.
 */
describe('withRememberedOption', () => {
  it('copies profileId and cwd verbatim and keeps an existing option entry while placing the new one', () => {
    const entry: RememberedProfile = {
      profileId: 'previous-profile',
      cwd: 'C:\\work',
      options: { alpha: { model: 'a', effort: null } },
    };
    const result = withRememberedOption(entry, 'beta', { model: 'b', effort: 'high' });
    expect(result).toEqual({
      profileId: 'previous-profile',
      cwd: 'C:\\work',
      options: {
        alpha: { model: 'a', effort: null },
        beta: { model: 'b', effort: 'high' },
      },
    });
  });

  it('returns an options dictionary with a null prototype', () => {
    const entry: RememberedProfile = { profileId: null, cwd: null, options: {} };
    const result = withRememberedOption(entry, 'alpha', { model: null, effort: null });
    expect(Object.getPrototypeOf(result.options)).toBeNull();
  });

  it('does not mutate the entry it is given', () => {
    const options = { alpha: { model: 'a', effort: null } };
    const entry: RememberedProfile = { profileId: 'p', cwd: 'C:\\work', options };
    const snapshot = JSON.parse(JSON.stringify(entry)) as unknown;
    withRememberedOption(entry, 'beta', { model: 'b', effort: null });
    expect(entry).toEqual(snapshot);
    expect(entry.options).toBe(options);
  });

  it.each(['__proto__', 'constructor', 'prototype'] as const)(
    'does not place a record under a forbidden profile id (%s), while still copying entry.profileId verbatim',
    (forbidden) => {
      const entry: RememberedProfile = { profileId: 'safe-id', cwd: null, options: {} };
      const result = withRememberedOption(entry, forbidden, { model: 'x', effort: 'y' });
      expect((result.options as Record<string, unknown>)[forbidden]).toBeUndefined();
      expect(Object.keys(result.options)).toEqual([]);
      expect(result.profileId).toBe('safe-id');
    }
  );

  it('treats a missing options field as an empty dictionary, placing only the new record', () => {
    const entry: RememberedProfile = { profileId: null, cwd: null };
    const result = withRememberedOption(entry, 'alpha', { model: 'a', effort: null });
    expect(result.options).toEqual({ alpha: { model: 'a', effort: null } });
  });
});

describe('createBrowserProfileMemoryDeps', () => {
  it('reads through the injected localStorage.getItem, keyed under a yagura.profile- namespace', () => {
    const getItemMock = vi.fn<(key: string) => string | null>(() => null);
    const deps = createBrowserProfileMemoryDeps({
      localStorage: { getItem: getItemMock, setItem: vi.fn() },
    });

    deps.storageRead();

    expect(getItemMock).toHaveBeenCalledTimes(1);
    const [key] = getItemMock.mock.calls[0] as [string];
    expect(key.startsWith('yagura.profile-')).toBe(true);
  });

  it('writes through the injected localStorage.setItem under the same key storageRead used', () => {
    const getItemMock = vi.fn<(key: string) => string | null>(() => null);
    const setItemMock = vi.fn<(key: string, value: string) => void>();
    const deps = createBrowserProfileMemoryDeps({
      localStorage: { getItem: getItemMock, setItem: setItemMock },
    });

    deps.storageRead();
    deps.storageWrite('{"profileId":null,"cwd":null,"options":{}}');

    const [readKey] = getItemMock.mock.calls[0] as [string];
    const [writeKey, writeValue] = setItemMock.mock.calls[0] as [string, string];
    expect(writeKey).toBe(readKey);
    expect(writeValue).toBe('{"profileId":null,"cwd":null,"options":{}}');
  });

  it('degrades storageRead to null when localStorage.getItem throws (denied storage)', () => {
    const deps = createBrowserProfileMemoryDeps({
      localStorage: {
        getItem: () => {
          throw new Error('denied');
        },
        setItem: vi.fn(),
      },
    });
    expect(() => deps.storageRead()).not.toThrow();
    expect(deps.storageRead()).toBeNull();
  });

  it('degrades storageWrite to a no-op when localStorage.setItem throws (denied storage)', () => {
    const deps = createBrowserProfileMemoryDeps({
      localStorage: {
        getItem: () => null,
        setItem: () => {
          throw new Error('denied');
        },
      },
    });
    expect(() => deps.storageWrite('x')).not.toThrow();
  });

  it('degrades to a working no-op when localStorage itself is inaccessible (no override supplied, jsdom always has one, so this only proves the call does not throw)', () => {
    const deps = createBrowserProfileMemoryDeps();
    expect(() => deps.storageRead()).not.toThrow();
    expect(() => deps.storageWrite('{"profileId":null,"cwd":null}')).not.toThrow();
  });
});

describe('createProfileMemory - read: options of the wrong shape', () => {
  it('reads a list of option records as no options at all', () => {
    const { deps } = makeDeps(
      '{"profileId":"p","cwd":"/w","options":[{"model":"m","effort":"e"}]}'
    );

    expect(createProfileMemory(deps).read()).toEqual({ profileId: 'p', cwd: '/w', options: {} });
  });

  it('drops a per-profile record that is a list, an object-less value or null, keeping its siblings', () => {
    const { deps } = makeDeps(
      '{"profileId":null,"cwd":null,"options":{"a":[{"model":"m"}],"b":"x","c":null,"d":{"model":"m","effort":7}}}'
    );

    const { options } = createProfileMemory(deps).read();

    expect(Object.keys(options)).toEqual(['d']);
    expect(options['d']).toEqual({ model: 'm', effort: null });
  });

  it('reads a stored list as the empty entry', () => {
    const { deps } = makeDeps('[{"profileId":"p"}]');

    expect(createProfileMemory(deps).read()).toEqual(DEFAULT_ENTRY);
  });

  it('forgets only the directory when asked, keeping the profile and the options', () => {
    const { deps, storageWriteMock } = makeDeps(
      '{"profileId":"p","cwd":"/w","options":{"p":{"model":"m","effort":null}}}'
    );

    createProfileMemory(deps).dropCwd();

    expect(JSON.parse(storageWriteMock.mock.calls[0]?.[0] ?? 'null')).toEqual({
      profileId: 'p',
      cwd: null,
      options: { p: { model: 'm', effort: null } },
    });
  });
});

describe('withRememberedOption: names that reach the prototype chain', () => {
  const record = { model: 'm', effort: null };

  it.each(['__proto__', 'constructor', 'prototype'])('does not place a record under %s', (id) => {
    const placed = withRememberedOption({ profileId: 'p', cwd: null, options: {} }, id, record);

    expect(Object.keys(placed.options)).toEqual([]);
  });

  it('places a record under an ordinary id', () => {
    const placed = withRememberedOption({ profileId: 'p', cwd: null }, 'codex', record);

    expect(placed.options['codex']).toEqual(record);
  });
});
