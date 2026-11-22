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

import type { CommandFailure } from './ipc/session-boundary';
import { describeWindowOpenFailure, shouldDropRememberedCwd } from './window-open-failure-model';
import { banners } from './ui-messages';

/** Operation-specific wording the caller supplies; never read from `i18n/`. */
const SUMMARY = 'Launch refused';

/** The argument name the daemon uses for a refused working directory. */
const CWD_ARGUMENT = 'cwd';

/** The sentence the core's Display gives a daemon rejection. */
function rejectedText(code: string, message: string): string {
  return `the daemon rejected the request: ${code}${message === '' ? '' : `: ${message}`}`;
}

function rejection(
  rejectedArgument: string | null,
  message = 'the daemon refused'
): CommandFailure {
  return {
    kind: 'rejected',
    code: 'invalid_argument',
    message,
    rejectedArgument,
    text: rejectedText('invalid_argument', message),
  };
}

describe('describeWindowOpenFailure: the reason goes through the display formatting', () => {
  it('folds line breaks, tabs, control characters and non-breaking spaces into single spaces', () => {
    const failure: CommandFailure = {
      kind: 'local',
      text: 'first\nsecond\tthird\u00a0fourth\u0007fifth',
    };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(
      `${SUMMARY}: first second third fourth fifth`
    );
  });

  it('folds a run of several control characters into one space, not one space each', () => {
    const failure: CommandFailure = { kind: 'bare', text: 'left\r\n\r\n\tright' };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: left right`);
  });

  it('drops control characters at the ends rather than keeping them as spaces', () => {
    const failure: CommandFailure = { kind: 'bare', text: '\n  boom  \n' };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: boom`);
  });

  it('caps a long reason, keeping its beginning and dropping its end', () => {
    const tokens = Array.from({ length: 400 }, (_, index) => `t${index}`);
    const raw = tokens.join('-');

    const line = describeWindowOpenFailure(SUMMARY, { kind: 'bare', text: raw });

    expect(line.startsWith(`${SUMMARY}: t0-t1-`)).toBe(true);
    expect(line).not.toContain('t399');
    expect(Array.from(line).length).toBeLessThan(Array.from(raw).length / 2);
  });

  it('carries a short reason through whole, for each of the three failure shapes', () => {
    const shapes: CommandFailure[] = [
      rejection(CWD_ARGUMENT, 'no such directory'),
      { kind: 'local', text: 'no such directory' },
      { kind: 'bare', text: 'no such directory' },
    ];

    for (const failure of shapes) {
      expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: no such directory`);
    }
  });
});

describe('describeWindowOpenFailure: an empty reason leaves the summary alone for a failure that carries no code', () => {
  it('returns the summary alone when the reason is empty', () => {
    expect(describeWindowOpenFailure(SUMMARY, { kind: 'bare', text: '' })).toBe(SUMMARY);
  });

  it('returns the summary alone when the reason holds nothing but whitespace and control characters', () => {
    expect(describeWindowOpenFailure(SUMMARY, { kind: 'local', text: '\t\n\u00a0 ' })).toBe(
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
      text: rejectedText('invalid_argument', ''),
    });
    const second = describeWindowOpenFailure(SUMMARY, {
      kind: 'rejected',
      code: 'window_not_found',
      message: '',
      rejectedArgument: null,
      text: rejectedText('window_not_found', ''),
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
 * The request was refused because the running daemon does not speak this
 * build's wire revision. Neither the arm's sentence nor the wire tag of the
 * refused request reaches the user: the line is the one sentence every
 * operation shows while the daemon cannot be used, so no operation summary
 * goes in front of it.
 */
describe('describeWindowOpenFailure: the daemon cannot be used with this build', () => {
  /** A value distinctive enough that only real leakage would produce it. */
  const REQUEST_TYPE = 'probe-wire-tag-open_window_from_profile';

  function unsupported(): CommandFailure {
    return {
      kind: 'unsupported',
      requestType: REQUEST_TYPE,
      text: `not sent, as the connected daemon speaks another wire revision: ${REQUEST_TYPE}`,
    };
  }

  it('is the incompatible-daemon sentence itself, with no summary in front and no wire tag', () => {
    const line = describeWindowOpenFailure(SUMMARY, unsupported());
    expect(line).toBe(banners.daemonIncompatible);
    expect(
      line.startsWith(
        "This daemon can't be used with this version of yagura: restart it (yagura-daemon --shutdown)."
      )
    ).toBe(true);
    expect(line).not.toContain(SUMMARY);
    expect(line).not.toContain(REQUEST_TYPE);
  });

  it('carries the restart-consequence sentence shared with the approver-registration-refused notice', () => {
    const RESTART_CLOSES_ALL_WINDOWS =
      'Restarting the daemon closes every window that is currently open.';
    expect(describeWindowOpenFailure(SUMMARY, unsupported())).toContain(RESTART_CLOSES_ALL_WINDOWS);
  });
});

describe('describeWindowOpenFailure: a taken name says how to get past it', () => {
  const HINT = 'Choose another name, or leave the name empty to get a numbered one.';
  const TAKEN = 'a window named “Claude Code” is already open';

  it('follows a name_taken refusal with the hint, as its own sentence', () => {
    const failure: CommandFailure = {
      kind: 'rejected',
      code: 'name_taken',
      message: TAKEN,
      rejectedArgument: 'name',
      text: rejectedText('name_taken', TAKEN),
    };

    expect(describeWindowOpenFailure(SUMMARY, failure)).toBe(`${SUMMARY}: ${TAKEN}. ${HINT}`);
  });

  it('leaves the hint off any other refusal, a refused directory included', () => {
    const others: CommandFailure[] = [
      rejection(CWD_ARGUMENT, 'no such directory'),
      {
        kind: 'rejected',
        code: 'profile_disabled',
        message: TAKEN,
        rejectedArgument: 'profile',
        text: rejectedText('profile_disabled', TAKEN),
      },
      { kind: 'local', text: TAKEN },
      { kind: 'bare', text: TAKEN },
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
    expect(shouldDropRememberedCwd({ kind: 'local', text: 'cwd is not a directory' })).toBe(false);
  });

  it('keeps it for a gate refusal, which never names an argument', () => {
    expect(
      shouldDropRememberedCwd({
        kind: 'unsupported',
        requestType: 'open_window_from_profile',
        text: 'not sent, as the connected daemon speaks another wire revision: open_window_from_profile',
      })
    ).toBe(false);
  });

  it('keeps it for a failure that carries text alone, whatever its text says', () => {
    expect(shouldDropRememberedCwd({ kind: 'bare', text: 'cwd is not a directory' })).toBe(false);
  });
});
