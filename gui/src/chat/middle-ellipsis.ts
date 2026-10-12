/** Names up to this many graphemes are shown whole. */
const SPLIT_THRESHOLD = 16;

/** Graphemes kept visible at the end of a long name. */
const TAIL_LENGTH = 8;

/** The part of `Intl.Segmenter` used here; the ES2020 lib does not declare it. */
interface GraphemeSegmenter {
  segment(input: string): Iterable<{ segment: string }>;
}

type GraphemeSegmenterConstructor = new () => GraphemeSegmenter;

function isSegmenterConstructor(value: unknown): value is GraphemeSegmenterConstructor {
  return typeof value === 'function';
}

function createGraphemeSegmenter(): GraphemeSegmenter | null {
  const ctor: unknown = Reflect.get(Intl, 'Segmenter');
  return isSegmenterConstructor(ctor) ? new ctor() : null;
}

// Older WebKit lacks Intl.Segmenter; code points still keep surrogate pairs whole there.
const segmenter = createGraphemeSegmenter();

function graphemes(text: string): string[] {
  if (segmenter === null) {
    return Array.from(text);
  }
  return Array.from(segmenter.segment(text), (part) => part.segment);
}

/**
 * Splits a name for an ellipsis in the middle: `head` shrinks behind the
 * ellipsis while `tail` stays visible. Names of more than 16 graphemes keep
 * their last 8 graphemes as `tail`; shorter names come back whole in `head`.
 * `head + tail` always equals `name`, and a grapheme is never split.
 */
export function splitForMiddleEllipsis(name: string): { head: string; tail: string } {
  const parts = graphemes(name);
  if (parts.length <= SPLIT_THRESHOLD) {
    return { head: name, tail: '' };
  }
  const cut = parts.length - TAIL_LENGTH;
  return { head: parts.slice(0, cut).join(''), tail: parts.slice(cut).join('') };
}
