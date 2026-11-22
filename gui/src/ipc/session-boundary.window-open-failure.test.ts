/**
 * Contract for the value a command's rejection carries across the Tauri
 * edge. Four kinds of value arrive: the three tagged shapes the core
 * serializes for a command that reaches the daemon, a bare string when Tauri
 * refused the call before the command ran, and an `Error` when the invoke
 * bridge itself threw. Every shape carries the one sentence the failure is
 * told by (`text`). Only a value that claims the tag is held to the contract,
 * so wording that used to reach the user is not turned into a boundary error.
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
  invokeMock: vi.fn<(...args: unknown[]) => Promise<unknown>>(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import {
  BoundaryError,
  CommandFailureError,
  narrowCommandFailure,
  type CommandFailure,
} from './session-boundary';
import { invokeOpenWindow } from './session-client';
import { shouldDropRememberedCwd } from '../window-open-failure-model';

const COMMAND = ['claude'];

/** The sentences the core's Display gives these failures. */
const REJECTED_TEXT =
  'the daemon rejected the request: invalid_argument: the working directory does not exist';
const UNSUPPORTED_TEXT =
  'not sent, as the connected daemon speaks another wire revision: open_window_from_profile';
const LOCAL_TEXT = 'window label must not be empty';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('narrowCommandFailure: the kinds of value it accepts', () => {
  it('reads a bare string as text alone', () => {
    expect(narrowCommandFailure('invalid args for command open_window')).toEqual({
      kind: 'bare',
      text: 'invalid args for command open_window',
    });
  });

  it('reads an Error instance as its message alone', () => {
    expect(narrowCommandFailure(new Error('the invoke bridge failed'))).toEqual({
      kind: 'bare',
      text: 'the invoke bridge failed',
    });
  });

  it('reads a subclass of Error as its message alone', () => {
    class TransportError extends Error {}

    expect(narrowCommandFailure(new TransportError('connection reset'))).toEqual({
      kind: 'bare',
      text: 'connection reset',
    });
  });

  it('accepts a daemon rejection with every field present', () => {
    expect(
      narrowCommandFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'the working directory does not exist',
        rejected_argument: 'cwd',
        text: REJECTED_TEXT,
      })
    ).toEqual({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'the working directory does not exist',
      rejectedArgument: 'cwd',
      text: REJECTED_TEXT,
    });
  });

  it('accepts a daemon rejection whose reason is empty', () => {
    expect(
      narrowCommandFailure({
        kind: 'rejected',
        code: 'superseded',
        message: '',
        text: 'the daemon rejected the request: superseded',
      })
    ).toEqual({
      kind: 'rejected',
      code: 'superseded',
      message: '',
      rejectedArgument: null,
      text: 'the daemon rejected the request: superseded',
    });
  });

  it('accepts a refusal the core raised on its own, carrying its sentence alone', () => {
    expect(narrowCommandFailure({ kind: 'local', text: LOCAL_TEXT })).toEqual({
      kind: 'local',
      text: LOCAL_TEXT,
    });
  });

  it.each([['open_window_from_profile'], ['send_handoff']])(
    'accepts a gate refusal of %s, reading the wire tag under requestType',
    (requestType) => {
      const text = `not sent, as the connected daemon speaks another wire revision: ${requestType}`;

      expect(
        narrowCommandFailure({ kind: 'unsupported', request_type: requestType, text })
      ).toEqual({ kind: 'unsupported', requestType, text });
    }
  );
});

describe('narrowCommandFailure: a rejection that names no argument', () => {
  it('reports no argument when the key is omitted', () => {
    const failure = narrowCommandFailure({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
      text: 'the daemon rejected the request: spawn_failed: the window could not be spawned',
    });

    expect(failure).toMatchObject({ kind: 'rejected', rejectedArgument: null });
  });

  it('reports no argument when the key carries JSON null', () => {
    const failure = narrowCommandFailure({
      kind: 'rejected',
      code: 'spawn_failed',
      message: 'the window could not be spawned',
      rejected_argument: null,
      text: 'the daemon rejected the request: spawn_failed: the window could not be spawned',
    });

    expect(failure).toMatchObject({ kind: 'rejected', rejectedArgument: null });
  });
});

