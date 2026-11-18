<script lang="ts">
  /**
   * What Focus shows in place of a window that ended without this GUI closing
   * it. Its buttons never take the keyboard focus on their own: a key being
   * typed must not press one. When the ended window's terminal held the focus,
   * the card itself takes it, so the next Tab reaches its first button rather
   * than the start of the page. Without a next window it offers what the
   * empty work area offers.
   */
  import { onMount } from 'svelte';

  import { formatChord } from '../keymap/key-chord';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import type { Keys } from '../keymap/keymap-model';
  import type { ExitedCard } from '../session/shell-state';
  import { describeExitedWindow, describeGoToWindow, workArea } from '../ui-messages';
  import EmptyState from './EmptyState.svelte';
  import Icon from './Icon.svelte';

  export let card: ExitedCard;
  export let keys: Keys;
  export let platform: KeyboardPlatform;
  /** The last session's windows on offer, `null` while none are offered here. */
  export let reopen: { count: number; timestamp: string | null } | null;
  /** Goes to the window the card names; the shell's reducer decides which. */
  export let onGoTo: () => void;
  export let onNewWindow: () => void;
  export let onChooseWindows: () => void;

  // Next window goes where the button does, so the button shows its key; a key switched off shows none.
  $: goToChord = keys['focus-next-window'];
  $: goToKey = goToChord === null ? null : formatChord(goToChord, platform).join('+');

  let root: HTMLDivElement;

  // Only focus that was lost with the terminal is taken; focus elsewhere stays put.
  onMount(() => {
    if (document.activeElement === null || document.activeElement === document.body) {
      root.focus({ preventScroll: true });
    }
  });
</script>

<div
  class="work-state exited-card"
  role="region"
  aria-labelledby="exited-card-title"
  tabindex="-1"
  bind:this={root}
>
  <span class="work-state__icon"><Icon name="circle-x" size="xl" /></span>
  <h2 class="work-state__title" id="exited-card-title">
    {describeExitedWindow(card.name, card.exitCode)}
  </h2>
  <p class="work-state__body">{workArea.exitedBody}</p>
  {#if card.next !== null && card.nextWindowId !== null}
    <div class="exited-card__actions">
      <button type="button" class="btn btn-secondary exited-card__go-to" on:click={onGoTo}>
        {describeGoToWindow(card.next)}
        {#if goToKey !== null}<kbd class="key-hint">{goToKey}</kbd>{/if}
      </button>
      <button type="button" class="btn btn-ghost" on:click={onNewWindow}>
        {workArea.newWindow}
      </button>
    </div>
  {:else}
    <EmptyState {keys} {platform} {reopen} {onNewWindow} {onChooseWindows}>
      <svelte:fragment slot="heading" />
    </EmptyState>
  {/if}
</div>

<style>
  .exited-card {
    max-width: 460px;
  }

  /* The app's ring, not the engine's own (WebKitGTK draws a wide glow). */
  .exited-card:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 4px;
  }

  .exited-card__actions {
    display: flex;
    gap: var(--space-2);
  }

  .exited-card__go-to {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
</style>
