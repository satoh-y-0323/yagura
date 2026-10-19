import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import svelteConfig from './svelte.config.js';

// Shared type-aware parserOptions applied to both .ts and .svelte files.
const typeAwareParserOptions = {
  projectService: true,
  extraFileExtensions: ['.svelte'],
  tsconfigRootDir: import.meta.dirname,
};

const PRODUCTION_SOURCES = ['src/**/*.ts', 'src/**/*.svelte'];
const TEST_SOURCES = ['**/*.test.ts'];

// Text reaching the DOM goes through Svelte's escaping; `{@html}` is refused by svelte/no-at-html-tags.
const RAW_HTML_MESSAGE = 'Render text through Svelte expressions, not raw HTML.';
const NO_RAW_HTML = [
  {
    selector: 'MemberExpression[property.name=/^(innerHTML|outerHTML)$/]',
    message: RAW_HTML_MESSAGE,
  },
  {
    selector: 'MemberExpression[property.value=/^(innerHTML|outerHTML)$/]',
    message: RAW_HTML_MESSAGE,
  },
  {
    selector: "CallExpression[callee.property.name='insertAdjacentHTML']",
    message: RAW_HTML_MESSAGE,
  },
  {
    selector: 'CallExpression[callee.property.name=/^(createContextualFragment|setHTMLUnsafe)$/]',
    message: RAW_HTML_MESSAGE,
  },
  {
    selector:
      "CallExpression[callee.object.name='document'][callee.property.name=/^(write|writeln)$/]",
    message: RAW_HTML_MESSAGE,
  },
  {
    selector: "SvelteAttribute[key.name='srcdoc']",
    message: RAW_HTML_MESSAGE,
  },
  {
    selector: "SvelteDirective[kind='Binding'][key.name.name=/^(innerHTML|outerHTML)$/]",
    message: RAW_HTML_MESSAGE,
  },
];

// UI text is English and lives in i18n/messages-en.ts; comments are not UI text.
const JAPANESE = '[\\u3040-\\u30ff\\u4e00-\\u9fff]';
const JAPANESE_MESSAGE = 'UI text is English; put it in i18n/messages-en.ts.';
const NO_JAPANESE_TEXT = [
  { selector: `Literal[value=/${JAPANESE}/]`, message: JAPANESE_MESSAGE },
  { selector: `TemplateElement[value.raw=/${JAPANESE}/]`, message: JAPANESE_MESSAGE },
  { selector: `SvelteText[value=/${JAPANESE}/]`, message: JAPANESE_MESSAGE },
  { selector: `SvelteLiteral[value=/${JAPANESE}/]`, message: JAPANESE_MESSAGE },
];

// Shortcuts are dispatched from one window-level keydown listener, which App.svelte owns.
const GLOBAL_KEYDOWN_MESSAGE = 'App.svelte owns the one window-level keydown listener.';
const SPECIAL_WINDOW_ELEMENT =
  "SvelteElement[kind='special'][name.name=/^svelte:(window|document|body)$/]";
const NO_GLOBAL_KEYDOWN = [
  {
    selector:
      "CallExpression[callee.object.name=/^(window|document)$/][callee.property.name='addEventListener'][arguments.0.value='keydown']",
    message: GLOBAL_KEYDOWN_MESSAGE,
  },
  {
    selector: `${SPECIAL_WINDOW_ELEMENT} SvelteDirective[kind='EventHandler'][key.name.name='keydown']`,
    message: GLOBAL_KEYDOWN_MESSAGE,
  },
  {
    selector: `${SPECIAL_WINDOW_ELEMENT} SvelteAttribute[key.name='onkeydown']`,
    message: GLOBAL_KEYDOWN_MESSAGE,
  },
];

// Visible text and accessible names come from ui-messages, so every locale reaches them.
const NO_LITERAL_MARKUP_TEXT = [
  {
    selector: 'SvelteText[value=/[A-Za-z]/]:not(SvelteStyleElement > SvelteText)',
    message: 'Take visible text from ui-messages.',
  },
  {
    selector:
      'SvelteAttribute[key.name=/^(aria-label|title|placeholder|alt)$/] > SvelteLiteral[value=/[A-Za-z]/]',
    message: 'Take accessible names from ui-messages.',
  },
];

