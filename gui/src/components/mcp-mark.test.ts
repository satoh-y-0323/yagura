/**
 * When a window shows the mark that says its child is connected to yagura.
 * Only a known connection shows it: a known absence and an unknown state
 * show nothing, so the view never claims a connection it has not seen.
 */
import { describe, it, expect } from 'vitest';

import { hints } from '../ui-messages';
import { mcpMarkOf } from './mcp-mark';

describe('mcpMarkOf', () => {
  it('shows the mark when the window is connected', () => {
    expect(mcpMarkOf(true)).not.toBeNull();
  });

  it('names a tooltip that reads "Connected to yagura"', () => {
    const mark = mcpMarkOf(true);

    expect(mark === null ? null : hints[mark.tooltipKey]).toBe('Connected to yagura');
  });

  it('shows nothing when the window is known not to be connected', () => {
    expect(mcpMarkOf(false)).toBeNull();
  });

  it('shows nothing when the connection is unknown', () => {
    expect(mcpMarkOf(null)).toBeNull();
  });
});
