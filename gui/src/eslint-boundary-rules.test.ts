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
 * separate question. What holds at runtime is the seal the entry point
 * installs, not the rule.
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

const brandGuardRuleModules = import.meta.glob('../eslint.config.js', {
  import: 'BRAND_GUARD_RULES',
  eager: true,
});

const displaySinkMessageModules = import.meta.glob('../eslint.config.js', {
  import: 'DISPLAY_SINK_MESSAGES',
  eager: true,
});

/** The one named export of the config read by `modules`, as a list of strings. */
function configStrings(modules: Record<string, unknown>, name: string): string[] {
  const value = Object.values(modules)[0];
  if (!Array.isArray(value) || value.some((item) => typeof item !== 'string')) {
    throw new Error(`eslint.config.js does not export ${name} as a list of strings`);
  }
  return value.filter((item): item is string => typeof item === 'string');
}

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

/** The rules eslint itself resolves for `path`. */
async function resolvedRules(path: string): Promise<ReturnType<typeof requireRecord>> {
  const config: unknown = await eslint.calculateConfigForFile(path);
  const rules = readOwnField(requireRecord(config, `resolved config of ${path}`), 'rules');
  return requireRecord(rules, `resolved rules of ${path}`);
}

/** The entry eslint itself resolves for `name` on `path`. */
async function resolvedRuleEntry(path: string, name: string): Promise<unknown> {
  return readOwnField(await resolvedRules(path), name);
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

/** Test fixtures build brands as they need; their values are never drawn. */
const FIXTURE_PREFIX = `${SOURCE_PREFIX}/test-utils/`;

const BRAND_FORGING_RULE = 'yagura/no-brand-forging';
const DISPLAY_TEXT_RULE = 'yagura/display-text';
const TS_COMMENT_RULE = '@typescript-eslint/ban-ts-comment';
const DISPLAYED_BRANDS = ['DisplayLine', 'DisplayBlock', 'UiText'];

/** The `type` of every brand in the options of a brand rule entry. */
function brandTypes(entry: unknown): unknown[] {
  const brands = readOwnField(
    requireRecord(ruleEntry(entry).options[0], 'brand options'),
    'brands'
  );
  if (!Array.isArray(brands)) {
    throw new Error('brand options carry no list of brands');
  }
  return brands.map((brand) => readOwnField(requireRecord(brand, 'brand'), 'type'));
}

/** The guards a source must resolve, by where it sits and what kind of file it is. */
function guardsFor(path: string): string[] {
  return configStrings(brandGuardRuleModules, 'BRAND_GUARD_RULES').filter(
    (name) => name !== DISPLAY_TEXT_RULE || path.endsWith('.svelte')
  );
}

/** The production sources the brand guards answer for. */
function guardedSources(): string[] {
  return productionSources().filter((path) => !path.startsWith(FIXTURE_PREFIX));
}

describe('the display guards apply to every gui source', () => {
  it('finds gui sources to check (sanity check against a vacuous pass)', () => {
    expect(productionSources().length).toBeGreaterThan(0);
    expect(guardedSources().length).toBeGreaterThan(0);
  });

  it(
    'resolves the raw HTML restriction and every display guard as errors for each of them',
    async () => {
      const guarded = new Set(guardedSources());
      for (const path of productionSources()) {
        const rules = await resolvedRules(path);
        const restricted = readOwnField(rules, 'no-restricted-syntax');
        expect(ruleEntry(restricted).severity, path).toBe(2);
        expect(restrictedSelectors(restricted), path).toEqual(
          expect.arrayContaining(RAW_HTML_SELECTORS)
        );
        if (path.endsWith('.svelte')) {
          const atHtml = readOwnField(rules, 'svelte/no-at-html-tags');
          expect(ruleEntry(atHtml).severity, path).toBe(2);
        }
        if (!guarded.has(path)) {
          continue;
        }
        for (const name of guardsFor(path)) {
          expect(ruleEntry(readOwnField(rules, name)).severity, `${path} ${name}`).toBe(2);
        }
        expect(brandTypes(readOwnField(rules, BRAND_FORGING_RULE)), path).toEqual(
          expect.arrayContaining(DISPLAYED_BRANDS)
        );
        if (path.endsWith('.svelte')) {
          expect(brandTypes(readOwnField(rules, DISPLAY_TEXT_RULE)), path).toEqual(
            expect.arrayContaining(DISPLAYED_BRANDS)
          );
        }
        const tsComment = requireRecord(
          ruleEntry(readOwnField(rules, TS_COMMENT_RULE)).options[0],
          `${path} ${TS_COMMENT_RULE}`
        );
        expect(
          ['ts-expect-error', 'ts-ignore', 'ts-nocheck'].map((key) => readOwnField(tsComment, key)),
          path
        ).toEqual([true, true, true]);
      }
    },
    RESOLVE_TIMEOUT_MS
  );
});

/**
 * The linter that reads no inline directive. `allowInlineConfig: false` drops
 * the line and block directives and the configuration comments; with
 * `svelte/comment-directive` off, the Svelte processor removes nothing for an
 * HTML comment either.
 */
const directiveBlindEslint = new ESLint({
  cwd: guiRoot(),
  allowInlineConfig: false,
  overrideConfig: { rules: { 'svelte/comment-directive': 'off' } },
});

/** The guarded sources that hold an inline directive; every directive starts with `eslint`. */
function sourcesWithDirectives(): string[] {
  const guarded = new Set(guardedSources());
  return Object.entries(sourceFiles)
    .filter(([, source]) => String(source).includes('eslint'))
    .map(([key]) => `${SOURCE_PREFIX}/${key.replace(/^\.\//, '')}`)
    .filter((path) => guarded.has(path));
}

describe('an inline directive cannot silence the display guards', () => {
  it(
    'reports no display guard once inline directives are ignored',
    async () => {
      const paths = sourcesWithDirectives();
      expect(paths.length).toBeGreaterThan(0);
      const guards = new Set(configStrings(brandGuardRuleModules, 'BRAND_GUARD_RULES'));
      const sinkMessages = new Set(
        configStrings(displaySinkMessageModules, 'DISPLAY_SINK_MESSAGES')
      );

      const results = await directiveBlindEslint.lintFiles(paths);
      const messages = results.flatMap((result) =>
        result.messages.map((message) => ({ file: result.filePath, ...message }))
      );

      expect(results.length).toBe(paths.length);
      expect(messages.filter((message) => message.ruleId === null)).toEqual([]);
      expect(
        messages
          .filter(
            ({ ruleId, message }) =>
              (ruleId !== null && guards.has(ruleId)) ||
              (ruleId === 'no-restricted-syntax' && sinkMessages.has(message))
          )
          .map(({ file, line, ruleId, message }) => `${file}:${line} ${ruleId} ${message}`)
      ).toEqual([]);
    },
    RESOLVE_TIMEOUT_MS
  );
});
