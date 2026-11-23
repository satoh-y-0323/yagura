<script lang="ts">
  import type { ThemeOption } from '../theme/window-theme-options';
  import { menus, themes } from '../ui-messages';
  import PopupMenu from './PopupMenu.svelte';
  import type { Rect } from './menu-position';

  export let isOpen = false;
  export let anchor: HTMLButtonElement | undefined;
  /** Where the menu opens instead of beside `anchor`, such as a right-click's point. */
  export let at: Rect | null = null;
  export let onClose: () => void;
  export let themeValue: string;
  export let themeOptions: readonly ThemeOption[];
  export let onThemeChange: (event: Event & { currentTarget: HTMLSelectElement }) => void;
  export let onRename: (() => void) | undefined;
  /** Moves the window within its room; Move up and Move down are offered only when set. */
  export let onMove: ((direction: 'up' | 'down') => void) | undefined = undefined;
  export let canMoveUp = false;
  export let canMoveDown = false;

  $: anchorRect = isOpen && anchor ? (at ?? anchor.getBoundingClientRect()) : null;

  function handleClose(reason: 'escape' | 'tab' | 'outside' | 'resize'): void {
    // Read before `onClose`: a parent that clears the menu also clears `anchor`.
    const opener = anchor;
    onClose();
    if (reason === 'escape') {
      opener?.focus();
    }
  }
</script>

<PopupMenu {isOpen} anchor={anchorRect} keepOpenWithin={anchor} onClose={handleClose}>
  <div class="window-menu__label">{menus.sectionLabel}</div>
  <div class="window-menu__theme-item">
    <span class="window-menu__theme-label">{menus.theme}</span>
    <select
      class="window-theme-select"
      aria-label={themes.windowSelectLabel}
      bind:value={themeValue}
      on:change={onThemeChange}
    >
      <option value="">{themeOptions.find((o) => o.value === '')?.label}</option>
      {#each themeOptions as option (option.value)}
        {#if option.value !== ''}
          <option value={option.value}>{option.label}</option>
        {/if}
      {/each}
    </select>
  </div>
  <button
    type="button"
    class="popup-menu__item"
    role="menuitem"
    on:click={() => {
      onRename?.();
    }}
  >
    <span class="popup-menu__label">{menus.rename}</span>
  </button>
  {#if onMove !== undefined}
    {@const move = onMove}
    <hr class="popup-menu__divider" />
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      disabled={!canMoveUp}
      on:click={() => move('up')}
    >
      <span class="popup-menu__label">{menus.moveUp}</span>
    </button>
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      disabled={!canMoveDown}
      on:click={() => move('down')}
    >
      <span class="popup-menu__label">{menus.moveDown}</span>
    </button>
  {/if}
</PopupMenu>

<style>
  .window-menu__label {
    font-size: var(--font-size-xs);
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--color-text-secondary);
    padding: 0 var(--space-sm);
  }

  .window-menu__theme-item {
    padding: var(--space-xs) var(--space-sm);
  }

  .window-menu__theme-label {
    color: var(--color-text-primary);
    font-size: var(--font-size-sm);
  }

  .window-theme-select {
    flex: 1;
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    color: var(--color-text-primary);
    font-family: inherit;
    font-size: var(--font-size-sm);
    cursor: pointer;
  }

  .window-theme-select:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }
</style>
