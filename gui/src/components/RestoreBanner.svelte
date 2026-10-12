<script lang="ts">
  import { buttons, hints, describeRestoreBanner } from '../ui-messages';
  import IconButton from './IconButton.svelte';

  /** Number of restore candidates from the previous boot. */
  export let count: number;
  /** Formatted recording time, `null` when unknown. */
  export let timestamp: string | null = null;
  /** Opens the restore dialog. */
  export let onChoose: () => void;
  /** Dismisses the banner without opening the dialog. */
  export let onDismiss: () => void;
</script>

<div class="restore-banner" role="alert">
  <span class="restore-banner__text">{describeRestoreBanner(count, timestamp)}</span>
  <button type="button" class="btn btn-primary restore-banner__action" on:click={onChoose}>
    {buttons.chooseWindows}
  </button>
  <IconButton class="restore-banner__close" label={hints.dismissBanner} on:click={onDismiss}>
    ✕
  </IconButton>
</div>

<style>
  .restore-banner {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-surface);
    color: var(--color-text-primary);
    font-size: var(--font-size-sm);
  }

  .restore-banner__text {
    flex: 1 1 auto;
  }

  :global(.restore-banner__close) {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: var(--font-size-base);
  }
</style>
