/**
 * Display preparation for untrusted text: text the daemon or a child process
 * wrote and a person reads. Every drawn string has the type of its maker
 * (`DisplayLine` and `DisplayBlock` from here, `UiText` from the message
 * resources); the types, `MessagesTextContract` and the `yagura/display-text`
 * and `yagura/no-brand-forging` lint rules hold that, except in an input's `value`.
 */
import type { UiText } from './ui-text';

declare const displayLineBrand: unique symbol;
declare const displayBlockBrand: unique symbol;

/**
 * One line of outside text, prepared for display: it holds no line break, so
 * it fits any one-line place (a heading, an option, a tooltip, an
 * `aria-label`). Only the functions of this module make one.
 */
export type DisplayLine = string & { readonly [displayLineBrand]: true };

/**
 * Outside text of several lines, prepared for display. It keeps its line
 * breaks, so it belongs only where lines are drawn (`<pre>`, `pre-wrap`): put
 * in a one-line place, its lines would run together. Only the functions of
 * this module make one.
 */
export type DisplayBlock = string & { readonly [displayBlockBrand]: true };

/** Longest display field kept before truncation (code points, not bytes). */
export const MAX_DISPLAY_FIELD_CHARS = 4096;

/**
 * Longest text of several lines the daemon sends kept for display, in code
 * points: a ticket's body (`TICKET_BODY_MAX_BYTES`) and what an integration
 * step printed (`STEP_OUTPUT_MAX_BYTES`). A UTF-8 cap in bytes is never more
 * code points than this.
 */
export const MAX_BLOCK_TEXT_CHARS = 16 * 1024;

/** Longest externally supplied reason kept on one line, in code points. */
export const EXTERNAL_TEXT_MAX_CODE_POINTS = 200;

/**
 * The characters written out rather than drawn, taken from Unicode properties:
 * controls, format characters, line and paragraph separators, and every default
 * ignorable code point. They break a line, reorder what is drawn, or show
 * nothing, so removing them would make one value look like another.
 */
const WRITTEN_OUT_CHARS = /[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}]/gu;

/**
 * The characters written out in a value that points at a place: those of
 * {@link WRITTEN_OUT_CHARS}, every space but U+0020, and U+2800, which all look
 * like a plain space and would make one place look like another.
 */
const PLACE_WRITTEN_OUT_CHARS =
  /(?! )[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}\p{Zs}\u2800]/gu;

/** The characters one line shows as a space each. */
const LINE_SPACES = /[\p{Cc}\p{Zl}\p{Zp}]/gu;

/** The form {@link writtenOut} gives a character. */
const WRITTEN_OUT_FORM = /\\u\{[0-9A-F]{4,6}\}/g;

const NO_CONTROLS: ReadonlySet<string> = new Set();
const APPROVAL_BODY_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n']);
// The controls the daemon lets through: `validate_chat_body` for chat, the ticket
// rules and `sanitize_output` for tickets and integration output.
const CHAT_BODY_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n', '\r']);
const TICKET_TEXT_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n', '\t']);
const OUTPUT_KEPT_CONTROLS: ReadonlySet<string> = new Set(['\n', '\t']);

