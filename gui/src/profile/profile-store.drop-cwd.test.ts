/**
 * Contract for forgetting the remembered working directory alone. It is its
 * own operation rather than a second `write`, so a refused launch cannot
 * persist whatever else the caller happens to be holding, and the successful
 * path keeps updating the memory exactly as before.
 *
 * The fake storage below round-trips what it is given, so every assertion is
 * about what the memory reports afterwards rather than about the call shape.
 */
import { describe, it, expect, vi } from 'vitest';

import { createProfileMemory, type ProfileMemoryDeps } from './profile-store';

interface RoundTripStorage {
  deps: ProfileMemoryDeps;
  writeCount(): number;
}

function makeStorage(stored: string | null = null): RoundTripStorage {
  let value = stored;
  let writes = 0;
  return {
    deps: {
      storageRead: () => value,
      storageWrite: (next: string) => {
        value = next;
        writes++;
      },
    },
    writeCount: () => writes,
  };
}

const REMEMBERED = JSON.stringify({
  profileId: 'shell',
  cwd: 'C:\\work\\gone',
  options: { shell: { model: 'fast', effort: 'low' } },
});

describe('ProfileMemory.dropCwd', () => {
  it('forgets the working directory', () => {
    const memory = createProfileMemory(makeStorage(REMEMBERED).deps);

    memory.dropCwd();

    expect(memory.read().cwd).toBeNull();
  });

  it('keeps the remembered profile id and launch options', () => {
    const memory = createProfileMemory(makeStorage(REMEMBERED).deps);

    memory.dropCwd();

    expect(memory.read()).toEqual({
      profileId: 'shell',
      cwd: null,
      options: { shell: { model: 'fast', effort: 'low' } },
    });
  });

  it('leaves an empty memory empty instead of throwing', () => {
    const memory = createProfileMemory(makeStorage(null).deps);

    expect(() => memory.dropCwd()).not.toThrow();
    expect(memory.read()).toEqual({ profileId: null, cwd: null, options: {} });
  });

  it('is idempotent', () => {
    const memory = createProfileMemory(makeStorage(REMEMBERED).deps);

    memory.dropCwd();
    memory.dropCwd();

    expect(memory.read()).toEqual({
      profileId: 'shell',
      cwd: null,
      options: { shell: { model: 'fast', effort: 'low' } },
    });
  });

  it('degrades to a no-op when storage refuses the write', () => {
    const deps: ProfileMemoryDeps = {
      storageRead: () => REMEMBERED,
      storageWrite: vi.fn(() => {
        throw new Error('storage is unavailable');
      }),
    };
    const memory = createProfileMemory(deps);

    expect(() => memory.dropCwd()).not.toThrow();
  });

  it('degrades to a no-op when storage refuses the read', () => {
    const deps: ProfileMemoryDeps = {
      storageRead: () => {
        throw new Error('storage is unavailable');
      },
      storageWrite: vi.fn(),
    };
    const memory = createProfileMemory(deps);

    expect(() => memory.dropCwd()).not.toThrow();
  });
});

describe('ProfileMemory.write still records a successful launch whole', () => {
  it('persists the profile id, working directory and options it is given', () => {
    const storage = makeStorage(null);
    const memory = createProfileMemory(storage.deps);

    memory.write({
      profileId: 'shell',
      cwd: 'C:\\work',
      options: { shell: { model: 'fast', effort: 'low' } },
    });

    expect(memory.read()).toEqual({
      profileId: 'shell',
      cwd: 'C:\\work',
      options: { shell: { model: 'fast', effort: 'low' } },
    });
    expect(storage.writeCount()).toBe(1);
  });

  it('records a directory again after one was forgotten', () => {
    const memory = createProfileMemory(makeStorage(REMEMBERED).deps);

    memory.dropCwd();
    memory.write({ profileId: 'shell', cwd: 'C:\\work\\present', options: {} });

    expect(memory.read().cwd).toBe('C:\\work\\present');
  });
});
