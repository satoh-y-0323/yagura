<script lang="ts">
  /**
   * One floating window: a draggable, resizable frame around the pane of a
   * single window.
   *
   * The frame owns no layout state. Pointer gestures are turned into
   * rectangles by the pure functions of `float-layout` and written straight
   * back through the registry mutators it is given, and every operation with
   * a contract goes through `window-actions`, so this component never decides
   * whether an operation is allowed.
   */
  import { onDestroy, onMount } from 'svelte';

  import {
    DEFAULT_WINDOW_SIZE,
    maximizedRect,
    moveRect,
    reclampRect,
    resizeRect,
    type Rect,
    type ResizeHandle,
    type Size,
  } from '../layout/float-layout';
  import type { SessionController } from '../session/session-controller';
  import type { WindowActions } from '../session/window-actions';
  import type { SessionStore } from '../stores/session-store';
  import type { WindowDisplay, WindowEntry } from '../stores/window-registry';
  import { resolveTone } from '../status-tone';
  import { bindPalette } from '../theme/theme-apply';
  import { resolveWindowPaletteFromState } from '../theme/theme-resolve';
  import type { ThemeStore } from '../theme/theme-store';
  import {
    describeExitCode,
    describePaneNotice,
    describeStatus,
    describeUnknownExplanation,
    describeUnknownReasonSuffix,
  } from '../ui-messages';
  import McpMark from './McpMark.svelte';
  import StatusReportMark from './StatusReportMark.svelte';
  import { pointerDrag, type PointerDragOptions } from './pointer-drag';
  import TerminalPane from './TerminalPane.svelte';
  import WindowControls from './WindowControls.svelte';

  /** The registry mutators that own this window's position and size. */
  interface WindowLayoutSink {
    setWindowPosition: (windowId: string, position: { x: number; y: number }) => void;
    setWindowSize: (windowId: string, size: Size) => void;
  }

  export let entry: WindowEntry;
  export let controller: SessionController;
  export let sessionStore: SessionStore;
  /** Size of the float area the window is placed in. */
  export let area: Size;
  export let layout: WindowLayoutSink;
  export let actions: WindowActions;
  /** The application theme store; this window reads and overrides it. */
  export let themeStore: ThemeStore;
  /** Whether the window is shown; `false` while it is minimized. */
  export let isVisible = true;
  /** Whether keyboard operations currently act on this window. */
  export let isCurrent = false;
  /** Callback to open the rename dialog for this window. */
  export let onRename: (() => void) | undefined = undefined;

  const RESIZE_HANDLES: readonly ResizeHandle[] = [
    'top',
    'right',
    'bottom',
    'left',
    'top-left',
    'top-right',
    'bottom-left',
    'bottom-right',
  ];

  /** The subtree the window palette is written to; see the theme scope below. */
  let themeScope: HTMLDivElement;
  let unbindWindowPalette: (() => void) | null = null;

  $: rect = resolveRect(entry.display, area);
  $: notice = describePaneNotice($sessionStore.phase);

  // The scope element only exists once the window is in the DOM, so the
  // palette is bound from `onMount` rather than at initialization.
  onMount(() => {
    unbindWindowPalette = bindPalette(
      themeStore,
      (state) => resolveWindowPaletteFromState(state, entry.vendor, entry.windowId),
      themeScope
    );
  });

  onDestroy(() => {
    unbindWindowPalette?.();
    unbindWindowPalette = null;
    // The override outlives the window otherwise: the store keys them by
    // window id, and ids are not reused but the entry would linger.
    themeStore.setWindowOverride(entry.windowId, null);
  });

  function resolveRect(display: WindowDisplay, bounds: Size): Rect {
    if (display.isMaximized) {
      return maximizedRect(bounds);
    }
    // Position and size stay null until the window has been placed once.
    return reclampRect(bounds, {
      position: display.position ?? { x: 0, y: 0 },
      size: display.size ?? DEFAULT_WINDOW_SIZE,
    });
  }

  function raise(): void {
    actions.focus(entry);
  }

  /**
   * Focus can enter the window without a pointer (keyboard, programmatic), so
   * it raises here too; a current window is already raised, and raising it on
   * every focus move would update the store for nothing.
   */
  function raiseOnFocusIn(): void {
    if (!isCurrent) {
      actions.focus(entry);
    }
  }

  /** The rect a move or resize starts from, or null while the window is maximized. */
  function gestureStartRect(): Rect | null {
    // A maximized window is drawn at the area bounds, so a gesture on it would
    // write coordinates that are not the ones on screen.
    return entry.display.isMaximized ? null : rect;
  }

  const moveDrag: PointerDragOptions<Rect> = {
    start: (event) => {
      // Capturing the pointer here would swallow the interaction with the
      // controls the bar contains: its buttons, and the menu they open, which
      // is mounted inside the bar as a descendant of WindowControls.
      if (event.target instanceof Element && event.target.closest('button, .popup-menu') !== null) {
        return null;
      }
      return gestureStartRect();
    },
    move: (startRect, delta) => {
      layout.setWindowPosition(entry.windowId, moveRect(area, startRect, delta).position);
    },
  };

  function resizeDrag(handle: ResizeHandle): PointerDragOptions<Rect> {
    return {
      start: gestureStartRect,
      move: (startRect, delta) => {
        const resized = resizeRect(area, startRect, delta, handle);
        layout.setWindowPosition(entry.windowId, resized.position);
        layout.setWindowSize(entry.windowId, resized.size);
      },
    };
  }
