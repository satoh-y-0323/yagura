/**
 * Two eslint rules are the only thing that reports the syntax which hides a
 * read of an untrusted record from both the opaque type and the source scan:
 * the `in` operator in an expression, and an angle-bracket assertion. They
 * live in a file eslint itself ignores, so nothing else here would notice
 * their removal.
 *
 * What is pinned below is the configuration eslint resolves -- the blocks of
 * the flat config walked as data, and what eslint itself answers for each
 * module that holds an untrusted record, so a path added to `ignores` cannot
 * drop both rules while the blocks still read intact -- together with the
 * scope of the narrower rule, derived from which modules actually take fields
 * off such a record. A third module joining those two and not being listed
 * fails here.
 *
 * That derivation reads the `.ts` and `.svelte` sources and their
 * single-quoted static imports. A namespace import, a dynamic `import()`, a
 * double-quoted specifier, or an exported signature whose parameter types
 * carry parentheses is not counted, so a module reaching the API by one of
 * those stays invisible to it.
 *
 * These answer for the configuration. Whether anyone runs `npm run lint` is a
 * separate question, and so is an `eslint-disable` comment in the module
 * itself, which turns a rule off for a line while the configuration it is
 * measured against stays intact. What holds at runtime is the seal the entry
 * point installs, not the rule.
 */
import { ESLint } from 'eslint';
import { describe, it, expect } from 'vitest';

import { readOwnField, requireRecord } from './ipc/session-boundary';

/**
 * The flat config as eslint resolves it. Read through `import.meta.glob` so
 * the JavaScript config needs no type declaration to be imported from here.
 */
const configModules = import.meta.glob('../eslint.config.js', {
  import: 'default',
  eager: true,
});

const sourceFiles = import.meta.glob('./**/*.{ts,svelte}', {
  query: '?raw',
  import: 'default',
  eager: true,
});

/** Glob keys are relative to this file; the config's paths are to `gui/`. */
const SOURCE_PREFIX = 'src';

/**
 * `gui/`, the root the config and its `ignores` are written against, taken
 * from this module's own location rather than from the working directory the
 * runner happened to start in. Node's `fileURLToPath` is not reachable here:
 * the tsconfig carries no node types.
 */
function guiRoot(): string {
  const { pathname } = new URL('..', import.meta.url);
  return decodeURIComponent(pathname).replace(/^\/(?=[A-Za-z]:)/, '');
}

const eslint = new ESLint({ cwd: guiRoot() });

/** Resolving a config is type-aware, so it costs more than a unit test. */
const RESOLVE_TIMEOUT_MS = 120_000;

/** The blocks of the flat config, each validated as a record. */
function configBlocks(): ReturnType<typeof requireRecord>[] {
  const config = Object.values(configModules)[0];
  if (!Array.isArray(config)) {
    throw new Error('eslint.config.js does not export an array');
  }
  return config.map((block, index) => requireRecord(block, `eslint.config.js[${index}]`));
}

/** The blocks whose `rules` carry `name`, with that rule's entry. */
function blocksConfiguring(name: string): { files: string[]; entry: unknown }[] {
  const found: { files: string[]; entry: unknown }[] = [];
  for (const block of configBlocks()) {
    const rules = readOwnField(block, 'rules');
    const record =
      typeof rules === 'object' && rules !== null ? requireRecord(rules, 'rules') : null;
    if (record === null) {
      continue;
    }
    const entry = readOwnField(record, name);
    if (entry === undefined) {
      continue;
    }
    const files = readOwnField(block, 'files');
    found.push({
      files: Array.isArray(files) ? files.filter((f): f is string => typeof f === 'string') : [],
      entry,
    });
  }
  return found;
}

/** The entry eslint itself resolves for `name` on `path`. */
async function resolvedRuleEntry(path: string, name: string): Promise<unknown> {
  const config: unknown = await eslint.calculateConfigForFile(path);
  const rules = readOwnField(requireRecord(config, `resolved config of ${path}`), 'rules');
  return readOwnField(requireRecord(rules, `resolved rules of ${path}`), name);
}

