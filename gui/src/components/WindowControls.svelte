<script lang="ts">
  /**
   * The operation buttons of one window header -- interrupt, the `⋯` menu
   * anchor, minimize and maximize (Canvas only) and close -- with the menu
   * they open, so the menu has exactly one owner and no caller ever holds its anchor.
   *
   * Each button carries `data-window-control`: focus left on one is not an
   * input target (`isInputTarget` in `focus-moves.ts`), because the window it
   * acts on can change under it. The menu items do not carry it.
   */
  import { resolveConfirmTarget } from '../session/confirm-target';
  import type { WindowActions } from '../session/window-actions';
  import type { ThemeStore } from '../theme/theme-store';
  import { windowThemeOptions } from '../theme/window-theme-options';
  import { buttons, describeInterruptHint, hints, nav } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import WindowMenu from './WindowMenu.svelte';

  /**
   * The window the buttons act on, read back from `WindowActions` rather than
   * from the registry module: every button hands the entry straight to an
   * action, so the store stays out of this import graph.
   */
  type ControlledWindow = Parameters<WindowActions['goTo']>[0];

  export let entry: ControlledWindow;
  export let actions: WindowActions;
  /** The application theme store; the menu reads and overrides it. */
  export let themeStore: ThemeStore;
  /** Whether minimize and maximize are offered; Focus has no window size to change. */
  export let offersSizeControls: boolean;
  /** Callback to open the rename dialog for this window. */
  export let onRename: (() => void) | undefined = undefined;

  let isMenuOpen = false;
  let menuAnchor: HTMLButtonElement | undefined;

  // Derived once so the reason and the tooltip read one verdict, and so the
  // button stays mounted whatever the status.
  $: interruptTarget = resolveConfirmTarget(entry.detected.status, 'interrupt');
  $: maximizeLabel = entry.display.isMaximized ? buttons.restore : buttons.maximize;
  $: maximizeHint = entry.display.isMaximized ? hints.restoreWindow : hints.maximizeWindow;
  $: windowThemeValue = $themeStore.windowOverrides[entry.windowId] ?? '';
  $: themeOptions = windowThemeOptions($themeStore.userPalettes);

  function handleThemeChange(event: Event & { currentTarget: HTMLSelectElement }): void {
    const value = event.currentTarget.value;
    themeStore.setWindowOverride(entry.windowId, value === '' ? null : value);
  }
</script>

<div class="window-controls">
  <IconButton
    data-window-control
    class="window-controls__button"
    label={buttons.interrupt}
    hint={describeInterruptHint(interruptTarget)}
    disabledReason={interruptTarget.allowed ? undefined : describeInterruptHint(interruptTarget)}
    on:click={() => {
      void actions.interrupt(entry);
    }}
  >
    <Icon name="square" size="sm" />
  </IconButton>
  <IconButton
    data-window-control
    class="window-controls__button"
    label={nav.windowMenu}
    bind:element={menuAnchor}
    on:click={() => {
      isMenuOpen = !isMenuOpen;
    }}
  >
    <Icon name="ellipsis" size="sm" />
  </IconButton>
  {#if offersSizeControls}
    <IconButton
      data-window-control
      class="window-controls__button"
      label={buttons.minimize}
      hint={hints.minimizeWindow}
      on:click={() => actions.minimize(entry)}
    >
      <Icon name="minus" size="sm" />
    </IconButton>
    <IconButton
      data-window-control
      class="window-controls__button"
      label={maximizeLabel}
      hint={maximizeHint}
      on:click={() => actions.maximize(entry)}
    >
      <Icon name={entry.display.isMaximized ? 'minimize-2' : 'maximize-2'} size="sm" />
    </IconButton>
  {/if}
  <IconButton
    data-window-control
    class="window-controls__button"
    label={buttons.close}
    hint={hints.closeWindow}
    on:click={() => {
      void actions.close(entry);
    }}
  >
    <Icon name="x" size="sm" />
  </IconButton>

  <WindowMenu
    isOpen={isMenuOpen}
    anchor={menuAnchor}
    onClose={() => {
      isMenuOpen = false;
    }}
    themeValue={windowThemeValue}
    {themeOptions}
    onThemeChange={handleThemeChange}
    onRename={() => onRename?.()}
  />
</div>

<style>
  .window-controls {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: var(--space-0_5);
  }
</style>
