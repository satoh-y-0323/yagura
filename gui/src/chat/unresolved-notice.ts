/**
 * The notice a sender sees when some of the names they mentioned reached no
 * one in the room: the daemon's `unresolved` list, which the boundary already
 * wrote the way the mention completion inserts each name.
 */
import type { DisplayLine } from '../display/untrusted-text';
import type { UiText } from '../display/ui-text';
import { chat } from '../ui-messages';

/**
 * Describes the mentions that reached no one, `null` when every one did.
 *
 * @param unresolved the mentions that reached no one, in the daemon's order
 */
export function describeUnresolved(unresolved: readonly DisplayLine[]): UiText | null {
  return unresolved.length === 0 ? null : chat.unresolvedMentions(unresolved);
}
