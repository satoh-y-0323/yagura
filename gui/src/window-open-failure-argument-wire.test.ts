/**
 * Which argument a rejection names is a value written twice: the daemon's
 * `RejectedArgument::wire_name` produces it, and this layer's launch-memory
 * rule compares against it. Both sides build their own fixtures, so a rename
 * on one side would leave both green while the remembered working directory
 * was never dropped again. The names are therefore read out of the Rust
 * source here and used verbatim against the rule, rather than restated.
 */
import { describe, it, expect } from 'vitest';

import windowSource from '../../crates/yagura-daemon/src/window.rs?raw';
import type { WindowOpenFailure } from './ipc/session-boundary';
import { bracedBody, itemBody } from './test-utils/markup-scan';
import { shouldDropRememberedCwd } from './window-open-failure-model';

/**
 * The Rust variant standing for the working directory. Only its identifier is
 * named here; the value it maps onto the wire is read from the source.
 */
const WORKING_DIRECTORY_VARIANT = 'Cwd';

/** The Rust method whose match arms carry the names clients read. */
const WIRE_NAME_HEADER = /pub fn wire_name\([^)]*\)[^{]*\{/;

/** The arms of the `match self` expression inside the wire-name method. */
function matchArms(source: string): string {
  const body = itemBody(source, WIRE_NAME_HEADER);
  const matchIndex = body.indexOf('match self');
  if (matchIndex === -1) {
    throw new Error('the wire-name method holds no match on self');
  }
  const open = body.indexOf('{', matchIndex);
  if (open === -1) {
    throw new Error('the match on self has no opening brace');
  }
  return bracedBody(body, open);
}

/** Each variant of the argument enum against the name it is sent under. */
function argumentNames(source: string): Map<string, string> {
  const names = new Map<string, string>();
  const arm = /Self::([A-Z][A-Za-z0-9_]*)\s*=>\s*"([^"]*)"/g;
  let match: RegExpExecArray | null;
  const arms = matchArms(source);
  while ((match = arm.exec(arms)) !== null) {
    names.set(match[1] ?? '', match[2] ?? '');
  }
  return names;
}

/** A rejection naming `argument`, the only shape the rule consults. */
function rejectionNaming(argument: string): WindowOpenFailure {
  return {
    kind: 'rejected',
    code: 'invalid_argument',
    message: 'the daemon refused',
    rejectedArgument: argument,
  };
}

/** Whether a refused launch costs the remembered working directory. */
type LaunchMemoryRule = (failure: WindowOpenFailure) => boolean;

/**
 * Every way `names` and the launch-memory rule disagree: a working-directory
 * name the rule does not act on, a name it acts on that the core never sends,
 * or another argument it mistakes for the working directory. Empty when the
 * two agree.
 */
function disagreementsWithLaunchMemory(
  names: Map<string, string>,
  dropsRememberedCwd: LaunchMemoryRule = shouldDropRememberedCwd
): string[] {
  const problems: string[] = [];
  if (names.size === 0) {
    problems.push('the Rust source names no rejected argument at all');
    return problems;
  }
  const workingDirectory = names.get(WORKING_DIRECTORY_VARIANT);
  if (workingDirectory === undefined) {
    problems.push(`no arm sends a name for the ${WORKING_DIRECTORY_VARIANT} variant`);
  } else {
    if (!dropsRememberedCwd(rejectionNaming(workingDirectory))) {
      problems.push(
        `${WORKING_DIRECTORY_VARIANT}: the remembered directory survived the name the core sends`
      );
    }
    if (dropsRememberedCwd(rejectionNaming(`${workingDirectory}_elsewhere`))) {
      problems.push(
        `${WORKING_DIRECTORY_VARIANT}: the remembered directory was dropped for a name the core never sends`
      );
    }
  }
  for (const [variant, name] of names) {
    if (variant === WORKING_DIRECTORY_VARIANT) {
      continue;
    }
    if (dropsRememberedCwd(rejectionNaming(name))) {
      problems.push(`${variant}: the remembered directory was dropped for another argument`);
    }
  }
  return problems;
}

