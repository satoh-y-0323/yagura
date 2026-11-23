/**
 * How a failed command is read once it is caught: the failure behind the
 * caught value, the one sentence it is told by, and which line the user sees.
 * The sentence is the core's own, carried as the failure's `text`, so an
 * operation that shows the reason shows the same words whether the rejection
 * reached it as a structured failure or as the sentence alone.
 *
 * The line is picked by the failure's kind, never by any other state: only a
 * request the gate refused for the daemon's wire revision shows the
 * incompatible-daemon sentence; any other failure shows the operation's own.
 */
import { describe, it, expect, vi } from 'vitest';

import { sanitizeForDisplay } from './display/untrusted-text';
import { CommandFailureError, type CommandFailure } from './ipc/session-boundary';
import { banners, describeOperationFailure, foldedFailureReason } from './ui-messages';

/** The module under test, loaded where each test reads it. */
async function model(): Promise<typeof import('./command-failure-model')> {
  return import('./command-failure-model');
}

const SUMMARY = sanitizeForDisplay('Could not delete the plan');

const REJECTED: CommandFailure = {
  kind: 'rejected',
  code: 'plan_not_found',
  message: 'no such plan',
  rejectedArgument: null,
  text: 'the daemon rejected the request: plan_not_found: no such plan',
};
const EMPTY_REASON: CommandFailure = {
  kind: 'rejected',
  code: 'superseded',
  message: '',
  rejectedArgument: null,
  text: 'the daemon rejected the request: superseded',
};
const LOCAL: CommandFailure = { kind: 'local', text: 'not connected to the yagura daemon' };
const UNSUPPORTED: CommandFailure = {
  kind: 'unsupported',
  requestType: 'delete_plan',
  text: 'not sent, as the connected daemon speaks another wire revision: delete_plan',
};
const BARE: CommandFailure = { kind: 'bare', text: 'invalid args for command delete_plan' };

describe('commandFailureOf: the failure behind a caught value', () => {
  it.each([REJECTED, LOCAL, UNSUPPORTED, BARE].map((failure) => [failure.kind, failure] as const))(
    'unwraps the %s failure a CommandFailureError carries',
    async (_kind, failure) => {
      const { commandFailureOf } = await model();

      expect(commandFailureOf(new CommandFailureError(failure))).toBe(failure);
    }
  );

  it.each<[string, unknown, string]>([
    ['a plain Error', new Error('the bridge broke'), 'the bridge broke'],
    ['a thrown string', 'unknown command', 'unknown command'],
    ['a thrown number', 42, '42'],
    ['a thrown object', { code: 'EPERM' }, '{"code":"EPERM"}'],
  ])('reads %s as text alone', async (_label, caught, text) => {
    const { commandFailureOf } = await model();

    expect(commandFailureOf(caught)).toEqual({ kind: 'bare', text });
  });
});

describe('foldedFailureReason: the one display line a caught value is told by', () => {
  it.each([REJECTED, EMPTY_REASON, LOCAL, UNSUPPORTED, BARE].map((f) => [f.kind, f] as const))(
    'is the sentence the %s failure carries',
    (_kind, failure) => {
      expect(foldedFailureReason(new CommandFailureError(failure))).toBe(failure.text);
    }
  );

  it.each<[string, unknown, string]>([
    ['a plain Error', new Error('the bridge broke'), 'the bridge broke'],
    ['a thrown string', 'unknown command', 'unknown command'],
    ['a thrown number', 42, '42'],
    ['a thrown object', { code: 'EPERM' }, '{"code":"EPERM"}'],
    ['an object whose encoding is nothing', { toJSON: () => undefined }, ''],
    ['a thrown function', () => 'source text', ''],
  ])('is the text of %s', (_label, caught, text) => {
    expect(foldedFailureReason(caught)).toBe(text);
  });

  it.each<[string, unknown]>([
    [
      'a daemon rejection',
      new CommandFailureError({ ...REJECTED, text: 'first line\r\nsecond\u001b[31m\u202Eline' }),
    ],
    ['a thrown string', 'first line\r\nsecond\u001b[31m\u202Eline'],
  ])(
    'folds the line breaks and control characters of %s, and writes out a format character',
    (_label, caught) => {
      expect(foldedFailureReason(caught)).toBe('first line second [31m\\u{202E}line');
    }
  );
});

describe('commandFailureLine: the line is picked by the kind of failure', () => {
  it('is the incompatible-daemon sentence for a gate refusal, whatever the operation says', async () => {
    const { commandFailureLine } = await model();

    const line = commandFailureLine(new CommandFailureError(UNSUPPORTED), () => SUMMARY);

    expect(line).toBe(banners.daemonIncompatible);
    expect(line).not.toContain('delete_plan');
  });

  it.each<[string, () => unknown]>([
    ['a daemon rejection', () => new CommandFailureError(REJECTED)],
    ['a refusal the core raised on its own', () => new CommandFailureError(LOCAL)],
    ['a failure that carries text alone', () => new CommandFailureError(BARE)],
    ['a plain Error', () => new Error('the bridge broke')],
    ['a thrown string', () => 'unknown command'],
  ])('is the operation line for %s', async (_label, caught) => {
    const { commandFailureLine } = await model();
    const describe = vi.fn<() => string>(() => 'the operation line');

    expect(commandFailureLine(caught(), describe)).toBe('the operation line');
    expect(describe).toHaveBeenCalledTimes(1);
  });
});

/**
 * Before a command's failure kept its kind, the WebView caught the core's
 * sentence as a string and showed it through `describeOperationFailure`. The
 * structured failure must show the same words, or every operation's line
 * changes under the user.
 */
describe('an operation line reads the same from the structured failure as from its sentence', () => {
  it.each([REJECTED, EMPTY_REASON, LOCAL, UNSUPPORTED].map((f) => [f.kind, f] as const))(
    'shows the %s failure as its sentence would have been shown',
    (_kind, failure) => {
      expect(describeOperationFailure(SUMMARY, new CommandFailureError(failure))).toBe(
        describeOperationFailure(SUMMARY, failure.text)
      );
    }
  );
});
