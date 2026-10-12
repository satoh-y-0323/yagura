/**
 * When a window shows the mark that says its child is connected to yagura.
 * Only a known connection shows it: a known absence and an unknown state
 * show nothing, so the view never claims a connection it has not seen.
 */
import { describe, it, expect } from 'vitest';

import { hints } from '../ui-messages';

// Loaded per case so each case fails on its own while the module is missing.
async function loadMcpMarkOf(): Promise<(typeof import('./mcp-mark'))['mcpMarkOf']> {
  const module = await import('./mcp-mark');
  return module.mcpMarkOf;
}

describe('mcpMarkOf', () => {
  it('shows the mark when the window is connected', async () => {
    const mcpMarkOf = await loadMcpMarkOf();

    expect(mcpMarkOf(true)).not.toBeNull();
  });

  it('names a tooltip that reads "Connected to yagura"', async () => {
    const mcpMarkOf = await loadMcpMarkOf();

    const mark = mcpMarkOf(true);

    expect(mark === null ? null : hints[mark.tooltipKey]).toBe('Connected to yagura');
  });

  it('shows nothing when the window is known not to be connected', async () => {
    const mcpMarkOf = await loadMcpMarkOf();

    expect(mcpMarkOf(false)).toBeNull();
  });

  it('shows nothing when the connection is unknown', async () => {
    const mcpMarkOf = await loadMcpMarkOf();

    expect(mcpMarkOf(null)).toBeNull();
  });
});
