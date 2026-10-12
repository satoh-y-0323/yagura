<script lang="ts">
  import { resolveConfirmTarget } from '../session/confirm-target';
  import type { WindowActions } from '../session/window-actions';
  import type { RoomActions } from '../session/room-actions';
  import type { WindowEntry } from '../stores/window-registry';
  import { attentionCount, resolveTone } from '../status-tone';
  import { groupByRoom } from '../sidebar/room-groups';
  import IconButton from './IconButton.svelte';
  import McpMark from './McpMark.svelte';
  import StatusReportMark from './StatusReportMark.svelte';
  import TileIcon from './TileIcon.svelte';
  import {
    buttons,
    describeAttentionBadge,
    describeInterruptHint,
    describeRoomTitle,
    describeUnknownBadge,
    describeUnknownExplanation,
    describeUnknownReasonSuffix,
    describeWindowListTitle,
    hints,
    windowList,
  } from '../ui-messages';

  export let entries: WindowEntry[];
  export let actions: WindowActions;
  export let roomActions: RoomActions;
  export let collapsed: ReadonlySet<string>;
  export let tabOrder: readonly string[];
  export let onGoToNextNeedingYou: () => void;

  /** Toggles one room's collapsed state; shared by the click and keydown handlers. */
  function handleToggleRoom(roomId: string): void {
    roomActions.toggleCollapsed(roomId);
  }
</script>

