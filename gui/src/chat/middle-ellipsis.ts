import { createGraphemeSegmenter } from '../display/graphemes';
import { sliceDisplayText, type DisplayBlock, type DisplayLine } from '../display/untrusted-text';

/** Names up to this many graphemes are shown whole. */
const SPLIT_THRESHOLD = 16;

/** Graphemes kept visible at the end of a long name. */
const TAIL_LENGTH = 8;

// Older WebKit lacks Intl.Segmenter; code points still keep surrogate pairs whole there.
const segmenter = createGraphemeSegmenter();

function segmentGraphemes(text: string): string[] {
  if (segmenter === null) {
    return Array.from(text);
  }
  return Array.from(segmenter.segment(text), (part) => part.segment);
}

// The form `formatPathForDisplay` (`untrusted-text.ts`) writes a hidden character out in.
const WRITTEN_OUT_CHAR = /(\\u\{[0-9A-F]{4,6}\})/;

// A written-out character stands for one character, so it is one grapheme: cut
// inside, its tail would read as other text.
function graphemes(text: string): string[] {
  return text
    .split(WRITTEN_OUT_CHAR)
    .flatMap((part, index) => (index % 2 === 1 ? [part] : segmentGraphemes(part)));
}

/** Where the visible tail of `name` starts, as a UTF-16 index; its length when it is shown whole. */
function tailStart(name: string): number {
  const parts = graphemes(name);
  if (parts.length <= SPLIT_THRESHOLD) {
    return name.length;
  }
  return parts.slice(0, parts.length - TAIL_LENGTH).join('').length;
}

/**
 * Splits a name prepared for display for an ellipsis in the middle: `head`
 * shrinks behind the ellipsis while `tail` stays visible. Names of more than
 * 16 graphemes keep their last 8 graphemes as `tail`; shorter names come back
 * whole in `head`. `head + tail` always equals `name`, both parts keep the
 * kind of `name`, and neither a grapheme nor a written-out character
 * (`\u{200B}`) is ever split; the latter counts as one.
 */
export function splitPreparedForMiddleEllipsis<T extends DisplayLine | DisplayBlock>(
  name: T
): { head: T; tail: T } {
  const cut = tailStart(name);
  return { head: sliceDisplayText(name, 0, cut), tail: sliceDisplayText(name, cut) };
}
