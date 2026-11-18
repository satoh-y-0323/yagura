<script lang="ts">
  /**
   * The application's single tooltip. It draws whatever `use:tooltip` last
   * published, in the host that `overlay-host` picks for the trigger, so a
   * trigger inside a modal dialog gets a tooltip that is not inert.
   */
  import { afterUpdate } from 'svelte';

  import { placeMenu } from './menu-position';
  import { overlayPortal } from './overlay-host';
  import { activeTooltip, TOOLTIP_CLASS } from './tooltip';

  /** Space between the trigger and the tooltip. */
  const GAP_PX = 4;

  let element: HTMLDivElement | undefined;
  let position = { left: 0, top: 0 };
  let isPlaced = false;

  $: active = $activeTooltip;
  $: if (active === null) {
    isPlaced = false;
  }

  afterUpdate(() => {
    if (active === null || element === undefined) {
      return;
    }
    const anchor = active.origin.getBoundingClientRect();
    const size = element.getBoundingClientRect();
    const next = placeMenu(
      { left: anchor.left, top: anchor.top, width: anchor.width, height: anchor.height + GAP_PX },
      { width: size.width, height: size.height },
      { width: window.innerWidth, height: window.innerHeight }
    );
    if (!isPlaced || next.left !== position.left || next.top !== position.top) {
      position = next;
      isPlaced = true;
    }
  });
</script>

{#if active !== null}
  <div
    class={TOOLTIP_CLASS}
    role={active.mode === 'toggle' ? 'status' : 'tooltip'}
    aria-hidden={active.announcedElsewhere ? 'true' : undefined}
    bind:this={element}
    use:overlayPortal={active.origin}
    style="left: {position.left}px; top: {position.top}px; visibility: {isPlaced
      ? 'visible'
      : 'hidden'};"
    on:pointerenter={active.holdOpen}
    on:pointerleave={active.release}
  >
    {active.text}
    {#each active.details as detail, index (index)}
      <div class="tooltip__detail">{detail}</div>
    {/each}
  </div>
{/if}

<style>
  /* `TOOLTIP_CLASS` is `tooltip`; Svelte cannot scope a class held in a constant. */
  :global(.tooltip) {
    position: fixed;
    max-width: 320px;
    padding: var(--space-1) var(--space-2);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-sm);
    background-color: var(--bg-raised);
    box-shadow: var(--shadow-popover);
    color: var(--fg-default);
    font-size: var(--text-xs);
    line-height: 1.4;
    overflow-wrap: anywhere;
    pointer-events: auto;
    z-index: var(--z-tooltip);
  }

  .tooltip__detail {
    color: var(--fg-muted);
  }
</style>
