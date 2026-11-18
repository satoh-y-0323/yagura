/**
 * Display preparation for untrusted text: text the daemon or a child process
 * wrote and a person reads. Display-only parts import from here, never from
 * `ipc/`, so every drawn string passes through these functions.
 */
/** Longest display field kept before truncation (code points, not bytes). */
export const MAX_DISPLAY_FIELD_CHARS = 4096;

/**
 * The hidden characters: the Unicode `Cc` category (C0, DEL, C1), the rest of
 * the daemon's forbidden label characters (`is_forbidden_label_char` in
 * `yagura-protocol`) and U+061C. They break a line, reorder what is drawn, or
 * have no width. ZWNJ and ZWJ are not among them: labels in some scripts and
 * emoji sequences need them.
 */
const HIDDEN_CHARS =
  /[\p{Cc}\u061C\u200B\u200E\u200F\u2028\u2029\u2060\uFEFF\u202A-\u202E\u2066-\u2069]/gu;

/**
 * The characters written out rather than drawn: {@link HIDDEN_CHARS} plus every
 * default ignorable code point. Removing them is no defence where two values
 * must not look alike, since the removed form equals another value.
 */
const WRITTEN_OUT_CHARS =
  /[\p{Cc}\p{Default_Ignorable_Code_Point}\u061C\u200B\u200E\u200F\u2028\u2029\u2060\uFEFF\u202A-\u202E\u2066-\u2069]/gu;

function capCodePoints(value: string, maxChars: number): string {
  const codePoints = Array.from(value);
  return codePoints.length <= maxChars ? value : `${codePoints.slice(0, maxChars).join('')}…`;
}

/**
 * Prepares one line of a label or of prose for display: strips the hidden
 * characters and caps the result at {@link MAX_DISPLAY_FIELD_CHARS} code
 * points. Use it for labels the daemon refuses these characters in at every
 * entry point, and for prose that names nothing. Never use it for a value
 * that points at a place (a path, a room name taken from a path): use
 * {@link formatPathForDisplay}.
 * @param value the text to display
 */
export function sanitizeForDisplay(value: string): string {
  return capCodePoints(value.replace(HIDDEN_CHARS, ''), MAX_DISPLAY_FIELD_CHARS);
}

/**
 * Prepares untrusted text of several lines for display. Every hidden or
 * default ignorable character outside `keptControls` is written out as
 * `\u{202E}` rather than removed or drawn: a hidden character could change
 * what the text appears to say. Capped at `maxChars` code points before
 * escaping, so an escape is never cut in half.
 * @param value the text to display
 * @param maxChars the most code points kept before the text is cut
 * @param keptControls the control characters drawn as they are
 */
export function formatMultilineText(
  value: string,
  maxChars: number,
  keptControls: ReadonlySet<string>
): string {
  return capCodePoints(value, maxChars).replace(WRITTEN_OUT_CHARS, (char) => {
    if (keptControls.has(char)) {
      return char;
    }
    const hex = (char.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
    return `\\u{${hex}}`;
  });
}

const APPROVAL_BODY_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n']);

/**
 * Prepares an approval request's body for display. The body is several lines
 * (command, directory, purpose), so line feeds are kept; every other hidden
 * character is written out, since an approver must see everything that will
 * run. Capped at {@link MAX_DISPLAY_FIELD_CHARS} code points.
 * @param value the body text to display
 */
export function formatApprovalBody(value: string): string {
  return formatMultilineText(value, MAX_DISPLAY_FIELD_CHARS, APPROVAL_BODY_KEPT_CONTROLS);
}

const PATH_KEPT_CONTROLS: ReadonlySet<string> = new Set();

/**
 * Prepares one line that points at a place (a path, or a room name taken
 * from a path) for display. The daemon cannot refuse hidden characters in
 * such a value, and removing them would make it look like another place while
 * drawing them could reorder it, so every one, line breaks included, is
 * written out. Capped at {@link MAX_DISPLAY_FIELD_CHARS} code points before
 * escaping.
 * @param value the path or path-derived name to display
 */
export function formatPathForDisplay(value: string): string {
  return formatMultilineText(value, MAX_DISPLAY_FIELD_CHARS, PATH_KEPT_CONTROLS);
}
