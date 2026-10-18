/**
 * The notice a sender sees when some of the names they mentioned reached no
 * one in the room. The names come from the daemon's `unresolved` list, and
 * each is written back the way the mention completion would insert it.
 */
import { describe, it, expect } from 'vitest';

import { describeUnresolved } from './unresolved-notice';

const TAIL = 'The message stays in the room.';

describe('describeUnresolved: telling the sender which mentions reached no one', () => {
  it('says nothing when every mention reached someone', () => {
    expect(describeUnresolved([])).toBeNull();
  });

  it('names the one mention that reached no one, in the singular', () => {
    expect(describeUnresolved(['reviewer'])).toBe(
      `Not delivered to @reviewer: no one in this room has that name. ${TAIL}`
    );
  });

  it('joins two mentions with "and", in the plural', () => {
    expect(describeUnresolved(['a', 'b'])).toBe(
      `Not delivered to @a and @b: no one in this room has those names. ${TAIL}`
    );
  });

  it('lists three or more mentions with commas and a final "and"', () => {
    expect(describeUnresolved(['a', 'b', 'c'])).toBe(
      `Not delivered to @a, @b, and @c: no one in this room has those names. ${TAIL}`
    );
    expect(describeUnresolved(['a', 'b', 'c', 'd'])).toBe(
      `Not delivered to @a, @b, @c, and @d: no one in this room has those names. ${TAIL}`
    );
  });

  it('keeps the order the daemon reported the mentions in', () => {
    expect(describeUnresolved(['zeta', 'alpha'])).toBe(
      `Not delivered to @zeta and @alpha: no one in this room has those names. ${TAIL}`
    );
  });

  it('quotes a name the way the mention completion inserts it', () => {
    expect(describeUnresolved(['code planner'])).toBe(
      `Not delivered to @"code planner": no one in this room has that name. ${TAIL}`
    );
    expect(describeUnresolved(['a.b', 'code_planner', '名前'])).toBe(
      `Not delivered to @"a.b", @code_planner, and @"名前": no one in this room has those names. ${TAIL}`
    );
  });
});
