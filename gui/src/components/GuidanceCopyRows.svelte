<script lang="ts">
  /**
   * A guidance's command and page as copyable rows, each with its own copy
   * button. Renders nothing for a part that is `null`.
   */
  export let command: string | null;
  export let url: string | null;
  export let urlLabel: string;
  export let copyCommandLabel: string;
  export let copyUrlLabel: string;
  export let onCopy: (text: string) => void;
</script>

{#if command !== null}
  {@const value = command}
  <div class="guidance-copy">
    <code class="guidance-copy__value">{value}</code>
    <button
      type="button"
      class="btn btn-secondary guidance-copy__button"
      on:click={() => onCopy(value)}>{copyCommandLabel}</button
    >
  </div>
{/if}
{#if url !== null}
  {@const value = url}
  <span class="guidance-copy__label">{urlLabel}</span>
  <div class="guidance-copy">
    <code class="guidance-copy__value">{value}</code>
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
    font-family: var(--font-family-mono);
    overflow-wrap: anywhere;
    user-select: text;
  }

  .guidance-copy__button {
    flex-shrink: 0;
    min-height: 44px;
    white-space: nowrap;
  }
</style>