/** The severity and options of a rule entry written as `[severity, options]`. */
function ruleEntry(entry: unknown): { severity: unknown; options: unknown[] } {
  if (!Array.isArray(entry)) {
    throw new Error('rule entry is not an array');
  }
  return { severity: entry[0], options: entry.slice(1) };
}

/** The `selector` of every option of a `no-restricted-syntax` entry. */
function restrictedSelectors(entry: unknown): unknown[] {
  return ruleEntry(entry).options.map((option) =>
    readOwnField(requireRecord(option, 'no-restricted-syntax'), 'selector')
  );
}

/**
 * The blocks whose `no-restricted-syntax` carries the `in` selector. Other
 * blocks restrict other syntax for other files.
 */
function blocksRestrictingIn(): { files: string[]; entry: unknown }[] {
  return blocksConfiguring('no-restricted-syntax').filter(({ entry }) =>
    restrictedSelectors(entry).includes('BinaryExpression[operator="in"]')
  );
}

/** The `assertionStyle` of every option of a `consistent-type-assertions` entry. */
function assertionStyles(entry: unknown): unknown[] {
  return ruleEntry(entry).options.map((option) =>
    readOwnField(requireRecord(option, 'consistent-type-assertions'), 'assertionStyle')
  );
}

/**
 * The exported names of the boundary module that name the opaque type in
 * their signature: what another module has to import to hold one.
 */