</script>

<article
  class="float-window"
  class:float-window--hidden={!isVisible}
  style="left: {rect.position.x}px; top: {rect.position.y}px; width: {rect.size
    .width}px; height: {rect.size.height}px; z-index: {entry.display.zOrder};"
  on:pointerdown|capture={raise}
  on:focusin={raiseOnFocusIn}
>
  <!-- The window palette is written here, not on the article: the frame keeps
       the application palette so a window never loses its outline.
       `display: contents` keeps the wrapper out of the layout. -->
  <div class="float-window__theme-scope" bind:this={themeScope}>
    <!-- While the window is maximized its controls are drawn by the tab bar,
         so drawing the frame bar as well would offer the same operations
         twice. -->
    {#if !entry.display.isMaximized}
      <div class="float-window__bar" role="presentation" use:pointerDrag={moveDrag}>
        <h2 class="float-window__title" title={entry.name}>{entry.name}</h2>
        <McpMark mcpConnected={entry.mcpConnected} />
        <StatusReportMark statusReportsMissing={entry.statusReportsMissing} />
        <!-- prettier-ignore -->
        <span class="float-window__status" data-tone={resolveTone(entry.detected.status)} title={describeUnknownExplanation(entry.detected, entry.detectionWarning)}>{describeStatus(entry.detected.status)}{describeUnknownReasonSuffix(entry.detected)}</span>
        {#if entry.detected.status === 'exited' && entry.exitCode !== null}
          <span class="float-window__exit">{describeExitCode(entry.exitCode)}</span>
        {/if}
        <WindowControls {entry} {actions} {themeStore} {onRename} />
      </div>
    {/if}

    {#if notice !== null}
      <p class="float-window__notice" role="status">{notice}</p>
    {/if}

    <TerminalPane
      {controller}
      {isVisible}
      {themeStore}
      windowId={entry.windowId}
      vendor={entry.vendor}
    />

    {#each RESIZE_HANDLES as handle (handle)}
      <!-- Pointer-only affordance: keyboard move/resize belongs to the shortcut
           layer, so the grips are not exposed to assistive technology. -->
      <div
        class="float-window__grip float-window__grip--{handle}"
        aria-hidden="true"
        use:pointerDrag={resizeDrag(handle)}
      ></div>
    {/each}
  </div>
</article>

<style>
  .float-window {
    position: absolute;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    box-shadow: var(--shadow-md);
  }

  .float-window--hidden {
    display: none;
  }

  /* Carries the window palette without taking part in the layout, so the
     frame above it keeps the application palette. */
  .float-window__theme-scope {
    display: contents;
  }

  .float-window__bar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-surface);
    cursor: move;
    touch-action: none;
    user-select: none;
  }

  .float-window__title {
    flex: 1 1 auto;
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-text-primary);
    font-size: var(--font-size-sm);
    font-weight: 600;
  }

  .float-window__status {
    flex: 0 0 auto;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    font-size: var(--font-size-sm);
  }

  .float-window__exit {
    flex: 0 0 auto;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .float-window__notice {
    flex: 0 0 auto;
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-sm);
  }

  .float-window__grip {
    position: absolute;
    touch-action: none;
  }

  .float-window__grip--top,
  .float-window__grip--bottom {
    left: 6px;
    right: 6px;
    height: 6px;
    cursor: ns-resize;
  }

  .float-window__grip--left,
  .float-window__grip--right {
    top: 6px;
    bottom: 6px;
    width: 6px;
    cursor: ew-resize;
  }

  .float-window__grip--top {
    top: 0;
  }

  .float-window__grip--bottom {
    bottom: 0;
  }

  .float-window__grip--left {
    left: 0;
  }

  .float-window__grip--right {
    right: 0;
  }

  .float-window__grip--top-left,
  .float-window__grip--top-right,
  .float-window__grip--bottom-left,
  .float-window__grip--bottom-right {
    width: 12px;
    height: 12px;
  }

  .float-window__grip--top-left {
    top: 0;
    left: 0;
    cursor: nwse-resize;
  }

  .float-window__grip--top-right {
    top: 0;
    right: 0;
    cursor: nesw-resize;
  }

  .float-window__grip--bottom-left {
    bottom: 0;
    left: 0;
    cursor: nesw-resize;
  }

  .float-window__grip--bottom-right {
    bottom: 0;
    right: 0;
    cursor: nwse-resize;
  }
</style>
