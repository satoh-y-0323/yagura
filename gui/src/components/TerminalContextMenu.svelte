<script lang="ts">
  import Icon from './Icon.svelte';
  import PopupMenu from './PopupMenu.svelte';
  import type { Rect } from './menu-position';
  import { menus } from '../ui-messages';
  import type { UiText } from '../display/ui-text';

  export let isOpen = false;
  /** Where the right-click landed, viewport-relative. */
  export let at: { x: number; y: number } | null;
  export let canCopy: boolean;
  /** The platform's copy and paste chords, as the user reads them. */
  export let copyKeys: UiText;
  export let pasteKeys: UiText;
  export let onCopy: () => void;
  export let onPaste: () => void;
  export let onClose: () => void;

  $: anchor = at === null ? null : ({ left: at.x, top: at.y, width: 0, height: 0 } satisfies Rect);
</script>

<PopupMenu {isOpen} {anchor} {onClose}>
  <button
    type="button"
    class="popup-menu__item"
    role="menuitem"
    disabled={!canCopy}
    on:click={onCopy}
  >
    <span class="popup-menu__icon" aria-hidden="true"><Icon name="copy" /></span>
    <span class="popup-menu__label">{menus.copy}</span>
    <span class="popup-menu__badge">{copyKeys}</span>
  </button>
  <button type="button" class="popup-menu__item" role="menuitem" on:click={onPaste}>
    <span class="popup-menu__label">{menus.paste}</span>
    <span class="popup-menu__badge">{pasteKeys}</span>
  </button>
</PopupMenu>
