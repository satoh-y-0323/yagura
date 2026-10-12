import { describe, it, expect } from 'vitest';

import { describeWindowListTitle } from './ui-messages';

describe('describeWindowListTitle', () => {
  it.each([0, 1, 32])('appends the count %i to the window list title', (count) => {
    expect(describeWindowListTitle(count)).toBe(`Open windows (${count})`);
  });
});
