/**
 * The key names an `open_window` failure travels under are written twice: in
 * the core's serde attributes and in this layer's narrowing. Both sides build
 * their own fixtures, so a spelling that drifted would leave both green while
 * the real path lost the field. The names are therefore read out of the Rust
 * source here and used verbatim against the boundary, rather than restated.
 */
import { describe, it, expect } from 'vitest';

import commandsSource from '../../src-tauri/src/commands.rs?raw';
import { bracedBody, itemBody } from '../test-utils/markup-scan';
import { narrowWindowOpenFailure, type WindowOpenFailure } from './session-boundary';

/** The Rust item whose serde representation this file reads. */
const FAILURE_ENUM_HEADER = 'pub enum WindowOpenFailure {';

/** One tagged arm of the wire enum. */
interface WireVariant {
  /** The value the tag key carries for this arm. */
  tagValue: string;
  /** Keys always present on the wire. */
  required: string[];
  /** Keys the wire may omit or send as null. */
  optional: string[];
}

/** The serde representation of a tagged Rust enum. */
interface WireShape {
  /** The key carrying the arm's name. */
  tagKey: string;
  variants: WireVariant[];
}

/** The arguments of the `#[serde(...)]` attribute directly above `header`. */
function serdeAttributeArgs(source: string, header: string): string {
  const headerIndex = source.indexOf(header);
  if (headerIndex === -1) {
    throw new Error(`no Rust item found for header: ${header}`);
  }
  const match = /#\[serde\(([^\]]*)\)\]\s*$/.exec(source.slice(0, headerIndex));
  if (match === null) {
    throw new Error(`no serde attribute directly above: ${header}`);
  }
  return match[1] ?? '';
}

/** `UpperCamelCase` under serde's `snake_case` renaming. */
function snakeCase(name: string): string {
  return name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
}

/** The fields one struct variant declares, split by whether the wire may omit them. */
function variantFields(variantBody: string): { required: string[]; optional: string[] } {
  const required: string[] = [];
  const optional: string[] = [];
  const fieldLine = /^[ \t]*([a-z_][A-Za-z0-9_]*)\s*:\s*([^,]+),[ \t]*$/gm;
  let match: RegExpExecArray | null;
  while ((match = fieldLine.exec(variantBody)) !== null) {
    const name = match[1] ?? '';
    const type = match[2] ?? '';
    if (type.startsWith('Option<')) {
      optional.push(name);
    } else {
      required.push(name);
    }
  }
  return { required, optional };
}

/** The serde representation of the internally tagged enum `header` introduces. */
function wireShapeOf(source: string, header: string): WireShape {
  const args = serdeAttributeArgs(source, header);
  const tagKey = /\btag\s*=\s*"([^"]+)"/.exec(args)?.[1];
  const renameAll = /\brename_all\s*=\s*"([^"]+)"/.exec(args)?.[1];
  if (tagKey === undefined) {
    throw new Error(`the serde attribute above ${header} names no tag`);
  }
  if (renameAll !== 'snake_case') {
    throw new Error(`unsupported rename_all above ${header}: ${String(renameAll)}`);
  }
  const body = itemBody(source, header);
  const variants: WireVariant[] = [];
  const variantHeader = /^[ \t]*([A-Z][A-Za-z0-9_]*)\s*\{/gm;
  let match: RegExpExecArray | null;
  while ((match = variantHeader.exec(body)) !== null) {
    const name = match[1] ?? '';
    const openIndex = match.index + match[0].length - 1;
    const fields = variantFields(bracedBody(body, openIndex));
    variants.push({ tagValue: snakeCase(name), ...fields });
  }
  return { tagKey, variants };
}

/** The distinct value sent under `key`, so its landing place can be found. */
function sampleFor(key: string): string {
  return `value-sent-under-${key}`;
}

/** The string values a narrowed failure ended up carrying, under any name. */
function carriedStrings(failure: WindowOpenFailure): Set<string> {
  const record: Record<string, unknown> = { ...failure };
  return new Set(
    Object.values(record).filter((value): value is string => typeof value === 'string')
  );
}

/** A wire object built from `variant`, with every key it declares. */
function wireObject(shape: WireShape, variant: WireVariant): Record<string, unknown> {
  const object: Record<string, unknown> = { [shape.tagKey]: variant.tagValue };
  for (const key of [...variant.required, ...variant.optional]) {
    object[key] = sampleFor(key);
  }
  return object;
}

/** The same object with `key` renamed, which no side is supposed to read. */
function withKeyRenamed(object: Record<string, unknown>, key: string): Record<string, unknown> {
  const renamed: Record<string, unknown> = {};
  for (const [name, value] of Object.entries(object)) {
    renamed[name === key ? `${name}_renamed` : name] = value;
  }
  return renamed;
}

/**
 * Every way `shape`'s key names and the boundary's disagree: a key the
 * boundary does not read, an arm it does not recognize, or a key it reads
 * from a spelling `shape` never uses. Empty when the two agree.
 */
