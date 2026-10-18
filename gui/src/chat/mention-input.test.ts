/**
 * The chat input's `@` completion: locating the mention being typed, offering
 * the room's participants plus the reserved human entry, and writing the
 * chosen name back into the text. Names carrying a space go in quoted, the
 * form the daemon's mention parser accepts.
 */
import { describe, it, expect } from 'vitest';

import {
  MENTION_USER_NAME,
  dismissMention,
  filterMentionCandidates,
  findMentionQuery,
  insertMention,
  keepMentionSelection,
  openMention,
  type DismissedMention,
  type MentionCandidate,
  type MentionQuery,
} from './mention-input';
import type { ChatParticipant } from '../ipc/session-boundary';

const ROSTER: ChatParticipant[] = [
  { window_id: 'win1', name: 'builder' },
  { window_id: 'win2', name: 'reviewer' },
  { window_id: 'win4', name: 'code planner' },
];

/** The candidate names, in the order they are offered. */
function names(roster: ChatParticipant[], query: string): string[] {
  return filterMentionCandidates(roster, query).map((candidate) => candidate.name);
}

describe('findMentionQuery: where the mention starts', () => {
  it('finds a mention being typed at the end of the text', () => {
    expect(findMentionQuery('hello @bui', 10)).toEqual({
      start: 6,
      end: 10,
      query: 'bui',
      quoted: false,
    });
  });

  it('finds a bare @ with nothing typed after it yet', () => {
    expect(findMentionQuery('hello @', 7)).toEqual({
      start: 6,
      end: 7,
      query: '',
      quoted: false,
    });
  });

  it('finds a mention at the very start of the text', () => {
    expect(findMentionQuery('@rev', 4)).toEqual({ start: 0, end: 4, query: 'rev', quoted: false });
  });

  it('finds the mention the caret sits in, not a later one', () => {
    expect(findMentionQuery('@bui and @rev', 4)).toEqual({
      start: 0,
      end: 4,
      query: 'bui',
      quoted: false,
    });
  });

  it('keeps reading across spaces while the mention is an unterminated quote', () => {
    expect(findMentionQuery('ping @"code pl', 14)).toEqual({
      start: 5,
      end: 14,
      query: 'code pl',
      quoted: true,
    });
  });

  it('offers nothing once the quoted mention has been closed', () => {
    expect(findMentionQuery('ping @"code planner"', 20)).toBeNull();
  });

  it('offers nothing when a space follows a bare mention', () => {
    expect(findMentionQuery('ping @builder now', 17)).toBeNull();
  });

  it('offers nothing for an @ glued to the preceding word', () => {
    expect(findMentionQuery('mail me@example', 15)).toBeNull();
  });

  it('offers nothing when the text before the caret holds no @ at all', () => {
    expect(findMentionQuery('plain text', 10)).toBeNull();
  });

  it('ignores what comes after the caret', () => {
    expect(findMentionQuery('@bui rest of the line', 4)).toEqual({
      start: 0,
      end: 4,
      query: 'bui',
      quoted: false,
    });
  });

  it('offers nothing for a caret outside the text', () => {
    expect(findMentionQuery('@bui', 9)).toBeNull();
    expect(findMentionQuery('@bui', -1)).toBeNull();
  });

  it('treats a newline like any other whitespace boundary', () => {
    expect(findMentionQuery('first line\n@rev', 15)).toEqual({
      start: 11,
      end: 15,
      query: 'rev',
      quoted: false,
    });
  });
});

