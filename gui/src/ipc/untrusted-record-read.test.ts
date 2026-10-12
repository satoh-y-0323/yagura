/**
 * One invariant of the two IPC boundary modules: a record that crossed the
 * Tauri edge is read through an ownership test, never by reaching a field
 * directly, so a polluted `Object.prototype` can never stand in for a field
 * the core did not send.
 *
 * The shape of that invariant is carried by the `UntrustedRecord` type, whose
 * fields the compiler refuses to index, dot or destructure. What the type
 * cannot see is what is measured here -- that no route asserts its way around
 * it, that the one route that does open it is reached only from a reader
 * answering for ownership, and that the test those readers run does discard an
 * inherited field at runtime.
 *
 * Reads that neither the type nor this file can see, such as one taken off a
 * fresh copy of the record, are answered for elsewhere: the entry point seals
 * `Object.prototype`, so nothing new can be put there to find -- in the running
 * application only, since node, where these tests run, is not sealed -- and its
 * built-in members stay there regardless, so a wire field must never take one
 * of their names. Two eslint rules, one of them scoped to these two modules,
 * flag the syntax that hides such a read from both when lint runs.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

import boundarySource from './session-boundary.ts?raw';
import clientSource from './session-client.ts?raw';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { BoundaryError } from './session-boundary';
import {
  invokeListDetections,
  invokeListProfiles,
  invokeListRestoreCandidates,
  invokeLoadKeymap,
  invokeLoadThemes,
} from './session-client';

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

interface MaskResult {
  /** `text` with comment and string-literal text blanked, length preserved. */
  code: string;
  /** The source with its line endings normalized: what `code`'s indices address. */
  text: string;
  /** Quoted runs that met a newline before their closing quote. */
  unterminated: number;
}

/**
 * Blanks what is not code, so an assertion quoted in prose or in a string is
 * not read as one. Template expressions stay code: an assertion hidden in one
 * would otherwise escape the scan. Regular-expression literals are read as
 * code, which is what the unterminated-run count answers for: one holding a
 * quote character makes the masking slip, and the count reports it.
 *
 * Line endings are normalized here, the one gate every scan below enters
 * through, so a `\n` written into any of their patterns stays meaningful on a
 * checkout that carries CRLF.
 */
function maskNonCode(raw: string): MaskResult {
  const source = raw.replace(/\r\n?/g, '\n');
  const out = source.split('');
  const templates: { depth: number }[] = [];
  let unterminated = 0;
  let i = 0;
  const blank = (from: number, to: number): void => {
    for (let k = from; k < to; k += 1) {
      if (out[k] !== '\n') {
        out[k] = ' ';
      }
    }
  };
  while (i < source.length) {
    const frame = templates[templates.length - 1];
    if (frame !== undefined && frame.depth === 0) {
      if (source.startsWith('\\', i)) {
        blank(i, i + 2);
        i += 2;
      } else if (source.startsWith('`', i)) {
        templates.pop();
        i += 1;
      } else if (source.startsWith('${', i)) {
        frame.depth = 1;
        i += 2;
      } else {
        blank(i, i + 1);
        i += 1;
      }
      continue;
    }
    if (source.startsWith('//', i)) {
      const end = source.indexOf('\n', i);
      const stop = end === -1 ? source.length : end;
      blank(i, stop);
      i = stop;
      continue;
    }
    if (source.startsWith('/*', i)) {
      const end = source.indexOf('*/', i + 2);
      const stop = end === -1 ? source.length : end + 2;
      blank(i, stop);
      i = stop;
      continue;
    }
    const ch = source[i];
    if (ch === '`') {
      templates.push({ depth: 0 });
      i += 1;
      continue;
    }
    if (ch === "'" || ch === '"') {
      let j = i + 1;
      while (j < source.length && source[j] !== ch) {
        if (source[j] === '\\') {
          j += 1;
        }
        if (source[j] === '\n') {
          unterminated += 1;
          break;
        }
        j += 1;
      }
      blank(i + 1, Math.min(j, source.length));
      i = j + 1;
      continue;
    }
    if (frame !== undefined) {
      if (ch === '{') {
        frame.depth += 1;
      } else if (ch === '}') {
        frame.depth -= 1;
      }
    }
    i += 1;
  }
  return { code: out.join(''), text: source, unterminated };
}

interface FunctionSpan {
  name: string;
  start: number;
  end: number;
  /** Everything up to the body's opening brace. */
  signature: string;
  body: string;
}

