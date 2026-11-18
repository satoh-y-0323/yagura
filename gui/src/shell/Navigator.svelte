<script lang="ts">
  /**
   * The sidebar: the window list with its header, its room headings and the
   * Needs you filter. It draws what the shell derived and calls back; the
   * filter's rows, the rooms and the work mode are decided elsewhere.
   */
  import { tick } from 'svelte';

  import Icon from '../components/Icon.svelte';
  import IconButton from '../components/IconButton.svelte';
  import NavigatorRow from '../components/NavigatorRow.svelte';
  import PopupMenu from '../components/PopupMenu.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import WindowMenu from '../components/WindowMenu.svelte';
  import type { Rect } from '../components/menu-position';
  import type { WorkMode } from '../session/current-window';
  import type { RoomActions } from '../session/room-actions';
  import {
    needsYouSections,
    visibleWindowOrder,
    type NeedsYouView,
  } from '../sidebar/needs-you-view';
  import { groupByRoom, type RoomGroup } from '../sidebar/room-groups';
  import {
    moveWithinRoom,
    resolveDropIndex,
    rowKeyTarget,
    type RowBounds,
    type RowKey,
  } from '../sidebar/row-order';
  import { attentionCount, summarizeWindowStatuses } from '../status-tone';
  import type { WindowEntry } from '../stores/window-registry';
  import type { ThemeStore } from '../theme/theme-store';
  import {
    describeAttentionBadge,
    describeRoomTitle,
    describeStatus,
    describeThemeName,
    hints,
    sidebar,
    themes,
  } from '../ui-messages';

  export let entries: readonly WindowEntry[];
  /** The rooms as the sidebar renders them, top to bottom. */
  export let groups: readonly RoomGroup<WindowEntry>[];
  /** The window order the rooms are sorted by; a reorder hands back a position in it. */
  export let order: readonly string[];
  export let currentWindowId: string | null;
  export let workMode: WorkMode;
  /** The windows a permission request waits on. */
  export let awaitingPermission: ReadonlySet<string>;
  /** What the Needs you filter lists; `null` while the filter is off. */
  export let needsYouView: NeedsYouView | null;
  export let roomActions: RoomActions;
  /** The application theme store; the row menu reads and overrides it. */
  export let themeStore: ThemeStore;
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
  export let onRenameWindow: (entry: WindowEntry) => void;
  /** Moves a window to `targetIndex` of {@link order}. */
  export let onMoveWindow: (windowId: string, targetIndex: number) => void;

  type MenuReason = 'escape' | 'tab' | 'outside' | 'resize';

  /** A row menu: the row it belongs to, the button it returns to, and a pointer's point. */
  interface RowMenu {
    windowId: string;
    anchor: HTMLButtonElement;
    at: Rect | null;
  }

  /** A press on a row, which becomes a drag once it leaves the threshold. */
  interface RowDrag {
    pointerId: number;
    windowId: string;
    originY: number;
    /** The held row's room, top to bottom, held row included. */
    roomIds: string[];
    slot: number | null;
  }

  const ROW_KEYS: readonly string[] = ['ArrowUp', 'ArrowDown', 'Home', 'End'] satisfies RowKey[];
  const NO_COLLAPSED_ROOMS: ReadonlySet<string> = new Set();

  let listElement: HTMLElement | undefined;
  let rowMenu: RowMenu | null = null;
  let drag: RowDrag | null = null;
  /** The row a drag just ended on, whose trailing click must not select it. */
  let draggedRowId: string | null = null;

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
  // The filter keeps its rows where it put them, so it offers no reordering.
  $: isReorderable = needsYouView === null;
  // The rows the list draws: a collapsed room's rows only when the filter lists them.
  $: walkOrder = (
    needsYouView === null
      ? visibleWindowOrder(
          groups.filter((group) => !group.collapsed),
          null
        )
      : visibleWindowOrder(groups, needsYouView)
  ).map((entry) => entry.windowId);
  $: menuEntry =
    rowMenu === null ? undefined : entries.find((entry) => entry.windowId === rowMenu?.windowId);
  $: if (rowMenu !== null && (menuEntry === undefined || !walkOrder.includes(rowMenu.windowId))) {
    closeRowMenu();
  }
  $: menuThemeValue =
    menuEntry === undefined ? '' : ($themeStore.windowOverrides[menuEntry.windowId] ?? '');
  $: menuThemeOptions = [
    { value: '', label: themes.windowFollowsAppOption },
    ...['dark', 'light', ...Object.keys($themeStore.userPalettes)].map((name) => ({
      value: name,
      label: describeThemeName(name),
    })),
  ];
  $: menuMoves =
    menuEntry !== undefined && isReorderable ? rowMoves(menuEntry.windowId, order, entries) : null;
  $: dropMark = drag === null ? null : dropMarkOf(drag);

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

  function rowButton(windowId: string): HTMLButtonElement | null {
    return (
      listElement?.querySelector<HTMLButtonElement>(
        `button[data-window-id="${CSS.escape(windowId)}"]`
      ) ?? null
    );
  }

  function rowButtonOf(target: EventTarget | null): HTMLButtonElement | null {
    const row =
      target instanceof Element
        ? target.closest<HTMLButtonElement>('button[data-window-id]')
        : null;
    return row !== null && listElement?.contains(row) === true ? row : null;
  }

  function isRowKey(key: string): key is RowKey {
    return ROW_KEYS.includes(key);
  }

  /** `from` after `steps` moves of `windowId` inside its room (negative is up). */
  function orderAfterMoves(
    from: readonly string[],
    members: readonly WindowEntry[],
    windowId: string,
    steps: number
  ): string[] {
    const direction = steps < 0 ? 'up' : 'down';
    let next = [...from];
    for (let step = 0; step < Math.abs(steps); step += 1) {
      next = moveWithinRoom(
        next,
        groupByRoom(members, NO_COLLAPSED_ROOMS, next),
        windowId,
        direction
      );
    }
    return next;
  }

  /**
   * The moves change only the window's own room, so taking the window alone to
   * its new position reproduces the room's new order.
   */
  function moveRow(windowId: string, steps: number): void {
    const targetIndex = orderAfterMoves(order, entries, windowId, steps).indexOf(windowId);
    if (targetIndex !== order.indexOf(windowId)) {
      onMoveWindow(windowId, targetIndex);
    }
  }

  /** Whether each Move item would change the order. */
  function rowMoves(
    windowId: string,
    from: readonly string[],
    members: readonly WindowEntry[]
  ): { up: boolean; down: boolean } {
    const current = from.indexOf(windowId);
    const lands = (steps: number): number =>
      orderAfterMoves(from, members, windowId, steps).indexOf(windowId);
    return { up: lands(-1) !== current, down: lands(1) !== current };
  }

  function moveFromRowMenu(direction: 'up' | 'down'): void {
    chooseFromRowMenu((entry) => moveRow(entry.windowId, direction === 'up' ? -1 : 1));
  }

  function openRowMenu(windowId: string, anchor: HTMLButtonElement, at: Rect | null): void {
    // Opening again from the same row, as the contextmenu event after a menu key does, keeps the menu.
    if (rowMenu !== null && rowMenu.anchor === anchor) {
      return;
    }
    rowMenu = { windowId, anchor, at };
  }

  function toggleRowMenu(entry: WindowEntry, anchor: HTMLButtonElement): void {
    if (rowMenu !== null && rowMenu.anchor === anchor) {
      closeRowMenu();
      return;
    }
    openRowMenu(entry.windowId, anchor, null);
  }

  function closeRowMenu(): void {
    rowMenu = null;
  }

  /** Closes the row menu, gives focus back to where it opened from, then acts. */
  function chooseFromRowMenu(action: (entry: WindowEntry) => void): void {
    const menu = rowMenu;
    const entry = menuEntry;
    closeRowMenu();
    if (menu === null || entry === undefined) {
      return;
    }
    menu.anchor.focus();
    action(entry);
    // A reordered list may move the focused row's node, which drops its focus.
    void tick().then(() => {
      if (menu.anchor.isConnected && document.activeElement === document.body) {
        menu.anchor.focus();
      }
    });
  }

  function handleThemeChange(event: Event & { currentTarget: HTMLSelectElement }): void {
    const value = event.currentTarget.value;
    if (rowMenu !== null) {
      themeStore.setWindowOverride(rowMenu.windowId, value === '' ? null : value);
    }
  }

  function selectRow(entry: WindowEntry): void {
    if (draggedRowId === entry.windowId) {
      draggedRowId = null;
      return;
    }
    onSelectWindow(entry);
  }

  function handleRowKeyDown(event: KeyboardEvent): void {
    // A key press is not the click that ends a drag.
    draggedRowId = null;
    const row = rowButtonOf(event.target);
    const windowId = row?.dataset.windowId;
    if (row === null || windowId === undefined || event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    if (event.key === 'ContextMenu' || (event.key === 'F10' && event.shiftKey)) {
      event.preventDefault();
      openRowMenu(windowId, row, null);
      return;
    }
    if (!event.shiftKey && isRowKey(event.key)) {
      event.preventDefault();
      rowButton(rowKeyTarget(walkOrder, windowId, event.key))?.focus();
    }
  }

  function handleRowContextMenu(event: MouseEvent): void {
    const row = rowButtonOf(event.target);
    const windowId = row?.dataset.windowId;
    if (row === null || windowId === undefined) {
      return;
    }
    event.preventDefault();
    openRowMenu(windowId, row, { left: event.clientX, top: event.clientY, width: 0, height: 0 });
  }

  /** The held row's room as drawn: the rows of the list its row sits in. */
  function roomRowsOf(row: HTMLButtonElement): HTMLButtonElement[] {
    const list = row.closest('ul');
    return list === null
      ? [row]
      : Array.from(list.querySelectorAll<HTMLButtonElement>('button[data-window-id]'));
  }

  function boundsOf(rows: readonly HTMLButtonElement[]): RowBounds[] {
    return rows.flatMap((row) => {
      const windowId = row.dataset.windowId;
      if (windowId === undefined) {
        return [];
      }
      const rect = row.getBoundingClientRect();
      return [{ windowId, top: rect.top, height: rect.height }];
    });
  }

  function handleRowPointerDown(event: PointerEvent): void {
    draggedRowId = null;
    const row = rowButtonOf(event.target);
    const windowId = row?.dataset.windowId;
    if (row === null || windowId === undefined || event.button !== 0 || !isReorderable) {
      return;
    }
    row.setPointerCapture(event.pointerId);
    const roomIds = roomRowsOf(row).flatMap((candidate) => candidate.dataset.windowId ?? []);
    drag = { pointerId: event.pointerId, windowId, originY: event.clientY, roomIds, slot: null };
  }

  function dropSlotOf(started: RowDrag, pointerY: number): number | null {
    const row = rowButton(started.windowId);
    return row === null
      ? null
      : resolveDropIndex({
          rows: boundsOf(roomRowsOf(row)),
          windowId: started.windowId,
          originY: started.originY,
          pointerY,
        });
  }

  function handleRowPointerMove(event: PointerEvent): void {
    if (drag !== null && drag.pointerId === event.pointerId) {
      drag = { ...drag, slot: dropSlotOf(drag, event.clientY) };
    }
  }

  function handleRowPointerUp(event: PointerEvent): void {
    const started = drag;
    if (started === null || started.pointerId !== event.pointerId) {
      return;
    }
    drag = null;
    const slot = dropSlotOf(started, event.clientY);
    if (slot !== null) {
      draggedRowId = started.windowId;
      moveRow(started.windowId, slot - started.roomIds.indexOf(started.windowId));
    }
  }

  function handleRowPointerCancel(): void {
    drag = null;
  }

  /** Where the held row would land: before the row now in its slot, or after the room's last row. */
  function dropMarkOf(started: RowDrag): { windowId: string; edge: 'before' | 'after' } | null {
    if (started.slot === null) {
      return null;
    }
    const others = started.roomIds.filter((windowId) => windowId !== started.windowId);
    const before = others[started.slot];
    if (before !== undefined) {
      return { windowId: before, edge: 'before' };
    }
    const last = others[others.length - 1];
    return last === undefined ? null : { windowId: last, edge: 'after' };
  }

  // Listened to on the list rather than on each row, so the rows stay display only.
  function rowGestures(node: HTMLElement): { destroy: () => void } {
    const listeners: [string, EventListener][] = [
      ['keydown', handleRowKeyDown as EventListener],
      ['contextmenu', handleRowContextMenu as EventListener],
      ['pointerdown', handleRowPointerDown as EventListener],
      ['pointermove', handleRowPointerMove as EventListener],
      ['pointerup', handleRowPointerUp as EventListener],
      ['pointercancel', handleRowPointerCancel],
    ];
    for (const [type, listener] of listeners) {
      node.addEventListener(type, listener);
    }
    return {
      destroy: () => {
        for (const [type, listener] of listeners) {
          node.removeEventListener(type, listener);
        }
      },
    };
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

  <div class="navigator__list" bind:this={listElement} use:rowGestures>
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
                onSelect={selectRow}
                onOpenMenu={toggleRowMenu}
                isMenuOpen={rowMenu?.windowId === entry.windowId}
                isDragging={dropMark !== null && drag?.windowId === entry.windowId}
                dropEdge={dropMark?.windowId === entry.windowId ? dropMark.edge : null}
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
                onSelect={selectRow}
                onOpenMenu={toggleRowMenu}
                isMenuOpen={rowMenu?.windowId === entry.windowId}
                isDragging={dropMark !== null && drag?.windowId === entry.windowId}
                dropEdge={dropMark?.windowId === entry.windowId ? dropMark.edge : null}
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
                  onSelect={selectRow}
                  onOpenMenu={toggleRowMenu}
                  isMenuOpen={rowMenu?.windowId === entry.windowId}
                  isDragging={dropMark !== null && drag?.windowId === entry.windowId}
                  dropEdge={dropMark?.windowId === entry.windowId ? dropMark.edge : null}
                />
              {/each}
            </ul>
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</aside>

{#if menuEntry !== undefined}
  <WindowMenu
    isOpen={rowMenu !== null}
    anchor={rowMenu?.anchor}
    at={rowMenu?.at ?? null}
    onClose={closeRowMenu}
    themeValue={menuThemeValue}
    themeOptions={menuThemeOptions}
    onThemeChange={handleThemeChange}
    onRename={() => chooseFromRowMenu(onRenameWindow)}
    onMove={menuMoves === null ? undefined : moveFromRowMenu}
    canMoveUp={menuMoves?.up ?? false}
    canMoveDown={menuMoves?.down ?? false}
  />
{/if}

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
