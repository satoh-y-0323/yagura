import { describe, expect, it } from 'vitest';

import { createInputModality } from './input-modality';

function key(name: string): Event {
  return Object.assign(new Event('keydown'), { key: name });
}

describe('createInputModality', () => {
  it('counts a key as keyboard use and a pointer press as pointer use', () => {
    const target = new EventTarget();
    const modality = createInputModality(target);

    expect(modality.isKeyboard()).toBe(false);
    target.dispatchEvent(key('Tab'));
    expect(modality.isKeyboard()).toBe(true);
    target.dispatchEvent(new Event('pointerdown'));
    expect(modality.isKeyboard()).toBe(false);
  });

  it('does not count a modifier pressed on its own', () => {
    const target = new EventTarget();
    const modality = createInputModality(target);

    for (const name of ['Shift', 'Control', 'Alt', 'Meta']) {
      target.dispatchEvent(key(name));
    }
    expect(modality.isKeyboard()).toBe(false);
  });

  it('stops following the target once disposed', () => {
    const target = new EventTarget();
    const modality = createInputModality(target);

    modality.dispose();
    target.dispatchEvent(key('Tab'));
    expect(modality.isKeyboard()).toBe(false);
  });
});
