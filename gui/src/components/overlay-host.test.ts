/**
 * Floating UI (tooltips, menus) is drawn inside the topmost open modal dialog,
 * or in the app's overlay layer when no modal contains the caller. A fake DOM
 * with only `closest` and `querySelector` is enough for that decision.
 */
import { describe, it, expect } from 'vitest';

interface Slot {
  name: string;
}
interface FakeDialog {
  querySelector(selector: string): Slot | null;
}

type ResolveOverlayHost = (
  origin: { closest(selector: string): unknown },
  root: { querySelector(selector: string): unknown }
) => unknown;

async function loadResolveOverlayHost(): Promise<ResolveOverlayHost> {
  const module = (await import('./overlay-host')) as Record<string, unknown>;
  return module['resolveOverlayHost'] as ResolveOverlayHost;
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
  it('returns the open modal’s own host, not the app layer, when the caller is inside an open modal', async () => {
    const resolve = await loadResolveOverlayHost();
    const dialog = dialogWithHost();
    const insideModal = {
      closest: (selector: string) => (selector === 'dialog[open]' ? dialog : null),
    };

    expect(resolve(insideModal, root)).toBe(DIALOG_HOST);
  });

  it('returns the app overlay layer when no open modal contains the caller', async () => {
    const resolve = await loadResolveOverlayHost();
    const outsideModal = { closest: () => null };

    expect(resolve(outsideModal, root)).toBe(APP_LAYER);
  });
});
