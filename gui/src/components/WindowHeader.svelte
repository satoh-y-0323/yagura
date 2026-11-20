<script context="module" lang="ts">
  let instanceCount = 0;
</script>

<script lang="ts">
  /**
   * The header of one window, the same in Focus and on the Canvas: where the
   * window belongs, its name, its status, its marks and its operations.
   * Display only; every operation goes through `WindowControls`.
   *
   * The marks are icons, so what each means -- with the vendor and the
   * working directory, which are not shown as text -- is the header's
   * accessible description rather than a tooltip alone.
   */
  import type { WindowActions } from '../session/window-actions';
  import type { WindowEntry } from '../stores/window-registry';
  import { roomNameOf } from '../sidebar/room-groups';
  import type { ThemeStore } from '../theme/theme-store';
  import {
    describeUnknownExplanation,
    describeUnknownReasonSuffix,
    describeWindowHeader,
    hints,
  } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import { mcpMarkOf } from './mcp-mark';
  import McpMark from './McpMark.svelte';
  import QueuedMark from './QueuedMark.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { statusReportMarkOf } from './status-report-mark';
  import StatusReportMark from './StatusReportMark.svelte';
  import { tooltip } from './tooltip';
  import WindowControls from './WindowControls.svelte';

  export let entry: WindowEntry;
  export let actions: WindowActions;
  /** The application theme store; the window menu reads and overrides it. */
  export let themeStore: ThemeStore;
  /** `canvas` adds minimize and maximize; Focus has no window size to change. */
  export let variant: 'focus' | 'canvas';
  /** Callback to open the rename dialog for this window. */
  export let onRename: (() => void) | undefined = undefined;

  const instance = instanceCount++;
  const titleId = `window-header-title-${instance}`;
  const descriptionId = `window-header-description-${instance}`;

  // The same name the sidebar's room heading shows for this window.
  $: placeName = roomNameOf(entry);
  $: unknownExplanation = describeUnknownExplanation(entry.detected, entry.displayDetectionWarning);
  $: description = describeWindowHeader({
    vendor: entry.displayVendor,
    cwd: entry.displayCwd,
    showsMcpMark: mcpMarkOf(entry.mcpConnected) !== null,
    showsStatusReportMark: statusReportMarkOf(entry.statusReportsMissing) !== null,
    queuedCount: entry.queuedCount,
  });
</script>

<div
  class="window-header window-header--{variant}"
  role="group"
  aria-labelledby={titleId}
  aria-describedby={descriptionId}
>
  <span class="window-header__place">
    <Icon name={entry.room === null ? 'folder' : 'messages-square'} size="xs" />
    <span
      class="window-header__place-name"
      use:tooltip={{ text: placeName, announcedElsewhere: true }}>{placeName}</span
    >
    <Icon name="chevron-right" size="xs" />
  </span>
  <h2
    id={titleId}
    class="window-header__title"
    use:tooltip={{ text: entry.displayName, announcedElsewhere: true }}
  >
    {entry.displayName}
  </h2>
  <StatusBadge
    status={entry.detected.status}
    variant="label"
    detail={describeUnknownReasonSuffix(entry.detected)}
  />
  {#if unknownExplanation !== ''}
    <IconButton
      data-window-control
      class="window-header__why"
      label={hints.whyUnknown}
      toggletip={unknownExplanation}
    >
      <Icon name="info" size="sm" />
    </IconButton>
  {/if}
  <span class="window-header__marks">
    <McpMark mcpConnected={entry.mcpConnected} />
    <StatusReportMark statusReportsMissing={entry.statusReportsMissing} />
    <QueuedMark count={entry.queuedCount} />
  </span>
  <span class="window-header__spacer"></span>
  <WindowControls
    {entry}
    {actions}
    {themeStore}
    {onRename}
    offersSizeControls={variant === 'canvas'}
  />
  <span id={descriptionId} hidden>{description}</span>
</div>

<style>
  .window-header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    border-bottom: var(--line-width) solid var(--line);
    background-color: var(--bg-chrome);
    color: var(--fg-default);
  }

  .window-header--focus {
    height: 36px;
    padding: 0 var(--space-2) 0 var(--space-3);
  }

  .window-header--canvas {
    height: 32px;
    padding: 0 var(--space-1) 0 var(--space-2);
  }

  .window-header__place {
    display: inline-flex;
    flex: 0 1 auto;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    max-width: 30%;
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .window-header__place-name,
  .window-header__title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .window-header__title {
    flex: 0 1 auto;
    min-width: 0;
    margin: 0;
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
  }

  /* A ghost icon button: no frame, the hover and focus layers come from the tokens. */
  :global(.window-header__why) {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    min-width: var(--icon-button-hit-min);
    min-height: var(--icon-button-hit-min);
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background-color: transparent;
    color: var(--fg-muted);
    cursor: pointer;
  }

  :global(.window-header__why:hover) {
    background-color: var(--bg-hover);
  }

  :global(.window-header__why:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  /* The marks recede behind the status pill: one muted colour for all of them. */
  .window-header__marks {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-2);
    color: var(--fg-muted);
  }

  .window-header__marks :global(svg) {
    color: var(--fg-muted);
  }

  .window-header__spacer {
    flex: 1 1 auto;
  }
</style>
