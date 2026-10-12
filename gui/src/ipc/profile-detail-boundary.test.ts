/**
 * Contract of the profile-detail boundary: a reply from the daemon is trusted
 * only after it matches the wire shape of `ProfileDetail` / `ProfileCommandSet`
 * in `yagura-protocol`, and what is accepted keeps every layer it carries.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowProfileCommandSet,
  narrowProfileDetail,
} from './profile-detail-boundary';

function commandWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { argv: ['claude', '--x'], shipped: ['claude'], changed: true, ...overrides };
}

function entryWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    path: ['model', 'aliases', 'fast'],
    value: 'haiku',
    shipped: 'sonnet',
    changed: true,
    ...overrides,
  };
}

function detailWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'claude-code',
    shipped_base: true,
    command: commandWire(),
    entries: [entryWire()],
    ...overrides,
  };
}

describe('narrowProfileDetail', () => {
  it('keeps every field of a well-formed reply', () => {
    expect(narrowProfileDetail(detailWire())).toEqual({
      profile: 'claude-code',
      shippedBase: true,
      command: { argv: ['claude', '--x'], shipped: ['claude'], changed: true },
      entries: [
        { path: ['model', 'aliases', 'fast'], value: 'haiku', shipped: 'sonnet', changed: true },
      ],
    });
  });

  it('keeps a non-string value as the JSON it is, and a missing shipped value as undefined', () => {
    const detail = narrowProfileDetail(
      detailWire({
        entries: [{ path: ['args'], value: ['a', 1, { b: null }], changed: false }],
      })
    );
    expect(detail.entries[0]?.value).toEqual(['a', 1, { b: null }]);
    expect(detail.entries[0]?.shipped).toBeUndefined();
    expect(detail.entries[0]?.changed).toBe(false);
  });

  it('reads an absent or null shipped argv as null, and an empty one as empty', () => {
    const absent = narrowProfileDetail(detailWire({ command: { argv: ['a'], changed: false } }));
    expect(absent.command.shipped).toBeNull();
    const nulled = narrowProfileDetail(detailWire({ command: commandWire({ shipped: null }) }));
    expect(nulled.command.shipped).toBeNull();
    const empty = narrowProfileDetail(detailWire({ command: commandWire({ shipped: [] }) }));
    expect(empty.command.shipped).toEqual([]);
  });

  it('accepts a profile with no entries and a false shipped_base', () => {
    const detail = narrowProfileDetail(detailWire({ entries: [], shipped_base: false }));
    expect(detail.entries).toEqual([]);
    expect(detail.shippedBase).toBe(false);
  });

  it.each([
    ['null', null],
    ['an array', []],
    ['a string', 'x'],
  ])('rejects a reply that is %s', (_name, value) => {
    expect(() => narrowProfileDetail(value)).toThrow(BoundaryError);
  });

  it.each([
    ['profile missing', { profile: undefined }],
    ['profile not a string', { profile: 3 }],
    ['shipped_base missing', { shipped_base: undefined }],
    ['shipped_base not a boolean', { shipped_base: 'yes' }],
    ['command missing', { command: undefined }],
    ['command not an object', { command: 'claude' }],
    ['entries missing', { entries: undefined }],
    ['entries not an array', { entries: {} }],
    ['command argv missing', { command: { changed: true } }],
    ['command argv holds a non-string', { command: commandWire({ argv: ['a', 1] }) }],
    ['command shipped holds a non-string', { command: commandWire({ shipped: [1] }) }],
    ['command shipped is a string', { command: commandWire({ shipped: 'claude' }) }],
    ['command changed missing', { command: { argv: [] } }],
    ['command changed not a boolean', { command: commandWire({ changed: 1 }) }],
    ['an entry is not an object', { entries: ['x'] }],
    ['an entry path missing', { entries: [{ value: 1, changed: true }] }],
    ['an entry path holds a non-string', { entries: [entryWire({ path: ['a', 2] })] }],
    ['an entry changed missing', { entries: [{ path: ['a'], value: 1 }] }],
    ['an entry changed not a boolean', { entries: [entryWire({ changed: 'no' })] }],
  ])('rejects a reply with %s', (_name, overrides) => {
    expect(() => narrowProfileDetail(detailWire(overrides))).toThrow(BoundaryError);
  });

  it('names the command and the part of a get_profile_detail reply that is wrong', () => {
    expect(() => narrowProfileDetail('x')).toThrow(/^get_profile_detail: expected an object/);
    expect(() => narrowProfileDetail(detailWire({ profile: 1 }))).toThrow(
      /^get_profile_detail\.profile: expected a string/
    );
    expect(() => narrowProfileDetail(detailWire({ command: 'x' }))).toThrow(
      /^get_profile_detail\.command: expected an object/
    );
    expect(() => narrowProfileDetail(detailWire({ entries: {} }))).toThrow(
      /^get_profile_detail\.entries: expected an array/
    );
  });

  it('names the failing entry by its index', () => {
    expect(() =>
      narrowProfileDetail(detailWire({ entries: [entryWire(), entryWire({ changed: 0 })] }))
    ).toThrow(/entries\[1\]/);
  });

  it('does not read a field the reply only inherits', () => {
    const inherited = Object.create({ profile: 'claude-code' }) as Record<string, unknown>;
    Object.assign(inherited, detailWire({ profile: undefined }));
    delete inherited.profile;
    expect(() => narrowProfileDetail(inherited)).toThrow(BoundaryError);
  });
});

describe('narrowProfileCommandSet', () => {
  it('names the command and the part of a set_profile_command reply that is wrong', () => {
    expect(() => narrowProfileCommandSet('x')).toThrow(/^set_profile_command: expected an object/);
    expect(() => narrowProfileCommandSet({ profile: 'p', command: 'x' })).toThrow(
      /^set_profile_command\.command: expected an object/
    );
  });

  it('keeps the profile and the argv in use', () => {
    expect(narrowProfileCommandSet({ profile: 'p', command: commandWire() })).toEqual({
      profile: 'p',
      command: { argv: ['claude', '--x'], shipped: ['claude'], changed: true },
    });
  });

  it('reads an absent shipped argv as null', () => {
    const set = narrowProfileCommandSet({
      profile: 'p',
      command: { argv: ['a'], changed: false },
    });
    expect(set.command.shipped).toBeNull();
  });

  it.each([
    ['not an object', 'x'],
    ['profile missing', { command: commandWire() }],
    ['profile not a string', { profile: 1, command: commandWire() }],
    ['command missing', { profile: 'p' }],
    ['command argv invalid', { profile: 'p', command: commandWire({ argv: 'a' }) }],
  ])('rejects a reply with %s', (_name, value) => {
    expect(() => narrowProfileCommandSet(value)).toThrow(BoundaryError);
  });
});
