/**
 * Floating UI (tooltips, menus) is drawn inside the topmost open modal dialog,
 * or in the app's overlay layer when no modal contains the caller. A fake DOM
 * with only `closest` and `querySelector` is enough for that decision.
 */
import { describe, it, expect } from 'vitest';

import { resolveOverlayHost } from './overlay-host';

interface Slot {
  name: string;
}
interface FakeDialog {
  querySelector(selector: string): Slot | null;
}

const APP_LAYER: Slot = { name: 'app overlay layer' };
const DIALOG_HOST: Slot = { name: 'overlay host inside the open dialog' };

const root = {
  querySelector: (selector: string): Slot | null =>
    selector === '#overlay-layer' ? APP_LAYER : null,
};

function dialogWithHost(): FakeDialog {
  return {
    querySelector: (selector) => (selector === '.modal-overlay-host' ? DIALOG_HOST : null),
  };
}

describe('resolveOverlayHost: where a floating element is drawn', () => {
  it('returns the open modal’s own host, not the app layer, when the caller is inside an open modal', () => {
    const dialog = dialogWithHost();
    const insideModal = {
      closest: (selector: string) => (selector === 'dialog[open]' ? dialog : null),
    };

    expect(resolveOverlayHost(insideModal, root)).toBe(DIALOG_HOST);
  });

  it('returns the app overlay layer when no open modal contains the caller', () => {
    const outsideModal = { closest: () => null };

    expect(resolveOverlayHost(outsideModal, root)).toBe(APP_LAYER);
  });
});
