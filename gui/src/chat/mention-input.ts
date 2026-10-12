/**
 * The chat input's `@` completion: locating the mention being typed, offering
 * the room's participants plus the reserved human entry, and writing the
 * chosen name back into the text.
 *
 * Pure, and matching the daemon's mention parser: names are compared with
 * case and `_` / `-` folded, and a name carrying a space is inserted quoted so
 * it stays one target.
 */

import type { ChatParticipant } from '../ipc/session-boundary';

/** The reserved mention that stands for the human user. */
export const MENTION_USER_NAME = 'user';

/** One entry the completion offers. */
export interface MentionCandidate {
  /** The name that would be inserted. */
  name: string;
  /** The participant's window id, `null` for the reserved entry. */
  windowId: string | null;
}

/** The mention being typed, as located in the text. */
export interface MentionQuery {
  /** Index of the `@` that opens the mention. */
  start: number;
  /** The caret position the mention was located from. */
  end: number;
  /** What has been typed between the opening form and the caret. */
  query: string;
  /** Whether the mention was opened in the quoted form. */
  quoted: boolean;
}

/** Folds case and treats `_` and `-` as the same character. */
function normalize(name: string): string {
  return name.toLowerCase().replace(/_/g, '-');
}

/**
 * Locates the mention the caret sits in, `null` when there is none.
 *
 * The `@` has to open a word, so an address in the middle of one is left
 * alone. An unterminated quote keeps reading across spaces; a closed one is a
 * finished mention and offers nothing.
 *
 * @param text the whole input text
 * @param caret the caret position within it
 */
export function findMentionQuery(text: string, caret: number): MentionQuery | null {
  if (caret < 0 || caret > text.length) {
    return null;
  }
  const typed = text.slice(0, caret);
  let start = -1;
  for (let index = typed.length - 1; index >= 0; index--) {
    if (typed.charAt(index) !== '@') {
      continue;
    }
    if (index === 0 || /\s/.test(typed.charAt(index - 1))) {
      start = index;
      break;
    }
  }
  if (start === -1) {
    return null;
  }
  const quoted = typed.charAt(start + 1) === '"';
  const query = typed.slice(start + (quoted ? 2 : 1));
  if (quoted ? query.includes('"') : /\s/.test(query)) {
    return null;
  }
  return { start, end: caret, query, quoted };
}

/**
 * The candidates whose name starts with `query`, in roster order with the
 * reserved human entry last.
 *
 * @param roster the room's participants
 * @param query what has been typed after the `@`
 */
export function filterMentionCandidates(
  roster: ChatParticipant[],
  query: string
): MentionCandidate[] {
  const candidates: MentionCandidate[] = [
    ...roster.map((participant) => ({
      name: participant.name,
      windowId: participant.window_id,
    })),
    { name: MENTION_USER_NAME, windowId: null },
  ];
  const prefix = normalize(query);
  return candidates.filter((candidate) => normalize(candidate.name).startsWith(prefix));
}

/**
 * Writes `name` into `text` in place of the mention being typed, followed by
 * one space so the next word starts cleanly.
 *
 * @param text the whole input text
 * @param mention the mention located in it
 * @param name the chosen candidate's name
 * @returns the new text and where the caret goes
 */
export function insertMention(
  text: string,
  mention: MentionQuery,
  name: string
): { text: string; caret: number } {
  const inserted = /\s/.test(name) ? `@"${name}" ` : `@${name} `;
  return {
    text: text.slice(0, mention.start) + inserted + text.slice(mention.end),
    caret: mention.start + inserted.length,
  };
}

/** A mention the user closed with Escape: the text it was closed in and its `@`. */
export interface DismissedMention {
  readonly text: string;
  readonly start: number;
}

/** Records that `mention`, located in `text`, was closed with Escape. */
export function dismissMention(text: string, mention: MentionQuery): DismissedMention {
  return { text, start: mention.start };
}

/**
 * The mention to offer at the caret, keeping a dismissed one closed. The
 * dismissal ends once the text changes or the caret leaves that mention, so a
 * later edit or a return to it offers the candidates again.
 *
 * @param dismissed the mention last closed with Escape, `null` for none
 * @returns the mention to offer and the dismissal still in force
 */
export function openMention(
  text: string,
  caret: number,
  dismissed: DismissedMention | null
): { mention: MentionQuery | null; dismissed: DismissedMention | null } {
  const mention = findMentionQuery(text, caret);
  if (dismissed !== null && dismissed.text === text && mention?.start === dismissed.start) {
    return { mention: null, dismissed };
  }
  return { mention, dismissed: null };
}
