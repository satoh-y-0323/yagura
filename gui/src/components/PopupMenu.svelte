<script lang="ts">
  import { onMount } from 'svelte';
  import { placeMenu, type Rect } from './menu-position';
  import { isImeKey } from '../keymap/key-chord';

  /** Why the menu asks to be closed. */
  type CloseReason = 'escape' | 'outside' | 'resize';

  export let isOpen = false;
  /** Viewport-relative box the menu opens next to; a point is a zero-size box. */
  export let anchor: Rect | null;
  /** A press inside this element does not count as outside (the button that toggles the menu). */
  export let keepOpenWithin: HTMLElement | undefined = undefined;
  export let onClose: (reason: CloseReason) => void;

  let menuElement: HTMLDivElement | undefined;
  let menuPosition = { left: 0, top: 0 };
  let isSubscribedToOutsidePointer = false;

  function handleWindowResize(): void {
    if (isOpen) {
      onClose('resize');
    }
  }

  onMount(() => {
    window.addEventListener('resize', handleWindowResize);
    return () => {
      window.removeEventListener('resize', handleWindowResize);
      syncOutsidePointerSubscription(false);
    };
  });

  // Delegated to a function rather than written inline: a reactive statement
  // that assigns the subscription state it also reads is an infinite reactive
  // loop as far as Svelte is concerned.
  $: syncOutsidePointerSubscription(isOpen);

  function syncOutsidePointerSubscription(isMenuOpen: boolean): void {
    if (isMenuOpen === isSubscribedToOutsidePointer) {
      return;
    }
    if (isMenuOpen) {
      window.addEventListener('pointerdown', handlePointerDown);
    } else {
      window.removeEventListener('pointerdown', handlePointerDown);
    }
    isSubscribedToOutsidePointer = isMenuOpen;
  }

  $: {
    if (isOpen && anchor && menuElement) {
      const menuRect = menuElement.getBoundingClientRect();
      const viewport = { width: window.innerWidth, height: window.innerHeight };
      menuPosition = placeMenu(
        anchor,
        { width: menuRect.width, height: menuRect.height },
        viewport
      );
    }
  }

  // `on:keydown` is bound to the menu itself, so Escape only reaches it once
  // focus has entered the menu. Not a focus trap — the menu is left on the next Tab.
  $: if (isOpen && menuElement) {
    menuElement.focus();
  }

  function handleKeyDown(event: KeyboardEvent): void {
    if (isImeKey(event)) {
      return;
    }
    if (event.key === 'Escape') {
      onClose('escape');
      event.preventDefault();
    }
  }

  function handlePointerDown(event: PointerEvent): void {
    if (!menuElement) {
      return;
    }
    const target = event.target as Node;
    if (keepOpenWithin?.contains(target) === true) {
      return;
    }
    // No preventDefault: the outside press is usually the start of another
    // gesture (a title-bar drag), which must still run.
    if (!menuElement.contains(target)) {
      onClose('outside');
    }
  }
</script>

{#if isOpen}
  <div
    class="popup-menu"
    bind:this={menuElement}
    role="menu"
    tabindex="-1"
    style="left: {menuPosition.left}px; top: {menuPosition.top}px;"
    on:keydown={handleKeyDown}
  >
    <slot />
  </div>
{/if}

<style>
  /* The menu can be mounted inside the float window drag bar, which sets the
     inherited cursor: move / user-select: none; both are put back here, or the
     menu would look draggable and read as unselectable. */
  .popup-menu {
    position: fixed;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    min-width: 200px;
    padding: var(--space-sm);
    cursor: auto;
    user-select: text;
    background-color: var(--color-surface);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-modal);
    z-index: 10000;
  }

  .popup-menu :global(.popup-menu__item) {
    padding: var(--space-xs) var(--space-sm);
    border: none;
    background-color: transparent;
    color: var(--color-text-primary);
    font-size: var(--font-size-sm);
    text-align: left;
    cursor: pointer;
  }

  .popup-menu :global(.popup-menu__item:hover:not(:disabled)) {
    background-color: var(--color-border);
    border-radius: var(--radius-sm);
  }

  .popup-menu :global(.popup-menu__item:disabled) {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .popup-menu :global(.popup-menu__divider) {
    margin: 0;
    border: none;
    border-top: 1px solid var(--color-border);
  }
</style>
