<script lang="ts">
  /**
   * A menu drawn in the overlay host (the open modal, else the app's overlay
   * layer), not where it is declared, so a menu opened from inside a floating
   * window or a dialog is neither clipped nor inert. Items are `role="menuitem"`
   * children, optionally laid out as `popup-menu__icon`, `popup-menu__label`
   * and `popup-menu__badge`; a `<select>` is reachable like an item.
   */
  import { afterUpdate, onMount, tick } from 'svelte';

  import { isImeKey } from '../keymap/key-chord';
  import { handleMenuKey, type MenuEntry } from './menu-navigation';
  import { placeMenu, type Rect } from './menu-position';
  import { overlayPortal } from './overlay-host';

  /** Why the menu asks to be closed. */
  type CloseReason = 'escape' | 'tab' | 'outside' | 'resize';

  const NAVIGABLE = '[role="menuitem"], [role="separator"], hr, select';

  export let isOpen = false;
  /** Viewport-relative box the menu opens next to; a point is a zero-size box. */
  export let anchor: Rect | null;
  /** The button that opens the menu: a press on it is not outside, and Tab leaves from it. */
  export let keepOpenWithin: HTMLElement | undefined = undefined;
  export let onClose: (reason: CloseReason) => void;

  let originMarker: HTMLSpanElement | undefined;
  let menuElement: HTMLDivElement | undefined;
  let menuPosition = { left: 0, top: 0 };
  let isPlaced = false;
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

  $: if (!isOpen) {
    isPlaced = false;
  }

  // After the DOM update, so the menu is measured where it finally lives (the
  // overlay host may give it other inherited styles than where it is declared).
  afterUpdate(() => {
    if (!isOpen || menuElement === undefined) {
      return;
    }
    if (anchor !== null) {
      const menuRect = menuElement.getBoundingClientRect();
      const next = placeMenu(
        anchor,
        { width: menuRect.width, height: menuRect.height },
        { width: window.innerWidth, height: window.innerHeight }
      );
      if (!isPlaced || next.left !== menuPosition.left || next.top !== menuPosition.top) {
        menuPosition = next;
        isPlaced = true;
      }
    }
    // After `tick`, because a menu that is still hidden cannot take focus and
    // Svelte does not run this callback again in the flush that reveals it.
    void tick().then(focusMenu);
  });

  // Not a focus trap: the menu is left on the next Tab. Focus goes to the menu
  // itself so that Escape and the arrow keys reach it.
  function focusMenu(): void {
    if (isOpen && menuElement !== undefined && !menuElement.contains(document.activeElement)) {
      menuElement.focus();
    }
  }

  function navigable(): HTMLElement[] {
    return menuElement === undefined
      ? []
      : Array.from(menuElement.querySelectorAll<HTMLElement>(NAVIGABLE));
  }

  function describeEntry(element: HTMLElement): MenuEntry {
    const isDisabled =
      element.getAttribute('aria-disabled') === 'true' ||
      (element instanceof HTMLButtonElement && element.disabled);
    if (element instanceof HTMLSelectElement) {
      return { role: 'menuitem', disabled: element.disabled };
    }
    return {
      role: element instanceof HTMLHRElement ? 'separator' : element.getAttribute('role'),
      disabled: isDisabled,
    };
  }

  function handleKeyDown(event: KeyboardEvent): void {
    if (isImeKey(event)) {
      return;
    }
    // A focused <select> keeps the arrows, Home and End for its own options.
    const isOnSelect = event.target instanceof HTMLSelectElement;
    if (isOnSelect && event.key !== 'Escape' && event.key !== 'Tab') {
      return;
    }
    const elements = navigable();
    const current = elements.findIndex((element) => element === document.activeElement);
    const outcome = handleMenuKey(event.key, elements.map(describeEntry), current);
    if (outcome === null) {
      return;
    }
    if (outcome.type === 'focus') {
      elements[outcome.index]?.focus();
      event.preventDefault();
      return;
    }
    if (outcome.restoreFocus) {
      event.preventDefault();
      onClose('escape');
      return;
    }
    // The menu lives at the end of the document, so the browser's own Tab would
    // start from there; starting from the button that opened it keeps the order.
    keepOpenWithin?.focus();
    onClose('tab');
  }

  // Hover and keyboard focus are one highlight: pointing at an item focuses it.
  function handlePointerOver(event: PointerEvent): void {
    const item = event.target instanceof Element ? event.target.closest('[role="menuitem"]') : null;
    if (item instanceof HTMLElement && !describeEntry(item).disabled) {
      item.focus({ preventScroll: true });
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

<span class="popup-menu__origin" hidden bind:this={originMarker}></span>

{#if isOpen}
  <div
    class="popup-menu"
    bind:this={menuElement}
    role="menu"
    tabindex="-1"
    style="left: {menuPosition.left}px; top: {menuPosition.top}px; visibility: {isPlaced ||
    anchor === null
      ? 'visible'
      : 'hidden'};"
    use:overlayPortal={originMarker ?? null}
    on:keydown={handleKeyDown}
    on:pointerover={handlePointerOver}
  >
    <slot />
  </div>
{/if}

<style>
  .popup-menu {
    position: fixed;
    display: flex;
    flex-direction: column;
    gap: var(--space-0_5);
    min-width: 200px;
    padding: var(--space-1);
    cursor: auto;
    user-select: text;
    background-color: var(--bg-raised);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-popover);
    color: var(--fg-default);
    font-size: var(--text-sm);
    pointer-events: auto;
    z-index: var(--z-popover);
  }

  /* One row is an icon frame, a label and an optional badge on the right. The
     frame stays reserved when an item has no icon, so labels line up. */
  .popup-menu :global(.popup-menu__item) {
    display: grid;
    grid-template-columns: var(--icon-md) 1fr auto;
    align-items: center;
    column-gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border: none;
    border-radius: var(--radius-sm);
    background-color: transparent;
    color: var(--fg-default);
    font-family: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }

  .popup-menu :global(.popup-menu__icon) {
    display: inline-flex;
    width: var(--icon-md);
    height: var(--icon-md);
    color: var(--fg-muted);
  }

  .popup-menu :global(.popup-menu__label) {
    grid-column: 2;
  }

  .popup-menu :global(.popup-menu__badge) {
    grid-column: 3;
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .popup-menu :global(.popup-menu__item:focus) {
    background-color: var(--bg-hover);
  }

  .popup-menu :global(.popup-menu__item:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: -2px;
  }

  .popup-menu :global(.popup-menu__item:disabled) {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .popup-menu :global(.popup-menu__divider) {
    margin: var(--space-1) 0;
    border: none;
    border-top: var(--line-width) solid var(--line);
  }
</style>
