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
 * The display guards are pinned here as well: every guarded source resolves
 * them exactly as `DISPLAY_GUARDS` declares them, and each guard is linted on
 * cases at the two fixtures under `lint-cases/` to show what it reports.
 *
 * These answer for the configuration. Whether anyone runs `npm run lint` is a
 * separate question. What holds at runtime is the seal the entry point
 * installs, not the rule.
 */
import { ESLint, type Linter } from 'eslint';
import { afterAll, describe, it, expect } from 'vitest';

import { ownEntries, readOwnField, requireRecord } from './ipc/session-boundary';

/**
 * The flat config as eslint resolves it. Read through `import.meta.glob` so
 * the JavaScript config needs no type declaration to be imported from here.
 */
const configModules = import.meta.glob('../eslint.config.js', {
  import: 'default',
  eager: true,
});

/** The config module's namespace, so a missing export fails only where it is read. */
const configNamespaces = import.meta.glob('../eslint.config.js', { eager: true });

const pluginModules = import.meta.glob('../eslint-plugin-yagura.js', {
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
const OUTSIDE_TEMPLATE_RULE = 'yagura/no-text-outside-template';
const DISPLAYED_BRANDS = ['DisplayLine', 'DisplayBlock', 'UiText'];

/** The two guarded files the guard cases are linted at; on disk each is an empty module. */
const GUARD_CASE_TS = `${SOURCE_PREFIX}/lint-cases/guard-case.ts`;
const GUARD_CASE_SVELTE = `${SOURCE_PREFIX}/lint-cases/GuardCase.svelte`;

/** `DISPLAY_GUARDS.rules`: every display guard, by name, with the entry the config declares. */
function displayGuardRules(): ReturnType<typeof requireRecord> {
  const namespace = requireRecord(Object.values(configNamespaces)[0], 'eslint.config.js');
  const guards = readOwnField(namespace, 'DISPLAY_GUARDS');
  if (guards === undefined) {
    throw new Error('eslint.config.js does not export DISPLAY_GUARDS');
  }
  return requireRecord(
    readOwnField(requireRecord(guards, 'DISPLAY_GUARDS'), 'rules'),
    'DISPLAY_GUARDS.rules'
  );
}

function displayGuardNames(): Set<string> {
  return new Set(ownEntries(displayGuardRules()).map(([name]) => name));
}

/** The rules of the local plugin, under the prefix the config registers it with. */
function pluginRuleNames(): string[] {
  const plugin = requireRecord(Object.values(pluginModules)[0], 'eslint-plugin-yagura.js');
  return ownEntries(requireRecord(readOwnField(plugin, 'rules'), 'plugin rules')).map(
    ([name]) => `yagura/${name}`
  );
}

const SEVERITY_NUMBERS: Record<string, number> = { off: 0, warn: 1, error: 2 };

/** A declared rule entry as eslint resolves it: the severity as a number, then the options. */
function asResolved(entry: unknown): unknown[] {
  const parts: unknown[] = Array.isArray(entry) ? entry : [entry];
  const [severity, ...options] = parts;
  return [typeof severity === 'string' ? SEVERITY_NUMBERS[severity] : severity, ...options];
}

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
    'resolves every display guard exactly as DISPLAY_GUARDS declares it, for each of them',
    async () => {
      const guards = displayGuardRules();
      expect(pluginRuleNames().filter((name) => readOwnField(guards, name) === undefined)).toEqual(
        []
      );
      expect(guardedSources()).toEqual(expect.arrayContaining([GUARD_CASE_TS, GUARD_CASE_SVELTE]));
      const declared = Object.fromEntries(
        ownEntries(guards).map(([name, entry]) => [name, asResolved(entry)])
      );
      expect(
        Object.entries(declared).filter(([, entry]) => entry[0] !== SEVERITY_NUMBERS['error'])
      ).toEqual([]);
      for (const path of guardedSources()) {
        const rules = await resolvedRules(path);
        const resolved = Object.fromEntries(
          Object.keys(declared).map((name) => [name, readOwnField(rules, name)])
        );
        expect(resolved, path).toEqual(declared);
        expect(brandTypes(readOwnField(rules, BRAND_FORGING_RULE)), path).toEqual(
          expect.arrayContaining(DISPLAYED_BRANDS)
        );
        expect(brandTypes(readOwnField(rules, DISPLAY_TEXT_RULE)), path).toEqual(
          expect.arrayContaining(DISPLAYED_BRANDS)
        );
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
  // An empty list of paths would otherwise lint all of `gui/`.
  passOnNoPatterns: true,
  overrideConfig: { rules: { 'svelte/comment-directive': 'off' } },
});

/**
 * One line of a guard case: plain, or one a rule must report with `reports`
 * (a message id, or for a directive case the rule).
 */
type CaseLine = string | { readonly code: string; readonly reports: string };

/** A line some display guard must report, whichever guard it is. */
const ANY_GUARD = 'any display guard';

function codeOf(line: CaseLine | undefined): string {
  return line === undefined ? '' : typeof line === 'string' ? line : line.code;
}

/** What a line is expected to be reported as, or undefined for a plain line. */
function reportOf(line: CaseLine | undefined): string | undefined {
  return line === undefined || typeof line === 'string' ? undefined : line.reports;
}

async function lintCase(
  linter: ESLint,
  filePath: string,
  lines: readonly CaseLine[]
): Promise<Linter.LintMessage[]> {
  const source = `${lines.map(codeOf).join('\n')}\n`;
  const [result] = await linter.lintText(source, { filePath });
  if (result === undefined) {
    throw new Error(`eslint returned no result for ${filePath}`);
  }
  return result.messages;
}

/** The messages eslint could not attribute to a rule: a case that failed to parse. */
function unparsed(messages: readonly Linter.LintMessage[]): string[] {
  return messages.filter(({ ruleId }) => ruleId === null).map(({ message }) => message);
}

/** The lines expected with a report other than ANY_GUARD, as `report: code`. */
function expectedReports(lines: readonly CaseLine[]): string[] {
  return lines
    .flatMap((line) => {
      const report = reportOf(line);
      return report === undefined || report === ANY_GUARD
        ? []
        : [`${report}: ${codeOf(line).trim()}`];
    })
    .sort();
}

/**
 * What `rule` reports, as `messageId: code`, leaving out the lines marked
 * ANY_GUARD: those are measured by `anyGuardRoutes`, whichever rule takes them.
 */
function reportsOf(
  rule: string,
  lines: readonly CaseLine[],
  messages: readonly Linter.LintMessage[]
): string[] {
  const shown = messages
    .filter(({ ruleId, line }) => ruleId === rule && reportOf(lines[line - 1]) !== ANY_GUARD)
    .map(
      ({ messageId, message, line }) => `${messageId ?? message}: ${codeOf(lines[line - 1]).trim()}`
    );
  return [...new Set(shown)].sort();
}

/** What the display guards report, as `ruleId: code`. */
function guardReportsOf(
  lines: readonly CaseLine[],
  messages: readonly Linter.LintMessage[],
  guards: ReadonlySet<string>
): string[] {
  const shown = messages
    .filter(({ ruleId }) => ruleId !== null && guards.has(ruleId))
    .map(({ ruleId, line }) => `${ruleId ?? ''}: ${codeOf(lines[line - 1]).trim()}`);
  return [...new Set(shown)].sort();
}

/** The ANY_GUARD lines, and those of them some display guard reports. */
function anyGuardRoutes(
  lines: readonly CaseLine[],
  messages: readonly Linter.LintMessage[],
  guards: ReadonlySet<string>
): { expected: string[]; reported: string[] } {
  const marked = lines.flatMap((line, index) =>
    reportOf(line) === ANY_GUARD ? [{ number: index + 1, code: codeOf(line).trim() }] : []
  );
  const reportedLines = new Set(
    messages.filter(({ ruleId }) => ruleId !== null && guards.has(ruleId)).map(({ line }) => line)
  );
  return {
    expected: marked.map(({ code }) => code),
    reported: marked.filter(({ number }) => reportedLines.has(number)).map(({ code }) => code),
  };
}

/** A fixture's content on disk, which the cases are linted over and put back to. */
function diskSource(path: string): string {
  const source = sourceFiles[`./${path.slice(`${SOURCE_PREFIX}/`.length)}`];
  if (typeof source !== 'string') {
    throw new Error(`${path} is not a gui source`);
  }
  return source;
}

const OUTSIDE_TEMPLATE_RED_TS: readonly CaseLine[] = [
  "import { createRawSnippet } from 'svelte';",
  "import { getCurrentWindow } from '@tauri-apps/api/window';",
  "import type { MenuItem } from '@tauri-apps/api/menu';",
  "import type { TrayIcon } from '@tauri-apps/api/tray';",
  "import { ask, confirm as confirmDialog, message } from '@tauri-apps/plugin-dialog';",
  '',
  'export {};',
  'declare const el: HTMLElement;',
  'declare const internals: ElementInternals;',
  'declare const input: HTMLInputElement;',
  'declare const output: HTMLOutputElement;',
  'declare const utterance: SpeechSynthesisUtterance;',
  'declare const raw: string;',
  'declare const raws: string[];',
  'declare const menuItem: MenuItem;',
  'declare const tray: TrayIcon;',
  "const key = 'textContent' as const;",
  '',
  { code: 'el.textContent = raw;', reports: 'outsideTemplate' },
  { code: 'el[key] = raw;', reports: 'outsideTemplate' },
  { code: 'el.title += 1;', reports: 'outsideTemplate' },
  { code: '({ raw: el.textContent } = { raw });', reports: 'outsideTemplate' },
  { code: "Reflect.set(el, 'textContent', raw);", reports: 'outsideTemplate' },
  { code: 'Object.assign(el, { textContent: raw });', reports: 'outsideTemplate' },
  { code: "Object.defineProperty(el, 'textContent', { value: raw });", reports: 'outsideTemplate' },
  {
    code: 'Object.defineProperties(el, { textContent: { value: raw } });',
    reports: 'outsideTemplate',
  },
  {
    code: "Reflect.defineProperty(el, 'textContent', { value: raw });",
    reports: 'outsideTemplate',
  },
  { code: 'internals.ariaLabel = raw;', reports: 'outsideTemplate' },
  { code: 'el.append(raw);', reports: 'outsideTemplate' },
  { code: "el.setAttribute('aria-label', raw);", reports: 'outsideTemplate' },
  { code: 'el.append(...raws);', reports: 'outsideTemplate' },
  { code: 'window.confirm(raw);', reports: 'outsideTemplate' },
  { code: 'document.write(raw);', reports: 'outsideTemplate' },
  { code: 'alert(raw);', reports: 'outsideTemplate' },
  { code: 'alert`${raw}`;', reports: 'outsideTemplate' },
  { code: 'alert(new Error(raw));', reports: 'outsideTemplate' },
  { code: 'const { confirm } = window; confirm(raw);', reports: 'outsideTemplate' },
  { code: 'const show = alert; show(raw);', reports: 'outsideTemplate' },
  { code: "new DOMParser().parseFromString(raw, 'text/html');", reports: 'outsideTemplate' },
  { code: 'new Option(raw);', reports: 'outsideTemplate' },
  { code: 'document.title = raw;', reports: 'outsideTemplate' },
  { code: 'document.createTextNode(raw);', reports: 'outsideTemplate' },
  { code: 'input.placeholder = raw;', reports: 'outsideTemplate' },
  { code: 'output.value = raw;', reports: 'outsideTemplate' },
  {
    code: 'export function setText<T extends string>(value: T): void { el.textContent = value; }',
    reports: 'outsideTemplate',
  },
  {
    code: 'export function setOn<E extends HTMLElement>(target: E): void { target.textContent = raw; }',
    reports: 'outsideTemplate',
  },
  { code: 'createRawSnippet(() => ({ render: () => raw }));', reports: ANY_GUARD },
  { code: 'new Notification(raw);', reports: ANY_GUARD },
  { code: 'new SpeechSynthesisUtterance(raw);', reports: ANY_GUARD },
  { code: 'new MediaMetadata({ title: raw });', reports: ANY_GUARD },
  { code: 'void getCurrentWindow().setTitle(raw);', reports: ANY_GUARD },
  { code: 'void menuItem.setText(raw);', reports: ANY_GUARD },
  { code: 'void tray.setTooltip(raw);', reports: ANY_GUARD },
  { code: 'void message(raw);', reports: ANY_GUARD },
  { code: 'void ask(raw);', reports: ANY_GUARD },
  { code: 'void confirmDialog(raw);', reports: ANY_GUARD },
  { code: 'void navigator.share({ text: raw });', reports: ANY_GUARD },
  { code: 'utterance.text = raw;', reports: ANY_GUARD },
];

const OUTSIDE_TEMPLATE_RED_SVELTE: readonly CaseLine[] = [
  '<script lang="ts">',
  '  export {};',
  '  const el = document.body;',
  '  const raw: string = String(Date.now());',
  { code: '  el.textContent = raw;', reports: 'outsideTemplate' },
  '</script>',
  '',
  { code: '<p on:click={() => (el.textContent = raw)}></p>', reports: 'outsideTemplate' },
];

const OUTSIDE_TEMPLATE_GREEN_TS: readonly CaseLine[] = [
  'export {};',
  'declare const el: HTMLElement;',
  'declare const input: HTMLInputElement;',
  'declare const textarea: HTMLTextAreaElement;',
  'declare const canvas: HTMLCanvasElement;',
  'declare const video: HTMLVideoElement;',
  'declare const stream: MediaStream;',
  'declare const node: Node;',
  'declare const raw: string;',
  "const row = { title: '' };",
  'function confirm(text: string): boolean {',
  '  return text.length > 0;',
  '}',
  '',
  'row.title = raw;',
  'confirm(raw);',
  'input.value = raw;',
  'textarea.value = raw;',
  'canvas.width = 3;',
  'el.hidden = true;',
  'video.srcObject = stream;',
  'el.style.left = raw;',
  "el.dataset['k'] = raw;",
  'el.classList.add(raw);',
  'el.closest(raw);',
  'el.matches(raw);',
  'document.querySelector(raw);',
  'el.getAttribute(raw);',
  'el.addEventListener(raw, () => {});',
  'document.getElementById(raw);',
  'document.createElement(raw);',
  "canvas.getContext('2d');",
  'window.matchMedia(raw);',
  'el.focus({ preventScroll: true });',
  "el.scrollIntoView({ block: 'nearest' });",
  'el.append(node);',
  'el.dispatchEvent(new Event(raw));',
  'setTimeout(() => {}, 1);',
];

const DISPLAY_TEXT_RED: readonly CaseLine[] = [
  '<script lang="ts">',
  "  import { formatChatBody, sanitizeForDisplay } from '../display/untrusted-text';",
  "  import type { DisplayLine } from '../display/untrusted-text';",
  '',
  '  export {};',
  '  const raw: string = String(Date.now());',
  '  let edited = raw;',
  '  const anyValue: any = raw;',
  '  const block = formatChatBody(raw);',
  '  const line = sanitizeForDisplay(raw);',
  '  const placeholder: string = raw;',
  "  const attrs: { 'aria-label': string } = { 'aria-label': raw };",
  '  const record: Record<string, string> = {};',
  '  const srcdoc: DisplayLine = line;',
  '  const srcdocAttrs: { srcdoc: DisplayLine } = { srcdoc: line };',
  '  const lineRecord: Record<string, DisplayLine> = {};',
  '  const tagName: string = raw;',
  '</script>',
  '',
  { code: '<p>{raw}</p>', reports: 'unprepared' },
  { code: '<p>{anyValue}</p>', reports: 'untyped' },
  { code: '<p title={block}></p>', reports: 'multiline' },
  { code: '<input {placeholder} />', reports: 'unprepared' },
  { code: '<p {...attrs}></p>', reports: 'unprepared' },
  { code: '<p {...record}></p>', reports: 'unprepared' },
  {
    code: "<svelte:element this={'span'} aria-label={raw}></svelte:element>",
    reports: 'unprepared',
  },
  { code: '<a download={raw}>{line}</a>', reports: 'unprepared' },
  { code: '<input type="button" value={raw} />', reports: 'unprepared' },
  {
    code: '<datalist id="unlinked"><option value={raw}></option></datalist>',
    reports: 'unprepared',
  },
  { code: '<iframe srcdoc={line} title={line}></iframe>', reports: 'rawMarkup' },
  { code: '<iframe {srcdoc} title={line}></iframe>', reports: 'rawMarkup' },
  { code: '<iframe srcdoc="<b>x</b>" title={line}></iframe>', reports: 'rawMarkup' },
  { code: '<iframe {...srcdocAttrs} title={line}></iframe>', reports: 'rawMarkup' },
  { code: '<iframe {...lineRecord} title={line}></iframe>', reports: 'rawMarkup' },
  {
    code: '<svelte:element this={tagName} {...lineRecord}></svelte:element>',
    reports: 'rawMarkup',
  },
  { code: '<div contenteditable="true" bind:textContent={edited}></div>', reports: 'rawMarkup' },
];

const DISPLAY_TEXT_GREEN: readonly CaseLine[] = [
  '<script lang="ts">',
  "  import { sanitizeForDisplay } from '../display/untrusted-text';",
  "  import type { DisplayLine } from '../display/untrusted-text';",
  '',
  '  export {};',
  '  const raw: string = String(Date.now());',
  '  const line = sanitizeForDisplay(raw);',
  '  const lineRecord: Record<string, DisplayLine> = {};',
  '  const count = 3;',
  '</script>',
  '',
  '<p class={raw}></p>',
  '<p data-x={raw}></p>',
  '<a href={raw}>{line}</a>',
  '<input type="text" value={raw} />',
  '<select><option value={raw}>{line}</option></select>',
  '<input list="linked" value={raw} /><datalist id="linked"><option value={raw}></option></datalist>',
  '<p>{line}</p>',
  '<p title={line}></p>',
  '<p>{count}</p>',
  '<p {...lineRecord}></p>',
];

const BRAND_FORGING_RED: readonly CaseLine[] = [
  "import { sanitizeForDisplay } from '../display/untrusted-text';",
  "import type { DisplayLine } from '../display/untrusted-text';",
  '',
  'export {};',
  'function make<T>(): T | undefined { return undefined; }',
  'declare function fill(items: string[]): void;',
  "const made = sanitizeForDisplay('x');",
  '',
  { code: "export const asserted = 'x' as DisplayLine;", reports: 'assertion' },
  { code: "export const angled = <DisplayLine>'x';", reports: 'assertion' },
  {
    code: 'export function isLine(value: string): value is DisplayLine { return value.length > 0; }',
    reports: 'predicate',
  },
  { code: 'export declare const declared: DisplayLine;', reports: 'declaration' },
  { code: 'export const typed = make<DisplayLine>();', reports: 'typeArgument' },
  { code: 'export const held: DisplayLine[] = [made];', reports: 'writable' },
  { code: 'fill([made]);', reports: 'writableArgument' },
  {
    code: 'export const deep: string[][][][][][][][][][][][][][] = [];',
    reports: 'writableTooDeep',
  },
];

const BRAND_FORGING_GREEN: readonly CaseLine[] = [
  "import { sanitizeForDisplay } from '../display/untrusted-text';",
  "import type { DisplayLine } from '../display/untrusted-text';",
  '',
  'export {};',
  'declare const unknownValue: unknown;',
  "const made = sanitizeForDisplay('x');",
  '',
  "export const fromMaker = sanitizeForDisplay('x');",
  'export const readonlyHeld: readonly DisplayLine[] = [made];',
  'export const plain = unknownValue as string;',
];

const HIDDEN_BY_HTML_COMMENT: readonly CaseLine[] = [
  '<script lang="ts">',
  '  export {};',
  '  const raw: string = String(Date.now());',
  '</script>',
  '',
  '<!-- eslint-disable-next-line svelte/no-at-html-tags -->',
  { code: '<p>{@html raw}</p>', reports: 'svelte/no-at-html-tags' },
];

const HIDDEN_BY_CONFIG_COMMENT: readonly CaseLine[] = [
  '/* eslint yagura/no-brand-forging: "off" */',
  "import type { DisplayLine } from '../display/untrusted-text';",
  '',
  'export {};',
  { code: "export const claimed = 'x' as DisplayLine;", reports: 'yagura/no-brand-forging' },
];

describe('the display guards report what they guard', () => {
  // The project service keeps the content it was last handed for a path, so the fixtures are
  // linted again as on disk before any other source is.
  afterAll(async () => {
    for (const path of [GUARD_CASE_TS, GUARD_CASE_SVELTE]) {
      await eslint.lintText(diskSource(path), { filePath: path });
    }
  }, RESOLVE_TIMEOUT_MS);

  it(
    'refuses text that script writes or hands to the DOM, and nothing else',
    async () => {
      const red = await lintCase(eslint, GUARD_CASE_TS, OUTSIDE_TEMPLATE_RED_TS);
      const redSvelte = await lintCase(eslint, GUARD_CASE_SVELTE, OUTSIDE_TEMPLATE_RED_SVELTE);
      const green = await lintCase(eslint, GUARD_CASE_TS, OUTSIDE_TEMPLATE_GREEN_TS);

      expect.soft([...unparsed(red), ...unparsed(redSvelte), ...unparsed(green)]).toEqual([]);
      expect
        .soft(reportsOf(OUTSIDE_TEMPLATE_RULE, OUTSIDE_TEMPLATE_RED_TS, red))
        .toEqual(expectedReports(OUTSIDE_TEMPLATE_RED_TS));
      expect
        .soft(reportsOf(OUTSIDE_TEMPLATE_RULE, OUTSIDE_TEMPLATE_RED_SVELTE, redSvelte))
        .toEqual(expectedReports(OUTSIDE_TEMPLATE_RED_SVELTE));
      expect.soft(reportsOf(OUTSIDE_TEMPLATE_RULE, OUTSIDE_TEMPLATE_GREEN_TS, green)).toEqual([]);
      const routes = anyGuardRoutes(OUTSIDE_TEMPLATE_RED_TS, red, displayGuardNames());
      expect(routes.reported).toEqual(routes.expected);
    },
    RESOLVE_TIMEOUT_MS
  );

  it(
    'refuses unprepared text and raw markup in a template, and nothing else',
    async () => {
      const red = await lintCase(eslint, GUARD_CASE_SVELTE, DISPLAY_TEXT_RED);
      const green = await lintCase(eslint, GUARD_CASE_SVELTE, DISPLAY_TEXT_GREEN);

      expect.soft([...unparsed(red), ...unparsed(green)]).toEqual([]);
      expect
        .soft(reportsOf(DISPLAY_TEXT_RULE, DISPLAY_TEXT_RED, red))
        .toEqual(expectedReports(DISPLAY_TEXT_RED));
      expect(reportsOf(DISPLAY_TEXT_RULE, DISPLAY_TEXT_GREEN, green)).toEqual([]);
    },
    RESOLVE_TIMEOUT_MS
  );

  it(
    'refuses a brand claimed outside its maker, and nothing else',
    async () => {
      const red = await lintCase(eslint, GUARD_CASE_TS, BRAND_FORGING_RED);
      const green = await lintCase(eslint, GUARD_CASE_TS, BRAND_FORGING_GREEN);

      expect.soft([...unparsed(red), ...unparsed(green)]).toEqual([]);
      expect
        .soft(reportsOf(BRAND_FORGING_RULE, BRAND_FORGING_RED, red))
        .toEqual(expectedReports(BRAND_FORGING_RED));
      expect(reportsOf(BRAND_FORGING_RULE, BRAND_FORGING_GREEN, green)).toEqual([]);
    },
    RESOLVE_TIMEOUT_MS
  );

  it(
    'reports a guard an inline directive would silence, once directives are ignored',
    async () => {
      const html = await lintCase(directiveBlindEslint, GUARD_CASE_SVELTE, HIDDEN_BY_HTML_COMMENT);
      const config = await lintCase(directiveBlindEslint, GUARD_CASE_TS, HIDDEN_BY_CONFIG_COMMENT);
      const guards = displayGuardNames();

      expect.soft([...unparsed(html), ...unparsed(config)]).toEqual([]);
      expect({
        html: guardReportsOf(HIDDEN_BY_HTML_COMMENT, html, guards),
        config: guardReportsOf(HIDDEN_BY_CONFIG_COMMENT, config, guards),
      }).toEqual({
        html: expectedReports(HIDDEN_BY_HTML_COMMENT),
        config: expectedReports(HIDDEN_BY_CONFIG_COMMENT),
      });
    },
    RESOLVE_TIMEOUT_MS
  );
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
      const guards = displayGuardNames();

      const results = await directiveBlindEslint.lintFiles(paths);
      const messages = results.flatMap((result) =>
        result.messages.map((message) => ({ file: result.filePath, ...message }))
      );

      expect(results.length).toBe(paths.length);
      expect(messages.filter((message) => message.ruleId === null)).toEqual([]);
      expect(
        messages
          .filter(({ ruleId }) => ruleId !== null && guards.has(ruleId))
          .map(({ file, line, ruleId, message }) => `${file}:${line} ${ruleId} ${message}`)
      ).toEqual([]);
    },
    RESOLVE_TIMEOUT_MS
  );
});
