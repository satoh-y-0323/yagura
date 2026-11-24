<script lang="ts">
  /**
   * An approval heading's sentence, with each name in its own element rather
   * than in quotes, which a name may hold. A name can still mimic the words, so
   * the marks of the cards beside it are what tell windows apart. Draws runs
   * only, so the dialog and the notice list share it inside their own heading.
   */
  import type { HeadlineParts } from '../session/approval-types';

  export let headline: HeadlineParts;
</script>

{#if headline.fromName !== null}<strong class="approval-name">{headline.fromName}</strong
  >{:else if headline.fromWords !== null}{headline.fromWords}{/if}{headline.verb}{#if headline.tool !== null}<code
    class="approval-tool-name">{headline.tool}</code
  >{/if}{headline.toolLink}{#if headline.toName !== null}<strong class="approval-name"
    >{headline.toName}</strong
  >{:else if headline.toWords !== null}{headline.toWords}{/if}

<style>
  /* A name stands apart from the words by weight and ground, neither of which its characters can draw. */
  .approval-name {
    padding: 0 var(--space-1);
    border-radius: var(--radius-xs);
    background-color: var(--bg-selected);
    font-weight: var(--weight-semibold);
  }

  .approval-tool-name {
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  @media (forced-colors: active) {
    .approval-name {
      border: var(--line-width) solid CanvasText;
    }
  }
</style>
