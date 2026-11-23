/**
 * The notice a sender sees when some of the names they mentioned reached no
 * one in the room: the daemon's `unresolved` list, each name written the way
 * the mention completion inserts it.
 */
import { sanitizeForDisplay, type DisplayLine } from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import { chat } from '../ui-messages';
import { formatMention } from './mention-input';

/**
 * Describes the mentions that reached no one, `null` when every one did.
 *
 * @param unresolved the names the daemon could not resolve, in its order
 */
export function describeUnresolved(unresolved: readonly DisplayLine[]): UiText | null {
  return unresolved.length === 0
    ? null
    : chat.unresolvedMentions(unresolved.map((name) => sanitizeForDisplay(formatMention(name))));
}
