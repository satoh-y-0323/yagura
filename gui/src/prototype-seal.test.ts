/**
 * The seal the app installs at startup, measured by what it does rather than
 * by how it is spelled: the entry point is loaded for real, with `mount`
 * standing in so the load stops at the point the app would be handed to
 * Svelte, and the prototype is then exercised.
 *
 * Sealing is process-wide and cannot be undone, so this file has to stay the
 * only one that reaches it -- the pollution fixtures elsewhere write to
 * `Object.prototype` and would fail under a seal. Vitest gives each test file
 * its own isolated process, and the first assertion below fails loudly rather
 * than passing vacuously if that ever stops being true.
 */
import { describe, it, expect, vi, beforeAll } from 'vitest';

import { sealObjectPrototype } from './prototype-seal';

const { mountMock, sealedWhenMounted } = vi.hoisted(() => {
  const sealedWhenMounted: boolean[] = [];
  return {
    sealedWhenMounted,
    mountMock: vi.fn(() => {
      sealedWhenMounted.push(Object.isSealed(Object.prototype));
      return {};
    }),
  };
});

vi.mock('svelte', async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  return { ...actual, mount: mountMock };
});

// Only the order of sealing and mounting is measured; loading the real App would
// tie this test's run time to the size of the whole app.
vi.mock('./App.svelte', () => ({ default: {} }));

/** A namespace object as TypeScript compiles one: a bare object whose members are assigned onto it. */
function namespaceWithOwnToString(): { toString: () => string } {
  const namespace: Record<string, unknown> = {};
  namespace['toString'] = () => 'a namespace member';
  return namespace;
}

describe('the entry point seals Object.prototype before it mounts the app', () => {
  it('hands mount a prototype that is already sealed', async () => {
    expect(Object.isExtensible(Object.prototype)).toBe(true);

    vi.stubGlobal('document', { getElementById: () => ({}), addEventListener: () => undefined });
    await import('./main');

    expect(mountMock).toHaveBeenCalledTimes(1);
    expect(sealedWhenMounted).toEqual([true]);
  });
});

describe('what the seal forbids', () => {
  beforeAll(sealObjectPrototype);

  it('refuses a name assigned onto Object.prototype', () => {
    expect(() => {
      (Object.prototype as unknown as Record<string, unknown>)['rejected_argument'] = 'cwd';
    }).toThrow(TypeError);
    expect(Object.prototype).not.toHaveProperty('rejected_argument');
  });

  it('refuses a name defined onto Object.prototype', () => {
    expect(() => {
      Object.defineProperty(Object.prototype, 'kind', { value: 'rejected', configurable: true });
    }).toThrow(TypeError);
    expect(Object.prototype).not.toHaveProperty('kind');
  });

  it('leaves the members Object.prototype already has writable, which reading by ownership covers', () => {
    expect(Object.getOwnPropertyDescriptor(Object.prototype, 'toString')?.writable).toBe(true);
    expect(Object.isFrozen(Object.prototype)).toBe(false);
  });
});

describe('what the seal leaves working', () => {
  beforeAll(sealObjectPrototype);

  it('lets an object shadow an inherited member with one of its own', () => {
    expect(String(namespaceWithOwnToString())).toBe('a namespace member');
  });

  it('lets an ordinary object take a new key', () => {
    const record: Record<string, unknown> = {};
    record['kind'] = 'local';

    expect(record['kind']).toBe('local');
  });
});
