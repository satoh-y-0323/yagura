<script lang="ts">
  /**
   * One window in the sidebar, on one line. Display only: what the row says and
   * how loud it is come from its props and the shared "needs you" decision.
   */
  import type { UiText } from '../display/ui-text';
  import type { WindowEntry } from '../stores/window-registry';
  import { isProminentWindow } from '../status-tone';
  import {
    describeUnknownExplanation,
    describeWindowLocation,
    describeWindowRow,
    nav,
    sidebar,
  } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import QueuedMark from './QueuedMark.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { tooltip } from './tooltip';

  export let entry: WindowEntry;
  export let isCurrent: boolean;
  /** The list's one Tab stop; the other rows are reached with the row keys. */
  export let isTabStop: boolean;
  /** Minimized on the canvas: drawn faint, with the minimize mark. */
  export let isMinimized: boolean;
  /** Still listed by the Needs you filter although it no longer needs the user. */
  export let isSettled = false;
  /** A permission request from this window waits for the user. */
  export let isAwaitingPermission: boolean;
  export let onSelect: (entry: WindowEntry) => void;
  /** The `⋯` button was pressed; the menu opens beside it. */
  export let onOpenMenu: (entry: WindowEntry, anchor: HTMLButtonElement) => void;
  export let isMenuOpen = false;
  /** The row is held by a drag. */
  export let isDragging = false;
  /** The held row would land on this edge of the row. */
  export let dropEdge: 'before' | 'after' | null = null;

  let menuButton: HTMLButtonElement | undefined;

  // Unknown is not counted as needing the user, but it is not quiet either.
  $: isProminent = !isSettled && isProminentWindow(entry);
  $: explanation = describeUnknownExplanation(entry.detected, entry.displayDetectionWarning);
  // The card repeats what the window's header says: where it runs and why its
  // status is unknown.
  $: location = describeWindowLocation({ vendor: entry.displayVendor, cwd: entry.displayCwd });
  let cardDetails: readonly UiText[];
  $: cardDetails = explanation === null ? [location] : [location, explanation];
  $: nameId = `navigator-row-name-${entry.windowId}`;
  $: whyId = `navigator-row-why-${entry.windowId}`;
  $: minimizedId = `navigator-row-minimized-${entry.windowId}`;
  $: describedBy =
    [explanation === null ? null : whyId, isMinimized ? minimizedId : null]
      .filter((id) => id !== null)
      .join(' ') || undefined;
</script>

<li class="navigator-row__item" data-drop-edge={dropEdge ?? undefined}>
  <button
    type="button"
    class="navigator-row"
    data-window-id={entry.windowId}
    data-prominent={isProminent}
    data-minimized={isMinimized}
    data-dragging={isDragging}
    tabindex={isTabStop ? 0 : -1}
    aria-current={isCurrent ? 'true' : undefined}
    aria-labelledby={nameId}
    aria-describedby={describedBy}
    use:tooltip={{ text: entry.displayName, details: cardDetails, announcedElsewhere: true }}
    on:click={() => onSelect(entry)}
  >
    <span id={nameId} hidden
      >{describeWindowRow(entry.displayName, entry.detected.status, isAwaitingPermission)}</span
    >
    {#if explanation !== null}
      <span id={whyId} hidden>{explanation}</span>
    {/if}
    {#if isMinimized}
      <span id={minimizedId} hidden>{sidebar.minimized}</span>
    {/if}
    <StatusBadge status={entry.detected.status} variant="icon" />
    <span class="navigator-row__name">{entry.displayName}</span>
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
  <!-- Out of the tab order: from the keyboard, Shift+F10 and the ContextMenu key open the same menu. -->
  <IconButton
    class="btn btn-ghost navigator-row__more"
    label={nav.windowMenu}
    tabindex={-1}
    aria-haspopup="menu"
    aria-expanded={isMenuOpen}
    bind:element={menuButton}
    on:click={() => {
      if (menuButton !== undefined) {
        onOpenMenu(entry, menuButton);
      }
    }}
  >
    <Icon name="ellipsis" size="sm" />
  </IconButton>
</li>

<style>
  .navigator-row__item {
    position: relative;
    list-style: none;
  }

  .navigator-row__item[data-drop-edge='before'] {
    box-shadow: inset 0 var(--line-width-strong) 0 var(--accent);
  }

  .navigator-row__item[data-drop-edge='after'] {
    box-shadow: inset 0 calc(-1 * var(--line-width-strong)) 0 var(--accent);
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

  .navigator-row[data-dragging='true'] {
    opacity: 0.6;
    cursor: grabbing;
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

  /* Revealed on hover and while focus is in the row; always there without hover. */
  .navigator-row__item :global(.navigator-row__more) {
    position: absolute;
    top: 50%;
    right: var(--space-0_5);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--icon-button-hit-min);
    min-height: var(--icon-button-hit-min);
    padding: 0;
    background-color: var(--bg-hover);
    color: var(--fg-muted);
    opacity: 0;
    transform: translateY(-50%);
  }

  .navigator-row__item:hover :global(.navigator-row__more),
  .navigator-row__item:focus-within :global(.navigator-row__more),
  .navigator-row__item :global(.navigator-row__more[aria-expanded='true']) {
    opacity: 1;
  }

  @media (hover: none) {
    .navigator-row__item :global(.navigator-row__more) {
      opacity: 1;
    }
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
