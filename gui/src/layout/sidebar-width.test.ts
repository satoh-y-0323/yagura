import { describe, it, expect } from 'vitest';

import {
  SIDEBAR_WIDTH_MIN,
  SIDEBAR_WIDTH_MAX,
  SIDEBAR_WIDTH_DEFAULT,
  clampSidebarWidth,
  readStoredSidebarWidth,
  createSidebarWidthStorage,
} from './sidebar-width';

function makeFakeStorage(): {
  storage: Pick<Storage, 'getItem' | 'setItem'>;
  store: Map<string, string>;
} {
  const store = new Map<string, string>();
  return {
    storage: {
      getItem: (key) => store.get(key) ?? null,
      setItem: (key, value) => {
        store.set(key, value);
      },
    },
    store,
  };
}

describe('clampSidebarWidth - clamps to the declared range, boundaries included', () => {
  it.each([
    [SIDEBAR_WIDTH_MIN - 1, SIDEBAR_WIDTH_MIN],
    [0, SIDEBAR_WIDTH_MIN],
    [-1000, SIDEBAR_WIDTH_MIN],
    [SIDEBAR_WIDTH_MAX + 1, SIDEBAR_WIDTH_MAX],
    [10000, SIDEBAR_WIDTH_MAX],
  ])('rounds %j to %j', (input, expected) => {
    expect(clampSidebarWidth(input)).toBe(expected);
  });

  it.each([[SIDEBAR_WIDTH_MIN], [SIDEBAR_WIDTH_MAX], [SIDEBAR_WIDTH_DEFAULT], [350]])(
    'returns %j unchanged when it is already within [MIN, MAX]',
    (input) => {
      expect(clampSidebarWidth(input)).toBe(input);
    }
  );
});

describe('clampSidebarWidth - non-finite values fall to SIDEBAR_WIDTH_DEFAULT', () => {
  it.each([[NaN], [Infinity], [-Infinity]])('falls back to the default for %j', (input) => {
    expect(clampSidebarWidth(input)).toBe(SIDEBAR_WIDTH_DEFAULT);
  });
});

describe('readStoredSidebarWidth - never throws, always returns a value within [MIN, MAX]', () => {
  it.each([
    [null],
    [''],
    ['not-a-number'],
    [String(SIDEBAR_WIDTH_MIN - 100)],
    [String(SIDEBAR_WIDTH_MAX + 100)],
    ['288.5'],
    ['999.9'],
  ])('does not throw and returns an in-range value for %j', (raw) => {
    let result: number | undefined;

    expect(() => {
      result = readStoredSidebarWidth(raw);
    }).not.toThrow();

    expect(result).toBeGreaterThanOrEqual(SIDEBAR_WIDTH_MIN);
    expect(result).toBeLessThanOrEqual(SIDEBAR_WIDTH_MAX);
  });
});

describe('createSidebarWidthStorage - normal round trip', () => {
  it('save() then load() returns the value that was saved, when it is already in range', () => {
    const { storage } = makeFakeStorage();
    const sidebarStorage = createSidebarWidthStorage(storage);

    sidebarStorage.save(400);

    expect(sidebarStorage.load()).toBe(400);
  });

  it('save() then load() returns the clamped value when the saved value is out of range', () => {
    const { storage } = makeFakeStorage();
    const sidebarStorage = createSidebarWidthStorage(storage);

    sidebarStorage.save(SIDEBAR_WIDTH_MAX + 500);

    expect(sidebarStorage.load()).toBe(SIDEBAR_WIDTH_MAX);
  });

  it('save() then load() returns the clamped value when the saved value is below the minimum', () => {
    const { storage } = makeFakeStorage();
    const sidebarStorage = createSidebarWidthStorage(storage);

    sidebarStorage.save(SIDEBAR_WIDTH_MIN - 500);

    expect(sidebarStorage.load()).toBe(SIDEBAR_WIDTH_MIN);
  });

  it('load() returns an in-range value when nothing has been stored yet', () => {
    const { storage } = makeFakeStorage();
    const sidebarStorage = createSidebarWidthStorage(storage);

    const loaded = sidebarStorage.load();

    expect(loaded).toBeGreaterThanOrEqual(SIDEBAR_WIDTH_MIN);
    expect(loaded).toBeLessThanOrEqual(SIDEBAR_WIDTH_MAX);
  });
});

describe('createSidebarWidthStorage - degrades instead of throwing when storage is broken', () => {
  it('load() does not throw and returns an in-range value when getItem throws', () => {
    const throwingStorage: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => {
        throw new Error('boom');
      },
      setItem: () => {},
    };
    const sidebarStorage = createSidebarWidthStorage(throwingStorage);

    let loaded: number | undefined;
    expect(() => {
      loaded = sidebarStorage.load();
    }).not.toThrow();

    expect(loaded).toBeGreaterThanOrEqual(SIDEBAR_WIDTH_MIN);
    expect(loaded).toBeLessThanOrEqual(SIDEBAR_WIDTH_MAX);
  });

  it('save() does not throw when setItem throws', () => {
    const throwingStorage: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => null,
      setItem: () => {
        throw new Error('boom');
      },
    };
    const sidebarStorage = createSidebarWidthStorage(throwingStorage);

    expect(() => sidebarStorage.save(300)).not.toThrow();
  });

  it('load() does not throw and returns an in-range value when the stored value is corrupted (non-numeric)', () => {
    const corruptedStorage: Pick<Storage, 'getItem' | 'setItem'> = {
      getItem: () => 'not-a-number',
      setItem: () => {},
    };
    const sidebarStorage = createSidebarWidthStorage(corruptedStorage);

    let loaded: number | undefined;
    expect(() => {
      loaded = sidebarStorage.load();
    }).not.toThrow();

    expect(loaded).toBeGreaterThanOrEqual(SIDEBAR_WIDTH_MIN);
    expect(loaded).toBeLessThanOrEqual(SIDEBAR_WIDTH_MAX);
  });
});

describe('sidebar width persistence details', () => {
  it('keeps the width under the yagura.sidebar-width key', () => {
    const { storage, store } = makeFakeStorage();
    createSidebarWidthStorage(storage).save(300);
    expect([...store.entries()]).toEqual([['yagura.sidebar-width', '300']]);
  });

  it('reads the width back from the yagura.sidebar-width key', () => {
    const { storage, store } = makeFakeStorage();
    store.set('yagura.sidebar-width', '350');
    expect(createSidebarWidthStorage(storage).load()).toBe(350);
  });

  it.each(['', '   ', '\t\n'])('treats the blank value %j as absent', (raw) => {
    expect(readStoredSidebarWidth(raw)).toBe(SIDEBAR_WIDTH_DEFAULT);
  });

  it('reads a number that has surrounding whitespace', () => {
    expect(readStoredSidebarWidth(' 300 ')).toBe(300);
  });
});