function disagreementsWithBoundary(shape: WireShape): string[] {
  const problems: string[] = [];
  for (const variant of shape.variants) {
    const whole = wireObject(shape, variant);
    let narrowed: WindowOpenFailure;
    try {
      narrowed = narrowWindowOpenFailure(whole);
    } catch (error) {
      problems.push(`${variant.tagValue}: refused by the boundary (${String(error)})`);
      continue;
    }
    if (narrowed.kind !== variant.tagValue) {
      problems.push(`${variant.tagValue}: narrowed to the arm "${narrowed.kind}"`);
      continue;
    }
    const carried = carriedStrings(narrowed);
    for (const key of [...variant.required, ...variant.optional]) {
      if (!carried.has(sampleFor(key))) {
        problems.push(`${variant.tagValue}.${key}: the value under this key did not arrive`);
      }
    }
    for (const key of variant.required) {
      let refused = false;
      try {
        narrowWindowOpenFailure(withKeyRenamed(whole, key));
      } catch {
        refused = true;
      }
      if (!refused) {
        problems.push(
          `${variant.tagValue}.${key}: accepted although this key was spelled otherwise`
        );
      }
    }
    for (const key of variant.optional) {
      const stillCarried = carriedStrings(narrowWindowOpenFailure(withKeyRenamed(whole, key)));
      if (stillCarried.has(sampleFor(key))) {
        problems.push(`${variant.tagValue}.${key}: read from a spelling the Rust side never sends`);
      }
    }
    const tagMoved = narrowWindowOpenFailure(withKeyRenamed(whole, shape.tagKey));
    if (tagMoved.kind === variant.tagValue) {
      problems.push(`${variant.tagValue}: the arm survived the tag key being spelled otherwise`);
    }
  }
  return problems;
}

const WIRE_SHAPE = wireShapeOf(commandsSource, FAILURE_ENUM_HEADER);

describe('the failure wire shape is read out of the Rust source', () => {
  it('reads a non-empty source (sanity check against a vacuous pass)', () => {
    expect(commandsSource.length).toBeGreaterThan(0);
  });

  it('finds a tag key and more than one tagged arm', () => {
    expect(WIRE_SHAPE.tagKey.length).toBeGreaterThan(0);
    expect(WIRE_SHAPE.variants.length).toBeGreaterThan(1);
  });

  it('finds at least one field under every arm', () => {
    for (const variant of WIRE_SHAPE.variants) {
      expect([...variant.required, ...variant.optional].length).toBeGreaterThan(0);
    }
  });

  it('finds exactly one arm that may omit a key', () => {
    const withOptional = WIRE_SHAPE.variants.filter((variant) => variant.optional.length > 0);
    expect(withOptional).toHaveLength(1);
  });
});

describe('the boundary reads the failure under the key names the core sends', () => {
  it('agrees with the Rust source on every key name and arm name', () => {
    expect(disagreementsWithBoundary(WIRE_SHAPE)).toEqual([]);
  });
});

/**
 * The check above is only worth its green if a disagreement would show up in
 * it. Rather than breaking the real source, the same reader is handed
 * fragments that disagree on purpose.
 */
describe('the agreement check answers for a source that disagrees', () => {
  const DISAGREEING_SOURCES: Array<[string, string]> = [
    [
      'a field spelled differently',
      [
        '#[serde(tag = "kind", rename_all = "snake_case")]',
        'pub enum WindowOpenFailure {',
        '    Rejected {',
        '        code: String,',
        '        message: String,',
        '        rejected_arg: Option<String>,',
        '    },',
        '    Local {',
        '        message: String,',
        '    },',
        '}',
      ].join('\n'),
    ],
    [
      'an arm named differently',
      [
        '#[serde(tag = "kind", rename_all = "snake_case")]',
        'pub enum WindowOpenFailure {',
        '    Refused {',
        '        code: String,',
        '        message: String,',
        '        rejected_argument: Option<String>,',
        '    },',
        '    Local {',
        '        message: String,',
        '    },',
        '}',
      ].join('\n'),
    ],
    [
      'the tag under another key',
      [
        '#[serde(tag = "variant", rename_all = "snake_case")]',
        'pub enum WindowOpenFailure {',
        '    Rejected {',
        '        code: String,',
        '        message: String,',
        '        rejected_argument: Option<String>,',
        '    },',
        '    Local {',
        '        message: String,',
        '    },',
        '}',
      ].join('\n'),
    ],
    [
      'a field the boundary never reads',
      [
        '#[serde(tag = "kind", rename_all = "snake_case")]',
        'pub enum WindowOpenFailure {',
        '    Rejected {',
        '        code: String,',
        '        message: String,',
        '        rejected_argument: Option<String>,',
        '        retry_after_ms: String,',
        '    },',
        '    Local {',
        '        message: String,',
        '    },',
        '}',
      ].join('\n'),
    ],
  ];

  it.each(DISAGREEING_SOURCES)('reports a disagreement for %s', (_label, source) => {
    const shape = wireShapeOf(source, FAILURE_ENUM_HEADER);

    expect(disagreementsWithBoundary(shape).length).toBeGreaterThan(0);
  });

  it('reports nothing for a fragment that restates the real shape', () => {
    const faithful = [
      '#[serde(tag = "kind", rename_all = "snake_case")]',
      'pub enum WindowOpenFailure {',
      '    Rejected {',
      '        code: String,',
      '        message: String,',
      '        rejected_argument: Option<String>,',
      '    },',
      '    Local {',
      '        message: String,',
      '    },',
      '}',
    ].join('\n');

    expect(disagreementsWithBoundary(wireShapeOf(faithful, FAILURE_ENUM_HEADER))).toEqual([]);
  });
});
