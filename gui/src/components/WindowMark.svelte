<script lang="ts">
  /**
   * A window's mark: the 5 by 5 mirrored pattern the daemon draws from the
   * window's ID, with the ID beside it. The child cannot choose its ID, and no
   * name can draw here, so the mark tells windows apart where names may not.
   * The shape carries the difference; the hue only helps.
   */
  import type { DisplayLine } from '../display/untrusted-text';
  import type { WindowMark } from '../session/approval-target';
  import { workArea } from '../ui-messages';
  import { tooltip } from './tooltip';

  export let mark: WindowMark;
  export let windowId: DisplayLine;
  /** `card` for the approval dialog's cards, `header` for a window's header. */
  export let size: 'card' | 'header';
  /** Whether the ID is drawn beside the mark; a card draws it in a row of its own. */
  export let showsId = true;

  const ROWS = 5;
  const FREE_COLUMNS = 3;
  const COLUMNS = 5;
  const HUE_STEP_DEGREES = 30;

  /** The filled cells; the right two columns mirror the left two. */
  function filledCells(cells: number): { x: number; y: number }[] {
    const filled: { x: number; y: number }[] = [];
    for (let y = 0; y < ROWS; y += 1) {
      for (let x = 0; x < COLUMNS; x += 1) {
        const column = x < FREE_COLUMNS ? x : COLUMNS - 1 - x;
        if (((cells >> (y * FREE_COLUMNS + column)) & 1) === 1) {
          filled.push({ x, y });
        }
      }
    }
    return filled;
  }

  $: cells = filledCells(mark.cells);
  $: hue = `${mark.hue * HUE_STEP_DEGREES}deg`;
  // One sentence for the name and the tooltip, so the tooltip repeats what is announced.
  $: label = workArea.windowMark(windowId);
</script>

<span class="window-mark window-mark--{size}">
  <span
    class="window-mark__frame"
    role="img"
    aria-label={label}
    style:--window-mark-hue={hue}
    use:tooltip={{ text: label, announcedElsewhere: true }}
  >
    <svg
      class="window-mark__cells"
      viewBox="0 0 5 5"
      aria-hidden="true"
      focusable="false"
      shape-rendering="crispEdges"
    >
      {#each cells as cell (cell.y * COLUMNS + cell.x)}
        <rect x={cell.x} y={cell.y} width="1" height="1" />
      {/each}
    </svg>
  </span>
  {#if showsId}
    <span class="window-mark__id" aria-hidden="true">{windowId}</span>
  {/if}
</span>

<style>
  .window-mark {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1_5);
    min-width: 0;
  }

  .window-mark__frame {
    display: inline-flex;
    flex: 0 0 auto;
    box-sizing: border-box;
    border: var(--line-width) solid var(--line);
    background-color: var(--bg-raised);
  }

  /* Sized so each cell is whole pixels: 5px on a card, 3px in a header. */
  .window-mark--card .window-mark__frame {
    width: 27px;
    height: 27px;
    padding: 1px;
    border-radius: var(--radius-sm);
  }

  .window-mark--header .window-mark__frame {
    width: 17px;
    height: 17px;
    padding: 0;
    border-radius: var(--radius-xs);
  }

  .window-mark__cells {
    width: 100%;
    height: 100%;
    fill: oklch(var(--window-mark-l) var(--window-mark-c) var(--window-mark-hue));
  }

  .window-mark__id {
    flex: 0 0 auto;
    color: var(--fg-faint);
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  /* Colour gives way and the shape stays. */
  @media (forced-colors: active) {
    .window-mark__frame {
      border-color: CanvasText;
    }

    .window-mark__cells {
      fill: CanvasText;
    }
  }
</style>
