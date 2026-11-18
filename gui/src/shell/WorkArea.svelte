<script lang="ts">
  /**
   * The work area: Focus fills it with the current window, Canvas floats every
   * window on screen. Every pane stays mounted in both modes, so switching
   * hides a terminal instead of attaching it again.
   */
  import FloatWindow from '../components/FloatWindow.svelte';
  import type { Size } from '../layout/float-layout';
  import { isWindowVisibleFor, type WorkView } from '../session/current-window';
  import type { Pane } from '../session/pane-plan';
  import type { WindowActions } from '../session/window-actions';
  import type { WindowEntry, WindowRegistry } from '../stores/window-registry';
  import type { ThemeStore } from '../theme/theme-store';
  import { hints } from '../ui-messages';

  export let windows: readonly WindowEntry[];
  /** The windows whose pane exists, in the order they render. */
  export let rendered: readonly { entry: WindowEntry; pane: Pane }[];
  export let workView: WorkView;
  export let currentWindowId: string | null;
  /** Size of the float area, measured by `trackArea`. */
  export let area: Size;
  export let layout: Pick<WindowRegistry, 'setWindowPosition' | 'setWindowSize'>;
  export let actions: WindowActions;
  export let themeStore: ThemeStore;
  /** Keeps `area` on the measured size of the element it is put on. */
  export let trackArea: (element: HTMLElement) => { destroy: () => void };
  /** The work area element, for tiling's measurement. */
  export let element: HTMLElement | undefined = undefined;
  export let onRename: (entry: WindowEntry) => void;
  /** Called once a press on a window has been released; see `FloatWindow`. */
  export let onPressed: (entry: WindowEntry, pressOnControl: boolean) => void;

  $: fillsArea = workView.mode === 'focus';
</script>

<section class="main-content" bind:this={element}>
  <div class="float-area" use:trackArea>
    {#if windows.length === 0}
      <div class="empty-main">
        <p>{hints.noWindows}</p>
      </div>
    {/if}
    {#each rendered as item (item.entry.windowId)}
      <FloatWindow
        entry={item.entry}
        {area}
        {layout}
        {actions}
        {themeStore}
        {fillsArea}
        controller={item.pane.controller}
        sessionStore={item.pane.sessionStore}
        isVisible={isWindowVisibleFor(workView, item.entry, windows)}
        isCurrent={item.entry.windowId === currentWindowId}
        onRename={() => onRename(item.entry)}
        onPressed={(pressOnControl: boolean) => onPressed(item.entry, pressOnControl)}
      />
    {/each}
  </div>
</section>

<style>
  .main-content {
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
  }

  /* Windows are absolutely positioned against this element and their
     rectangles come from its measured size, so it establishes the containing
     block and never scrolls. */
  .float-area {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
    /* Keeps the windows' z-order from competing with the overlay layer. */
    isolation: isolate;
  }

  .empty-main {
    display: flex;
    height: 100%;
    align-items: center;
    justify-content: center;
    color: var(--color-text-secondary);
  }
</style>
