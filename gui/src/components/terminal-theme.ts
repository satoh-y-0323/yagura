import terminalTokens from '../../../crates/yagura-protocol/terminal-tokens.json';
import { hasOwnKey } from '../theme/palette';

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
 * Which palette token (design token) each terminal colour is read from, as a
 * CSS custom property name, so terminal colours are never hard-coded and
 * theme switching stays driven by the tokens alone. The mapping lives in
 * `terminal-tokens.json`, which the daemon reads too.
 *
 * The typed binding pins the key set to `TerminalTheme`, making a missing key
 * a compile error, while the exported index-signature type keeps string-keyed
 * lookups usable.
 */
const TOKEN_OF_KEY: Readonly<Record<keyof TerminalTheme, string>> = terminalTokens.tokens;

export const THEME_TOKENS: Readonly<Record<string, string>> = Object.fromEntries(
  Object.entries(TOKEN_OF_KEY).map(([key, token]) => [key, `--${token}`])
);

function isThemeKey(key: string): key is keyof TerminalTheme {
  return hasOwnKey(TOKEN_OF_KEY, key);
}

/** The 16 ANSI colours of a theme, in palette order (0-15). */
export const ANSI_THEME_KEYS: readonly (keyof TerminalTheme)[] =
  terminalTokens.ansi.filter(isThemeKey);

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

/**
 * The colours a palette gives, read by token name without the leading `--`,
 * dropping those it lacks.
 */
export function themeOfPalette(palette: Readonly<Record<string, string>>): Partial<TerminalTheme> {
  return buildTheme((token) => palette[token.replace(/^--/, '')] ?? '');
}
