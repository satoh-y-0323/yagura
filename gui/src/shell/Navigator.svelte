<script lang="ts">
  /**
   * The sidebar: the window list with its header, its room headings and the
   * Needs you filter. It draws what the shell derived and calls back; the
   * filter's rows, the rooms and the work mode are decided elsewhere.
   */
  import Icon from '../components/Icon.svelte';
  import IconButton from '../components/IconButton.svelte';
  import NavigatorRow from '../components/NavigatorRow.svelte';
  import PopupMenu from '../components/PopupMenu.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import type { Rect } from '../components/menu-position';
  import type { WorkMode } from '../session/current-window';
  import type { RoomActions } from '../session/room-actions';
  import { needsYouSections, type NeedsYouView } from '../sidebar/needs-you-view';
  import type { RoomGroup } from '../sidebar/room-groups';
  import { attentionCount, summarizeWindowStatuses } from '../status-tone';
  import type { WindowEntry } from '../stores/window-registry';
  import {
    describeAttentionBadge,
    describeRoomTitle,
    describeStatus,
    hints,
    sidebar,
  } from '../ui-messages';

  export let entries: readonly WindowEntry[];
  /** The rooms as the sidebar renders them, top to bottom. */
  export let groups: readonly RoomGroup<WindowEntry>[];
  export let currentWindowId: string | null;
  export let workMode: WorkMode;
  /** The windows a permission request waits on. */
  export let awaitingPermission: ReadonlySet<string>;
  /** What the Needs you filter lists; `null` while the filter is off. */
  export let needsYouView: NeedsYouView | null;
  export let roomActions: RoomActions;
  /** The width the user gave the sidebar, in pixels. */
  export let width: number;
  /** The sidebar element, for the shell's focus moves. */
  export let element: HTMLElement | null = null;
  export let onToggleNeedsYou: () => void;
  export let onSelectWindow: (entry: WindowEntry) => void;
  export let onChooseWorkMode: (mode: WorkMode) => void;
  export let onTileWindows: () => void;
  export let onNewWindow: () => void;
  export let onOpenChat: (roomId: string) => void;

  type MenuReason = 'escape' | 'tab' | 'outside' | 'resize';

  let viewButton: HTMLButtonElement | undefined;
  let isViewMenuOpen = false;
  let menuRoomId: string | null = null;
  let menuAnchor: Rect | null = null;
  let menuTrigger: HTMLElement | undefined;

  $: needingCount = attentionCount(entries);
  $: isCanvas = workMode === 'canvas';
  $: sections = needsYouView === null ? [] : needsYouSections(needsYouView.rows);
  // The room menu belongs to a heading the full list draws; it goes when that heading does.
  $: menuGroup =
    needsYouView === null ? groups.find((group) => group.roomId === menuRoomId) : undefined;
  $: if (menuRoomId !== null && menuGroup === undefined) {
    closeRoomMenu();
  }
  $: viewAnchor = isViewMenuOpen && viewButton ? rectOf(viewButton) : null;

  function rectOf(target: HTMLElement): Rect {
    const rect = target.getBoundingClientRect();
    return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
  }

  /** By position: a folder's id is its path, which may hold spaces an IDREF list cannot. */
  function roomElementId(part: string, index: number): string {
    return `navigator-room-${part}-${index}`;
  }

  function entryOf(windowId: string): WindowEntry | undefined {
    return entries.find((entry) => entry.windowId === windowId);
  }

  function groupOf(roomId: string): RoomGroup<WindowEntry> | undefined {
    return groups.find((group) => group.roomId === roomId);
  }

  function closeViewMenu(reason: MenuReason): void {
    isViewMenuOpen = false;
    if (reason === 'escape') {
      viewButton?.focus();
    }
  }

  /** Closes the view menu, gives focus back to its button, then carries out the choice. */
  function chooseFromViewMenu(action: () => void): void {
    isViewMenuOpen = false;
    viewButton?.focus();
    action();
  }

  function openRoomMenu(roomId: string, event: MouseEvent): void {
    const trigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : undefined;
    if (menuRoomId === roomId && trigger === menuTrigger) {
      closeRoomMenu();
      return;
    }
    menuRoomId = roomId;
    menuAnchor =
      trigger === undefined
        ? { left: event.clientX, top: event.clientY, width: 0, height: 0 }
        : rectOf(trigger);
    menuTrigger = trigger;
  }

  function closeRoomMenu(): void {
    menuRoomId = null;
    menuAnchor = null;
    menuTrigger = undefined;
  }

  function handleRoomMenuClose(reason: MenuReason): void {
    const trigger = menuTrigger;
    closeRoomMenu();
    if (reason === 'escape') {
      trigger?.focus();
    }
  }

  /** Closes the room menu, gives focus back to its button, then acts on the room. */
  function chooseFromRoomMenu(action: (roomId: string) => void): void {
    const roomId = menuRoomId;
    const trigger = menuTrigger;
    closeRoomMenu();
    trigger?.focus();
    if (roomId !== null) {
      action(roomId);
    }
  }