/** A Rust fragment declaring the wire-name method over `arms`. */
function wireNameSource(arms: string): string {
  return [
    'impl RejectedArgument {',
    "    pub fn wire_name(self) -> &'static str {",
    '        match self {',
    arms,
    '        }',
    '    }',
    '}',
  ].join('\n');
}

/** The same fragment, with the arms written out of `names`. */
function wireNameSourceOf(names: Array<[string, string]>): string {
  return wireNameSource(
    names.map(([variant, name]) => `            Self::${variant} => "${name}",`).join('\n')
  );
}

const REAL_NAMES = argumentNames(windowSource);
const WORKING_DIRECTORY_NAME = REAL_NAMES.get(WORKING_DIRECTORY_VARIANT) ?? '';

describe('the rejected-argument names are read out of the Rust source', () => {
  it('reads a non-empty source (sanity check against a vacuous pass)', () => {
    expect(windowSource.length).toBeGreaterThan(0);
  });

  it('finds at least one argument name', () => {
    expect(REAL_NAMES.size).toBeGreaterThan(0);
  });

  it('finds an arm for the working-directory variant', () => {
    expect(REAL_NAMES.has(WORKING_DIRECTORY_VARIANT)).toBe(true);
  });

  it('finds a non-empty name under every arm', () => {
    for (const name of REAL_NAMES.values()) {
      expect(name.length).toBeGreaterThan(0);
    }
  });
});

describe('the launch memory acts on the argument name the core sends', () => {
  it('agrees with the Rust source on which name drops the remembered directory', () => {
    expect(disagreementsWithLaunchMemory(REAL_NAMES)).toEqual([]);
  });
});

/**
 * The check above is only worth its green if a disagreement would show up in
 * it. Rather than breaking the real source, the same reader is handed
 * fragments that disagree on purpose.
 */
describe('the agreement check answers for a source that disagrees', () => {
  const DISAGREEING_SOURCES: Array<[string, string]> = [
    [
      'the working-directory argument sent under another name',
      wireNameSourceOf([[WORKING_DIRECTORY_VARIANT, `${WORKING_DIRECTORY_NAME}_renamed`]]),
    ],
    [
      'the working-directory variant renamed',
      wireNameSourceOf([[`${WORKING_DIRECTORY_VARIANT}Renamed`, WORKING_DIRECTORY_NAME]]),
    ],
    [
      'another argument sharing the working-directory name',
      wireNameSourceOf([
        [WORKING_DIRECTORY_VARIANT, WORKING_DIRECTORY_NAME],
        ['Command', WORKING_DIRECTORY_NAME],
      ]),
    ],
    ['no argument named at all', wireNameSource('')],
  ];

  it.each(DISAGREEING_SOURCES)('reports a disagreement for %s', (_label, source) => {
    expect(disagreementsWithLaunchMemory(argumentNames(source)).length).toBeGreaterThan(0);
  });

  it('reports nothing for a fragment holding the real arms verbatim', () => {
    const verbatim = wireNameSource(matchArms(windowSource));

    expect(argumentNames(verbatim)).toEqual(REAL_NAMES);
    expect(disagreementsWithLaunchMemory(argumentNames(verbatim))).toEqual([]);
  });
});

/**
 * The same check, handed rules that disagree on purpose. This covers the
 * direction no Rust fragment can reach: a rule loose enough to act on a name
 * the core never sends.
 */
describe('the agreement check answers for a rule that disagrees', () => {
  const NEVER_DROPS: LaunchMemoryRule = () => false;
  const DROPS_ON_ANY_NAME_CONTAINING_IT: LaunchMemoryRule = (failure) =>
    failure.kind === 'rejected' &&
    failure.rejectedArgument !== null &&
    failure.rejectedArgument.includes(WORKING_DIRECTORY_NAME);

  it('reports a disagreement for a rule that never drops the remembered directory', () => {
    expect(disagreementsWithLaunchMemory(REAL_NAMES, NEVER_DROPS).length).toBeGreaterThan(0);
  });

  it('reports a disagreement for a rule that drops on a name the core never sends', () => {
    expect(
      disagreementsWithLaunchMemory(REAL_NAMES, DROPS_ON_ANY_NAME_CONTAINING_IT).length
    ).toBeGreaterThan(0);
  });
});