describe('narrowCommandFailure: a value that claims the tag is held to it', () => {
  const rejected = {
    kind: 'rejected',
    code: 'invalid_argument',
    message: 'refused',
    text: REJECTED_TEXT,
  };

  it.each<[string, unknown, RegExp]>([
    ['code is missing', { ...rejected, code: undefined }, /\.code: /],
    ['message is missing', { ...rejected, message: undefined }, /\.message: /],
    ['text is missing', { ...rejected, text: undefined }, /\.text: /],
    ['code is a number', { ...rejected, code: 7 }, /\.code: /],
    ['message is a number', { ...rejected, message: 7 }, /\.message: /],
    ['text is a number', { ...rejected, text: 7 }, /\.text: /],
    ['rejected_argument is a number', { ...rejected, rejected_argument: 7 }, /rejected_argument/],
    ['a local refusal has no text', { kind: 'local' }, /\.text: /],
    [
      'a local refusal carries its sentence under message instead',
      { kind: 'local', message: LOCAL_TEXT },
      /\.text: /,
    ],
    ['a local refusal text is an object', { kind: 'local', text: { text: 'refused' } }, /\.text: /],
    [
      'a gate refusal has no request_type',
      { kind: 'unsupported', text: UNSUPPORTED_TEXT },
      /request_type/,
    ],
    [
      'a gate refusal request_type is not a string',
      { kind: 'unsupported', request_type: 7, text: UNSUPPORTED_TEXT },
      /request_type/,
    ],
    [
      'a gate refusal spells request_type otherwise (camelCase)',
      { kind: 'unsupported', requestType: 'open_window_from_profile', text: UNSUPPORTED_TEXT },
      /request_type/,
    ],
    [
      'a gate refusal has no text',
      { kind: 'unsupported', request_type: 'open_window_from_profile' },
      /\.text: /,
    ],
    ['the tag names a kind this side does not know', { kind: 'exploded', text: 'x' }, /"exploded"/],
    ['the tag is not a string', { kind: 7, text: 'x' }, /kind/],
    ['the tag is null', { kind: null, text: 'x' }, /kind/],
  ])('refuses a value whose %s, naming the part that is wrong', (_label, value, names) => {
    let caught: unknown = null;
    try {
      narrowCommandFailure(value);
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(BoundaryError);
    expect((caught as Error).message).toMatch(names);
  });
});

describe('narrowCommandFailure: a value that claims no tag is read for its text', () => {
  it.each([
    ['an object with no tag', { reason: 'refused' }, '{"reason":"refused"}'],
    ['an array', ['refused'], '["refused"]'],
    ['a number', 7, '7'],
    ['null', null, 'null'],
    ['undefined', undefined, 'undefined'],
  ])('reads %s as text alone', (_label, value, expected) => {
    expect(narrowCommandFailure(value)).toEqual({ kind: 'bare', text: expected });
  });

  it('does not throw for an object whose own reference cycle cannot be encoded', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic['self'] = cyclic;

    expect(narrowCommandFailure(cyclic)).toEqual({ kind: 'bare', text: '' });
  });

  it('reads an object whose encoding is nothing as empty text', () => {
    expect(narrowCommandFailure({ toJSON: () => undefined })).toEqual({ kind: 'bare', text: '' });
  });
});

describe('CommandFailureError: the one Error a narrowed failure is thrown as', () => {
  const shapes: CommandFailure[] = [
    {
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'the working directory does not exist',
      rejectedArgument: 'cwd',
      text: REJECTED_TEXT,
    },
    { kind: 'local', text: LOCAL_TEXT },
    { kind: 'unsupported', requestType: 'open_window_from_profile', text: UNSUPPORTED_TEXT },
    { kind: 'bare', text: 'invalid args for command open_window' },
  ];

  it.each(shapes.map((failure) => [failure.kind, failure] as const))(
    'carries the %s failure itself, and its sentence as the message',
    (_kind, failure) => {
      const error = new CommandFailureError(failure);

      expect(error).toBeInstanceOf(Error);
      expect(error.failure).toBe(failure);
      expect(error.message).toBe(failure.text);
    }
  );
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
  ['text', REJECTED_TEXT],
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

describe('narrowCommandFailure: a value claims a tag only by owning one', () => {
  it.each([
    ['an object with no key of its own', {}, '{}'],
    ['an object whose own keys name no tag', { reason: 'refused' }, '{"reason":"refused"}'],
  ])('reads %s as text alone, whatever its prototype carries', (_label, value, expected) => {
    const narrowed = underPollutedPrototype(() => narrowCommandFailure(value));

    expect(narrowed).toEqual({ kind: 'bare', text: expected });
  });

  it('leaves the remembered working directory alone for such a value', () => {
    const narrowed = underPollutedPrototype(() => narrowCommandFailure({}));

    expect(shouldDropRememberedCwd(narrowed)).toBe(false);
  });

  it('still reads a value that owns its tag, under the same pollution', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowCommandFailure({ kind: 'local', text: 'the command was empty' })
    );

    expect(narrowed).toEqual({ kind: 'local', text: 'the command was empty' });
  });
});

