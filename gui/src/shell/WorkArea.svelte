<script lang="ts">
  /**
   * The work area: Focus fills it with the current window, Canvas floats every
   * window on screen. Every pane stays mounted in both modes, so switching
   * hides a terminal instead of attaching it again.
   */
  import EmptyState from '../components/EmptyState.svelte';
  import ExitedCard from '../components/ExitedCard.svelte';
  import FloatWindow from '../components/FloatWindow.svelte';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import type { Keys } from '../keymap/keymap-model';
  import type { Size } from '../layout/float-layout';
  import type { ReopenOffer } from '../notices/notice-model';
  import { isWindowVisibleFor, type WorkView } from '../session/current-window';
  import type { Pane } from '../session/pane-plan';
  import type { ExitedCard as ExitedCardState } from '../session/shell-state';
  import type { WindowActions } from '../session/window-actions';
  import type { WindowEntry, WindowRegistry } from '../stores/window-registry';
  import type { ThemeStore } from '../theme/theme-store';

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
  /** The card standing in for the window Focus showed, `null` while there is none. */
  export let exitedCard: ExitedCardState | null;
  /** The effective keys, for the keys the empty work area and the card show. */
  export let keys: Keys;
  export let platform: KeyboardPlatform;
  /** The last session's windows on offer in the work area, `null` while none are. */
  export let reopen: ReopenOffer | null;
  export let onNewWindow: () => void;
  export let onChooseWindows: () => void;
  export let onExitedCardGoTo: () => void;

  $: fillsArea = workView.mode === 'focus';
</script>

<section class="main-content" bind:this={element}>
  <div class="float-area" use:trackArea>
    {#if exitedCard !== null}
      <div class="work-area-state">
        <ExitedCard
          card={exitedCard}
          {keys}
          {platform}
          {reopen}
          onGoTo={onExitedCardGoTo}
          {onNewWindow}
          {onChooseWindows}
        />
      </div>
    {:else if windows.length === 0}
      <div class="work-area-state">
        <EmptyState {keys} {platform} {reopen} {onNewWindow} {onChooseWindows} />
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

  .work-area-state {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-6);
    overflow: auto;
    background-color: var(--bg-canvas);
  }
</style>