// Theme resolution treats every vendor the same; vendor names are data, not code.
const VENDOR_NAMES = '/claude|codex|gemini|grok|copilot|anthropic|openai/i';
const VENDOR_MESSAGE = 'Theme resolution must not special-case a vendor.';
const NO_VENDOR_NAMES = [
  { selector: `Identifier[name=${VENDOR_NAMES}]`, message: VENDOR_MESSAGE },
  { selector: `Literal[value=${VENDOR_NAMES}]`, message: VENDOR_MESSAGE },
  { selector: `TemplateElement[value.raw=${VENDOR_NAMES}]`, message: VENDOR_MESSAGE },
];

// A button named by aria-label alone shows no tooltip; IconButton derives both from one label.
const NO_BARE_LABELLED_BUTTON = [
  {
    selector:
      "SvelteStartTag[parent.kind='html'][parent.name.name='button']:has(> SvelteAttribute[key.name='aria-label'])",
    message:
      'Use IconButton: it takes one required label and sets aria-label and the tooltip from it.',
  },
  // Button text comes from the i18n messages through `{...}`, so a button with none shows only an icon.
  {
    selector:
      "SvelteElement[kind='html'][name.name='button']:not(:has(SvelteMustacheTag:not(SvelteStartTag SvelteMustacheTag)))",
    message:
      'Use IconButton: a button with no text shows only an icon, and needs a label and a tooltip.',
  },
];

// What is typed here is read by a machine (mentions, commands, paths, names), and macOS WebKit
// rewrites quotes, dashes and words as they are typed unless spellcheck is off for the field.
const NO_TEXT_SUBSTITUTION_MESSAGE =
  'Set spellcheck="false" on a text field: macOS rewrites quotes and words typed into it.';
const SPELLCHECK_OFF = "SvelteAttribute[key.name='spellcheck'][value.0.value='false']";
const NON_TEXT_INPUT =
  "SvelteAttribute[key.name='type'][value.0.value=/^(checkbox|radio|range|color|file|button|submit|reset|hidden)$/]";
const NO_TEXT_SUBSTITUTION = [
  {
    selector: `SvelteStartTag[parent.kind='html'][parent.name.name='textarea']:not(:has(> ${SPELLCHECK_OFF}))`,
    message: NO_TEXT_SUBSTITUTION_MESSAGE,
  },
  {
    selector: `SvelteStartTag[parent.kind='html'][parent.name.name='input']:not(:has(> ${NON_TEXT_INPUT})):not(:has(> ${SPELLCHECK_OFF}))`,
    message: NO_TEXT_SUBSTITUTION_MESSAGE,
  },
];

const SOURCE_SYNTAX = [
  ...NO_RAW_HTML,
  ...NO_JAPANESE_TEXT,
  ...NO_GLOBAL_KEYDOWN,
  ...NO_BARE_LABELLED_BUTTON,
  ...NO_TEXT_SUBSTITUTION,
];

