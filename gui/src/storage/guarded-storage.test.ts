import { afterEach, describe, expect, it } from 'vitest';

import { createGuardedLocalStorage } from './guarded-storage';

function backing(): { storage: Pick<Storage, 'getItem' | 'setItem'>; items: Map<string, string> } {
  const items = new Map<string, string>();
  return {
    items,
    storage: {
      getItem: (key) => items.get(key) ?? null,
      setItem: (key, value) => {
        items.set(key, value);
      },
    },
  };
}

describe('createGuardedLocalStorage', () => {
  it('reads and writes the value under its own key', () => {
    const { storage, items } = backing();
    const guarded = createGuardedLocalStorage('k', storage);

    expect(guarded.read()).toBeNull();
    guarded.write('v');

    expect(items.get('k')).toBe('v');
    expect(guarded.read()).toBe('v');
  });

  it('reads null and drops the write when an operation throws', () => {
    const guarded = createGuardedLocalStorage('k', {
      getItem: () => {
        throw new Error('denied');
      },
      setItem: () => {
        throw new Error('denied');
      },
    });

    expect(guarded.read()).toBeNull();
    expect(() => guarded.write('v')).not.toThrow();
  });

  describe('without an override', () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');

    afterEach(() => {
      if (original === undefined) {
        Reflect.deleteProperty(globalThis, 'localStorage');
      } else {
        Object.defineProperty(globalThis, 'localStorage', original);
      }
    });

    it('reads null and drops the write when there is no localStorage', () => {
      Object.defineProperty(globalThis, 'localStorage', { value: undefined, configurable: true });
      const guarded = createGuardedLocalStorage('k');

      expect(guarded.read()).toBeNull();
      expect(() => guarded.write('v')).not.toThrow();
    });

    it('reads null and drops the write when merely reaching localStorage throws', () => {
      Object.defineProperty(globalThis, 'localStorage', {
        get: () => {
          throw new Error('SecurityError');
        },
        configurable: true,
      });
      const guarded = createGuardedLocalStorage('k');

      expect(guarded.read()).toBeNull();
      expect(() => guarded.write('v')).not.toThrow();
    });

    it('uses the global localStorage', () => {
      const { storage, items } = backing();
      Object.defineProperty(globalThis, 'localStorage', { value: storage, configurable: true });
      const guarded = createGuardedLocalStorage('k');

      guarded.write('v');

      expect(items.get('k')).toBe('v');
      expect(guarded.read()).toBe('v');
    });
  });
});