function opaqueApiNames(boundarySource: string): string[] {
  const signature = /export function ([A-Za-z_$][\w$]*)\(([^)]*)\)\s*:\s*([^{]*)\{/g;
  const names = ['UntrustedRecord'];
  for (const match of boundarySource.matchAll(signature)) {
    if (`${match[2] ?? ''}${match[3] ?? ''}`.includes('UntrustedRecord')) {
      names.push(match[1] ?? '');
    }
  }
  return names;
}

/** Only the import statements of `source`, so a mention in prose is not one. */
function importStatements(source: string): string {
  return [...source.matchAll(/import\s[\s\S]*?from\s*'[^']*';/g)]
    .map((match) => match[0])
    .join('\n');
}

/**
 * The production modules that hold an untrusted record: the one declaring the
 * type, and any importing part of its API. Tests are left out -- the rule
 * scopes the reading of wire values, which only production modules do.
 */
function modulesHoldingOpaqueRecords(): string[] {
  const entries = Object.entries(sourceFiles).filter(
    ([key]) => !key.endsWith('.test.ts') && typeof sourceFiles[key] === 'string'
  );
  const declaring = entries.find(([, source]) =>
    /export interface UntrustedRecord\b/.test(String(source))
  );
  if (declaring === undefined) {
    throw new Error('no module declares UntrustedRecord');
  }
  const names = opaqueApiNames(String(declaring[1]));
  return entries
    .filter(
      ([key, source]) =>
        key === declaring[0] ||
        names.some((name) => new RegExp(`\\b${name}\\b`).test(importStatements(String(source))))
    )
    .map(([key]) => `${SOURCE_PREFIX}${key.slice(1)}`)
    .sort();
}

describe('eslint.config.js keeps the rules that report a hidden record read', () => {
  it('resolves to a non-empty list of blocks (sanity check against a vacuous pass)', () => {
    expect(configBlocks().length).toBeGreaterThan(0);
  });

  it('finds at least one module holding untrusted records (same sanity check)', () => {
    expect(modulesHoldingOpaqueRecords().length).toBeGreaterThan(0);
  });

  it('restricts the `in` operator in an expression', () => {
    const blocks = blocksRestrictingIn();

    expect(blocks.length).toBe(1);
    const { severity } = ruleEntry(blocks[0]?.entry);
    expect(severity).toBe('error');
    expect(restrictedSelectors(blocks[0]?.entry)).toContain('BinaryExpression[operator="in"]');
  });

  it('scopes that restriction to exactly the modules holding untrusted records', () => {
    const blocks = blocksRestrictingIn();

    expect([...(blocks[0]?.files ?? [])].sort()).toEqual(modulesHoldingOpaqueRecords());
  });

  it('refuses an angle-bracket assertion across every gui source', () => {
    const blocks = blocksConfiguring('@typescript-eslint/consistent-type-assertions');

    expect(blocks.length).toBe(1);
    const { severity } = ruleEntry(blocks[0]?.entry);
    expect(severity).toBe('error');
    expect(assertionStyles(blocks[0]?.entry)).toEqual(['as']);
    expect(blocks[0]?.files).toEqual(['**/*.ts', '**/*.svelte']);
  });
});

describe('eslint applies those rules to the modules holding untrusted records', () => {
  it(
    'ignores none of them',
    async () => {
      const modules = modulesHoldingOpaqueRecords();
      const ignored = await Promise.all(modules.map((module) => eslint.isPathIgnored(module)));

      expect(ignored).toEqual(modules.map(() => false));
    },
    RESOLVE_TIMEOUT_MS
  );

  it(
    'resolves both rules as errors for each of them',
    async () => {
      for (const module of modulesHoldingOpaqueRecords()) {
        const restricted = await resolvedRuleEntry(module, 'no-restricted-syntax');
        expect(ruleEntry(restricted).severity).toBe(2);
        expect(restrictedSelectors(restricted)).toContain('BinaryExpression[operator="in"]');

        const assertions = await resolvedRuleEntry(
          module,
          '@typescript-eslint/consistent-type-assertions'
        );
        expect(ruleEntry(assertions).severity).toBe(2);
        expect(assertionStyles(assertions)).toEqual(['as']);
      }
    },
    RESOLVE_TIMEOUT_MS
  );
});

/**
 * The selectors that refuse raw HTML sinks. A later block's
 * `no-restricted-syntax` replaces an earlier one for the same file, so a new
 * block that forgets these would silently let raw HTML through; resolving
 * every file catches that.
 */
const RAW_HTML_SELECTORS = [
  'MemberExpression[property.name=/^(innerHTML|outerHTML)$/]',
  'MemberExpression[property.value=/^(innerHTML|outerHTML)$/]',
  "CallExpression[callee.property.name='insertAdjacentHTML']",
  'CallExpression[callee.property.name=/^(createContextualFragment|setHTMLUnsafe)$/]',
  "CallExpression[callee.object.name='document'][callee.property.name=/^(write|writeln)$/]",
  "SvelteAttribute[key.name='srcdoc']",
  "SvelteDirective[kind='Binding'][key.name.name=/^(innerHTML|outerHTML)$/]",
];

/** Every non-test gui source, as a path relative to `gui/`. */
function productionSources(): string[] {
  return Object.keys(sourceFiles)
    .filter((key) => !key.endsWith('.test.ts'))
    .map((key) => `${SOURCE_PREFIX}/${key.replace(/^\.\//, '')}`);
}

describe('eslint refuses raw HTML in every gui source', () => {
  it('finds gui sources to check (sanity check against a vacuous pass)', () => {
    expect(productionSources().length).toBeGreaterThan(0);
  });

  it(
    'resolves the raw HTML restriction as an error for each of them',
    async () => {
      for (const path of productionSources()) {
        const restricted = await resolvedRuleEntry(path, 'no-restricted-syntax');
        expect(ruleEntry(restricted).severity, path).toBe(2);
        expect(restrictedSelectors(restricted), path).toEqual(
          expect.arrayContaining(RAW_HTML_SELECTORS)
        );
        if (path.endsWith('.svelte')) {
          const atHtml = await resolvedRuleEntry(path, 'svelte/no-at-html-tags');
          expect(ruleEntry(atHtml).severity, path).toBe(2);
        }
      }
    },
    RESOLVE_TIMEOUT_MS
  );
});
