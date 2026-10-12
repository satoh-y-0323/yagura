<script lang="ts">
  /**
   * The operation buttons of one window — the `⋯` menu anchor and the
   * interrupt / minimize / maximize / close row — with the menu they open
   * Both the float window title bar and the tab bar mount this, so
   * the menu has exactly one owner and no caller ever holds its anchor.
   */
  import { resolveConfirmTarget } from '../session/confirm-target';
  import type { WindowActions } from '../session/window-actions';
  import type { ThemeStore } from '../theme/theme-store';
  import {
    buttons,
    describeInterruptHint,
    describeThemeName,
    hints,
    nav,
    themes,
  } from '../ui-messages';
  import IconButton from './IconButton.svelte';
  import WindowMenu from './WindowMenu.svelte';

  /**
   * The window the buttons act on, read back from `WindowActions` rather than
   * from the registry module: every button hands the entry straight to an
   * action, so the store stays out of this import graph.
   */
  type ControlledWindow = Parameters<WindowActions['focus']>[0];

  export let entry: ControlledWindow;
  export let actions: WindowActions;
  /** The application theme store; the menu reads and overrides it. */
  export let themeStore: ThemeStore;
  /** Callback to open the rename dialog for this window. */
  export let onRename: (() => void) | undefined = undefined;

  let isMenuOpen = false;
  let menuAnchor: HTMLButtonElement | undefined;

  // Derived once so `disabled` and the tooltip read one verdict, and so the
  // button stays mounted whatever the status.
  $: interruptTarget = resolveConfirmTarget(entry.detected.status, 'interrupt');
  $: maximizeLabel = entry.display.isMaximized ? buttons.restore : buttons.maximize;
  $: maximizeHint = entry.display.isMaximized ? hints.restoreWindow : hints.maximizeWindow;
  $: windowThemeValue = $themeStore.windowOverrides[entry.windowId] ?? '';
  $: windowThemeNames = ['dark', 'light', ...Object.keys($themeStore.userPalettes)];
  $: themeOptions = [
    { value: '', label: themes.windowFollowsAppOption },
    ...windowThemeNames.map((name) => ({ value: name, label: describeThemeName(name) })),
  ];

  function handleThemeChange(event: Event & { currentTarget: HTMLSelectElement }): void {
    const value = event.currentTarget.value;
    themeStore.setWindowOverride(entry.windowId, value === '' ? null : value);
  }
</script>

<div class="window-controls">
  <IconButton
    class="window-controls__menu-anchor"
    label={nav.windowMenu}
    bind:element={menuAnchor}
    on:click={() => {
      isMenuOpen = !isMenuOpen;
    }}
  >
    ⋯
  </IconButton>
  <IconButton
    class="window-controls__button"
    label={buttons.interrupt}
    hint={describeInterruptHint(interruptTarget)}
    disabled={!interruptTarget.allowed}
    on:click={() => {
      void actions.interrupt(entry);
    }}
  >
    ⏹
  </IconButton>
  <IconButton
    class="window-controls__button"
    label={buttons.minimize}
    hint={hints.minimizeWindow}
    on:click={() => actions.minimize(entry)}
  >
    −
  </IconButton>
  <IconButton
    class="window-controls__button"
    label={maximizeLabel}
    hint={maximizeHint}
    on:click={() => actions.maximize(entry)}
  >
    □
  </IconButton>
  <IconButton
    class="window-controls__button"
    label={buttons.close}
    hint={hints.closeWindow}
    on:click={() => {
      void actions.close(entry);
    }}
  >
    ✕
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
    gap: var(--space-sm);
  }
</style>
