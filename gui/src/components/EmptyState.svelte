<script lang="ts">
  /**
   * The work area while no window is on it: the way to open one, the offer to
   * reopen the last session's windows, and the main keys. Display only; the
   * shell decides when it shows and what the buttons do.
   */
  import type { UiText } from '../display/ui-text';
  import type { ActionId } from '../keymap/key-binding';
  import { chordText } from '../keymap/key-chord';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import type { Keys } from '../keymap/keymap-model';
  import type { ReopenOffer } from '../notices/notice-model';
  import { buttons, describeReopenOffer, workArea } from '../ui-messages';
  import Icon from './Icon.svelte';

  /** The effective keys, so every key shown is the one that works. */
  export let keys: Keys;
  export let platform: KeyboardPlatform;
  /** The last session's windows on offer, `null` while none are offered here. */
  export let reopen: ReopenOffer | null;
  export let onNewWindow: () => void;
  /** Opens the dialog that picks which of the last session's windows to reopen. */
  export let onChooseWindows: () => void;

  // Only keys that do something while no window is open, so none of them leads nowhere.
  const SHORTCUTS: readonly { action: ActionId; label: UiText }[] = [
    { action: 'open-command-palette', label: workArea.commands },
    { action: 'toggle-settings', label: workArea.settings },
  ];

  function keyLabel(currentKeys: Keys, action: ActionId): UiText | null {
    const chord = currentKeys[action];
    return chord === null ? null : chordText(chord, platform);
  }

  $: newWindowKey = keyLabel(keys, 'open-window-form');
  // A key that is switched off has nothing to show, so its row is left out.
  let shortcuts: readonly { label: UiText; key: UiText }[];
  $: shortcuts = SHORTCUTS.flatMap(({ action, label }) => {
    const key = keyLabel(keys, action);
    return key === null ? [] : [{ label, key }];
  });
</script>

<div class="work-state empty-state">
  <slot name="heading">
    <span class="work-state__icon"><Icon name="layout-panel-top" size="xl" /></span>
    <h2 class="work-state__title">{workArea.noWindowsTitle}</h2>
    <p class="work-state__body">{workArea.noWindowsBody}</p>
  </slot>
  <button type="button" class="btn btn-primary empty-state__action" on:click={onNewWindow}>
    <Icon name="plus" size="sm" />
    {workArea.newWindow}
    {#if newWindowKey !== null}<kbd class="key-hint key-hint--on-accent">{newWindowKey}</kbd>{/if}
  </button>
  {#if reopen !== null}
    <div class="empty-state__reopen">
      <span class="empty-state__reopen-text">
        {describeReopenOffer(reopen.count, reopen.savedAtEpochS)}
      </span>
      <button type="button" class="btn btn-secondary" on:click={onChooseWindows}>
        {buttons.chooseWindows}
      </button>
    </div>
  {/if}
  {#if shortcuts.length > 0}
    <dl class="empty-state__keys">
      {#each shortcuts as shortcut (shortcut.label)}
        <dt>{shortcut.label}</dt>
        <dd><kbd class="key-hint">{shortcut.key}</kbd></dd>
      {/each}
    </dl>
  {/if}
</div>

<style>
  .empty-state {
    width: min(440px, 100%);
  }

  .empty-state__action {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }

  .empty-state__reopen {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    margin-top: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-md);
    background-color: var(--bg-raised);
    text-align: left;
  }

  .empty-state__reopen-text {
    flex: 1 1 auto;
    min-width: 0;
    font-weight: var(--weight-semibold);
  }

  .empty-state__keys {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--space-1_5) var(--space-4);
    width: 100%;
    margin: var(--space-2) 0 0;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    text-align: left;
  }

  .empty-state__keys dd {
    margin: 0;
    justify-self: end;
  }
</style>
