<script lang="ts">
  /**
   * One window in the sidebar, on one line. Display only: what the row says and
   * how loud it is come from its props and the shared "needs you" decision.
   */
  import type { WindowEntry } from '../stores/window-registry';
  import { needsYou } from '../status-tone';
  import { describeUnknownExplanation, describeWindowRow, sidebar } from '../ui-messages';
  import Icon from './Icon.svelte';
  import QueuedMark from './QueuedMark.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { tooltip } from './tooltip';

  export let entry: WindowEntry;
  export let isCurrent: boolean;
  /** Minimized on the canvas: drawn faint, with the minimize mark. */
  export let isMinimized: boolean;
  /** Still listed by the Needs you filter although it no longer needs the user. */
  export let isSettled = false;
  /** A permission request from this window waits for the user. */
  export let isAwaitingPermission: boolean;
  export let onSelect: (entry: WindowEntry) => void;

  // Unknown is not counted as needing the user, but it is not quiet either.
  $: isProminent = !isSettled && (needsYou(entry) || entry.detected.status === 'unknown');
  $: explanation = describeUnknownExplanation(entry.detected, entry.displayDetectionWarning);
  $: nameId = `navigator-row-name-${entry.windowId}`;
  $: whyId = `navigator-row-why-${entry.windowId}`;
  $: minimizedId = `navigator-row-minimized-${entry.windowId}`;
  $: describedBy =
    [explanation === '' ? null : whyId, isMinimized ? minimizedId : null]
      .filter((id) => id !== null)
      .join(' ') || undefined;
</script>

<li class="navigator-row__item">
  <button
    type="button"
    class="navigator-row"
    data-window-id={entry.windowId}
    data-prominent={isProminent}
    data-minimized={isMinimized}
    aria-current={isCurrent ? 'true' : undefined}
    aria-labelledby={nameId}
    aria-describedby={describedBy}
    on:click={() => onSelect(entry)}
  >
    <span id={nameId} hidden
      >{describeWindowRow(entry.displayName, entry.detected.status, isAwaitingPermission)}</span
    >
    {#if explanation !== ''}
      <span id={whyId} hidden>{explanation}</span>
    {/if}
    {#if isMinimized}
      <span id={minimizedId} hidden>{sidebar.minimized}</span>
    {/if}
    <StatusBadge status={entry.detected.status} variant="icon" />
    <span
      class="navigator-row__name"
      use:tooltip={{ text: entry.displayName, announcedElsewhere: true }}>{entry.displayName}</span
    >
    {#if entry.displayVendor !== null}
      <span class="navigator-row__vendor">{entry.displayVendor}</span>
    {/if}
    <span class="navigator-row__tail">
      {#if isAwaitingPermission}
        <span
          class="navigator-row__permission"
          use:tooltip={{ text: sidebar.awaitingPermission, announcedElsewhere: true }}
        >
          <Icon name="shield-exclamation-point" size="xs" />
        </span>
      {/if}
      <QueuedMark count={entry.queuedCount} />
      {#if isMinimized}
        <Icon name="minus" size="xs" />
      {/if}
    </span>
  </button>
</li>

<style>
  .navigator-row__item {
    list-style: none;
  }

  .navigator-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    height: var(--row-height);
    padding: 0 var(--space-1_5) 0 var(--space-2);
    border: none;
    border-radius: var(--radius-sm);
    background-color: transparent;
    color: var(--fg-muted);
    font-family: inherit;
    font-size: var(--text-sm);
    font-weight: var(--weight-regular);
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }

  .navigator-row:hover {
    background-color: var(--bg-hover);
  }

  .navigator-row:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: -2px;
  }

  .navigator-row[aria-current='true'] {
    background-color: var(--bg-selected);
  }

  .navigator-row[aria-current='true']::before {
    content: '';
    position: absolute;
    top: var(--space-1_5);
    bottom: var(--space-1_5);
    left: 0;
    width: 2px;
    border-radius: 2px;
    background-color: var(--accent);
  }

  /* The vendor has no basis of its own, so it gives up its width before the name does. */
  .navigator-row__name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .navigator-row[data-prominent='true'] .navigator-row__name {
    color: var(--fg-default);
    font-weight: var(--weight-medium);
  }

  .navigator-row[data-minimized='true'] .navigator-row__name {
    color: var(--fg-faint);
    font-weight: var(--weight-regular);
  }

  .navigator-row__vendor {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .navigator-row__tail {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1_5);
    margin-left: auto;
    color: var(--fg-muted);
    font-size: var(--text-2xs);
  }

  .navigator-row[data-minimized='true'] .navigator-row__tail {
    color: var(--fg-faint);
  }

  .navigator-row__permission {
    display: inline-flex;
    color: var(--st-approval);
  }

  /* In a row the count sits bare beside its envelope, quieter than the title bar's framed mark. */
  .navigator-row__tail :global(.queued-mark) {
    padding: 0;
    border: none;
    color: inherit;
    font-size: inherit;
  }

  @media (forced-colors: active) {
    .navigator-row[aria-current='true']::before {
      content: none;
    }

    .navigator-row[aria-current='true'] {
      border-left: 2px solid Highlight;
    }
  }
</style>