</script>

<aside class="navigator" bind:this={element} style="flex-basis: {width}px;">
  <div class="navigator__header">
    <h2 class="navigator__title">
      {sidebar.title}
      <span class="navigator__count">{entries.length}</span>
    </h2>
    <button
      type="button"
      class="btn btn-ghost navigator__tool navigator__filter"
      aria-pressed={needsYouView !== null}
      on:click={onToggleNeedsYou}
    >
      <Icon name="funnel" size="xs" />
      {sidebar.needsYou}
      {#if needingCount > 0}
        <span class="navigator__filter-count">{needingCount}</span>
      {/if}
    </button>
    <IconButton
      class="btn btn-ghost navigator__tool"
      label={sidebar.viewMenu(isCanvas ? sidebar.canvasMode : sidebar.focusMode)}
      aria-haspopup="menu"
      aria-expanded={isViewMenuOpen}
      bind:element={viewButton}
      on:click={() => {
        isViewMenuOpen = !isViewMenuOpen;
      }}
    >
      <Icon name={isCanvas ? 'layout-grid' : 'square'} size="sm" />
    </IconButton>
    <IconButton
      class="btn btn-ghost navigator__tool"
      label={sidebar.newWindow}
      on:click={onNewWindow}
    >
      <Icon name="plus" size="sm" />
    </IconButton>
  </div>

  <div class="navigator__list">
    {#if entries.length === 0}
      <p class="navigator__empty">{sidebar.empty}</p>
    {:else if needsYouView !== null}
      {#if needsYouView.showsNothingNeedsYou}
        <div class="navigator__nothing">
          <Icon name="check" size="md" />
          <p>{sidebar.nothingNeedsYou}</p>
          <button type="button" class="btn btn-ghost" on:click={onToggleNeedsYou}
            >{sidebar.showAllWindows}</button
          >
        </div>
      {/if}
      <!-- A section starts at its first row, and rows never move, so that row names the section. -->
      {#each sections as section (section.rows[0]?.windowId)}
        {@const group = groupOf(section.roomId)}
        {@const count = group?.entries.length ?? section.rows.length}
        <!-- A static heading: the filter shows a collapsed room's rows too, so it does not toggle. -->
        <h3
          class="navigator__room-heading navigator__room-label"
          aria-label={describeRoomTitle(section.name, count)}
        >
          <Icon name={group?.kind === 'folder' ? 'folder' : 'messages-square'} size="sm" />
          <span class="navigator__room-name">{section.name}</span>
          <span class="navigator__room-count">{count}</span>
        </h3>
        <ul class="navigator__rows">
          {#each section.rows as row (row.windowId)}
            {@const entry = entryOf(row.windowId)}
            {#if entry !== undefined}
              <NavigatorRow
                {entry}
                isCurrent={entry.windowId === currentWindowId}
                isMinimized={isCanvas && entry.display.isMinimized}
                isSettled={row.settled}
                isAwaitingPermission={awaitingPermission.has(entry.windowId)}
                onSelect={onSelectWindow}
              />
            {/if}
          {/each}
        </ul>
      {/each}
      {#if needsYouView.unknown.length > 0}
        <h3 class="navigator__section">{sidebar.statusUnknown}</h3>
        <ul class="navigator__rows">
          {#each needsYouView.unknown as row (row.windowId)}
            {@const entry = entryOf(row.windowId)}
            {#if entry !== undefined}
              <NavigatorRow
                {entry}
                isCurrent={entry.windowId === currentWindowId}
                isMinimized={isCanvas && entry.display.isMinimized}
                isAwaitingPermission={awaitingPermission.has(entry.windowId)}
                onSelect={onSelectWindow}
              />
            {/if}
          {/each}
        </ul>
      {/if}
    {:else}
      {#each groups as group, index (group.roomId)}
        {@const summary = summarizeWindowStatuses(group.entries)}
        {@const headingId = roomElementId('name', index)}
        {@const summaryId = roomElementId('summary', index)}
        {@const hasSummary = group.collapsed && (summary.count > 0 || summary.unknownOnly)}
        <div class="navigator__room">
          <div class="navigator__room-heading">
            <button
              type="button"
              class="navigator__room-toggle"
              aria-expanded={!group.collapsed}
              aria-labelledby={headingId}
              aria-describedby={hasSummary ? summaryId : undefined}
              on:click={() => roomActions.toggleCollapsed(group.roomId)}
            >
              <span id={headingId} hidden
                >{describeRoomTitle(group.name, group.entries.length)}</span
              >
              {#if hasSummary}
                <span id={summaryId} hidden
                  >{summary.count > 0
                    ? describeAttentionBadge(summary.count)
                    : describeStatus('unknown')}</span
                >
              {/if}
              <Icon name={group.collapsed ? 'chevron-right' : 'chevron-down'} size="xs" />
              <Icon name={group.kind === 'room' ? 'messages-square' : 'folder'} size="sm" />
              <span class="navigator__room-name">{group.name}</span>
              <span class="navigator__room-count">{group.entries.length}</span>
              {#if group.collapsed}
                {#if summary.count > 0 && summary.mark !== null}
                  <span class="navigator__room-summary">
                    <StatusBadge status={summary.mark} variant="count" count={summary.count} />
                  </span>
                {:else if summary.unknownOnly}
                  <span class="navigator__room-summary">
                    <StatusBadge status="unknown" variant="icon" />
                  </span>
                {/if}
              {/if}
            </button>
            <IconButton
              class="btn btn-ghost navigator__tool navigator__room-more"
              label={group.kind === 'room' ? sidebar.roomActions : sidebar.folderActions}
              aria-haspopup="menu"
              aria-expanded={menuRoomId === group.roomId}
              on:click={(event) => openRoomMenu(group.roomId, event)}
            >
              <Icon name="ellipsis" size="sm" />
            </IconButton>
          </div>
          {#if !group.collapsed}
            <ul class="navigator__rows">
              {#each group.entries as entry (entry.windowId)}
                <NavigatorRow
                  {entry}
                  isCurrent={entry.windowId === currentWindowId}
                  isMinimized={isCanvas && entry.display.isMinimized}
                  isAwaitingPermission={awaitingPermission.has(entry.windowId)}
                  onSelect={onSelectWindow}
                />
              {/each}
            </ul>
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</aside>

<PopupMenu
  isOpen={isViewMenuOpen}
  anchor={viewAnchor}
  keepOpenWithin={viewButton}
  onClose={closeViewMenu}
>
  <button
    type="button"
    class="popup-menu__item"
    role="menuitem"
    aria-current={isCanvas ? undefined : 'true'}
    on:click={() => chooseFromViewMenu(() => onChooseWorkMode('focus'))}
  >
    <span class="popup-menu__icon">
      {#if !isCanvas}<Icon name="check" size="md" />{/if}
    </span>
    <span class="popup-menu__label">{sidebar.focusMode}</span>
  </button>
  <button
    type="button"
    class="popup-menu__item"
    role="menuitem"
    aria-current={isCanvas ? 'true' : undefined}
    on:click={() => chooseFromViewMenu(() => onChooseWorkMode('canvas'))}
  >
    <span class="popup-menu__icon">
      {#if isCanvas}<Icon name="check" size="md" />{/if}
    </span>
    <span class="popup-menu__label">{sidebar.canvasMode}</span>
  </button>
  <hr class="popup-menu__divider" />
  <button
    type="button"
    class="popup-menu__item"
    role="menuitem"
    disabled={!isCanvas}
    on:click={() => chooseFromViewMenu(onTileWindows)}
  >
    <span class="popup-menu__label">{hints.tileWindows}</span>
    {#if !isCanvas}<span class="popup-menu__badge">{sidebar.canvasOnly}</span>{/if}
  </button>
</PopupMenu>

<PopupMenu
  isOpen={menuGroup !== undefined}
  anchor={menuAnchor}
  keepOpenWithin={menuTrigger}
  onClose={handleRoomMenuClose}
>
  {#if menuGroup !== undefined}
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      disabled={!isCanvas}
      on:click={() => chooseFromRoomMenu(roomActions.tile)}
    >
      <span class="popup-menu__label">{sidebar.tileRoom}</span>
      {#if !isCanvas}<span class="popup-menu__badge">{sidebar.canvasOnly}</span>{/if}
    </button>
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      disabled={!isCanvas || menuGroup.minimized === menuGroup.entries.length}
      on:click={() => chooseFromRoomMenu(roomActions.minimizeAll)}
    >
      <span class="popup-menu__label">{sidebar.minimizeAll}</span>
      {#if !isCanvas}<span class="popup-menu__badge">{sidebar.canvasOnly}</span>{/if}
    </button>
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      disabled={!isCanvas || menuGroup.minimized === 0}
      on:click={() => chooseFromRoomMenu(roomActions.restoreAll)}
    >
      <span class="popup-menu__label">{sidebar.showAll}</span>
      {#if !isCanvas}<span class="popup-menu__badge">{sidebar.canvasOnly}</span>{/if}
    </button>
    {#if menuGroup.kind === 'room'}
      <button
        type="button"
        class="popup-menu__item"
        role="menuitem"
        on:click={() => chooseFromRoomMenu(onOpenChat)}
      >
        <span class="popup-menu__label">{sidebar.openChat}</span>
      </button>
    {/if}
    <hr class="popup-menu__divider" />
    <button
      type="button"
      class="popup-menu__item"
      role="menuitem"
      on:click={() => chooseFromRoomMenu(roomActions.toggleCollapsed)}
    >
      <span class="popup-menu__label"
        >{menuGroup.collapsed ? sidebar.expand : sidebar.collapse}</span
      >
    </button>
  {/if}
</PopupMenu>

<style>
  .navigator {
    display: flex;
    flex: 0 0 auto;
    flex-direction: column;
    min-height: 0;
    background-color: var(--bg-chrome);
  }

  .navigator__header {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1_5);
    height: var(--navigator-header-height);
    padding: 0 var(--space-2) 0 var(--space-3);
  }

  .navigator__title {
    flex: 1 1 auto;
    min-width: 0;
    margin: 0;
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
  }

  .navigator__count {
    margin-left: var(--space-1_5);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-regular);
  }

  .navigator :global(.navigator__tool) {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    gap: var(--space-1);
    min-width: var(--icon-button-hit-min);
    min-height: var(--icon-button-hit-min);
    padding: 0 var(--space-1_5);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .navigator :global(.navigator__tool:hover) {
    color: var(--fg-default);
  }

  .navigator__filter[aria-pressed='true'] {
    background-color: var(--bg-selected);
    color: var(--fg-default);
  }

  .navigator__filter-count {
    font-weight: var(--weight-semibold);
    font-variant-numeric: tabular-nums;
  }

  .navigator__list {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: var(--space-0_5) var(--space-1_5) var(--space-2);
  }

  .navigator__rows {
    margin: 0;
    padding: 0;
  }

  .navigator__room-heading {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    margin-top: var(--space-1_5);
    border-radius: var(--radius-sm);
  }

  .navigator__room-toggle {
    display: flex;
    flex: 1 1 auto;
    align-items: center;
    gap: var(--space-1_5);
    min-width: 0;
    height: var(--row-height);
    padding: 0 var(--space-1_5);
    border: none;
    border-radius: var(--radius-sm);
    background-color: transparent;
    color: var(--fg-muted);
    font-family: inherit;
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    text-align: left;
    cursor: pointer;
  }

  .navigator__room-toggle:hover {
    background-color: var(--bg-hover);
  }

  .navigator__room-toggle:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: -2px;
  }

  .navigator__room-label {
    gap: var(--space-1_5);
    height: var(--row-height);
    margin-bottom: 0;
    padding: 0 var(--space-1_5);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
  }

  .navigator__room-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .navigator__room-count {
    flex: 0 0 auto;
    color: var(--fg-faint);
    font-size: var(--text-2xs);
    font-weight: var(--weight-medium);
  }

  .navigator__room-summary {
    display: inline-flex;
    margin-left: auto;
  }

  /* Revealed on hover and while focus is inside the heading; always there without hover. */
  .navigator__room-heading :global(.navigator__room-more) {
    opacity: 0;
  }

  .navigator__room-heading:hover :global(.navigator__room-more),
  .navigator__room-heading:focus-within :global(.navigator__room-more),
  .navigator__room-heading :global(.navigator__room-more[aria-expanded='true']) {
    opacity: 1;
  }

  @media (hover: none) {
    .navigator__room-heading :global(.navigator__room-more) {
      opacity: 1;
    }
  }

  .navigator__section {
    margin: var(--space-3) 0 var(--space-1);
    padding: 0 var(--space-1_5);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
  }

  .navigator__empty {
    margin: 0;
    padding: var(--space-4) var(--space-2);
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .navigator__nothing {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-6) var(--space-2);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    text-align: center;
  }

  .navigator__nothing p {
    margin: 0;
  }
</style>
