<script lang="ts">
  /**
   * The plug mark shown next to a window whose child is connected to yagura
   * over MCP. Renders nothing while the connection is false or
   * unknown; see {@link mcpMarkOf}.
   */
  import { mcpMarkOf } from './mcp-mark';
  import { hints } from '../ui-messages';

  export let mcpConnected: boolean | null;

  $: mark = mcpMarkOf(mcpConnected);
</script>

{#if mark !== null}
  <svg
    class="mcp-mark"
    role="img"
    aria-label={hints[mark.tooltipKey]}
    focusable="false"
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <title>{hints[mark.tooltipKey]}</title>
    <path d="M9 8V2" />
    <path d="M15 8V2" />
    <path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z" />
    <path d="M12 22v-5" />
  </svg>
{/if}

<style>
  .mcp-mark {
    flex: 0 0 auto;
    color: var(--color-success);
  }
</style>
