/**
 * The sole place in the GUI that touches the system clipboard. It goes through
 * the native side rather than `navigator.clipboard`, whose read asks the user
 * for permission differently in each platform's web view.
 */
import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager';

/** The clipboard's text, or an empty string when it holds no text. */
export async function readClipboardText(): Promise<string> {
  try {
    return await readText();
  } catch (error) {
    // The plugin rejects when the clipboard holds something other than text.
    console.warn('failed to read text from the clipboard:', error);
    return '';
  }
}

/** Replaces the clipboard's contents with `text`. */
export async function writeClipboardText(text: string): Promise<void> {
  await writeText(text);
}
