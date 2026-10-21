import { themeOfPalette, type TerminalTheme } from '../components/terminal-theme';
import type { Palette } from './palette';
import { resolveWindowPaletteFromState } from './theme-resolve';
import type { ThemeState, ThemeStore } from './theme-store';

/** An element whose CSS custom properties a palette is written to. */
export interface PaletteTarget {
  style: { setProperty(name: string, value: string): void };
}

/** A terminal whose colours follow a palette. */
export interface TerminalThemeTarget {
  setTheme: (theme: Partial<TerminalTheme>) => void;
}

/**
 * Writes a palette to a target as CSS custom properties, one per key, with
 * the `--` prefix the stylesheets read them by.
 */
export function applyPalette(target: PaletteTarget, palette: Palette): void {
  for (const [key, value] of Object.entries(palette)) {
    target.style.setProperty(`--${key}`, value);
  }
}

/**
 * Points a terminal at a palette. The mapping from palette keys to terminal
 * colours stays in `terminal-theme`, so a token that is missing is omitted
 * there rather than being decided again here.
 */
export function applyTerminalTheme(target: TerminalThemeTarget, palette: Palette): void {
  target.setTheme(themeOfPalette(palette));
}

/**
 * Keeps one window's terminal on the palette that window resolves to, from
 * the moment it is bound until the returned function is called.
 */
export function bindTerminalTheme(
  store: Pick<ThemeStore, 'subscribe'>,
  params: { windowId: string; vendor: string | null },
  target: TerminalThemeTarget
): () => void {
  return store.subscribe((state) => {
    applyTerminalTheme(
      target,
      resolveWindowPaletteFromState(state, params.vendor, params.windowId)
    );
  });
}

/**
 * Keeps one element's custom properties on the palette `resolve` picks out of
 * the state, from the moment it is bound until the returned function is
 * called.
 */
export function bindPalette(
  store: Pick<ThemeStore, 'subscribe'>,
  resolve: (state: ThemeState) => Palette,
  target: PaletteTarget
): () => void {
  return store.subscribe((state) => {
    applyPalette(target, resolve(state));
  });
}
