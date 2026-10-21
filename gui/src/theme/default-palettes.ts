import { themeOfPalette } from '../components/terminal-theme';
import type { WirePalette } from '../ipc/session-client';
import { completeTheme, wirePaletteOf } from '../terminal/terminal-colors';
import type { Palette } from './palette';
import { resolveWindowPalette, type WindowPaletteSource } from './theme-resolve';
import type { ThemeStore } from './theme-store';

/**
 * The colors a window that has no view yet is answered with: what the GUI
 * would draw it with once it shows it, before any override.
 */
export interface DefaultPalettes {
  app: WirePalette;
  /** Only the vendors whose palette differs from the application's. */
  vendors: Record<string, WirePalette>;
}

/** A palette as the daemon answers color queries with it. */
export function wirePaletteOfPalette(palette: Palette): WirePalette {
  return wirePaletteOf(completeTheme(themeOfPalette(palette)));
}

/**
 * The default palettes of a theme state, resolved by the same rule a window
 * is drawn by (`resolveWindowPalette`) with no window override.
 */
export function defaultPalettesOf(state: WindowPaletteSource): DefaultPalettes {
  const vendors: Record<string, WirePalette> = Object.create(null) as Record<string, WirePalette>;
  for (const vendor of Object.keys(state.vendorAssignments)) {
    const palette = resolveWindowPalette(
      vendor,
      null,
      state.vendorAssignments,
      state.userPalettes,
      state.appPalette
    );
    if (palette !== state.appPalette) {
      vendors[vendor] = wirePaletteOfPalette(palette);
    }
  }
  return { app: wirePaletteOfPalette(state.appPalette), vendors };
}

/**
 * Sends the default palettes now and on every change of the theme state,
 * until the returned function is called. Start it only once the themes file
 * has been read: before that, the state lacks the user's themes and
 * assignments, and sending it would replace the palettes the daemon saved.
 */
export function startDefaultPaletteSync(
  store: Pick<ThemeStore, 'subscribe'>,
  send: (palettes: DefaultPalettes) => Promise<void>
): () => void {
  return store.subscribe((state) => {
    send(defaultPalettesOf(state)).catch((error: unknown) => {
      console.error('failed to send the default palettes:', error);
    });
  });
}
