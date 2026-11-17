/**
 * Display preparation for untrusted text: text the daemon or a child process
 * wrote and a person reads. Display-only parts import from here, never from
 * `ipc/`, so every drawn string passes through these functions.
 */
/** Longest display field kept before truncation (code points, not bytes). */
export const MAX_DISPLAY_FIELD_CHARS = 4096;

/**
 * C0/C1 control characters and DEL, stripped entirely from display fields.
 * The Unicode `Cc` (Control) category covers all three ranges in one class,
 * avoiding a literal control-character range in the pattern (no-control-regex).
 * Narrower than `describeOperationFailure` (`render.ts`), and it strips where
 * that one folds a run to a single space.
 */
const DISPLAY_CONTROL_CHARS = /\p{Cc}/gu;

/**
 * Strips control characters and caps the result at
 * {@link MAX_DISPLAY_FIELD_CHARS} code points. Exported so every display of
 * daemon-supplied text passes through the same treatment.
 * @param value the text to display
 */
export function sanitizeForDisplay(value: string): string {
  const stripped = value.replace(DISPLAY_CONTROL_CHARS, '');
  const codePoints = Array.from(stripped);
  if (codePoints.length <= MAX_DISPLAY_FIELD_CHARS) {
    return stripped;
  }
  return `${codePoints.slice(0, MAX_DISPLAY_FIELD_CHARS).join('')}…`;
}

/**
 * Control characters and the bidirectional controls (U+061C, U+200E/F,
 * U+202A-E, U+2066-9) that reorder what is drawn.
 */
const MULTILINE_HIDDEN_CHARS = /[\p{Cc}\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu;

/**
 * Prepares untrusted text of several lines for display. Every control
 * character outside `keptControls`, and every bidirectional control, is
 * written out as `\u{202E}` rather than removed or drawn: a hidden character
 * could change what the text appears to say. Capped at `maxChars` code points
 * before escaping, so an escape is never cut in half.
 * @param value the text to display
 * @param maxChars the most code points kept before the text is cut
 * @param keptControls the control characters drawn as they are
 */
export function formatMultilineText(
  value: string,
  maxChars: number,
  keptControls: ReadonlySet<string>
): string {
  const codePoints = Array.from(value);
  const capped =
    codePoints.length <= maxChars ? value : `${codePoints.slice(0, maxChars).join('')}…`;
  return capped.replace(MULTILINE_HIDDEN_CHARS, (char) => {
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
 * (command, directory, purpose), so line feeds are kept; every other control
 * is written out, since an approver must see everything that will run. Capped
 * at {@link MAX_DISPLAY_FIELD_CHARS} code points.
 * @param value the body text to display
 */
export function formatApprovalBody(value: string): string {
  return formatMultilineText(value, MAX_DISPLAY_FIELD_CHARS, APPROVAL_BODY_KEPT_CONTROLS);
}
