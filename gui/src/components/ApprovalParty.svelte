<script lang="ts">
  /**
   * One side of an approval request as a card: who asked, or what it would act
   * on. A window's name is always drawn with its mark, which its name cannot
   * forge; a client that is no window shows an icon instead, and a window that
   * does not exist yet an empty card.
   *
   * The name comes first in the markup so it is read first; the grid moves the
   * mark to the left. Nothing in the card takes focus, so the two orders never
   * meet a keyboard.
   */
  import type { UiText } from '../display/ui-text';
  import type { ApprovalParty } from '../session/approval-target';
  import Icon from './Icon.svelte';
  import WindowMark from './WindowMark.svelte';

  export let party: ApprovalParty;
  /** The card's visible heading: From or To. */
  export let heading: UiText;
  /** The card's id, for the decision buttons to be described by. */
  export let id: string;

  $: headingId = `${id}-heading`;
</script>

<section
  class="approval-party"
  class:approval-party--untargeted={party.kind === 'untargeted'}
  {id}
  aria-labelledby={headingId}
>
  <span class="approval-party__heading" id={headingId}>{heading}</span>
  <div class="approval-party__body">
    <span class="approval-party__name">{party.label}</span>
    {#if party.profile !== null}
      <span class="approval-party__profile">{party.profile}</span>
    {/if}
    {#if party.status !== null || party.windowId !== null}
      <span class="approval-party__facts">
        {#if party.status !== null}
          <span class="approval-party__status">{party.status}</span>
        {/if}
        {#if party.windowId !== null}
          <!-- The mark's name already says the ID; this copy is for the eye. -->
          <span class="approval-party__id" aria-hidden="true">{party.windowId}</span>
        {/if}
      </span>
    {/if}
    {#if party.mark !== null && party.windowId !== null}
      <span class="approval-party__mark">
        <WindowMark mark={party.mark} windowId={party.windowId} size="card" showsId={false} />
      </span>
    {:else if party.kind === 'external'}
      <span class="approval-party__mark approval-party__mark--none">
        <Icon name="plug" size="md" />
      </span>
    {/if}
  </div>
</section>

<style>
  .approval-party {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
    box-sizing: border-box;
    padding: var(--space-3);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-md);
    background-color: var(--bg-chrome);
  }

  .approval-party--untargeted {
    border-style: dashed;
    border-color: var(--line-strong);
    background-color: transparent;
  }

  .approval-party__heading {
    color: var(--fg-muted);
    font-size: var(--text-2xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .approval-party__body {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    column-gap: var(--space-2);
    row-gap: var(--space-1);
    align-items: center;
    min-width: 0;
  }

  .approval-party__body > :not(.approval-party__mark) {
    grid-column: 2;
  }

  .approval-party--untargeted .approval-party__body > * {
    grid-column: 1 / -1;
  }

  .approval-party__mark {
    grid-column: 1;
    grid-row: 1 / span 2;
    display: inline-flex;
    align-self: start;
  }

  .approval-party__mark--none {
    align-items: center;
    justify-content: center;
    width: 27px;
    height: 27px;
    color: var(--fg-muted);
  }

  .approval-party__name {
    color: var(--fg-default);
    font-size: var(--text-md);
    font-weight: var(--weight-semibold);
    overflow-wrap: anywhere;
  }

  .approval-party--untargeted .approval-party__name {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: var(--weight-regular);
  }

  .approval-party__profile {
    color: var(--fg-muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  .approval-party__facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1_5);
    align-items: center;
  }

  /* The status and the ID run under both columns, as wide as the card. */
  .approval-party__body > .approval-party__facts {
    grid-column: 1 / -1;
  }

  .approval-party__status {
    padding: 0 var(--space-1_5);
    border-radius: var(--radius-full);
    background-color: var(--bg-hover);
    color: var(--fg-default);
    font-size: var(--text-xs);
    line-height: var(--leading-xs);
  }

  .approval-party__id {
    color: var(--fg-faint);
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
  }

  @media (forced-colors: active) {
    .approval-party__status {
      border: var(--line-width) solid CanvasText;
    }
  }
</style>
