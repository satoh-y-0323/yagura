/**
 * The notice a sender sees when some of the names they mentioned reached no
 * one in the room. The mentions come from the daemon's `unresolved` list,
 * which the boundary already wrote the way the mention completion inserts them.
 */
import { describe, it, expect } from 'vitest';

import { sanitizeForDisplay, type DisplayLine } from '../display/untrusted-text';
import { describeUnresolved } from './unresolved-notice';

const TAIL = 'The message stays in the room.';

/** The daemon's `unresolved` list as the boundary hands it on: each name written as its mention. */
function unresolved(...mentions: string[]): DisplayLine[] {
  return mentions.map((mention) => sanitizeForDisplay(mention));
}

describe('describeUnresolved: telling the sender which mentions reached no one', () => {
  it('says nothing when every mention reached someone', () => {
    expect(describeUnresolved([])).toBeNull();
  });

  it('names the one mention that reached no one, in the singular', () => {
    expect(describeUnresolved(unresolved('@reviewer'))).toBe(
      `Not delivered to @reviewer: no one in this room has that name. ${TAIL}`
    );
  });

  it('joins two mentions with "and", in the plural', () => {
    expect(describeUnresolved(unresolved('@a', '@b'))).toBe(
      `Not delivered to @a and @b: no one in this room has those names. ${TAIL}`
    );
  });

  it('lists three or more mentions with commas and a final "and"', () => {
    expect(describeUnresolved(unresolved('@a', '@b', '@c'))).toBe(
      `Not delivered to @a, @b, and @c: no one in this room has those names. ${TAIL}`
    );
    expect(describeUnresolved(unresolved('@a', '@b', '@c', '@d'))).toBe(
      `Not delivered to @a, @b, @c, and @d: no one in this room has those names. ${TAIL}`
    );
  });

  it('keeps the order the daemon reported the mentions in', () => {
    expect(describeUnresolved(unresolved('@zeta', '@alpha'))).toBe(
      `Not delivered to @zeta and @alpha: no one in this room has those names. ${TAIL}`
    );
  });
});
