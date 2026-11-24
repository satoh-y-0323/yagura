/** The part of `Intl.Segmenter` used here; the ES2020 lib does not declare it. */
export interface GraphemeSegmenter {
  segment: (input: string) => Iterable<{ segment: string }>;
}

type GraphemeSegmenterConstructor = new () => GraphemeSegmenter;

function isSegmenterConstructor(value: unknown): value is GraphemeSegmenterConstructor {
  return typeof value === 'function';
}

/**
 * A segmenter that splits text into graphemes, or `null` where the engine has
 * no `Intl.Segmenter` (older WebKit). Each caller decides what to fall back to.
 */
export function createGraphemeSegmenter(): GraphemeSegmenter | null {
  const ctor: unknown = Reflect.get(Intl, 'Segmenter');
  return isSegmenterConstructor(ctor) ? new ctor() : null;
}