const VARIATION_SELECTOR_TEXT = 0xfe0e;
const VARIATION_SELECTOR_EMOJI = 0xfe0f;
const ZERO_WIDTH_JOINER = 0x200d;
const COMBINING_KEYCAP = 0x20e3;
const PICTOGRAPH = /^\p{Extended_Pictographic}$/u;
const EMOJI_MODIFIER = /^\p{Emoji_Modifier}$/u;
const KEYCAP_BASE = /^[0-9#*]$/;
const BLACK_FLAG = 0x1f3f4;
const TAG_FIRST = 0xe0020;
const TAG_LAST_SPEC = 0xe007e;
const CANCEL_TAG = 0xe007f;
// A bound on the walk, not a rule: every RGI tag spec is far shorter.
const MAX_TAG_SPEC_CHARS = 32;
const RGI_EMOJI_TAG_SEQUENCE = rgiEmojiTagSequence();

// The `v` flag is past the ES2020 target, so the pattern is built at run time.
// Where a WebView cannot build it, tag characters stay written out.
function rgiEmojiTagSequence(): RegExp | null {
  try {
    return new RegExp('^\\p{RGI_Emoji_Tag_Sequence}$', 'v');
  } catch {
    return null;
  }
}

function capCodePoints(value: string, maxChars: number): string {
  const codePoints = Array.from(value);
  return codePoints.length <= maxChars ? value : `${codePoints.slice(0, maxChars).join('')}…`;
}

function writtenOut(char: string): string {
  const hex = (char.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
  return `\\u{${hex}}`;
}

function isSurrogatePair(high: number, low: number): boolean {
  return high >= 0xd800 && high <= 0xdbff && low >= 0xdc00 && low <= 0xdfff;
}

function codePointBefore(text: string, offset: number): string {
  const pair = isSurrogatePair(text.charCodeAt(offset - 2), text.charCodeAt(offset - 1));
  return text.slice(pair ? offset - 2 : Math.max(offset - 1, 0), offset);
}

function codePointAfter(text: string, offset: number): string {
  const codePoint = text.codePointAt(offset);
  return codePoint === undefined ? '' : String.fromCodePoint(codePoint);
}

function isTagSpec(codePoint: number | undefined): boolean {
  return codePoint !== undefined && codePoint >= TAG_FIRST && codePoint <= TAG_LAST_SPEC;
}

/**
 * True where the tag character at `offset` belongs to an RGI emoji tag
 * sequence (a flag such as England's). Any other run of tags draws as its base
 * alone while carrying text no reader sees.
 */
function inRgiTagSequence(offset: number, text: string): boolean {
  if (RGI_EMOJI_TAG_SEQUENCE === null) {
    return false;
  }
  let start = offset;
  for (let walked = 0; isTagSpec(codePointBefore(text, start).codePointAt(0)); walked += 1) {
    if (walked === MAX_TAG_SPEC_CHARS) {
      return false;
    }
    start -= 2;
  }
  const base = codePointBefore(text, start);
  if (base.codePointAt(0) !== BLACK_FLAG) {
    return false;
  }
  let end = start;
  for (let walked = 0; isTagSpec(text.codePointAt(end)); walked += 1) {
    if (walked === MAX_TAG_SPEC_CHARS) {
      return false;
    }
    end += 2;
  }
  if (text.codePointAt(end) !== CANCEL_TAG) {
    return false;
  }
  return RGI_EMOJI_TAG_SEQUENCE.test(text.slice(start - base.length, end + 2));
}

/**
 * True where `char`, at `offset` in `text`, gives an emoji its shape: a
 * presentation selector right after a pictograph, the U+FE0F of a keycap, a
 * ZWJ between two pictographs, or a tag of an RGI emoji tag sequence.
 * Anywhere else such a character shows nothing, so drawing it would let a
 * writer carry text no reader sees.
 */
function shapesEmoji(char: string, offset: number, text: string): boolean {
  const codePoint = char.codePointAt(0);
  if (codePoint !== undefined && codePoint >= TAG_FIRST && codePoint <= CANCEL_TAG) {
    return inRgiTagSequence(offset, text);
  }
  const before = codePointBefore(text, offset);
  const after = codePointAfter(text, offset + char.length);
  if (codePoint === VARIATION_SELECTOR_TEXT || codePoint === VARIATION_SELECTOR_EMOJI) {
    if (PICTOGRAPH.test(before)) {
      return true;
    }
    return (
      codePoint === VARIATION_SELECTOR_EMOJI &&
      KEYCAP_BASE.test(before) &&
      after.codePointAt(0) === COMBINING_KEYCAP
    );
  }
  if (codePoint === ZERO_WIDTH_JOINER) {
    const modified =
      before.codePointAt(0) === VARIATION_SELECTOR_EMOJI || EMOJI_MODIFIER.test(before);
    const base = modified ? codePointBefore(text, offset - before.length) : before;
    return PICTOGRAPH.test(base) && PICTOGRAPH.test(after);
  }
  return false;
}

function shapesNothing(): boolean {
  return false;
}

/**
 * Caps `value` at `maxChars` code points, then writes out every character of
 * `writtenOutChars` outside `keptControls`, and outside what `drawn` keeps.
 * Capping first keeps all of what the daemon accepted and never cuts an escape
 * in half. Private, so no caller can pick which characters stay unseen.
 */
function formatMultilineText(
  value: string,
  maxChars: number,
  keptControls: ReadonlySet<string>,
  drawn: (char: string, offset: number, text: string) => boolean = shapesNothing,
  writtenOutChars: RegExp = WRITTEN_OUT_CHARS
): string {
  return capCodePoints(value, maxChars).replace(
    writtenOutChars,
    (char: string, offset: number, text: string) =>
      keptControls.has(char) || drawn(char, offset, text) ? char : writtenOut(char)
  );
}

/**
 * One line to read: each control or separator becomes one space, every other
 * written-out character but an emoji shape is written out, and only then is
 * the line capped at `maxChars` code points, so a line that grows by writing
 * out still fits its place. A cut moves before a written-out character or a
 * code point it would split, and is marked with `…` after the cap.
 */
function prepareLine(value: string, maxChars: number): DisplayLine {
  const written = value
    .replace(LINE_SPACES, ' ')
    .replace(WRITTEN_OUT_CHARS, (char: string, offset: number, text: string) =>
      shapesEmoji(char, offset, text) ? char : writtenOut(char)
    );
  const codePoints = Array.from(written);
  if (codePoints.length <= maxChars) {
    return written as DisplayLine;
  }
  const kept = codePoints.slice(0, maxChars).join('');
  return `${written.slice(0, unsplitIndex(written, kept.length))}…` as DisplayLine;
}

/**
 * Prepares one line of a label or of prose for display: each control or
 * separator becomes one space, every invisible character but an emoji shape is
 * written out (`\u{200C}`), and the result is capped at
 * {@link MAX_DISPLAY_FIELD_CHARS} code points after writing out. Never use it
 * for a value that points at a place (a path, a room name taken from a path):
 * use {@link formatPathForDisplay}.
 * @param value the text to display
 */
export function sanitizeForDisplay(value: string): DisplayLine {
  return prepareLine(value, MAX_DISPLAY_FIELD_CHARS);
}

/**
 * Prepares an approval request's body for display. The body is several lines
 * (command, directory, purpose), so line feeds are kept; every other hidden
 * character is written out, since an approver must see everything that will
 * run. Capped at {@link MAX_DISPLAY_FIELD_CHARS} code points.
 * @param value the body text to display
 */
export function formatApprovalBody(value: string): DisplayBlock {
  return formatMultilineText(
    value,
    MAX_DISPLAY_FIELD_CHARS,
    APPROVAL_BODY_KEPT_CONTROLS
  ) as DisplayBlock;
}

/**
 * Prepares one line that points at a place (a path, or a room name taken
 * from a path) for display. The daemon cannot refuse hidden characters in
 * such a value, and removing them would make it look like another place while
 * drawing them could reorder it, so every one, line breaks and every space but
 * U+0020 included, is written out. Capped at {@link MAX_DISPLAY_FIELD_CHARS}
 * code points before escaping.
 * @param value the path or path-derived name to display
 */
export function formatPathForDisplay(value: string): DisplayLine {
  return formatPlace(value, MAX_DISPLAY_FIELD_CHARS) as DisplayLine;
}

function formatPlace(value: string, maxChars: number): string {
  return formatMultilineText(value, maxChars, NO_CONTROLS, shapesNothing, PLACE_WRITTEN_OUT_CHARS);
}

/**
 * Prepares a chat message's body, written for people to read. Line feeds and
 * carriage returns stay, an emoji keeps its shape, and every other hidden or
 * default ignorable character is written out. Capped at
 * {@link MAX_DISPLAY_FIELD_CHARS} code points before escaping.
 * @param value the body to display
 */
export function formatChatBody(value: string): DisplayBlock {
  return formatMultilineText(
    value,
    MAX_DISPLAY_FIELD_CHARS,
    CHAT_BODY_KEPT_CONTROLS,
    shapesEmoji
  ) as DisplayBlock;
}

/**
 * Prepares a ticket's body, a result, or other ticket text of several lines,
 * by the chat body's rule except that tabs stay instead of carriage returns.
 * Capped at {@link MAX_BLOCK_TEXT_CHARS} code points before escaping.
 * @param value the text to display
 */
export function formatTicketText(value: string): DisplayBlock {
  return formatMultilineText(
    value,
    MAX_BLOCK_TEXT_CHARS,
    TICKET_TEXT_KEPT_CONTROLS,
    shapesEmoji
  ) as DisplayBlock;
}

/**
 * Prepares what an integration step printed, or its error, by the chat
 * body's rule except that tabs stay instead of carriage returns. Capped at
 * {@link MAX_BLOCK_TEXT_CHARS} code points before escaping.
 * @param value the output to display
 */
export function formatOutputBlock(value: string): DisplayBlock {
  return formatMultilineText(
    value,
    MAX_BLOCK_TEXT_CHARS,
    OUTPUT_KEPT_CONTROLS,
    shapesEmoji
  ) as DisplayBlock;
}

function formatArgument(argument: string): string {
  const written = formatPlace(argument, Infinity).replace(/"/g, '\\u{0022}');
  // A space drawn as a space would hide where the element ends; an empty one would vanish.
  return written === '' || /\s/u.test(written) ? `"${written}"` : written;
}

/**
 * Prepares a command line (argv) for display on one line. Each element is
 * written out like a path, a `"` inside it included, and quoted when it is
 * empty or still holds a space. The elements and the spaces between them are
 * capped together at {@link MAX_DISPLAY_FIELD_CHARS} code points before
 * escaping: a cut element is quoted by what is left of it, and an element of
 * which nothing is left is dropped with its separator.
 * @param argv the program and its arguments
 */
export function formatCommandLine(argv: readonly string[]): DisplayLine {
  const shown: string[] = [];
  let budget = MAX_DISPLAY_FIELD_CHARS;
  let cut = false;
  for (const [index, argument] of argv.entries()) {
    if (index > 0) {
      if (budget === 0) {
        cut = true;
        break;
      }
      budget -= 1;
    }
    const codePoints = Array.from(argument);
    if (codePoints.length > budget) {
      const left = codePoints.slice(0, budget).join('');
      if (left !== '') {
        shown.push(formatArgument(left));
      }
      cut = true;
      break;
    }
    budget -= codePoints.length;
    shown.push(formatArgument(argument));
  }
  return `${shown.join(' ')}${cut ? '…' : ''}` as DisplayLine;
}

/**
 * Reduces a failure's reason to one display line: runs of line breaks,
 * control characters and whitespace are folded into single spaces, the ends
 * are trimmed, the rest is written out as {@link sanitizeForDisplay} writes
 * it, and the result is capped at {@link EXTERNAL_TEXT_MAX_CODE_POINTS} code
 * points after writing out, so that one message cannot take over the notice
 * area. It changes how a name looks, so names go through
 * {@link sanitizeForDisplay}.
 * @param text the external text, as it arrived
 */
export function foldExternalText(text: string): DisplayLine {
  const folded = text.replace(/[\p{Cc}\p{Zl}\p{Zp}\s]+/gu, ' ').trim();
  return prepareLine(folded, EXTERNAL_TEXT_MAX_CODE_POINTS);
}

/** `index`, moved back to the start of a written-out character or a code point it falls inside. */
function unsplitIndex(text: string, index: number): number {
  for (const match of text.matchAll(WRITTEN_OUT_FORM)) {
    if (match.index < index && index < match.index + match[0].length) {
      return match.index;
    }
  }
  return isSurrogatePair(text.charCodeAt(index - 1), text.charCodeAt(index)) ? index - 1 : index;
}

function sliceIndex(length: number, index: number): number {
  return index < 0 ? Math.max(length + index, 0) : Math.min(index, length);
}

function slicePrepared(text: string, start: number, end: number | undefined): string {
  const from = unsplitIndex(text, sliceIndex(text.length, start));
  const to = unsplitIndex(text, sliceIndex(text.length, end ?? text.length));
  return text.slice(from, to);
}

/**
 * Part of prepared text, taken as `String.prototype.slice` takes it (UTF-16
 * indices, a negative one counted from the end), with each end moved back to
 * the start of a written-out character (`\u{202E}`) or a code point it would
 * cut. A part keeps the kind of its whole, since slicing adds no character.
 * @param text the prepared text
 * @param start where the part begins
 * @param end where the part ends; the end of `text` when left out
 */
export function sliceDisplayText<T extends DisplayLine | DisplayBlock>(
  text: T,
  start: number,
  end?: number
): T {
  return slicePrepared(text, start, end) as T;
}

/**
 * Part of the UI's own text, cut as {@link sliceDisplayText} cuts, as one
 * display line. Only the message resources make `UiText`, so the part is
 * prepared here as {@link sanitizeForDisplay} prepares a line.
 * @param text the UI text
 * @param start where the part begins
 * @param end where the part ends; the end of `text` when left out
 */
export function sliceUiText(text: UiText, start: number, end?: number): DisplayLine {
  return sanitizeForDisplay(slicePrepared(text, start, end));
}

/**
 * Lines of prepared text and of the UI's own text, joined by one line feed
 * each into one block. Nothing is cut: each part already holds its own cap,
 * and cutting the whole would drop the last parts first.
 * @param parts the lines, in order
 */
export function joinDisplayBlocks(
  parts: readonly (UiText | DisplayLine | DisplayBlock)[]
): DisplayBlock {
  return parts.join('\n') as DisplayBlock;
}

/**
 * The text a caught value is told by, before any display treatment. A string
 * is its own text and an `Error` is its message (a `CommandFailureError`'s
 * message is its failure's text). Any other object is encoded, since `String`
 * would flatten every one of them to the same word, and is empty when it
 * cannot be encoded. `undefined` and a function carry nothing to show (a
 * function's `String` is its source), so they are empty; anything else is
 * `String` of it.
 * @param value the value an operation threw or rejected with
 */
export function caughtValueText(value: unknown): string {
  switch (typeof value) {
    case 'string':
      return value;
    case 'undefined':
    case 'function':
      return '';
    case 'object':
      return value === null ? 'null' : objectText(value);
    default:
      return String(value);
  }
}

function objectText(value: object): string {
  if (value instanceof Error) {
    return value.message;
  }
  try {
    return JSON.stringify(value) ?? '';
  } catch {
    // A circular reference or a BigInt makes JSON.stringify throw. Losing the
    // text beats losing the failure.
    return '';
  }
}