export default [
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  // Must stay after the typescript-eslint base, which assigns the TS parser to every file.
  ...svelte.configs.recommended,
  {
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.node,
        ...globals.es2021,
      },
    },
  },
  {
    files: ['**/*.ts'],
    languageOptions: {
      parserOptions: { ...typeAwareParserOptions },
    },
  },
  {
    files: ['**/*.svelte'],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        ...typeAwareParserOptions,
        svelteConfig,
      },
    },
  },
  {
    files: ['**/*.ts', '**/*.svelte'],
    rules: {
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
      '@typescript-eslint/no-non-null-assertion': 'error',
      // An angle-bracket assertion carries no `as`, so the IPC boundary scan cannot see one.
      '@typescript-eslint/consistent-type-assertions': [
        'error',
        { assertionStyle: 'as', objectLiteralTypeAssertions: 'allow' },
      ],
    },
  },
  {
    // A later block's no-restricted-syntax replaces an earlier one for the same file, so each
    // narrower block below repeats the selectors of the wider ones.
    files: PRODUCTION_SOURCES,
    ignores: TEST_SOURCES,
    rules: {
      'no-restricted-syntax': ['error', ...SOURCE_SYNTAX],
    },
  },
  {
    files: ['src/**/*.svelte'],
    ignores: ['src/App.svelte'],
    rules: {
      'no-restricted-syntax': ['error', ...SOURCE_SYNTAX, ...NO_LITERAL_MARKUP_TEXT],
    },
  },
  {
    files: ['src/App.svelte'],
    rules: {
      'no-restricted-syntax': [
        'error',
        ...NO_RAW_HTML,
        ...NO_JAPANESE_TEXT,
        ...NO_BARE_LABELLED_BUTTON,
        ...NO_TEXT_SUBSTITUTION,
        ...NO_LITERAL_MARKUP_TEXT,
      ],
    },
  },
  {
    files: ['src/theme/theme-resolve.ts'],
    rules: {
      'no-restricted-syntax': ['error', ...SOURCE_SYNTAX, ...NO_VENDOR_NAMES],
    },
  },
  {
    // Only ui-messages.ts reads the locale resources, so every caller gets the active locale.
    files: PRODUCTION_SOURCES,
    ignores: [...TEST_SOURCES, 'src/ui-messages.ts', 'src/i18n/**'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          patterns: [{ regex: '(^|/)i18n(/|$)', message: 'Read UI text through ui-messages.ts.' }],
        },
      ],
    },
  },
  {
    // Display-only parts read props and call back; the shell owns IPC and stores.
    files: [
      'src/banner-model.ts',
      'src/components/AddWindowDialog.svelte',
      'src/components/AppRail.svelte',
      'src/components/ApprovalSettings.svelte',
      'src/components/IntegrationSettings.svelte',
      'src/components/KeyboardSettings.svelte',
      'src/components/LaunchProfileFields.svelte',
      'src/components/PlanBoard.svelte',
      'src/components/PlansPanel.svelte',
      'src/components/PopupMenu.svelte',
      'src/components/ProfileSettings.svelte',
      'src/components/RolesEditor.svelte',
      'src/components/SettingsPanel.svelte',
      'src/components/SettingsSwitch.svelte',
      'src/components/TabBar.svelte',
      'src/components/TerminalContextMenu.svelte',
      'src/components/WindowControls.svelte',
      'src/components/WindowMenu.svelte',
    ],
    rules: {
      '@typescript-eslint/no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              regex: '(^|/)(ipc|stores)/|^@tauri-apps/',
              allowTypeImports: true,
              message:
                'A display-only part reads props and calls back; the shell owns IPC and stores.',
            },
          ],
        },
      ],
    },
  },
  {
    // The approval dialog never moves focus, so a keystroke meant elsewhere cannot decide a request.
    files: ['src/components/ApprovalDialog.svelte'],
    rules: {
      'no-restricted-properties': [
        'error',
        { property: 'focus', message: 'The approval dialog must not take focus.' },
      ],
    },
  },
  {
    // Collapsed rooms are session state; they must not survive a restart.
    files: ['src/sidebar/collapsed-rooms.ts'],
    rules: {
      'no-restricted-globals': ['error', 'localStorage', 'sessionStorage'],
      'no-restricted-properties': [
        'error',
        { object: 'window', property: 'localStorage' },
        { object: 'window', property: 'sessionStorage' },
        { object: 'globalThis', property: 'localStorage' },
        { object: 'globalThis', property: 'sessionStorage' },
      ],
    },
  },
  {
    // The modules that read values arriving from the Tauri core.
    files: [
      'src/ipc/session-boundary.ts',
      'src/ipc/session-client.ts',
      'src/ipc/plan-boundary.ts',
      'src/ipc/model-boundary.ts',
      'src/ipc/mcp-registration-boundary.ts',
      'src/ipc/profile-detail-boundary.ts',
    ],
    rules: {
      'no-restricted-syntax': [
        'error',
        {
          selector: 'BinaryExpression[operator="in"]',
          message:
            'The `in` operator in an expression answers for the prototype chain, and narrowing by it makes the following read type-check. Ask an untrusted record for a field with readOwnField or ownEntries instead.',
        },
        ...SOURCE_SYNTAX,
      ],
    },
  },
  {
    ignores: [
      'dist/**',
      'scripts/**',
      'src-tauri/target/**',
      'src-tauri/target-*/**',
      'vite.config.ts',
      'svelte.config.js',
      'eslint.config.js',
    ],
  },
];
