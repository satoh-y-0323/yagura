/**
 * Whether a window shows the mark that says its child is connected to
 * yagura over MCP. Pure so {@link McpMark.svelte} has no branching logic of
 * its own.
 */
import type { hints } from '../ui-messages';

/** What to show for a window whose MCP connection is known and present. */
export interface McpMark {
  /** The `hints` key holding the tooltip / accessible-name text. */
  tooltipKey: keyof typeof hints;
}

/**
 * The mark to show for a window, `null` when nothing should show.
 *
 * Only a known connection (`true`) shows it: a known absence (`false`) and
 * an unknown state (`null`) both show nothing, so the view never claims a
 * connection it has not seen.
 * @param mcpConnected the window's MCP connection state
 */
export function mcpMarkOf(mcpConnected: boolean | null): McpMark | null {
  return mcpConnected === true ? { tooltipKey: 'mcpConnected' } : null;
}
