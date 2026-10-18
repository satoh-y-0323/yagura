/**
 * Contract for the two value -> value mappings a refused window launch goes
 * through: the line the dialog shows, and whether the remembered working
 * directory survives. Both live outside the components because there is no
 * rendering harness to measure them through.
 *
 * The display formatting is measured by its effect -- control characters
 * folded, a long reason capped -- rather than by naming the formatter, so
 * that the contract holds however the folding is reached.
 */
import { describe, it, expect } from 'vitest';

import { WindowOpenFailureError, type WindowOpenFailure } from './ipc/session-boundary';
import {
  describeWindowOpenFailure,
  shouldDropRememberedCwd,
  windowOpenFailureOf,
} from './window-open-failure-model';

/** Operation-specific wording the caller supplies; never read from `i18n/`. */
const SUMMARY = 'Launch refused';

/** The argument name the daemon uses for a refused working directory. */
const CWD_ARGUMENT = 'cwd';

function rejection(
  rejectedArgument: string | null,
  message = 'the daemon refused'
): WindowOpenFailure {
  return { kind: 'rejected', code: 'invalid_argument', message, rejectedArgument };
}

describe('describeWindowOpenFailure: the reason goes through the display formatting', () => {
  it('folds line breaks, tabs, control characters and non-breaking spaces into single spaces', () => {
    const failure: WindowOpenFailure = {
      kind: 'local',
      message: 'first\nsecond\tthird\u00a0fourth\u0007fifth',
    };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(
      `${SUMMARY}: first second third fourth fifth`
    );
  });

  it('folds a run of several control characters into one space, not one space each', () => {
    const failure: WindowOpenFailure = { kind: 'bare', message: 'left\r\n\r\n\tright' };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: left right`);
  });

  it('drops control characters at the ends rather than keeping them as spaces', () => {
    const failure: WindowOpenFailure = { kind: 'bare', message: '\n  boom  \n' };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: boom`);
  });

  it('caps a long reason, keeping its beginning and dropping its end', () => {
    const tokens = Array.from({ length: 400 }, (_, index) => `t${index}`);
    const raw = tokens.join('-');

    const line = describeWindowOpenFailure(SUMMARY, { kind: 'bare', message: raw });

    expect(line.startsWith(`${SUMMARY}: t0-t1-`)).toBe(true);
    expect(line).not.toContain('t399');
    expect(Array.from(line).length).toBeLessThan(Array.from(raw).length / 2);
  });

  it('carries a short reason through whole, for each of the three failure shapes', () => {
    const shapes: WindowOpenFailure[] = [
      rejection(CWD_ARGUMENT, 'no such directory'),
      { kind: 'local', message: 'no such directory' },
      { kind: 'bare', message: 'no such directory' },
    ];

    for (const failure of shapes) {
      expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: no such directory`);
    }
  });
});

describe('describeWindowOpenFailure: an empty reason leaves the summary alone for a failure that carries no code', () => {
  it('returns the summary alone when the reason is empty', () => {
    expect(describeWindowOpenFailure(SUMMARY, { kind: 'bare', message: '' })).toBe(SUMMARY);
  });

  it('returns the summary alone when the reason holds nothing but whitespace and control characters', () => {
    expect(describeWindowOpenFailure(SUMMARY, { kind: 'local', message: '\t\n\u00a0 ' })).toBe(
      SUMMARY
    );
  });
});

describe('describeWindowOpenFailure: a rejection whose reason is empty still names its code', () => {
  it('leaves the code readable after the summary', () => {
    const line = describeWindowOpenFailure(SUMMARY, rejection(CWD_ARGUMENT, ''));

    expect(line.startsWith(SUMMARY)).toBe(true);
    expect(line).toContain('invalid_argument');
  });

  it('names the code the rejection carries, not a fixed one', () => {
    const first = describeWindowOpenFailure(SUMMARY, {
      kind: 'rejected',
      code: 'invalid_argument',
      message: '',
      rejectedArgument: CWD_ARGUMENT,
    });
    const second = describeWindowOpenFailure(SUMMARY, {
      kind: 'rejected',
      code: 'window_not_found',
      message: '',
      rejectedArgument: null,
    });

    expect(first).toContain('invalid_argument');
    expect(first).not.toContain('window_not_found');
    expect(second).toContain('window_not_found');
    expect(second).not.toContain('invalid_argument');
  });
});

describe('describeWindowOpenFailure: a rejection that names no argument still renders', () => {
  it('builds the line from a rejection whose argument name is absent', () => {
    expect(describeWindowOpenFailure(SUMMARY, rejection(null, 'the command was empty'))).toBe(
      `${SUMMARY}: the command was empty`
    );
  });
});

/**
 * The daemon-side gate refused this
 * request because it has not settled a wire revision the request needs.
 * Unlike the other three shapes, this arm carries no `message` -- only the
 * wire tag of the request that was refused, which must never reach the
 * user -- so the display text comes from a fixed i18n line rather than the
 * summary/detail join the other shapes go through.
 */
describe('describeWindowOpenFailure: the daemon has not settled a wire revision this request needs', () => {
  /** A value distinctive enough that only real leakage would produce it. */
  const REQUEST_TYPE = 'probe-wire-tag-open_window_from_profile';

  function unsupported(): WindowOpenFailure {
    return { kind: 'unsupported', requestType: REQUEST_TYPE };
  }

  // (i) and (ii) are gated on the same call, not split across two `it`
  // blocks: today the failure falls all the way back to the bare summary
  // (no dedicated branch exists yet), which would let a standalone "does not
  // contain the wire tag" check pass for the wrong reason -- the summary
  // never contained it either. Gating on (i) first keeps that check honest.
  it('does not fall back to the generic summary, and never carries the wire tag the arm reports (i, ii)', () => {
    const line = describeWindowOpenFailure(SUMMARY, unsupported());
    expect(line).not.toBe(SUMMARY);
    expect(line).not.toContain(REQUEST_TYPE);
  });

  it('carries the restart-consequence sentence shared with the approver-registration-refused banner (iii)', () => {
    const RESTART_CLOSES_ALL_WINDOWS =
      'Restarting the daemon closes every window that is currently open.';
    expect(describeWindowOpenFailure(SUMMARY, unsupported())).toContain(RESTART_CLOSES_ALL_WINDOWS);
  });
});

describe('describeWindowOpenFailure: a taken name says how to get past it', () => {
  const HINT = 'Choose another name, or leave the name empty to get a numbered one.';
  const TAKEN = 'a window named “Claude Code” is already open';

  it('follows a name_taken refusal with the hint, as its own sentence', () => {
    const failure: WindowOpenFailure = {
      kind: 'rejected',
      code: 'name_taken',
      message: TAKEN,
      rejectedArgument: 'name',
    };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: ${TAKEN}. ${HINT}`);
  });

  it('leaves the hint off any other refusal, a refused directory included', () => {
    const others: WindowOpenFailure[] = [
      rejection(CWD_ARGUMENT, 'no such directory'),
      { kind: 'rejected', code: 'profile_disabled', message: TAKEN, rejectedArgument: 'profile' },
      { kind: 'local', message: TAKEN },
      { kind: 'bare', message: TAKEN },
    ];

    for (const failure of others) {
      expect(describeWindowOpenFailure(SUMMARY, failure)).not.toContain(HINT);
    }
  });
});

