<script lang="ts">
  /**
   * An approval heading's sentence, with each name in its own element rather
   * than in quotes, which a name may hold. A name can still mimic the words, so
   * the marks of the cards beside it are what tell windows apart. Draws runs
   * only, so the dialog and the notice list share it inside their own heading.
   */
  import type { HeadlineParts } from '../session/approval-types';
  import ApprovalPartyName from './ApprovalPartyName.svelte';

  export let headline: HeadlineParts;
</script>

<ApprovalPartyName party={headline.from} />{headline.verb}{#if headline.tool !== null}<code
    class="approval-tool-name">{headline.tool}</code
  >{/if}{headline.toolLink}{#if headline.to !== null}<ApprovalPartyName party={headline.to} />{/if}

<style>
  .approval-tool-name {
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
</style>