describe('filterMentionCandidates: who is offered', () => {
  it('offers every participant plus the reserved human entry for an empty query', () => {
    expect(names(ROSTER, '')).toEqual(['builder', 'reviewer', 'code planner', MENTION_USER_NAME]);
  });

  it('keeps the window id of a participant and leaves the reserved entry without one', () => {
    const candidates = filterMentionCandidates(ROSTER, '');
    expect(candidates[0]).toEqual({ name: 'builder', windowId: 'win1' });
    expect(candidates[candidates.length - 1]).toEqual({
      name: MENTION_USER_NAME,
      windowId: null,
    });
  });

  it('narrows to the participants whose name starts with the query', () => {
    expect(names(ROSTER, 'r')).toEqual(['reviewer']);
  });

  it('matches without regard to case', () => {
    expect(names(ROSTER, 'BUI')).toEqual(['builder']);
  });

  it('treats an underscore and a hyphen in the name as the same character', () => {
    const roster: ChatParticipant[] = [{ window_id: 'win9', name: 'code_planner' }];
    expect(names(roster, 'code-pl')).toEqual(['code_planner']);
    expect(names(roster, 'code_pl')).toEqual(['code_planner']);
  });

  it('matches a name carrying a space from the unquoted text typed so far', () => {
    expect(names(ROSTER, 'code pl')).toEqual(['code planner']);
  });

  it('offers the reserved human entry when the query prefixes it', () => {
    expect(names(ROSTER, 'us')).toEqual([MENTION_USER_NAME]);
  });

  it('offers nothing when no name starts with the query', () => {
    expect(names(ROSTER, 'zz')).toEqual([]);
  });

  it('matches on a prefix only, not anywhere in the name', () => {
    expect(names(ROSTER, 'planner')).toEqual([]);
  });

  it('still offers the reserved entry last when a participant carries that same name', () => {
    const roster: ChatParticipant[] = [{ window_id: 'win7', name: 'user' }];
    const candidates = filterMentionCandidates(roster, 'user');
    expect(candidates).toEqual([
      { name: 'user', windowId: 'win7' },
      { name: MENTION_USER_NAME, windowId: null },
    ]);
  });

  it('offers the reserved entry alone for an empty roster', () => {
    expect(names([], '')).toEqual([MENTION_USER_NAME]);
  });
});

describe('insertMention: writing the chosen name back', () => {
  const query = (text: string, caret: number) => {
    const found = findMentionQuery(text, caret);
    if (found === null) {
      throw new Error(`no mention query in ${JSON.stringify(text)} at ${caret}`);
    }
    return found;
  };

  it('replaces what was typed with the plain form and leaves the caret after a trailing space', () => {
    const text = 'hello @bui';
    expect(insertMention(text, query(text, 10), 'builder')).toEqual({
      text: 'hello @builder ',
      caret: 15,
    });
  });

  it('quotes a name carrying a space', () => {
    const text = 'ping @code';
    expect(insertMention(text, query(text, 10), 'code planner')).toEqual({
      text: 'ping @"code planner" ',
      caret: 21,
    });
  });

  it('closes the quote of a mention that was typed in the quoted form', () => {
    const text = 'ping @"code pl';
    expect(insertMention(text, query(text, 14), 'code planner')).toEqual({
      text: 'ping @"code planner" ',
      caret: 21,
    });
  });

  it('keeps the text that follows the caret', () => {
    const text = '@rev rest of the line';
    expect(insertMention(text, query(text, 4), 'reviewer')).toEqual({
      text: '@reviewer  rest of the line',
      caret: 10,
    });
  });

  it('writes the reserved human entry as a plain mention', () => {
    const text = 'thanks @';
    expect(insertMention(text, query(text, 8), MENTION_USER_NAME)).toEqual({
      text: 'thanks @user ',
      caret: 13,
    });
  });
});

