/** A `localStorage`-backed value that degrades to `null` / no-op instead of throwing. */
export interface GuardedStorage {
  /** The persisted value, or `null` when absent, unreadable, or storage itself is denied. */
  read(): string | null;
  /** Persists a value. Best effort: a blocked or denied storage silently drops the write. */
  write(value: string): void;
}

/**
 * Builds a {@link GuardedStorage} over `key`, guarding both a per-operation
 * exception (`getItem` / `setItem` throwing) and storage being denied
 * outright (accessing `localStorage` itself throwing) -- both failure modes
 * need the same fallback, so `theme-store.ts` and `profile-store.ts` share
 * this one implementation instead of duplicating the guard.
 *
 * @param override storage to use instead of `globalThis.localStorage`, for tests and embedders.
 */
export function createGuardedLocalStorage(
  key: string,
  override?: Pick<Storage, 'getItem' | 'setItem'>
): GuardedStorage {
  // Reaching `localStorage` can throw as well as using it, so every access sits inside a `try`.
  const open = (): Pick<Storage, 'getItem' | 'setItem'> => override ?? globalThis.localStorage;

  return {
    read: () => {
      try {
        return open().getItem(key);
      } catch {
        return null;
      }
    },
    write: (value) => {
      try {
        open().setItem(key, value);
      } catch {
        // Best effort: a blocked storage must not break the caller.
      }
    },
  };
}
