/**
 * The built-in chord of every action, per host platform. A keymap file only
 * overrides actions; everything it leaves out falls back to this table.
 */

import type { KeyboardPlatform } from './keyboard-platform';
import type { ActionId } from './key-binding';

type DefaultKeys = Readonly<Record<ActionId, string>>;

const WINDOWS_AND_LINUX: DefaultKeys = Object.freeze({
  'focus-next-window': 'Ctrl+Tab',
  'focus-previous-window': 'Ctrl+Shift+Tab',
  'focus-next-attention': 'Ctrl+Shift+a',
  'open-window-form': 'Ctrl+Shift+n',
  'close-window': 'Ctrl+Shift+w',
  'interrupt-window': 'Ctrl+Shift+k',
  'minimize-window': 'Ctrl+Shift+m',
  'toggle-maximize-window': 'Ctrl+Shift+Enter',
  'toggle-sidebar-focus': 'Ctrl+Shift+e',
  'toggle-chat': 'Ctrl+Shift+l',
  'toggle-plans': 'Ctrl+Shift+p',
  'toggle-settings': 'Ctrl+,',
});

// Cmd is the mac convention; the app menu owns Cmd+Q/W/H/M and the edit chords.
const MAC: DefaultKeys = Object.freeze({
  'focus-next-window': 'Ctrl+Tab',
  'focus-previous-window': 'Ctrl+Shift+Tab',
  'focus-next-attention': 'Shift+Meta+a',
  'open-window-form': 'Meta+n',
  'close-window': 'Shift+Meta+w',
  'interrupt-window': 'Meta+.',
  'minimize-window': 'Shift+Meta+m',
  'toggle-maximize-window': 'Shift+Meta+Enter',
  'toggle-sidebar-focus': 'Shift+Meta+e',
  'toggle-chat': 'Shift+Meta+l',
  'toggle-plans': 'Shift+Meta+p',
  'toggle-settings': 'Meta+,',
});

/**
 * The default chord of every action on `platform`, in the canonical chord form.
 * Windows and Linux share one table. The returned table is frozen.
 */
export function defaultKeysFor(platform: KeyboardPlatform): DefaultKeys {
  return platform === 'mac' ? MAC : WINDOWS_AND_LINUX;
}
