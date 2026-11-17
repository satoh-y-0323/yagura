<script lang="ts">
  /** The mark that flags plans needing the user; shared by the rail and the Board tab. */
  import type { PlansAttention } from '../plans/plan-board-model';
  import { plans } from '../ui-messages';

  export let attention: NonNullable<PlansAttention>;

  $: text = plans.plansAttentionBadge(attention.kind === 'pending', attention.count);
</script>

<span class="plans-attention-mark" data-tone="attention" aria-label={text} title={text}></span>

<style>
  .plans-attention-mark {
    display: inline-block;
    min-width: 16px;
    height: 16px;
    padding: 0 3px;
    border-radius: 999px;
    background-color: var(--st-approval-tint);
    color: var(--fg-default);
    box-shadow: inset 0 0 0 var(--line-width) var(--st-approval);
    font-size: 10px;
    line-height: 16px;
    text-align: center;
    vertical-align: middle;
  }
</style>
