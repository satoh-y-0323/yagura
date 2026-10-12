/**
 * Contract for the value an `open_window` rejection carries across the Tauri
 * edge. Four kinds of value arrive: the two tagged shapes the core
 * serializes, a bare string when Tauri refused the call before the command
 * ran, and an `Error` when the invoke bridge itself threw. Only a value that
 * claims the tag is held to the contract, so wording that used to reach the
 * user is not turned into a boundary error.
 *
 * `@tauri-apps/api/core` is mocked for the round-trip half, which measures
 * what `invokeOpenWindow` hands its caller for each of the four.
 *
 * A third half measures ownership: a value claims a tag, and carries a field,
 * only by owning it, so the checks below run against a polluted
 * `Object.prototype`. `shouldDropRememberedCwd` is read from the failure
 * model for those, since what the pollution may not reach is the decision to
 * discard a remembered working directory.
 */
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { BoundaryError, WindowOpenFailureError, narrowWindowOpenFailure } from './session-boundary';
import { invokeOpenWindow } from './session-client';
import { shouldDropRememberedCwd } from '../window-open-failure-model';

const COMMAND = ['claude'];

beforeEach(() => {
  invokeMock.mockReset();
});

describe('narrowWindowOpenFailure: the four kinds of value it accepts', () => {
  it('reads a bare string as text alone', () => {
    expect(narrowWindowOpenFailure('invalid args for command open_window')).toEqual({
      kind: 'bare',
      message: 'invalid args for command open_window',
    });
  });

  it('reads an Error instance as its message alone', () => {
    expect(narrowWindowOpenFailure(new Error('the invoke bridge failed'))).toEqual({
      kind: 'bare',
      message: 'the invoke bridge failed',
    });
  });

  it('reads a subclass of Error as its message alone', () => {
    class TransportError extends Error {}

    expect(narrowWindowOpenFailure(new TransportError('connection reset'))).toEqual({
      kind: 'bare',
      message: 'connection reset',
    });
  });

  it('accepts a daemon rejection with every field present', () => {
    expect(
      narrowWindowOpenFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'the working directory does not exist',
        rejected_argument: 'cwd',
      })
    ).toEqual({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'the working directory does not exist',
      rejectedArgument: 'cwd',
    });
  });

  it('accepts a refusal the core raised on its own', () => {
    expect(
      narrowWindowOpenFailure({ kind: 'local', message: 'that window name is already taken' })
    ).toEqual({ kind: 'local', message: 'that window name is already taken' });
  });
});

describe('narrowWindowOpenFailure: a rejection that names no argument', () => {
  it('reports no argument when the key is omitted', () => {
    const failure = narrowWindowOpenFailure({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
    });

    expect(failure).toEqual({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
      rejectedArgument: null,
    });
  });

  it('reports no argument when the key carries JSON null', () => {
    const failure = narrowWindowOpenFailure({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
      rejected_argument: null,
    });

    expect(failure).toEqual({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
      rejectedArgument: null,
    });
  });
});

describe('narrowWindowOpenFailure: a value that claims the tag is held to it', () => {
  it.each([
    ['code is missing', { kind: 'rejected', message: 'refused' }],
    ['message is missing', { kind: 'rejected', code: 'invalid_argument' }],
    ['code is a number', { kind: 'rejected', code: 7, message: 'refused' }],
    ['message is a number', { kind: 'rejected', code: 'invalid_argument', message: 7 }],
    [
      'rejected_argument is a number',
      { kind: 'rejected', code: 'invalid_argument', message: 'refused', rejected_argument: 7 },
    ],
    ['a local refusal has no message', { kind: 'local' }],
    ['a local refusal message is an object', { kind: 'local', message: { text: 'refused' } }],
    ['the tag names a kind this side does not know', { kind: 'exploded', message: 'refused' }],
    ['the tag is not a string', { kind: 7, message: 'refused' }],
    ['the tag is null', { kind: null, message: 'refused' }],
  ])('refuses a value whose %s', (_label, value) => {
    expect(() => narrowWindowOpenFailure(value)).toThrow(BoundaryError);
  });
});

describe('narrowWindowOpenFailure: a value that claims no tag is read for its text', () => {
  it.each([
    ['an object with no tag', { reason: 'refused' }, '{"reason":"refused"}'],
    ['an array', ['refused'], '["refused"]'],
    ['a number', 7, '7'],
    ['null', null, 'null'],
    ['undefined', undefined, 'undefined'],
  ])('reads %s as text alone', (_label, value, expected) => {
    expect(narrowWindowOpenFailure(value)).toEqual({ kind: 'bare', message: expected });
  });

  it('does not throw for an object whose own reference cycle cannot be encoded', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic['self'] = cyclic;

    expect(narrowWindowOpenFailure(cyclic)).toEqual({ kind: 'bare', message: '' });
  });
});

/**
 * A rejection every object inherits once `Object.prototype` carries these
 * keys. Nothing here is an own key of the values narrowed below.
 */
const INHERITED_REJECTION: ReadonlyArray<readonly [string, string]> = [
  ['kind', 'rejected'],
  ['code', 'invalid_argument'],
  ['message', 'the working directory does not exist'],
  ['rejected_argument', 'cwd'],
];

function objectPrototypeRecord(): Record<string, unknown> {
  return Object.prototype as unknown as Record<string, unknown>;
}

function clearPollution(): void {
  const proto = objectPrototypeRecord();
  for (const [key] of INHERITED_REJECTION) {
    delete proto[key];
  }
}