describe('openMention: a mention closed with Escape stays closed until the text or the caret leaves it', () => {
  /** The chat input as the component sees it on each key: its text, caret and what was dismissed. */
  interface Input {
    text: string;
    caret: number;
    dismissed: DismissedMention | null;
  }

  /** The mention located at the caret; the test fixtures always have one. */
  function located(text: string, caret: number): MentionQuery {
    const found = findMentionQuery(text, caret);
    if (found === null) {
      throw new Error(`fixture has no mention at ${caret} in ${JSON.stringify(text)}`);
    }
    return found;
  }

  /** The input right after Escape closed the mention at its caret. */
  function closedWithEscape(text: string, caret: number): Input {
    return { text, caret, dismissed: dismissMention(text, located(text, caret)) };
  }

  /** Re-reads the mention after an edit or a caret move, carrying the dismissal forward. */
  function step(
    input: Input,
    text: string,
    caret: number
  ): Input & { mention: MentionQuery | null } {
    const next = openMention(text, caret, input.dismissed);
    return { text, caret, dismissed: next.dismissed, mention: next.mention };
  }

  it('offers the mention at the caret when nothing was dismissed', () => {
    for (const [text, caret] of [
      ['hello @bui', 10],
      ['hello @', 7],
      ['ping @"code pl', 14],
      ['@bui and @rev', 4],
    ] as const) {
      expect(openMention(text, caret, null)).toEqual({
        mention: findMentionQuery(text, caret),
        dismissed: null,
      });
    }
    expect(openMention('no mention here', 5, null)).toEqual({ mention: null, dismissed: null });
  });

  it('keeps the dismissed mention closed on the key events that follow with nothing changed', () => {
    let input: Input = closedWithEscape('hello @bui', 10);
    for (let keyup = 0; keyup < 3; keyup += 1) {
      const next = step(input, 'hello @bui', 10);
      expect(next.mention).toBeNull();
      input = next;
    }
  });

  it('keeps it closed while the caret moves inside the same mention', () => {
    const closed = closedWithEscape('hello @builder', 14);
    for (const caret of [7, 8, 10, 13, 14]) {
      expect(step(closed, 'hello @builder', caret).mention).toBeNull();
    }
    const quoted = closedWithEscape('ping @"code pl', 14);
    expect(step(quoted, 'ping @"code pl', 9).mention).toBeNull();
  });

  it('opens again once a character is typed into the mention', () => {
    const next = step(closedWithEscape('hello @bui', 10), 'hello @buil', 11);
    expect(next.mention).toEqual(findMentionQuery('hello @buil', 11));
  });

  it('opens again once a character is deleted from the mention', () => {
    const next = step(closedWithEscape('hello @bui', 10), 'hello @bu', 9);
    expect(next.mention).toEqual(findMentionQuery('hello @bu', 9));
  });

  it('opens again when the text changes and changes back, since the change ended the dismissal', () => {
    const typed = step(closedWithEscape('hello @bui', 10), 'hello @buil', 11);
    const back = step(typed, 'hello @bui', 10);
    expect(back.mention).toEqual(findMentionQuery('hello @bui', 10));
  });

  it('opens again once the caret has left the mention and comes back, with the text unchanged', () => {
    const closed = closedWithEscape('hello @bui', 10);
    const outside = step(closed, 'hello @bui', 3);
    expect(outside.mention).toBeNull();
    const back = step(outside, 'hello @bui', 10);
    expect(back.mention).toEqual(findMentionQuery('hello @bui', 10));
  });

  it('treats the caret just before the @ as outside the mention', () => {
    const closed = closedWithEscape('hello @bui', 10);
    const atStart = step(closed, 'hello @bui', 6);
    const back = step(atStart, 'hello @bui', 10);
    expect(back.mention).toEqual(findMentionQuery('hello @bui', 10));
  });

  it('offers another mention in the same text: only the dismissed one stays closed', () => {
    const text = '@bui and @rev';
    const closed = closedWithEscape(text, 4);
    const other = step(closed, text, 13);
    expect(other.mention).toEqual(findMentionQuery(text, 13));
  });
});

