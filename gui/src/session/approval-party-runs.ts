/**
 * How an approval party (`HeadlineParty` in `approval-types.ts`) reads: the
 * runs a heading, a card and a sentence draw it with.
 */
import type { UiText } from '../display/ui-text';
import type { DisplayLine } from '../display/untrusted-text';
import type { HeadlineParty } from './approval-types';

/** One run of a party's text; `chip` sets a name apart from the words around it. */
export interface PartyRun {
  text: UiText | DisplayLine;
  chip: boolean;
}

/**
 * A party as the runs it reads in. A room's name is always a chip, as words
 * always surround it; a window's name is one when `windowName` is `chip`.
 * @param party the party to read
 * @param windowName `chip` where words surround a window's name, `plain` where it stands alone
 */
export function partyRuns(
  party: HeadlineParty,
  windowName: 'chip' | 'plain' = 'chip'
): readonly PartyRun[] {
  switch (party.kind) {
    case 'name':
      return [{ text: party.name, chip: windowName === 'chip' }];
    case 'words':
      return [{ text: party.words, chip: false }];
    case 'room':
      return [
        { text: party.parts.before, chip: false },
        { text: party.parts.name, chip: true },
        { text: party.parts.after, chip: false },
      ];
    default: {
      const unknown: never = party;
      return unknown;
    }
  }
}