describe('narrowCommandFailure: a field is read only when the value owns it', () => {
  it.each([
    ['the reason text', { kind: 'rejected', code: 'invalid_argument', text: 'refused' }],
    ['the error code', { kind: 'rejected', message: 'refused', text: 'refused' }],
    ['the sentence', { kind: 'rejected', code: 'invalid_argument', message: 'refused' }],
    ['all of them', { kind: 'rejected' }],
  ])('refuses a rejection that owns its tag but inherits %s', (_label, value) => {
    expect(() => underPollutedPrototype(() => narrowCommandFailure(value))).toThrow(BoundaryError);
  });

  it('reports no argument when only the prototype names one', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowCommandFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'refused',
        text: 'the daemon rejected the request: invalid_argument: refused',
      })
    );

    expect(narrowed).toEqual({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'refused',
      rejectedArgument: null,
      text: 'the daemon rejected the request: invalid_argument: refused',
    });
  });

  it('leaves the remembered working directory alone when only the prototype names it', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowCommandFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'refused',
        text: 'the daemon rejected the request: invalid_argument: refused',
      })
    );

    expect(shouldDropRememberedCwd(narrowed)).toBe(false);
  });

  it('still drops it when the value owns the argument name, under the same pollution', () => {
    const narrowed = underPollutedPrototype(() =>
      narrowCommandFailure({
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'refused',
        rejected_argument: 'cwd',
        text: 'the daemon rejected the request: invalid_argument: refused',
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
  it('carries a daemon rejection through as a branchable failure told by its sentence', async () => {
    invokeMock.mockRejectedValueOnce({
      kind: 'rejected',
      code: 'invalid_argument',
      message: 'the working directory does not exist',
      rejected_argument: 'cwd',
      text: REJECTED_TEXT,
    });

    const caught: unknown = await invokeOpenWindow({ command: COMMAND }).catch(
      (error: unknown) => error
    );

    expect(caught).toBeInstanceOf(CommandFailureError);
    expect(caught).toMatchObject({
      message: REJECTED_TEXT,
      failure: {
        kind: 'rejected',
        code: 'invalid_argument',
        message: 'the working directory does not exist',
        rejectedArgument: 'cwd',
        text: REJECTED_TEXT,
      },
    });
  });

  it('carries a refusal the core raised on its own through unchanged', async () => {
    invokeMock.mockRejectedValueOnce({ kind: 'local', text: LOCAL_TEXT });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toMatchObject({
      message: LOCAL_TEXT,
      failure: { kind: 'local', text: LOCAL_TEXT },
    });
  });

  it('carries a gate refusal through told by its sentence', async () => {
    invokeMock.mockRejectedValueOnce({
      kind: 'unsupported',
      request_type: 'open_window_from_profile',
      text: UNSUPPORTED_TEXT,
    });

    await expect(invokeOpenWindow({ command: COMMAND })).rejects.toMatchObject({
      message: UNSUPPORTED_TEXT,
      failure: {
        kind: 'unsupported',
        requestType: 'open_window_from_profile',
        text: UNSUPPORTED_TEXT,
      },
    });
  });

  it.each([
    ['a bare string Tauri refused with', 'invalid args for command open_window'],
    ['an Error the invoke bridge threw', new Error('invalid args for command open_window')],
  ])('does not turn %s into a boundary error', async (_label, rejected) => {
    invokeMock.mockRejectedValueOnce(rejected);

    const caught: unknown = await invokeOpenWindow({ command: COMMAND }).catch(
      (error: unknown) => error
    );

    expect(caught).toBeInstanceOf(CommandFailureError);
    expect(caught).not.toBeInstanceOf(BoundaryError);
    expect(caught).toMatchObject({
      message: 'invalid args for command open_window',
      failure: { kind: 'bare', text: 'invalid args for command open_window' },
    });
  });
});
