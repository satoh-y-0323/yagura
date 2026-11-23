/**
 * How a chat mention is written so the daemon's mention parser reads the
 * whole name as one target. The chat input inserts this form, and the
 * boundary writes the names the daemon could not resolve back in it.
 */

// A bare mention ends where the daemon's word characters end. Quoting every
// name outside this ASCII subset costs nothing: a quoted mention always resolves.
const BARE_MENTION_NAME = /^[A-Za-z0-9_-]+$/;

/**
 * Writes `name` as a mention that addresses it whole: bare when the daemon's
 * parser reads it to its end, quoted otherwise.
 *
 * @param name the participant's name, as the daemon holds it
 */
export function formatMention(name: string): string {
  return BARE_MENTION_NAME.test(name) ? `@${name}` : `@"${name}"`;
}
