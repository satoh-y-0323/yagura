<script lang="ts">
  /**
   * The tab strip shown across the top of the float area while the app is in
   * tab mode. It owns no ordering or drag-threshold
   * arithmetic itself: pointer coordinates go to `resolveDropIndex`
   * (`../layout/tab-order`), and a click vs. a completed drag is dispatched
   * back through `onSelectTab` / `onReorderTab` — the tab switch those
   * trigger lives in `App.svelte`, not here.
   *
   * The right-edge operation buttons are `WindowControls`, mounted
   * here the same way `FloatWindow` mounts it, acting on `activeEntry` (the
   * maximized window the strip is currently showing).
   */
  import { resolveDropIndex, type TabBounds } from '../layout/tab-order';
  import type { WindowActions } from '../session/window-actions';
  import { resolveTone } from '../status-tone';
  import type { ThemeStore } from '../theme/theme-store';
  import { hints, nav } from '../ui-messages';
  import WindowControls from './WindowControls.svelte';

  /**
   * Read back from `WindowActions` rather than imported from the registry
   * module, the same way `WindowControls.svelte` derives its own `entry`
   * type: this component operates entirely through its props, with no store
   * import of its own.
   */
  type TabWindow = Parameters<WindowActions['focus']>[0];

  export let entries: TabWindow[];
  /** The maximized window WindowControls acts on. */
  export let activeEntry: TabWindow;
  export let actions: WindowActions;
  /** The application theme store; the right-edge WindowControls reads it. */
  export let themeStore: ThemeStore;
  /** Callback to open the rename dialog for the active window. */
  export let onRename: (() => void) | undefined = undefined;
  /** Called with the clicked tab's window id (a click, not a drag). */
  export let onSelectTab: (windowId: string) => void;
  /** Called with the dragged tab's window id and its resolved drop index. */
  export let onReorderTab: (windowId: string, targetIndex: number) => void;

  /** A drag in progress, or the click it turns out to have been. */
  interface DragState {
    pointerId: number;
    windowId: string;
    originX: number;
  }

  let drag: DragState | null = null;
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- an imperative DOM-ref cache for pointer-drag math, never read by the template, so it drives no reactivity of its own
  const tabButtons = new Map<string, HTMLButtonElement>();

  /** Tracks each tab's own button element, so its bounds can be measured on drop (Svelte action). */
  function trackTabButton(node: HTMLButtonElement, windowId: string): { destroy: () => void } {
    tabButtons.set(windowId, node);
    return {
      destroy: () => {
        tabButtons.delete(windowId);
      },
    };
  }

  /** The on-screen bounds of every tab currently rendered, in render order. */
  function currentTabBounds(): TabBounds[] {
    const bounds: TabBounds[] = [];
    for (const entry of entries) {
      const button = tabButtons.get(entry.windowId);
      if (button === undefined) {
        continue;
      }
      const rect = button.getBoundingClientRect();
      bounds.push({ windowId: entry.windowId, left: rect.left, width: rect.width });
    }
    return bounds;
  }

  function handleTabPointerDown(event: PointerEvent, windowId: string): void {
    if (event.button !== 0) {
      return;
    }
    const target = event.currentTarget;
    if (!(target instanceof Element)) {
      return;
    }
    target.setPointerCapture(event.pointerId);
    drag = { pointerId: event.pointerId, windowId, originX: event.clientX };
  }

  function handleTabPointerUp(event: PointerEvent, windowId: string): void {
    const started = drag;
    drag = null;
    if (
      started === null ||
      started.pointerId !== event.pointerId ||
      started.windowId !== windowId
    ) {
      return;
    }
    const dropIndex = resolveDropIndex({
      tabs: currentTabBounds(),
      windowId,
      originX: started.originX,
      pointerX: event.clientX,
    });
    if (dropIndex === null) {
      onSelectTab(windowId);
      return;
    }
    onReorderTab(windowId, dropIndex);
  }

  /**
   * Pointer capture can be released implicitly — the captured tab leaves the
   * DOM, or the OS interrupts the gesture — with no `pointerup` ever reaching
   * the button, which would leave `drag` dangling. The cancel clears `drag`
   * unconditionally, without checking `pointerId`. A `drag` left behind by a
   * gesture that never completed is overwritten by the next press on any tab.
   */
  function handleTabPointerCancel(): void {
    drag = null;
  }

  /**
   * A keyboard activation (Space/Enter) reaches a native `<button>` as a
   * `click` with no accompanying pointer events, so it is the only path
   * that would otherwise miss `onSelectTab` entirely. A pointer-driven click
   * already went through `handleTabPointerUp` above; `MouseEvent.detail` is
   * `0` only for the synthetic, non-pointer case, which keeps this from
   * firing `onSelectTab` a second time for the same mouse click.
   */
  function handleTabClick(event: MouseEvent, windowId: string): void {
    if (event.detail === 0) {
      onSelectTab(windowId);
    }
  }
</script>

<nav class="tab-bar" aria-label={nav.windowTabs}>
  <div class="tab-bar__tabs">
    {#each entries as entry (entry.windowId)}
      <button
        type="button"
        class="tab-bar__tab"
        class:tab-bar__tab--minimized={entry.display.isMinimized}
        aria-current={entry.windowId === activeEntry.windowId ? 'true' : undefined}
        title={hints.reorderTab}
        use:trackTabButton={entry.windowId}
        on:pointerdown={(event) => handleTabPointerDown(event, entry.windowId)}
        on:pointerup={(event) => handleTabPointerUp(event, entry.windowId)}
        on:pointercancel={handleTabPointerCancel}
        on:click={(event) => handleTabClick(event, entry.windowId)}
      >
        <span class="tab-bar__dot" data-tone={resolveTone(entry.detected.status)} aria-hidden="true"
        ></span>
        <span class="tab-bar__label">{entry.name}</span>
      </button>
    {/each}
  </div>
  <div class="tab-bar__controls">
    <WindowControls entry={activeEntry} {actions} {themeStore} {onRename} />
  </div>
</nav>

<style>
  .tab-bar {
    display: flex;
    align-items: center;
    flex: 0 0 auto;
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-surface);
  }

  .tab-bar__tabs {
    display: flex;
    align-items: stretch;
    flex: 1 1 auto;
    min-width: 0;
    overflow-x: auto;
  }

  .tab-bar__tab {
    display: flex;
    align-items: center;
    flex: 0 0 auto;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border: none;
    border-top: 2px solid transparent;
    background: none;
    color: var(--color-text-primary);
    font-family: inherit;
    font-size: var(--font-size-sm);
    cursor: pointer;
    touch-action: none;
    user-select: none;
  }

  .tab-bar__tab[aria-current='true'] {
    background-color: var(--color-surface);
    border-top-color: var(--color-primary);
  }

  .tab-bar__tab--minimized {
    opacity: 0.45;
  }

  .tab-bar__dot {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .tab-bar__label {
    max-width: 16em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-bar__controls {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    padding: 0 var(--space-sm);
  }
</style>
