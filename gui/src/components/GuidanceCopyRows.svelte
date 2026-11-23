<script lang="ts">
  /**
   * A guidance's command and page as copyable rows, each with its own copy
   * button. Renders nothing for a part that is `null`. Each row shows the
   * display form and copies the text as it arrived.
   */
  import type { DisplayLine } from '../display/untrusted-text';
  import type { UiText } from '../display/ui-text';

  export let command: string | null;
  export let displayCommand: DisplayLine | null;
  export let url: string | null;
  export let displayUrl: DisplayLine | null;
  export let urlLabel: UiText;
  export let copyCommandLabel: UiText;
  export let copyUrlLabel: UiText;
  export let onCopy: (text: string) => void;
</script>

{#if command !== null && displayCommand !== null}
  {@const value = command}
  <div class="guidance-copy">
    <code class="guidance-copy__value">{displayCommand}</code>
    <button
      type="button"
      class="btn btn-secondary guidance-copy__button"
      on:click={() => onCopy(value)}>{copyCommandLabel}</button
    >
  </div>
{/if}
{#if url !== null && displayUrl !== null}
  {@const value = url}
  <span class="guidance-copy__label">{urlLabel}</span>
  <div class="guidance-copy">
    <code class="guidance-copy__value">{displayUrl}</code>
    <button
      type="button"
      class="btn btn-secondary guidance-copy__button"
      on:click={() => onCopy(value)}>{copyUrlLabel}</button
    >
  </div>
{/if}

<style>
  .guidance-copy__label {
    color: var(--color-text-secondary);
  }

  .guidance-copy {
    display: flex;
    gap: var(--space-sm);
    align-items: stretch;
  }

  /* 44px is the smallest comfortable pointer target. */
  .guidance-copy__value {
    flex: 1;
    min-width: 0;
    min-height: 44px;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    padding: 0 var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
    user-select: text;
  }

  .guidance-copy__button {
    flex-shrink: 0;
    min-height: 44px;
    white-space: nowrap;
  }
</style>