describe('shouldDropRememberedCwd: the directory is dropped only when the rejection names it', () => {
  it('drops it when the rejection names the working-directory argument', () => {
    expect(shouldDropRememberedCwd(rejection(CWD_ARGUMENT))).toBe(true);
  });

  it('drops it even when the reason text never mentions the directory', () => {
    expect(shouldDropRememberedCwd(rejection(CWD_ARGUMENT, 'os error 3'))).toBe(true);
  });

  it.each([['name'], ['command'], ['detection'], ['profile'], ['vendor']])(
    'keeps it when the rejection names %s instead',
    (argument) => {
      expect(shouldDropRememberedCwd(rejection(argument))).toBe(false);
    }
  );

  it('keeps it when the rejection names no argument at all', () => {
    expect(shouldDropRememberedCwd(rejection(null))).toBe(false);
  });

  it('keeps it when the reason text names the directory but the rejection does not', () => {
    const failure = rejection(null, 'cwd "C:\\gone" does not exist');

    expect(shouldDropRememberedCwd(failure)).toBe(false);
  });

  it('keeps it for a refusal the core raised on its own, whatever its text says', () => {
    expect(shouldDropRememberedCwd({ kind: 'local', message: 'cwd is not a directory' })).toBe(
      false
    );
  });

  it('keeps it for a gate refusal, which never names an argument', () => {
    expect(
      shouldDropRememberedCwd({ kind: 'unsupported', requestType: 'open_window_from_profile' })
    ).toBe(false);
  });

  it('keeps it for a failure that carries text alone, whatever its text says', () => {
    expect(shouldDropRememberedCwd({ kind: 'bare', message: 'cwd is not a directory' })).toBe(
      false
    );
  });
});

describe('windowOpenFailureOf: the failure behind a caught value', () => {
  it('unwraps the failure a WindowOpenFailureError carries', () => {
    const failure = rejection(CWD_ARGUMENT);

    expect(windowOpenFailureOf(new WindowOpenFailureError(failure))).toEqual(failure);
  });

  it('reads a plain Error as text alone', () => {
    expect(windowOpenFailureOf(new Error('the bridge broke'))).toEqual({
      kind: 'bare',
      message: 'the bridge broke',
    });
  });

  it('reads a thrown string as text alone', () => {
    expect(windowOpenFailureOf('unknown command')).toEqual({
      kind: 'bare',
      message: 'unknown command',
    });
  });

  it('reads a thrown value that is neither an Error nor a string as text alone', () => {
    expect(windowOpenFailureOf(42)).toEqual({ kind: 'bare', message: '42' });
  });
});