/** Every top-level function declaration of `code`, with its body. */
function functionSpans(code: string): FunctionSpan[] {
  const spans: FunctionSpan[] = [];
  const header = /^(?:export )?(?:async )?function\s+([A-Za-z_$][\w$]*)/gm;
  for (const match of code.matchAll(header)) {
    const offset = code.slice(match.index).search(/\{[ \t]*\n/);
    if (offset === -1) {
      continue;
    }
    const open = match.index + offset;
    let depth = 0;
    let k = open;
    for (; k < code.length; k += 1) {
      if (code[k] === '{') {
        depth += 1;
      } else if (code[k] === '}') {
        depth -= 1;
        if (depth === 0) {
          break;
        }
      }
    }
    spans.push({
      name: match[1] ?? '',
      start: match.index,
      end: k,
      signature: code.slice(match.index, open),
      body: code.slice(open, k + 1),
    });
  }
  return spans;
}

/** The whole line `index` falls on. */
function lineAt(text: string, index: number): string {
  const from = text.lastIndexOf('\n', index) + 1;
  const to = text.indexOf('\n', index);
  return text.slice(from, to === -1 ? text.length : to);
}

function enclosingSpan(spans: FunctionSpan[], index: number): FunctionSpan | null {
  let found: FunctionSpan | null = null;
  for (const span of spans) {
    if (span.start <= index && index <= span.end) {
      found = span;
    }
  }
  return found;
}

/**
 * The two assertions that can defeat the type: one that claims a value is an
 * untrusted record without checking, and a double assertion, the only form
 * that turns one back into a record whose fields are reachable directly.
 */
const OPAQUE_ASSERTION = /\bas\s+UntrustedRecord\b|\bas\s+unknown\s+as\s+/g;

/**
 * Every such assertion of `source`, named by the function that holds it (by
 * its line when it sits outside one), in source order. A name appearing twice
 * means that function holds two.
 */
function assertionSites(source: string): string[] {
  const { code, text } = maskNonCode(source);
  const spans = functionSpans(code);
  const sites: string[] = [];
  for (const match of code.matchAll(OPAQUE_ASSERTION)) {
    const span = enclosingSpan(spans, match.index);
    sites.push(span === null ? lineAt(text, match.index).trim() : span.name);
  }
  return sites;
}

/**
 * The places allowed to hold one: where an untrusted record is built out of a
 * checked value, and the private reader that indexes one to answer for a
 * field it owns. Any other name reaching this list is a route around the type.
 */
const REGISTERED_ASSERTION_SITES: readonly string[] = ['asRecord', 'fieldsOf'];

const SCANNED: ReadonlyArray<readonly [string, string]> = [
  ['session-boundary.ts', boundarySource],
  ['session-client.ts', clientSource],
];

describe('no route asserts its way around the untrusted-record type', () => {
  it.each(SCANNED)('%s offers the scan functions to classify', (_label, source) => {
    expect(functionSpans(maskNonCode(source).code).length).toBeGreaterThan(0);
  });

  it('session-boundary.ts holds such an assertion only where it is registered', () => {
    expect(assertionSites(boundarySource)).toEqual(REGISTERED_ASSERTION_SITES);
  });

  it('session-client.ts holds none at all', () => {
    expect(assertionSites(clientSource)).toEqual([]);
  });

  it.each(SCANNED)('%s leaves no quoted run for the scan to mistake for code', (_label, source) => {
    expect(maskNonCode(source).unterminated).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// What the scan itself detects, and what it deliberately does not
// ---------------------------------------------------------------------------

const REGRESSED_BUILD = [
  'function trustIt(value: unknown): UntrustedRecord {',
  '  return value as UntrustedRecord;',
  '}',
].join('\n');

const REGRESSED_STRIP = [
  'function openIt(record: UntrustedRecord): Record<string, unknown> {',
  '  return record as unknown as Record<string, unknown>;',
  '}',
].join('\n');

/** One fragment per form the scan must not report. */
const ASSERTION_LOOKALIKES: ReadonlyArray<readonly [string, string]> = [
  [
    'a single assertion to another type',
    ['function f(value: unknown): unknown[] {', '  return value as unknown[];', '}'].join('\n'),
  ],
  [
    'the type named in a doc comment',
    '/** Answers with value as UntrustedRecord. */\nfunction f(): void {}',
  ],
  [
    'the type named in a string literal',
    ['function f(): string {', "  return 'as UntrustedRecord';", '}'].join('\n'),
  ],
];

describe('the scan reports an assertion the register does not name', () => {
  it('reports one that builds an untrusted record', () => {
    expect(assertionSites(REGRESSED_BUILD)).toEqual(['trustIt']);
  });

  it('reports one that strips a record back to a directly readable one', () => {
    expect(assertionSites(REGRESSED_STRIP)).toEqual(['openIt']);
  });

  it.each(ASSERTION_LOOKALIKES)('reports nothing for %s', (_label, fragment) => {
    expect(assertionSites(fragment)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// The other route back: the function that opens the type
// ---------------------------------------------------------------------------

/**
 * Whether a signature takes the opaque record, and whether it hands back one
 * whose fields are reachable by a string key. A function declared that way is
 * a route back to an indexable record, whatever it is called, so the scan
 * derives it from the shape instead of registering a name.
 */
const OPAQUE_PARAMETER = /:\s*UntrustedRecord\b/;
const INDEXABLE_RETURN = /^(?:Record\s*<\s*string\s*,|\{\s*\[[^\]]*:\s*string\s*\]\s*:)/;

/**
 * An ownership test, or the iteration that answers with own keys alone: the
 * two treatments that make a value read off the record a value the core sent.
 */
const OWNERSHIP_TEST = /\bhasOwnProperty\b|\bObject\s*\.\s*entries\s*\(/;

/** The type a signature declares it returns, `null` when it declares none. */
function returnType(signature: string): string | null {
  const close = signature.lastIndexOf(')');
  if (close === -1) {
    return null;
  }
  const tail = signature.slice(close + 1).trim();
  return tail.startsWith(':') ? tail.slice(1).trim() : null;
}

/** A call of `name`, as opposed to a mention of it. */
function callPattern(name: string, flags = ''): RegExp {
  return new RegExp(`\\b${name}\\s*\\(`, flags);
}

/** Every function of `spans` declared to open the opaque type. */
function openerNames(spans: FunctionSpan[]): string[] {
  return spans
    .filter((span) => {
      const returns = returnType(span.signature);
      return (
        OPAQUE_PARAMETER.test(span.signature) && returns !== null && INDEXABLE_RETURN.test(returns)
      );
    })
    .map((span) => span.name);
}

/** Every function of `spans` whose own body runs an ownership test. */
function ownershipTestNames(spans: FunctionSpan[]): string[] {
  return spans.filter((span) => OWNERSHIP_TEST.test(span.body)).map((span) => span.name);
}

/** True when `span` runs the test itself or hands it to one that does. */
function testsOwnership(span: FunctionSpan, delegates: readonly string[]): boolean {
  return (
    OWNERSHIP_TEST.test(span.body) ||
    delegates.some((name) => name !== span.name && callPattern(name).test(span.body))
  );
}

interface OpenerCall {
  /** The enclosing function's name, or the call's line when it sits outside one. */
  site: string;
  /** Whether that function answers for ownership. */
  guarded: boolean;
}

/** Every call of one of `openers` in `source`, and how its holder reads. */
function openerCalls(source: string, openers: readonly string[]): OpenerCall[] {
  const { code, text } = maskNonCode(source);
  const spans = functionSpans(code);
  const delegates = ownershipTestNames(spans);
  const calls: OpenerCall[] = [];
  for (const opener of openers) {
    for (const match of code.matchAll(callPattern(opener, 'g'))) {
      if (/\bfunction\s+$/.test(code.slice(0, match.index))) {
        continue;
      }
      const span = enclosingSpan(spans, match.index);
      calls.push(
        span === null
          ? { site: lineAt(text, match.index).trim(), guarded: false }
          : { site: span.name, guarded: testsOwnership(span, delegates) }
      );
    }
  }
  return calls;
}

/** The sites that open the type without answering for ownership. */
function unguardedOpenerSites(source: string, openers: readonly string[]): string[] {
  return openerCalls(source, openers)
    .filter((call) => !call.guarded)
    .map((call) => call.site);
}

/** The openers of one source, derived from its own signatures. */
function derivedOpeners(source: string): string[] {
  return openerNames(functionSpans(maskNonCode(source).code));
}

const SCANNED_OPENERS: readonly string[] = SCANNED.flatMap(([, source]) => derivedOpeners(source));

describe('the type is opened only where the opener is answered for', () => {
  it('derives an opener from the scanned modules rather than registering one', () => {
    expect(SCANNED_OPENERS.length).toBeGreaterThan(0);
  });

  it.each(SCANNED)('%s opens the type only inside a reader that tests ownership', (_l, source) => {
    expect(unguardedOpenerSites(source, SCANNED_OPENERS)).toEqual([]);
  });

  it('has such a call to judge, so the scan above answers about something', () => {
    const guarded = SCANNED.flatMap(([, source]) => openerCalls(source, SCANNED_OPENERS)).filter(
      (call) => call.guarded
    );

    expect(guarded.length).toBeGreaterThan(0);
  });
});

/** The declaration every fragment below opens the type with. */
const OPENER_DECLARATION = [
  'function openIt(record: UntrustedRecord): Record<string, unknown> {',
  '  return record as unknown as Record<string, unknown>;',
  '}',
].join('\n');

/** One fragment per way a call can hold, or fail to hold, the invariant. */
const OPENER_FRAGMENTS: ReadonlyArray<readonly [string, string, readonly string[]]> = [
  [
    'a reader that tests ownership itself',
    [
      OPENER_DECLARATION,
      'function readIt(record: UntrustedRecord, key: string): unknown {',
      '  return Object.prototype.hasOwnProperty.call(record, key) ? openIt(record)[key] : undefined;',
      '}',
    ].join('\n'),
    [],
  ],
  [
    'a reader that hands the test to another function',
    [
      OPENER_DECLARATION,
      'function ownsIt(record: UntrustedRecord, key: string): boolean {',
      '  return Object.prototype.hasOwnProperty.call(record, key);',
      '}',
      'function readIt(record: UntrustedRecord, key: string): unknown {',
      '  return ownsIt(record, key) ? openIt(record)[key] : undefined;',
      '}',
    ].join('\n'),
    [],
  ],
  [
    'an iteration over the own keys',
    [
      OPENER_DECLARATION,
      'function entriesOf(record: UntrustedRecord): [string, unknown][] {',
      '  return Object.entries(openIt(record));',
      '}',
    ].join('\n'),
    [],
  ],
  [
    'a reader that tests nothing',
    [
      OPENER_DECLARATION,
      'function peekField(record: UntrustedRecord, key: string): unknown {',
      '  return openIt(record)[key];',
      '}',
    ].join('\n'),
    ['peekField'],
  ],
  [
    'a call that sits outside every function',
    [OPENER_DECLARATION, "const leaked = openIt(record)['key'];"].join('\n'),
    ["const leaked = openIt(record)['key'];"],
  ],
];

describe('the scan reports a call that opens the type and answers for nothing', () => {
  it.each(OPENER_FRAGMENTS)('reports %s as expected', (_label, fragment, expected) => {
    expect(unguardedOpenerSites(fragment, derivedOpeners(fragment))).toEqual(expected);
  });
});

// ---------------------------------------------------------------------------
// The positive side: every scanned module does read through such a reader
// ---------------------------------------------------------------------------

/**
 * Every function of `spans` that answers for ownership itself or hands the
 * question to one that does: the readers a module has to call to get a field
 * off an untrusted record. Derived from the scanned sources, so a rename in
 * them carries here without a name being registered.
 */
function ownershipReaderNames(spans: FunctionSpan[]): string[] {
  const delegates = ownershipTestNames(spans);
  return spans.filter((span) => testsOwnership(span, delegates)).map((span) => span.name);
}

const OWNERSHIP_READERS: readonly string[] = SCANNED.flatMap(([, source]) =>
  ownershipReaderNames(functionSpans(maskNonCode(source).code))
);

/** How many times `source` calls one of `readers`, its declarations excluded. */
function ownershipReaderCalls(source: string, readers: readonly string[]): number {
  const { code } = maskNonCode(source);
  let calls = 0;
  for (const reader of readers) {
    for (const match of code.matchAll(callPattern(reader, 'g'))) {
      if (!/\bfunction\s+$/.test(code.slice(0, match.index))) {
        calls += 1;
      }
    }
  }
  return calls;
}

describe('every scanned module takes its fields from a reader that owns the test', () => {
  it.each(SCANNED)('%s calls such a reader at least once', (_label, source) => {
    expect(ownershipReaderCalls(source, OWNERSHIP_READERS)).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// The same sources, as a checkout that carries CRLF hands them over
// ---------------------------------------------------------------------------

/**
 * `source` with every line ending written the way git leaves one where
 * `core.autocrlf` converts them. No `.gitattributes` pins the working tree, so
 * this is what a fresh clone holds and what `?raw` passes on unchanged.
 */
function asCrlf(source: string): string {
  return source.replace(/\r\n?|\n/g, '\r\n');
}

describe('the scan reads a source whose lines end in CRLF the same way', () => {
  it('is given a CRLF form of the source, with no bare newline left in it', () => {
    const crlf = asCrlf(boundarySource);

    expect(crlf.split('\r\n').length).toBeGreaterThan(1);
    expect(crlf.split('\n').length).toBe(crlf.split('\r\n').length);
    // Against the LF form, not the source: on a CRLF checkout they are equal.
    expect(crlf).not.toBe(crlf.replace(/\r\n/g, '\n'));
  });

  it('classifies a function declared in a CRLF fragment', () => {
    const spans = functionSpans(maskNonCode('function f(x) {\r\n  return x;\r\n}\r\n').code);

    expect(spans.map((span) => span.name)).toEqual(['f']);
    expect(spans[0]?.body).toBe('{\n  return x;\n}');
  });
});

// ---------------------------------------------------------------------------
// What the invariant buys on the `session-client` routes
// ---------------------------------------------------------------------------

/**
 * A reply carrying `fields` on its prototype alone. An index read finds them,
 * an ownership test does not: `{ __proto__: ... }` would create no own key
 * here and make every assertion below vacuous.
 */
function inherited(fields: Record<string, unknown>): Record<string, unknown> {
  return Object.create(fields) as Record<string, unknown>;
}

/** The same, with `own` as the reply's own keys. */
function inheritedPlusOwn(
  fields: Record<string, unknown>,
  own: Record<string, unknown>
): Record<string, unknown> {
  return Object.assign(inherited(fields), own);
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe('the pollution fixture separates an owned read from an inherited one', () => {
  it('shows the inherited value to an index read and nothing to an ownership test', () => {
    const reply = inheritedPlusOwn({ names: ['fixture'] }, { warning: null });

    expect(reply['names']).toEqual(['fixture']);
    expect(Object.prototype.hasOwnProperty.call(reply, 'names')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(reply, 'warning')).toBe(true);
  });
});

describe('session-client reads a reply field only when the reply owns it', () => {
  it('refuses a list_detections reply whose names only its prototype carries', async () => {
    invokeMock.mockResolvedValueOnce(inherited({ names: ['claude'] }));

    await expect(invokeListDetections()).rejects.toThrow(BoundaryError);
  });

  it('refuses a list_profiles reply whose profiles only its prototype carries', async () => {
    invokeMock.mockResolvedValueOnce(inherited({ profiles: [], warnings: [] }));

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('refuses a profile entry whose availability only its prototype carries', async () => {
    invokeMock.mockResolvedValueOnce({
      profiles: [
        inheritedPlusOwn(
          { available: true },
          {
            id: 'claude',
            display_name: 'Claude',
            command: ['claude'],
            model: null,
            effort: null,
          }
        ),
      ],
      warnings: [],
    });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('refuses a launch option whose aliases only its prototype carries', async () => {
    invokeMock.mockResolvedValueOnce({
      profiles: [
        {
          id: 'claude',
          display_name: 'Claude',
          command: ['claude'],
          available: true,
          model: inherited({ aliases: [] }),
          effort: null,
        },
      ],
      warnings: [],
    });

    await expect(invokeListProfiles()).rejects.toThrow(BoundaryError);
  });

  it('refuses a restore-candidate reply whose windows only its prototype carries', async () => {
    invokeMock.mockResolvedValueOnce(inherited({ windows: [] }));

    await expect(invokeListRestoreCandidates()).rejects.toThrow(BoundaryError);
  });

  it('reports no keymap failure when only the prototype names one', async () => {
    invokeMock.mockResolvedValueOnce(
      inheritedPlusOwn({ failure: 'not_found' }, { keys: {}, legacy_entries: 0 })
    );

    const result = await invokeLoadKeymap();

    expect(Object.prototype.hasOwnProperty.call(result, 'failure')).toBe(false);
  });

  it('reports no default theme when only the prototype names one', async () => {
    invokeMock.mockResolvedValueOnce(
      inheritedPlusOwn({ default_theme: 'dark' }, { themes: {}, vendor_assignments: {} })
    );

    const result = await invokeLoadThemes();

    expect(result.defaultTheme).toBeNull();
  });

  it('still reads a keymap failure the reply owns, under the same fixture', async () => {
    invokeMock.mockResolvedValueOnce(
      inheritedPlusOwn(
        { keys: {}, legacy_entries: 0 },
        { keys: {}, legacy_entries: 0, failure: 'invalid_toml' }
      )
    );

    const result = await invokeLoadKeymap();

    expect(result.failure).toBe('invalid_toml');
  });
});
