/**
 * The launch-profile memory: the profile and working directory last used at
 * this terminal, which is what the add-window dialog opens on. It also
 * remembers the last `model` / `effort` used per profile id, so reopening a
 * profile that was launched with a non-default option offers it again.
 *
 * It degrades the way `theme-store.ts` does (both delegate to
 * `guarded-storage.ts`): a WebView that denies storage reads as "nothing
 * remembered" and writes as a no-op, so the dialog still opens and still
 * launches windows.
 */

import { createGuardedLocalStorage } from '../storage/guarded-storage';

/** Key the remembered profile selection is persisted under. */
const STORAGE_KEY = 'yagura.profile-last-used';

/** The last `model` / `effort` remembered for one profile id. */
export interface RememberedLaunchOptions {
  /** The `model` value last used, `null` when none was. */
  model: string | null;
  /** The `effort` value last used, `null` when none was. */
  effort: string | null;
}

/** What was last launched from the add-window dialog. */
export interface RememberedProfile {
  /** The profile id, `null` when nothing usable is remembered. */
  profileId: string | null;
  /** The working directory, `null` when nothing usable is remembered. */
  cwd: string | null;
  /**
   * Last `model` / `effort` used per profile id. `read()` always returns
   * this filled in (never omitted); optional here only so a caller writing
   * an entry can leave it out and have an empty dictionary persisted.
   */
  options?: Record<string, RememberedLaunchOptions>;
}

/** The storage the memory reads the entry from and writes it back to. */
export interface ProfileMemoryDeps {
  /** The persisted entry, `null` when nothing is stored. */
  storageRead(): string | null;
  /** Persists an entry. */
  storageWrite(value: string): void;
}

/** Reader/writer pair over one {@link RememberedProfile}. */
export interface ProfileMemory {
  /** The remembered entry; the empty entry when there is none. */
  read(): Required<RememberedProfile>;
  /** Persists `entry`, best effort. */
  write(entry: RememberedProfile): void;
  /**
   * Forgets the remembered working directory alone, best effort. Its own
   * operation rather than a second `write`: a refused launch must not be
   * able to persist anything else the caller happens to be holding.
   */
  dropCwd(): void;
}

/** The field's value when it is a string, `null` for anything else. */
function readStringField(record: Record<string, unknown>, key: string): string | null {
  const value = record[key];
  return typeof value === 'string' ? value : null;
}

/**
 * The three property names that would otherwise reach the prototype chain
 * through a plain `{}`-typed access, rather than a genuinely stored key.
 */
const FORBIDDEN_OPTION_KEYS = new Set(['__proto__', 'constructor', 'prototype']);

/**
 * Narrows the per-profile-id `options` dictionary. A non-object value (or
 * one that is missing) narrows to an empty dictionary; a per-profile record
 * that is not itself an object is dropped rather than adopted under a
 * `null` / garbage value. Built with a null prototype, and freshly on every
 * call, so a profile id of `__proto__` cannot reach it and two calls never
 * share the same dictionary instance.
 */
function parseOptions(value: unknown): Record<string, RememberedLaunchOptions> {
  const options: Record<string, RememberedLaunchOptions> = Object.create(null) as Record<
    string,
    RememberedLaunchOptions
  >;
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return options;
  }
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (FORBIDDEN_OPTION_KEYS.has(key)) {
      continue;
    }
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
      continue;
    }
    const record = entry as Record<string, unknown>;
    options[key] = {
      model: readStringField(record, 'model'),
      effort: readStringField(record, 'effort'),
    };
  }
  return options;
}

/** The entry `read()` reports when nothing usable is stored. */
function emptyEntry(): Required<RememberedProfile> {
  return { profileId: null, cwd: null, options: parseOptions(undefined) };
}

/**
 * Parses a stored entry. Anything that is not an object of the expected shape
 * narrows field by field to `null`: a stored value is the previous run's
 * output, but also whatever else shares the origin's storage.
 */
function parseEntry(stored: string | null): Required<RememberedProfile> {
  if (stored === null) {
    return emptyEntry();
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(stored);
  } catch {
    // Malformed storage is indistinguishable from empty storage here, and
    // the empty entry covers both.
    return emptyEntry();
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return emptyEntry();
  }
  const record = parsed as Record<string, unknown>;
  return {
    profileId: readStringField(record, 'profileId'),
    cwd: readStringField(record, 'cwd'),
    options: parseOptions(record['options']),
  };
}

/**
 * Builds the `options` dictionary `App.svelte` writes back after a launch:
 * `entry`'s `profileId` and `cwd` pass through untouched, and its `options`
 * are copied into a fresh null-prototype dictionary before `record` is
 * placed under `profileId` -- so the caller never assembles or mutates the
 * dictionary itself. A `profileId` in {@link FORBIDDEN_OPTION_KEYS} is
 * dropped rather than placed, matching the guard `parseOptions` applies on
 * the read side.
 */
export function withRememberedOption(
  entry: RememberedProfile,
  profileId: string,
  record: RememberedLaunchOptions
): Required<RememberedProfile> {
  const options: Record<string, RememberedLaunchOptions> = Object.create(null) as Record<
    string,
    RememberedLaunchOptions
  >;
  for (const [key, value] of Object.entries(entry.options ?? {})) {
    options[key] = value;
  }
  if (!FORBIDDEN_OPTION_KEYS.has(profileId)) {
    options[profileId] = record;
  }
  return { profileId: entry.profileId, cwd: entry.cwd, options };
}

/** Creates the launch-profile memory over `deps`. */
export function createProfileMemory(deps: ProfileMemoryDeps): ProfileMemory {
  const read = (): Required<RememberedProfile> => {
    try {
      return parseEntry(deps.storageRead());
    } catch {
      // Storage access can fail outright, not just return nothing.
      return emptyEntry();
    }
  };
  const write = (entry: RememberedProfile): void => {
    try {
      // `options` is always written, even empty, so the stored shape has
      // a fixed three keys regardless of what the caller supplied.
      deps.storageWrite(
        JSON.stringify({
          profileId: entry.profileId,
          cwd: entry.cwd,
          options: entry.options ?? {},
        })
      );
    } catch {
      // Best effort: a blocked storage must not keep the window that was
      // just launched from opening.
    }
  };

  return {
    read,
    write,
    dropCwd: () => {
      // Written from what is stored, not from what a caller is holding, so a
      // refused launch cannot persist anything besides the directory it drops.
      const remembered = read();
      write({ profileId: remembered.profileId, cwd: null, options: remembered.options });
    },
  };
}

/**
 * Builds {@link ProfileMemoryDeps} from the browser globals, guarded the same
 * way `createBrowserThemeDeps` guards its own.
 *
 * @param g overrides for the globals, for tests and embedders.
 */
export function createBrowserProfileMemoryDeps(g?: {
  localStorage?: Pick<Storage, 'getItem' | 'setItem'>;
}): ProfileMemoryDeps {
  const storage = createGuardedLocalStorage(STORAGE_KEY, g?.localStorage);

  return {
    storageRead: () => storage.read(),
    storageWrite: (value) => storage.write(value),
  };
}
