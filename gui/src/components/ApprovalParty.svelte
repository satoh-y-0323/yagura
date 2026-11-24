<script lang="ts">
  /**
   * One side of an approval request as a card: who asked, or what it would act
   * on. A window's name is always drawn with its mark, which its name cannot
   * forge; a client that is no window shows an icon instead. What a request
   * names in place of a window (one not opened yet, or a chat room) has no mark,
   * only the words or the room's name its heading names it by.
   *
   * What the daemon vouches for (the mark, the profile and the status) comes
   * before the name in the markup, and the mark in the card's label, so it is
   * read before the name, whose characters may mimic it. The grid places each
   * part by row, so the eye sees another order. Nothing in the card takes
   * focus, so no keyboard meets the order.
   *
   * The compact form is a chip of the mark and the name, for a list row. Its
   * heading, profile, status and ID are hidden from the eye but still read, so
   * a screen reader hears the same card in either form. It is no landmark: a
   * list of notices would otherwise hold two for every row.
   */
  import type { UiText } from '../display/ui-text';
  import type { ApprovalParty } from '../session/approval-types';
  import ApprovalPartyName from './ApprovalPartyName.svelte';
  import Icon from './Icon.svelte';
  import WindowMark from './WindowMark.svelte';

  export let party: ApprovalParty;
  /** The card's visible heading: From or To. */
  export let heading: UiText;
  /** The card's id, for the decision buttons to be described by. */
  export let id: string;
  /** `compact` is the chip a notice row draws; `card` is the dialog's card. */
  export let size: 'card' | 'compact' = 'card';

  $: headingId = `${id}-heading`;
  $: markId = `${id}-mark`;
  $: nameId = `${id}-name`;
  $: labelledBy =
    size === 'compact'
      ? undefined
      : [headingId, ...(party.kind === 'window' ? [markId] : []), nameId].join(' ');
</script>

<svelte:element
  this={size === 'compact' ? 'div' : 'section'}
  class="approval-party"
  class:approval-party--untargeted={party.kind === 'untargeted'}
  class:approval-party--compact={size === 'compact'}
  {id}
  aria-labelledby={labelledBy}
>
  <span class="approval-party__heading" id={headingId}>{heading}</span>
  <div class="approval-party__body">
    {#if party.kind === 'window'}
      <span class="approval-party__mark" id={markId}>
        <WindowMark
          mark={party.window.mark}
          windowId={party.window.id}
          size={size === 'compact' ? 'header' : 'card'}
          showsId={false}
        />
      </span>
    {:else if party.kind === 'external'}
      <span class="approval-party__mark approval-party__mark--none">
        <Icon name="plug" size={size === 'compact' ? 'sm' : 'md'} />
      </span>
    {/if}
    {#if party.profile !== null}
      <span class="approval-party__profile">{party.profile}</span>
    {/if}
    {#if party.status !== null || party.kind === 'window'}
      <span class="approval-party__facts">
        {#if party.status !== null}
          <span class="approval-party__status">{party.status}</span>
        {/if}
        {#if party.kind === 'window'}
          <!-- The mark's name already says the ID; this copy is for the eye. -->
          <span class="approval-party__id" aria-hidden="true">{party.window.id}</span>
        {/if}
      </span>
    {/if}
    <span class="approval-party__name" id={nameId}
      ><ApprovalPartyName party={party.label} windowName="plain" /></span
    >
  </div>
</svelte:element>

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
    grid-row: 1;
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
    grid-row: 2;
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
    grid-row: 3;
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

  .approval-party--compact {
    position: relative;
    display: inline-flex;
    flex-direction: row;
    align-items: center;
    padding: var(--space-0_5) var(--space-2) var(--space-0_5) var(--space-1);
    border-radius: var(--radius-full);
    background-color: transparent;
  }

  .approval-party--compact.approval-party--untargeted {
    padding-left: var(--space-2);
  }

  .approval-party--compact .approval-party__body {
    display: inline-flex;
    gap: var(--space-1_5);
  }

  .approval-party--compact .approval-party__name {
    font-size: var(--text-xs);
    font-weight: var(--weight-regular);
  }

  .approval-party--compact .approval-party__mark {
    align-self: center;
  }

  .approval-party--compact .approval-party__mark--none {
    width: auto;
    height: auto;
  }

  /* Clipped to nothing rather than removed, so it is still read out. */
  .approval-party--compact .approval-party__heading,
  .approval-party--compact .approval-party__profile,
  .approval-party--compact .approval-party__facts {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  @media (forced-colors: active) {
    .approval-party__status {
      border: var(--line-width) solid CanvasText;
    }
  }
</style>
