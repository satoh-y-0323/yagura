import type { KeyboardPlatform } from './keyboard-platform';

/** The chords that copy the terminal selection and paste into the terminal. */
export interface ClipboardChords {
  copy: string;
  paste: string;
}

/**
 * Terminal convention per platform: Ctrl+Shift+C / V where Ctrl+C is SIGINT,
 * and the app menu's Cmd+C / V on macOS.
 */
export const CLIPBOARD_CHORDS: Readonly<Record<KeyboardPlatform, ClipboardChords>> = {
  windows: { copy: 'Ctrl+Shift+c', paste: 'Ctrl+Shift+v' },
  linux: { copy: 'Ctrl+Shift+c', paste: 'Ctrl+Shift+v' },
  mac: { copy: 'Meta+c', paste: 'Meta+v' },
};
