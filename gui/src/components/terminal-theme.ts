/** The colours a window's terminal is drawn with. */
export interface TerminalTheme {
  background: string;
  foreground: string;
  cursor: string;
  selectionBackground: string;
  selectionForeground: string;
  selectionInactiveBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

/**
 * Maps each terminal colour to the CSS custom property (design token) it is
 * read from, so terminal colours are never hard-coded and theme switching
 * stays driven by the tokens alone.
 *
 * The `satisfies` clause pins the key set to `TerminalTheme`, making a typo a
 * compile error, while the declared index-signature type keeps string-keyed
 * lookups usable.
 */
export const THEME_TOKENS: Readonly<Record<string, string>> = {
  background: '--color-background',
  foreground: '--color-foreground',
  cursor: '--color-primary',
  selectionBackground: '--term-selection-background',
  selectionForeground: '--term-selection-foreground',
  selectionInactiveBackground: '--term-selection-inactive-background',
  black: '--term-ansi-black',
  red: '--term-ansi-red',
  green: '--term-ansi-green',
  yellow: '--term-ansi-yellow',
  blue: '--term-ansi-blue',
  magenta: '--term-ansi-magenta',
  cyan: '--term-ansi-cyan',
  white: '--term-ansi-white',
  brightBlack: '--term-ansi-bright-black',
  brightRed: '--term-ansi-bright-red',
  brightGreen: '--term-ansi-bright-green',
  brightYellow: '--term-ansi-bright-yellow',
  brightBlue: '--term-ansi-bright-blue',
  brightMagenta: '--term-ansi-bright-magenta',
  brightCyan: '--term-ansi-bright-cyan',
  brightWhite: '--term-ansi-bright-white',
} satisfies Record<keyof TerminalTheme, string>;

/** The 16 ANSI colours of a theme, in palette order (0-15). */
export const ANSI_THEME_KEYS = [
  'black',
  'red',
  'green',
  'yellow',
  'blue',
  'magenta',
  'cyan',
  'white',
  'brightBlack',
  'brightRed',
  'brightGreen',
  'brightYellow',
  'brightBlue',
  'brightMagenta',
  'brightCyan',
  'brightWhite',
] as const satisfies readonly (keyof TerminalTheme)[];

/**
 * Builds the theme from the keys of THEME_TOKENS, dropping every key whose
 * token resolves to an empty string.
 *
 * @param readToken Function to retrieve the value for a given token name.
 * @returns The colours that resolved.
 */
export function buildTheme(readToken: (token: string) => string): Partial<TerminalTheme> {
  const theme: Record<string, string> = {};

  for (const [key, token] of Object.entries(THEME_TOKENS)) {
    const value = readToken(token);
    if (value !== '') {
      theme[key] = value;
    }
  }

  return theme;
}
