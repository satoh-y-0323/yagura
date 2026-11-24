<script lang="ts">
  /**
   * How an approval heading and a card name one party: a name in its own
   * element rather than in quotes, which a name may hold, and words as they are.
   * The one place the name's chip is drawn, so a heading and a card set a name
   * apart alike. Draws runs only, so it sits inside the caller's own element.
   */
  import { partyRuns, type HeadlineParty } from '../session/approval-types';

  export let party: HeadlineParty;
  /**
   * How a window's name is drawn: `chip` in a heading, where words surround it;
   * `plain` on a card, where the name stands alone.
   */
  export let windowName: 'chip' | 'plain' = 'chip';

  $: runs = partyRuns(party, windowName);
</script>

{#each runs as run, index (index)}{#if run.chip}<strong class="approval-name">{run.text}</strong
    >{:else}{run.text}{/if}{/each}

<style>
  /* A name stands apart from the words by weight and ground, neither of which its characters can draw. */
  .approval-name {
    padding: 0 var(--space-1);
    border-radius: var(--radius-xs);
    background-color: var(--bg-selected);
    font-weight: var(--weight-semibold);
  }

  @media (forced-colors: active) {
    .approval-name {
      border: var(--line-width) solid CanvasText;
    }
  }
</style>
