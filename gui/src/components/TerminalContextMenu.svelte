<script lang="ts">
  import PopupMenu from './PopupMenu.svelte';
  import type { Rect } from './menu-position';
  import { menus } from '../ui-messages';

  export let isOpen = false;
  /** Where the right-click landed, viewport-relative. */
  export let at: { x: number; y: number } | null;
  export let canCopy: boolean;
  /** The platform's copy and paste chords, as the user reads them. */
  export let copyKeys: string;
  export let pasteKeys: string;
  export let onCopy: () => void;
  export let onPaste: () => void;
  export let onClose: () => void;

  $: anchor = at === null ? null : ({ left: at.x, top: at.y, width: 0, height: 0 } satisfies Rect);
</script>

<PopupMenu {isOpen} {anchor} {onClose}>
  <button
    type="button"
    class="popup-menu__item terminal-menu__item"
    role="menuitem"
    disabled={!canCopy}
    on:click={onCopy}
  >
    <span>{menus.copy}</span><span class="terminal-menu__keys">{copyKeys}</span>
  </button>
  <button
    type="button"
    class="popup-menu__item terminal-menu__item"
    role="menuitem"
    on:click={onPaste}
  >
    <span>{menus.paste}</span><span class="terminal-menu__keys">{pasteKeys}</span>
  </button>
</PopupMenu>

<style>
  .terminal-menu__item {
    justify-content: space-between;
    gap: var(--space-md);
  }

  .terminal-menu__keys {
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }
</style>
