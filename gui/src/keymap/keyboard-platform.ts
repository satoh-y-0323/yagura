/** The keyboard conventions a shortcut is judged and displayed under. */
export type KeyboardPlatform = 'windows' | 'mac' | 'linux';

/**
 * Maps a `navigator.platform` string to a keyboard platform. Anything that is
 * neither Windows nor Apple is treated as Linux.
 */
export function keyboardPlatformOf(navigatorPlatform: string): KeyboardPlatform {
  if (navigatorPlatform.startsWith('Win')) return 'windows';
  if (
    navigatorPlatform.startsWith('Mac') ||
    navigatorPlatform.startsWith('iPhone') ||
    navigatorPlatform.startsWith('iPad')
  ) {
    return 'mac';
  }
  return 'linux';
}

/**
 * The running host's keyboard platform. Reads `navigator` defensively so the
 * module still imports under vitest's node environment.
 */
export function hostKeyboardPlatform(): KeyboardPlatform {
  return typeof navigator === 'undefined' ? 'linux' : keyboardPlatformOf(navigator.platform);
}
