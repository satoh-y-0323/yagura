import { messagesEn } from './messages-en';

/**
 * The shape of a locale, derived from the English resource: a second
 * locale that is missing a key is a `Messages` type error, not a runtime
 * fallback.
 */
export type Messages = typeof messagesEn;

/**
 * Locale selection is a policy, kept in this one place. English only for now;
 * a future locale plugs in here.
 */
export function resolveMessages(): Messages {
  return messagesEn;
}
