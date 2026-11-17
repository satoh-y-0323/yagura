// Components read colors from tokens and keep the keyboard focus ring visible.
const COLOR_RULES = {
  'color-no-hex': true,
  'color-named': 'never',
  'function-disallowed-list': ['rgb', 'rgba', 'hsl', 'hsla'],
};

// Files that still break a rule. Each leaves this list in the unit named beside it.
const MIGRATING = [
  // rgba() fallback for the backdrop; moves to the derived --backdrop token (overlay unit).
  'src/components/ModalDialog.svelte',
  // outline: none on the invisible typing input, whose focus the pane shows; decided with the terminal pane.
  'src/components/TerminalPane.svelte',
];

export default {
  overrides: [
    { files: ['**/*.svelte'], customSyntax: 'postcss-html' },
    // The palette is where color values live.
    {
      files: ['src/styles/tokens.css'],
      rules: Object.fromEntries(Object.keys(COLOR_RULES).map((name) => [name, null])),
    },
  ],
  ignoreFiles: MIGRATING,
  rules: {
    ...COLOR_RULES,
    'declaration-property-value-disallowed-list': {
      outline: ['none', '0'],
    },
  },
};
