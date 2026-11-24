/**
 * Types the brand cases of `eslint-boundary-rules.test.ts` import. Declared here, outside the
 * display guards, so the case that uses one is the only place the rule sees it.
 */
import type { DisplayLine } from '../display/untrusted-text';

/** A predicate type the case asserting to it carries no predicate node of. */
export type IsLine = (x: string) => x is DisplayLine;

/** Two interfaces holding each other, one of them a `DisplayLine`. */
export interface RecA {
  b: RecB;
  line: DisplayLine;
}

export interface RecB {
  a: RecA;
}
