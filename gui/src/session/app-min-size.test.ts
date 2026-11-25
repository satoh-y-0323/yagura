import { describe, expect, it } from 'vitest';

import { MIN_WINDOW_SIZE } from '../layout/float-layout';
import { SIDEBAR_WIDTH_MAX } from '../layout/sidebar-width';
import { appMinSize } from './app-min-size';

describe('appMinSize', () => {
  it('leaves room for the widest sidebar and one window at its minimum beside the rail', () => {
    const size = appMinSize({ railRight: 44, dividerWidth: 5, railContentBottom: 268.4 });

    expect(size).toEqual({
      width: 44 + SIDEBAR_WIDTH_MAX + 5 + MIN_WINDOW_SIZE.width,
      height: 269,
    });
  });

  it('rounds a fractional width up, so no pixel of the shell is cut', () => {
    expect(appMinSize({ railRight: 44.2, dividerWidth: 5, railContentBottom: 1 }).width).toBe(
      45 + SIDEBAR_WIDTH_MAX + 5 + MIN_WINDOW_SIZE.width
    );
  });
});