<section class="status-panel">
  <div class="status-panel__header-row">
    <h2 class="status-panel__title">{describeWindowListTitle(entries.length)}</h2>
    {#if attentionCount(entries) > 0}
      <IconButton
        class="status-panel__count-badge"
        data-tone="attention"
        label={describeAttentionBadge(attentionCount(entries))}
        hint={hints.goToNextNeedingYou}
        on:click={onGoToNextNeedingYou}>{attentionCount(entries)}</IconButton
      >
    {/if}
    <slot name="header-action" />
  </div>
  {#if entries.length === 0}
    <p class="status-panel__empty">{hints.noWindows}</p>
  {:else}
    <div class="status-panel__rooms">
      {#each groupByRoom(entries, collapsed, tabOrder) as group (group.roomId)}
        <div class="status-panel__room">
          <div class="status-panel__room-heading">
            <div
              class="status-panel__room-target"
              role="button"
              tabindex="0"
              data-collapsed={group.collapsed}
              aria-expanded={!group.collapsed}
              aria-label={describeRoomTitle(group.name, group.entries.length)}
              title={group.collapsed ? hints.expandRoom : hints.collapseRoom}
              on:click={() => handleToggleRoom(group.roomId)}
              on:keydown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  handleToggleRoom(group.roomId);
                }
              }}
            >
              <svg
                class="status-panel__room-chevron"
                aria-hidden="true"
                focusable="false"
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
                <path d="M4 6l4 4 4-4" />
              </svg>
              <span class="status-panel__room-name">{group.name}</span>
              {#if group.attention > 0}
                <span
                  class="status-panel__room-badge"
                  data-tone="attention"
                  aria-label={describeAttentionBadge(group.attention)}>{group.attention}</span
                >
              {/if}
              {#if group.unknown > 0}
                <span
                  class="status-panel__room-badge"
                  data-tone="unknown"
                  aria-label={describeUnknownBadge(group.unknown)}>{group.unknown}</span
                >
              {/if}
              <span class="status-panel__room-count">{group.entries.length}</span>
            </div>
            <div class="status-panel__room-actions">
              <IconButton
                class="status-panel__button"
                label={hints.tileRoom}
                on:click={(event) => {
                  event.stopPropagation();
                  roomActions.tile(group.roomId);
                }}
              >
                <TileIcon size={14} />
              </IconButton>
              <IconButton
                class="status-panel__button"
                label={hints.minimizeRoom}
                disabled={group.minimized === group.entries.length}
                on:click={(event) => {
                  event.stopPropagation();
                  roomActions.minimizeAll(group.roomId);
                }}
              >
                <!-- prettier-ignore -->
                <svg aria-hidden="true" focusable="false" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="5" y1="18" x2="19" y2="18"/></svg>
              </IconButton>
              <IconButton
                class="status-panel__button"
                label={hints.restoreRoom}
                disabled={group.minimized === 0}
                on:click={(event) => {
                  event.stopPropagation();
                  roomActions.restoreAll(group.roomId);
                }}
              >
                <!-- prettier-ignore -->
                <svg aria-hidden="true" focusable="false" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="6" width="16" height="12" rx="1"/><line x1="4" y1="10" x2="20" y2="10"/></svg>
              </IconButton>
            </div>
          </div>
          {#if !group.collapsed}
            <ul class="status-panel__list">
              {#each group.entries as entry (entry.windowId)}
                <!-- Derived once so `disabled` and the tooltip read one verdict. -->
                {@const interruptTarget = resolveConfirmTarget(entry.detected.status, 'interrupt')}
                <li
                  class="status-panel__item"
                  data-tone={resolveTone(entry.detected.status)}
                  data-minimized={entry.display.isMinimized}
                >
                  <div
                    class="status-panel__target"
                    data-window-id={entry.windowId}
                    role="button"
                    tabindex="0"
                    aria-label={entry.name}
                    title={hints.focusWindow}
                    on:click={() => actions.focus(entry)}
                    on:keydown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault();
                        actions.focus(entry);
                      }
                    }}
                  >
                    <div class="status-panel__header">
                      <span class="status-panel__name" title={entry.name}>{entry.name}</span>
                      <McpMark mcpConnected={entry.mcpConnected} />
                      <StatusReportMark statusReportsMissing={entry.statusReportsMissing} />
                      <!-- prettier-ignore -->
                      <span class="status-panel__status" title={describeUnknownExplanation(entry.detected, entry.detectionWarning)}>{windowList.shortStatus[entry.detected.status]}{describeUnknownReasonSuffix(entry.detected)}</span>
                    </div>
                    <div class="status-panel__info">
                      {#if entry.vendor}
                        <span class="status-panel__vendor">{entry.vendor}</span>
                      {/if}
                      {#if entry.display.isMinimized}
                        <span class="status-panel__cwd">{hints.minimizedMarker}</span>
                      {:else}
                        <span class="status-panel__cwd" title={entry.cwd}>{entry.cwd}</span>
                      {/if}
                    </div>
                  </div>
                  <div class="status-panel__actions">
                    <IconButton
                      class="status-panel__button status-panel__button--warning"
                      label={buttons.interrupt}
                      hint={describeInterruptHint(interruptTarget)}
                      disabled={!interruptTarget.allowed}
                      on:click={() => {
                        void actions.interrupt(entry);
                      }}
                    >
                      ⏹
                    </IconButton>
                    <IconButton
                      class="status-panel__button status-panel__button--danger"
                      label={buttons.close}
                      hint={hints.closeWindow}
                      on:click={() => {
                        void actions.close(entry);
                      }}
                    >
                      ✕
                    </IconButton>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .status-panel {
    display: flex;
    flex-direction: column;
    flex: 1 1 auto;
    min-height: 0;
  }

  /* The sidebar itself has no padding so the rows reach both edges; the
     heading supplies its own inset, aligned with the row text. */
  .status-panel__header-row {
    padding: var(--space-sm);
  }

  .status-panel__title {
    margin: 0;
    flex: 1 1 auto;
    font-size: var(--font-size-base);
    font-weight: 600;
  }

  :global(.status-panel__count-badge) {
    flex: 0 0 auto;
    border: none;
    font-family: inherit;
    cursor: pointer;
    padding: 0 var(--space-xs);
    border-radius: 999px;
    font-size: var(--font-size-sm);
    font-weight: 600;
  }

  .status-panel__empty {
    margin: 0;
    padding: var(--space-md);
    text-align: center;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .status-panel__rooms {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow-y: auto;
  }

  .status-panel__room {
    display: flex;
    flex-direction: column;
  }

  .status-panel__room-target {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    cursor: pointer;
  }

  .status-panel__room-target[data-collapsed='true'] .status-panel__room-chevron {
    transform: rotate(-90deg);
  }

  .status-panel__room-chevron {
    flex: 0 0 auto;
  }

  .status-panel__room-name {
    flex: 1 1 auto;
    min-width: 0;
    font-size: var(--font-size-sm);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status-panel__room-badge,
  .status-panel__room-count {
    flex: 0 0 auto;
    min-width: 1.25em;
    padding: 0 var(--space-xs);
    border-radius: 999px;
    text-align: center;
    color: var(--color-text-secondary);
  }

  .status-panel__room-actions {
    display: flex;
    gap: var(--space-xs);
  }

  .status-panel__list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }

  /* Rows sit flush against each other, so a single shared edge separates
     them instead of a frame plus a gap. */
  .status-panel__item {
    padding: var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-background);
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .status-panel__item[data-minimized='true'] {
    opacity: 0.55;
  }

  .status-panel__target {
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  /* justify-content: space-between for .status-panel__header is consolidated
     into components.css (shared with .dialog-header). */
  .status-panel__name {
    flex: 1 1 auto;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status-panel__status {
    flex: 0 0 auto;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    font-size: var(--font-size-sm);
    font-weight: 500;
  }

  .status-panel__info {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    font-size: var(--font-size-sm);
    color: var(--color-text-secondary);
  }

  .status-panel__vendor {
    display: inline-block;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-border);
    width: fit-content;
  }

  .status-panel__cwd {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status-panel__actions {
    display: flex;
    gap: var(--space-xs);
  }

  :global(.status-panel__button.status-panel__button--warning) {
    background-color: var(--color-warning-action);
    color: var(--color-on-warning-action);
    border-color: var(--color-warning-action);
  }

  :global(.status-panel__button.status-panel__button--warning:hover) {
    background-color: var(--color-warning-action-hover);
  }

  :global(.status-panel__button.status-panel__button--danger) {
    background-color: var(--color-error);
    color: var(--color-background);
    border-color: var(--color-error);
  }

  :global(.status-panel__button.status-panel__button--danger:hover) {
    background-color: var(--color-error-hover);
  }
</style>
