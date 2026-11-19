/**
 * The built-in chord of every action, per host platform, or `null` for an action
 * that ships without one. A keymap file only overrides actions; everything it
 * leaves out falls back to this table.
 */

import type { KeyboardPlatform } from './keyboard-platform';
import type { ActionId } from './key-binding';

/** The default chord of every action, or `null` for an action without one. */
export type DefaultKeys = Readonly<Record<ActionId, string | null>>;

const WINDOWS_AND_LINUX: DefaultKeys = Object.freeze({
  'open-window-picker': 'Ctrl+Shift+o',
  'open-command-palette': 'Ctrl+Shift+p',
  'focus-next-window': 'Ctrl+Shift+f',
  'focus-previous-window': 'Ctrl+Shift+b',
  'focus-next-attention': 'Ctrl+Shift+j',
  'open-window-form': 'Ctrl+Shift+n',
  'close-window': 'Ctrl+Shift+w',
  'interrupt-window': 'Ctrl+Shift+k',
  'rename-window': null,
  'minimize-window': 'Ctrl+Shift+m',
  'toggle-maximize-window': 'Ctrl+Shift+Enter',
  'toggle-work-mode': null,
  'tile-windows': null,
  'toggle-sidebar-focus': 'Ctrl+Shift+e',
  'toggle-needs-you-filter': null,
  'toggle-chat': 'Ctrl+Shift+l',
  'toggle-plans': 'Ctrl+Shift+t',
  'toggle-settings': 'Ctrl+Shift+s',
});

// Cmd is the mac convention; the app menu owns Cmd+Q/W/H/M and the edit chords.
const MAC: DefaultKeys = Object.freeze({
  'open-window-picker': 'Meta+p',
  'open-command-palette': 'Shift+Meta+p',
  'focus-next-window': 'Shift+Meta+]',
  'focus-previous-window': 'Shift+Meta+[',
  'focus-next-attention': 'Shift+Meta+a',
  'open-window-form': 'Meta+n',
  'close-window': 'Shift+Meta+w',
  'interrupt-window': 'Meta+.',
  'rename-window': null,
  'minimize-window': 'Shift+Meta+m',
  'toggle-maximize-window': 'Shift+Meta+Enter',
  'toggle-work-mode': null,
  'tile-windows': null,
  'toggle-sidebar-focus': 'Shift+Meta+e',
  'toggle-needs-you-filter': null,
  'toggle-chat': 'Shift+Meta+l',
  'toggle-plans': 'Shift+Meta+t',
  'toggle-settings': 'Meta+,',
});

/**
 * The default chord of every action on `platform`, in the canonical chord form,
 * or `null` where it has none. Windows and Linux share one table. The returned table is frozen.
 */
export function defaultKeysFor(platform: KeyboardPlatform): DefaultKeys {
  return platform === 'mac' ? MAC : WINDOWS_AND_LINUX;
}
