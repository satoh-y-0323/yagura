<script lang="ts">
  import PopupMenu from './PopupMenu.svelte';
  import { menus, themes } from '../ui-messages';

  export let isOpen = false;
  export let anchor: HTMLButtonElement | undefined;
  export let onClose: () => void;
  export let themeValue: string;
  export let themeOptions: { value: string; label: string }[];
  export let onThemeChange: (event: Event & { currentTarget: HTMLSelectElement }) => void;
  export let onRename: (() => void) | undefined;

  $: anchorRect = isOpen && anchor ? anchor.getBoundingClientRect() : null;

  function handleClose(reason: 'escape' | 'outside' | 'resize'): void {
    onClose();
    if (reason === 'escape') {
      anchor?.focus();
    }
  }
</script>

<PopupMenu {isOpen} anchor={anchorRect} keepOpenWithin={anchor} onClose={handleClose}>
  <div class="window-menu__label">{menus.sectionLabel}</div>
  <div class="window-menu__theme-item">
    <span class="window-menu__theme-label">{menus.theme}</span>
    <select
      class="window-theme-select"
      title={themes.windowSelectLabel}
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
    {menus.rename}
  </button>
  <hr class="popup-menu__divider" />
  <button type="button" class="popup-menu__item" role="menuitem" disabled>
    {menus.copyOutput}
  </button>
  <button type="button" class="popup-menu__item" role="menuitem" disabled>
    {menus.detectionDefinition}
  </button>
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

  .window-theme-select:focus {
    outline: none;
    border-color: var(--color-primary);
    box-shadow: 0 0 0 2px var(--color-primary-hover);
  }
</style>
