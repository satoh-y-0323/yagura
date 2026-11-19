import { createGraphemeSegmenter } from '../display/graphemes';

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

/**
 * Splits a name for an ellipsis in the middle: `head` shrinks behind the
 * ellipsis while `tail` stays visible. Names of more than 16 graphemes keep
 * their last 8 graphemes as `tail`; shorter names come back whole in `head`.
 * `head + tail` always equals `name`, and neither a grapheme nor a
 * written-out character (`\u{200B}`) is ever split; the latter counts as one.
 */
export function splitForMiddleEllipsis(name: string): { head: string; tail: string } {
  const parts = graphemes(name);
  if (parts.length <= SPLIT_THRESHOLD) {
    return { head: name, tail: '' };
  }
  const cut = parts.length - TAIL_LENGTH;
  return { head: parts.slice(0, cut).join(''), tail: parts.slice(cut).join('') };
}