/**
 * Runs `read` with the rejection above on `Object.prototype`, restoring it
 * before returning so the assertion that follows is not itself read through
 * the pollution. The pollution is written and cleared here alone: no test
 * body reaches `Object.prototype` to set it up or take it down.
 *
 * The restore only bounds the pollution while `read` finishes its work before
 * it returns, so a thenable result is refused rather than left to widen the
 * window across an await. Refusing it after the restore keeps the failure
 * itself out of the polluted window.
 */
function underPollutedPrototype<T>(read: () => T): T {
  const proto = objectPrototypeRecord();
  let result: T;
  try {
    for (const [key, value] of INHERITED_REJECTION) {
      proto[key] = value;
    }
    result = read();
  } finally {
    clearPollution();
  }
  if (typeof (result as { then?: unknown })?.then === 'function') {
    throw new Error('the polluted read must finish before it returns, not hand back a thenable');
  }
  return result;
}

afterEach(clearPollution);

describe('narrowWindowOpenFailure: a value claims a tag only by owning one', () => {
  it.each([
    ['an object with no key of its own', {}, '{}'],
    ['an object whose own keys name no tag', { reason: 'refused' }, '{"reason":"refused"}'],
  ])('reads %s as text alone, whatever its prototype carries', (_label, value, expected) => {
    const narrowed = underPollutedPrototype(() => narrowWindowOpenFailure(value));

    expect(narrowed).toEqual({ kind: 'bare', message: expected });
  });

  it('leaves the remembered working directory alone for such a value', () => {
    const narrowed = underPollutedPrototype(() => narrowWindowOpenFailure({}));

    expect(shouldDropRememberedCwd(narrowed)).toBe(false);
  });

  it('still reads a value that owns its tag, under the same pollution', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowWindowOpenFailure({ kind: 'local', message: 'the command was empty' })
    );

    expect(narrowed).toEqual({ kind: 'local', message: 'the command was empty' });
  });
});

describe('narrowWindowOpenFailure: a field is read only when the value owns it', () => {
  it.each([
    ['the reason text', { kind: 'rejected', code: 'invalid_argument' }],
    ['the error code', { kind: 'rejected', message: 'refused' }],
    ['both of them', { kind: 'rejected' }],
  ])('refuses a rejection that owns its tag but inherits %s', (_label, value) => {
    expect(() => underPollutedPrototype(() => narrowWindowOpenFailure(value))).toThrow(
      BoundaryError
    );
  });

  it('reports no argument when only the prototype names one', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowWindowOpenFailure({ kind: 'rejected', code: 'invalid_argument', message: 'refused' })
    );

    expect(narrowed).toEqual({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'refused',
      rejectedArgument: null,
    });
  });

  it('leaves the remembered working directory alone when only the prototype names it', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowWindowOpenFailure({ kind: 'rejected', code: 'invalid_argument', message: 'refused' })
    );

    expect(shouldDropRememberedCwd(narrowed)).toBe(false);
  });

  it('still drops it when the value owns the argument name, under the same pollution', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowWindowOpenFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'refused',
        rejected_argument: 'cwd',
      })
    );

    expect(shouldDropRememberedCwd(narrowed)).toBe(true);
  });
});

/**
 * The checks above are only worth their green if the fixture can tell the two
 * readers apart. Rather than altering the boundary, the same value under the
 * same pollution is read both ways here.
 */
describe('the pollution fixture separates an owned read from an inherited one', () => {
  it('shows the prototype value to a plain index and nothing to an ownership check', () => {
    const value: Record<string, unknown> = {
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'refused',
    };

    const seen = underPollutedPrototype(() => ({
      byIndex: value['rejected_argument'],
      byOwnership: Object.prototype.hasOwnProperty.call(value, 'rejected_argument')
        ? value['rejected_argument']
        : undefined,
    }));

    expect(seen.byIndex).toBe('cwd');
    expect(seen.byOwnership).toBeUndefined();
  });

  it('shows nothing to either read once the pollution is gone', () => {
    const value: Record<string, unknown> = { kind: 'rejected' };

    expect(value['rejected_argument']).toBeUndefined();
    expect(Object.prototype.hasOwnProperty.call(value, 'rejected_argument')).toBe(false);
    expect(value['code']).toBeUndefined();
    expect(Object.prototype.hasOwnProperty.call(value, 'code')).toBe(false);
  });
});

describe('invokeOpenWindow: what a caller catches for each of the four kinds', () => {
  it('carries a daemon rejection through as a branchable failure', async () => {
    invokeMock.mockRejectedValueOnce({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'the working directory does not exist',
      rejected_argument: 'cwd',
    });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toMatchObject({
      failure: {
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'the working directory does not exist',
        rejectedArgument: 'cwd',
      },
    });
  });

  it('carries a refusal the core raised on its own through unchanged', async () => {
    invokeMock.mockRejectedValueOnce({
      kind: 'local',
      message: 'that window name is already taken',
    });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toThrow(
      'that window name is already taken'
    );
  });

  it.each([
    ['a bare string Tauri refused with', 'invalid args for command open_window'],
    ['an Error the invoke bridge threw', new Error('invalid args for command open_window')],
  ])('does not turn %s into a boundary error', async (_label, rejected) => {
    invokeMock.mockRejectedValueOnce(rejected);

    const caught: unknown = await invokeOpenWindow({ command: COMMAND }).catch(
      (error: unknown) => error
    );

    expect(caught).toBeInstanceOf(WindowOpenFailureError);
    expect(caught).not.toBeInstanceOf(BoundaryError);
    expect(caught).toMatchObject({
      message: 'invalid args for command open_window',
      failure: { kind: 'bare', message: 'invalid args for command open_window' },
    });
  });
});