describe('keepMentionSelection: the chosen candidate survives a re-read of the same mention', () => {
  /** The mention located at the caret; the test fixtures always have one. */
  function at(text: string, caret: number = text.length): MentionQuery {
    const found = findMentionQuery(text, caret);
    if (found === null) {
      throw new Error(`fixture has no mention at ${caret} in ${JSON.stringify(text)}`);
    }
    return found;
  }

  function offered(roster: ChatParticipant[], mention: MentionQuery): MentionCandidate[] {
    return filterMentionCandidates(roster, mention.query);
  }

  it('keeps the position chosen with the arrow keys when the mention and the list are unchanged', () => {
    const mention = at('hello @');
    const candidates = offered(ROSTER, mention);
    expect(
      keepMentionSelection(
        { mention, candidates, active: 2 },
        { mention: at('hello @'), candidates }
      )
    ).toBe(2);
  });

  it('keeps the reserved human entry chosen when the list is unchanged', () => {
    const mention = at('hello @');
    const candidates = offered(ROSTER, mention);
    const user = candidates.length - 1;
    expect(candidates[user]?.name).toBe(MENTION_USER_NAME);
    expect(
      keepMentionSelection({ mention, candidates, active: user }, { mention, candidates })
    ).toBe(user);
  });

  it('goes back to the first candidate once the query grows', () => {
    const before = at('hello @');
    const after = at('hello @r');
    expect(
      keepMentionSelection(
        { mention: before, candidates: offered(ROSTER, before), active: 1 },
        { mention: after, candidates: offered(ROSTER, after) }
      )
    ).toBe(0);
  });

  it('goes back to the first candidate when the caret moves to another @', () => {
    const text = '@ and @';
    const first = at(text, 1);
    const second = at(text, text.length);
    expect(first.query).toBe(second.query);
    expect(
      keepMentionSelection(
        { mention: first, candidates: offered(ROSTER, first), active: 2 },
        { mention: second, candidates: offered(ROSTER, second) }
      )
    ).toBe(0);
  });

  it('goes back to the first candidate when the mention turns into the quoted form', () => {
    const bare = at('hello @');
    const quoted: MentionQuery = { ...bare, quoted: true };
    expect(
      keepMentionSelection(
        { mention: bare, candidates: offered(ROSTER, bare), active: 2 },
        { mention: quoted, candidates: offered(ROSTER, quoted) }
      )
    ).toBe(0);
  });

  it('starts at the first candidate when no mention was open before', () => {
    const mention = at('hello @');
    const candidates = offered(ROSTER, mention);
    expect(
      keepMentionSelection({ mention: null, candidates: [], active: 2 }, { mention, candidates })
    ).toBe(0);
  });

  it('follows the chosen participant to its new position when a participant joins ahead of it', () => {
    const mention = at('hello @');
    const before = offered(ROSTER, mention);
    expect(before[1]?.windowId).toBe('win2');
    const joined: ChatParticipant[] = [{ window_id: 'win9', name: 'newcomer' }, ...ROSTER];
    const after = offered(joined, mention);
    expect(
      keepMentionSelection(
        { mention, candidates: before, active: 1 },
        { mention, candidates: after }
      )
    ).toBe(2);
  });

  it('follows the chosen participant by window id, not by a name another window now carries', () => {
    const mention = at('hello @');
    const before = offered(ROSTER, mention);
    const renamed: ChatParticipant[] = [
      { window_id: 'win7', name: 'reviewer' },
      { window_id: 'win1', name: 'builder' },
      { window_id: 'win2', name: 'reviewer-renamed' },
    ];
    const after = offered(renamed, mention);
    expect(
      keepMentionSelection(
        { mention, candidates: before, active: 1 },
        { mention, candidates: after }
      )
    ).toBe(2);
  });

  it('follows the reserved human entry by its name when the list grows', () => {
    const mention = at('hello @');
    const before = offered(ROSTER, mention);
    const joined: ChatParticipant[] = [...ROSTER, { window_id: 'win9', name: 'newcomer' }];
    const after = offered(joined, mention);
    expect(
      keepMentionSelection(
        { mention, candidates: before, active: before.length - 1 },
        { mention, candidates: after }
      )
    ).toBe(after.length - 1);
  });

  it('goes back to the first candidate when the chosen participant left', () => {
    const mention = at('hello @');
    const before = offered(ROSTER, mention);
    expect(before[2]?.windowId).toBe('win4');
    const left = ROSTER.filter((participant) => participant.window_id !== 'win4');
    expect(
      keepMentionSelection(
        { mention, candidates: before, active: 2 },
        { mention, candidates: offered(left, mention) }
      )
    ).toBe(0);
  });
});
