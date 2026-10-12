import { describe, expect, it } from 'vitest';

import { CLIPBOARD_CHORDS } from './clipboard-chords';

describe('CLIPBOARD_CHORDS', () => {
  it('copies and pastes with Ctrl+Shift+C / V where Ctrl+C interrupts, and Cmd+C / V on macOS', () => {
    expect(CLIPBOARD_CHORDS).toEqual({
      windows: { copy: 'Ctrl+Shift+c', paste: 'Ctrl+Shift+v' },
      linux: { copy: 'Ctrl+Shift+c', paste: 'Ctrl+Shift+v' },
      mac: { copy: 'Meta+c', paste: 'Meta+v' },
    });
  });
});
