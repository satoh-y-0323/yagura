import { describe, it, expect, afterEach, vi } from 'vitest';

import { splitForMiddleEllipsis } from './middle-ellipsis';

// Written as escapes so this file stays free of Japanese text.
const KA_WITH_DAKUTEN = '\u304b\u3099'; // one grapheme, two code points
const KANJI = ['\u65e5', '\u672c', '\u8a9e', '\u90e8', '\u5c4b', '\u540d', '\u524d', '\u9577'];
const FAMILY = '\u{1F468}\u200d\u{1F469}\u200d\u{1F467}'; // one grapheme, five code points
const THUMBS_UP_DARK = '\u{1F44D}\u{1F3FF}'; // one grapheme, two code points
const ASTRAL = '\u{20BB7}'; // one code point, two UTF-16 units

function kanjiRun(count: number): string {
  return Array.from({ length: count }, (_, i) => KANJI[i % KANJI.length]).join('');
}

function expectPartsRebuildName(name: string): void {
  const { head, tail } = splitForMiddleEllipsis(name);
  expect(head + tail).toBe(name);
}

describe('splitForMiddleEllipsis - names of 16 graphemes or fewer are not split', () => {
  it.each([[''], ['a'], ['general'], ['a'.repeat(16)], [kanjiRun(16)]])(
    '%j stays whole',
    (name) => {
      expect(splitForMiddleEllipsis(name)).toEqual({ head: name, tail: '' });
    }
  );
});

describe('splitForMiddleEllipsis - names of 17 graphemes or more keep the last 8 as the tail', () => {
  it('17 ASCII characters split into 9 and 8', () => {
    const name = 'abcdefghijklmnopq';
    expect(splitForMiddleEllipsis(name)).toEqual({ head: 'abcdefghi', tail: 'jklmnopq' });
  });

  it('a long name keeps exactly the last 8 characters as the tail', () => {
    const name = 'review-backend-api-2026-09-30-final';
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: name.slice(0, name.length - 8),
      tail: name.slice(name.length - 8),
    });
  });

  it('17 kanji split into 9 and 8 kanji', () => {
    const name = kanjiRun(17);
    const chars = Array.from(name);
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: chars.slice(0, 9).join(''),
      tail: chars.slice(9).join(''),
    });
  });

  it.each([['abcdefghijklmnopq'], [kanjiRun(40)], ['x'.repeat(100)]])(
    'head + tail rebuilds %j',
    (name) => {
      expectPartsRebuildName(name);
    }
  );
});

describe('splitForMiddleEllipsis - counts graphemes, so combined characters are never split', () => {
  it('Intl.Segmenter is available in this environment (precondition for this block)', () => {
    expect(Reflect.has(Intl, 'Segmenter')).toBe(true);
  });

  it('16 kana with a combining dakuten (32 code points) stay whole', () => {
    const name = KA_WITH_DAKUTEN.repeat(16);
    expect(splitForMiddleEllipsis(name)).toEqual({ head: name, tail: '' });
  });

  it('17 kana with a combining dakuten split into 9 and 8 whole kana', () => {
    const name = KA_WITH_DAKUTEN.repeat(17);
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: KA_WITH_DAKUTEN.repeat(9),
      tail: KA_WITH_DAKUTEN.repeat(8),
    });
  });

  it('12 letters and a ZWJ family (17 code points, 13 graphemes) stay whole', () => {
    const name = 'a'.repeat(12) + FAMILY;
    expect(splitForMiddleEllipsis(name)).toEqual({ head: name, tail: '' });
  });

  it('a ZWJ family on the tail boundary moves into the tail whole', () => {
    // 10 letters, the family, then 7 letters: 18 graphemes, the tail starts at the family.
    const name = 'a'.repeat(10) + FAMILY + 'b'.repeat(7);
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: 'a'.repeat(10),
      tail: FAMILY + 'b'.repeat(7),
    });
  });

  it('a ZWJ family just before the tail stays whole in the head', () => {
    // 9 letters, the family, then 8 letters: 18 graphemes, the family is the last of the head.
    const name = 'a'.repeat(9) + FAMILY + 'b'.repeat(8);
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: 'a'.repeat(9) + FAMILY,
      tail: 'b'.repeat(8),
    });
  });

  it('17 skin-toned emoji split into 9 and 8 whole emoji', () => {
    const name = THUMBS_UP_DARK.repeat(17);
    expect(splitForMiddleEllipsis(name)).toEqual({
      head: THUMBS_UP_DARK.repeat(9),
      tail: THUMBS_UP_DARK.repeat(8),
    });
  });
});

describe('splitForMiddleEllipsis - falls back to code points when Intl.Segmenter is missing', () => {
  const original = Object.getOwnPropertyDescriptor(Intl, 'Segmenter');

  afterEach(() => {
    if (original) {
      Object.defineProperty(Intl, 'Segmenter', original);
    }
    vi.resetModules();
  });

  // Loaded after removing Segmenter so the result does not depend on whether
  // the module looks for it on import or on each call.
  async function loadWithoutSegmenter(): Promise<typeof splitForMiddleEllipsis> {
    Object.defineProperty(Intl, 'Segmenter', {
      value: undefined,
      configurable: true,
      writable: true,
    });
    vi.resetModules();
    const mod = await import('./middle-ellipsis');
    return mod.splitForMiddleEllipsis;
  }

  it('does not throw and still splits plain names the same way', async () => {
    const split = await loadWithoutSegmenter();
    expect(split('abcdefghijklmnopq')).toEqual({ head: 'abcdefghi', tail: 'jklmnopq' });
    expect(split('a'.repeat(16))).toEqual({ head: 'a'.repeat(16), tail: '' });
    expect(split(kanjiRun(17))).toEqual({
      head: kanjiRun(17).slice(0, 9),
      tail: kanjiRun(17).slice(9),
    });
  });

  it('counts code points: 12 letters and a ZWJ family (17 code points) are split', async () => {
    const split = await loadWithoutSegmenter();
    const name = 'a'.repeat(12) + FAMILY;
    const points = Array.from(name);
    expect(split(name)).toEqual({
      head: points.slice(0, 9).join(''),
      tail: points.slice(9).join(''),
    });
  });

  it('never splits a surrogate pair: 17 astral characters split into 9 and 8 whole characters', async () => {
    const split = await loadWithoutSegmenter();
    const name = ASTRAL.repeat(17);
    expect(split(name)).toEqual({ head: ASTRAL.repeat(9), tail: ASTRAL.repeat(8) });
  });

  it('16 astral characters (32 UTF-16 units) stay whole', async () => {
    const split = await loadWithoutSegmenter();
    const name = ASTRAL.repeat(16);
    expect(split(name)).toEqual({ head: name, tail: '' });
  });
});
