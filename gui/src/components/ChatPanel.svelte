<script lang="ts">
  /**
   * The chat room, as one floating panel outside the window registry: who is
   * in the room, what has been said, how far each envelope got, and the
   * composer with its `@` completion.
   *
   * It owns no IPC: the room state is handed in, and posting goes
   * back out through the callbacks the shell supplies.
   */
  import { tick } from 'svelte';

  import IconButton from './IconButton.svelte';
  import { observeSize } from './observe-size';
  import { closeOnEscape } from './panel-focus';
  import { pointerDrag, type PointerDragOptions } from './pointer-drag';
  import { isImeKey } from '../keymap/key-chord';
  import { resolveDisplayName, type ChatRoomState } from '../chat/chat-store';
  import { splitForMiddleEllipsis } from '../chat/middle-ellipsis';
  import {
    dismissMention,
    filterMentionCandidates,
    insertMention,
    openMention,
    type DismissedMention,
    type MentionCandidate,
    type MentionQuery,
  } from '../chat/mention-input';
  import { clampRoomRailWidth, createRoomRailWidthStorage } from '../chat/room-rail-width';
  import {
    CHAT_PANEL_MIN_SIZE,
    clampChatPanelRect,
    chatPanelMaxSize,
    defaultChatPanelRect,
    readStoredChatPanelRect,
    type ChatPanelRect,
  } from '../chat/chat-panel-rect';
  import type {
    ChatDeliveryState,
    ChatMessageFromKind,
    ChatRoomInfo,
  } from '../ipc/session-boundary';
  import { isResizedByUser, type PanelSize } from '../settings/panel-size';
  import { createGuardedLocalStorage } from '../storage/guarded-storage';
  import {
    chat,
    describeDeliveryState,
    describeHaltReason,
    describeUndeliveredUnknownReason,
  } from '../ui-messages';

  /** One run of the message body: a mention, or the plain text around it. */
  interface BodyRun {
    text: string;
    isMention: boolean;
  }

  export let room: ChatRoomState;
  export let onClose: () => void;
  /** Posts the body as written; resolves true once it is on its way. */
  export let onSend: (body: string) => Promise<boolean>;
  /** Posts the body to every participant; resolves true once it is on its way. */
  export let onBroadcast: (body: string) => Promise<boolean>;
  /** The room list, in wire order, for the left-hand room rail. */
  export let rooms: ChatRoomInfo[];
  /** The room shown on the right; marks its row selected in the rail. */
  export let currentRoomId: string;
  /** Called with a room's id when its row in the rail is clicked. */
  export let onSelectRoom: (roomId: string) => void;

  const PANEL_STORAGE_KEY = 'yagura.chat-panel-rect';
  const MENTION_PATTERN = /@(?:"[^"]*"|[^\s]+)/g;

  const rectStorage = createGuardedLocalStorage(PANEL_STORAGE_KEY);
  const roomRailWidthStorage = createRoomRailWidthStorage();

  let viewport: PanelSize = measureViewport();
  /** The rect the user chose; a smaller window shows it smaller without changing it. */
  let chosenRect: ChatPanelRect =
    readStoredChatPanelRect(rectStorage.read()) ?? defaultChatPanelRect(viewport);
  /** The width the user chose; a narrower panel draws the rail narrower without changing it. */
  let storedRoomRailWidth = roomRailWidthStorage.load();
  let isRoomRailDragging = false;

  let draft = '';
  let isPosting = false;
  let isRosterExpanded = false;
  let inputElement: HTMLTextAreaElement | undefined;
  let mention: MentionQuery | null = null;
  /** The mention last closed with Escape, kept closed until the text or the caret leaves it. */
  let dismissed: DismissedMention | null = null;
  let candidates: MentionCandidate[] = [];
  let activeCandidate = 0;

  $: halt = room.halted;
  $: isMentionOpen = mention !== null && candidates.length > 0;
  $: canPost = draft.trim() !== '' && !isPosting;
  /** The current room's display name, from the room list rather than its raw id. */
  $: currentRoomName =
    rooms.find((entry) => entry.room_id === currentRoomId)?.name ?? currentRoomId;
  $: shownRect = clampChatPanelRect(chosenRect, viewport);
  $: maxPanelSize = chatPanelMaxSize(shownRect, viewport);
  $: roomRailWidth = clampRoomRailWidth(storedRoomRailWidth, shownRect.width);
  // The same floor clampChatPanelRect applies, so the drawn size never outgrows the rect it shows.
  $: minPanelWidth = Math.min(CHAT_PANEL_MIN_SIZE.width, viewport.width);
  $: minPanelHeight = Math.min(CHAT_PANEL_MIN_SIZE.height, viewport.height);

  /** The viewport the panel is kept inside of. */
  function measureViewport(): PanelSize {
    const root = document.documentElement;
    return { width: root.clientWidth, height: root.clientHeight };
  }

  function storeRect(): void {
    rectStorage.write(JSON.stringify(chosenRect));
  }

  // A smaller window only changes how the panel is shown, not the rect the user chose.
  function handleViewportResize(): void {
    viewport = measureViewport();
  }

  function handleCornerResize(read: PanelSize): void {
    if (!isResizedByUser(read, shownRect)) {
      return;
    }
    chosenRect = clampChatPanelRect(
      { ...shownRect, width: read.width, height: read.height },
      viewport
    );
    storeRect();
  }

  // A move changes where the panel sits, not the size the user chose.
  const moveDrag: PointerDragOptions<ChatPanelRect> = {
    start: (event) =>
      event.target instanceof Element && event.target.closest('button') !== null ? null : shownRect,
    move: (startRect, { dx, dy }) => {
      const moved = clampChatPanelRect(
        { ...startRect, x: startRect.x + dx, y: startRect.y + dy },
        viewport
      );
      chosenRect = { ...chosenRect, x: moved.x, y: moved.y };
    },
    end: () => {
      storeRect();
    },
  };

  // Starts from the drawn width, which a narrow panel keeps below the stored one.
  const roomRailDrag: PointerDragOptions<number> = {
    start: () => {
      isRoomRailDragging = true;
      return roomRailWidth;
    },
    move: (startWidth, { dx }) => {
      storedRoomRailWidth = clampRoomRailWidth(startWidth + dx, shownRect.width);
    },
    end: () => {
      isRoomRailDragging = false;
      roomRailWidthStorage.save(storedRoomRailWidth);
    },
  };

  function describeFromKind(kind: ChatMessageFromKind): string {
    switch (kind) {
      case 'user':
        return chat.fromKindUser;
      case 'window':
        return chat.fromKindWindow;
      case 'system':
        return chat.fromKindSystem;
      case 'external':
        return chat.fromKindExternal;
      default: {
        const unreachable: never = kind;
        return unreachable;
      }
    }
  }

  /** The status tone vocabulary the delivery chip is coloured from. */
  function deliveryTone(state: ChatDeliveryState): string {
    switch (state) {
      case 'confirmed':
        return 'active';
      case 'pending':
      case 'unconfirmed':
      case 'not_observed':
        return 'quiet';
      case 'write_failed':
        return 'halted';
      default:
        return 'unknown';
    }
  }

  /** The participant's name for a delivery, falling back to the window id. */
  function deliveryTarget(windowId: string): string {
    return resolveDisplayName(room, windowId);
  }

  function formatPostedAt(atMs: number): string {
    return new Date(atMs).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }

  /** Splits a body so its mentions can be drawn apart from the text. */
  function bodyRuns(body: string): BodyRun[] {
    const runs: BodyRun[] = [];
    let cursor = 0;
    MENTION_PATTERN.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = MENTION_PATTERN.exec(body)) !== null) {
      if (match.index > cursor) {
        runs.push({ text: body.slice(cursor, match.index), isMention: false });
      }
      runs.push({ text: match[0], isMention: true });
      cursor = match.index + match[0].length;
    }
    if (cursor < body.length) {
      runs.push({ text: body.slice(cursor), isMention: false });
    }
    return runs;
  }

  function closeMention(): void {
    mention = null;
    candidates = [];
    activeCandidate = 0;
  }

  /** Re-reads the mention the caret sits in, after every edit or caret move. */
  function syncMention(): void {
    const element = inputElement;
    if (element === undefined) {
      closeMention();
      return;
    }
    const next = openMention(draft, element.selectionStart, dismissed);
    dismissed = next.dismissed;
    const found = next.mention;
    if (found === null) {
      closeMention();
      return;
    }
    mention = found;
    candidates = filterMentionCandidates(room.roster, found.query);
    activeCandidate = 0;
  }

  async function applyCandidate(candidate: MentionCandidate): Promise<void> {
    const current = mention;
    if (current === null) {
      return;
    }
    const next = insertMention(draft, current, candidate.name);
    draft = next.text;
    closeMention();
    await tick();
    inputElement?.focus();
    inputElement?.setSelectionRange(next.caret, next.caret);
  }

  async function post(broadcast: boolean): Promise<void> {
    const body = draft.trim();
    if (body === '' || isPosting) {
      return;
    }
    isPosting = true;
    try {
      const posted = broadcast ? await onBroadcast(body) : await onSend(body);
      if (posted) {
        draft = '';
        closeMention();
      }
    } finally {
      isPosting = false;
    }
  }

  function handleInputKeydown(event: KeyboardEvent): void {
    // The input method owns the Enter that confirms and the Esc that cancels.
    if (isImeKey(event)) {
      return;
    }
    if (isMentionOpen) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        const step = event.key === 'ArrowDown' ? 1 : candidates.length - 1;
        activeCandidate = (activeCandidate + step) % candidates.length;
        event.preventDefault();
        return;
      }
      if (event.key === 'Enter' || event.key === 'Tab') {
        const candidate = candidates[activeCandidate];
        if (candidate !== undefined) {
          event.preventDefault();
          void applyCandidate(candidate);
        }
        return;
      }
      if (event.key === 'Escape') {
        if (mention !== null) {
          dismissed = dismissMention(draft, mention);
        }
        closeMention();
        event.preventDefault();
        return;
      }
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void post(false);
    }
  }
</script>

<svelte:window on:resize={handleViewportResize} />

<section
  class="chat-panel"
  use:closeOnEscape={onClose}
  use:observeSize={{ size: shownRect, onResize: handleCornerResize }}
  style:left="{shownRect.x}px"
  style:top="{shownRect.y}px"
  style:min-width="{minPanelWidth}px"
  style:min-height="{minPanelHeight}px"
  style:max-width="{maxPanelSize.width}px"
  style:max-height="{maxPanelSize.height}px"
  aria-label={chat.panelTitle}
>
  <div class="room-rail" style="width: {roomRailWidth}px;">
    <div class="room-rail__head">{chat.roomsHeading}</div>
    <ul class="room-rail__list">
      {#each rooms.filter((row) => row.name !== '') as row (row.room_id)}
        {@const name = splitForMiddleEllipsis(row.name)}
        <li
          class="room-row"
          title={row.name}
          data-selected={row.room_id === currentRoomId}
          data-empty={row.participant_count === 0}
        >
          <div
            class="room-row__hit"
            role="button"
            tabindex="0"
            on:click={() => onSelectRoom(row.room_id)}
            on:keydown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                onSelectRoom(row.room_id);
              }
            }}
          >
            {#if row.halted}
              <span
                class="room-row__stop"
                aria-label={chat.roomStoppedLabel}
                title={chat.roomStoppedLabel}>⏸</span
              >
            {/if}
            <span class="room-row__name"
              ><span class="room-row__name-head">{name.head}</span><span class="room-row__name-tail"
                >{name.tail}</span
              ></span
            >
            <span class="room-row__meta">{chat.roomParticipantCount(row.participant_count)}</span>
            {#if row.needs_user}
              <span
                class="room-row__dot"
                aria-label={chat.roomNeedsYouLabel}
                title={chat.roomNeedsYouLabel}
              ></span>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  </div>

  <!-- Pointer-only affordance, like the sidebar's divider: keyboard resize
       belongs to the shortcut layer. -->
  <div
    class="room-rail-divider"
    class:room-rail-divider--dragging={isRoomRailDragging}
    aria-hidden="true"
    use:pointerDrag={roomRailDrag}
  ></div>

  <div class="chat-panel__column">
    <div class="chat-panel__bar" role="presentation" use:pointerDrag={moveDrag}>
      <h2 class="chat-panel__title">{chat.panelTitle}</h2>
      <span class="chat-panel__room">{currentRoomName}</span>
      <IconButton class="chat-panel__close" label={chat.closePanel} on:click={onClose}>
        ✕
      </IconButton>
    </div>

    {#if halt !== null}
      <p class="halt-banner" role="status">
        <span class="halt-banner__text">{describeHaltReason(halt.reason)}</span>
      </p>
    {/if}

    {#if room.roster.length > 0}
      <div class="chat-panel__roster-row">
        <button
          type="button"
          class="chat-panel__roster-label"
          aria-expanded={isRosterExpanded}
          on:click={() => (isRosterExpanded = !isRosterExpanded)}
        >
          {chat.rosterLabel}
        </button>
        {#each room.roster as participant (participant.window_id)}
          <span class="roster-chip">{participant.name}</span>
        {/each}
      </div>
    {/if}

    <div class="chat-panel__body">
      {#if isRosterExpanded}
        <h3 class="section-heading">
          <span>{chat.rosterHeading(room.roster.length)}</span>
          <span class="section-heading__rule" aria-hidden="true"></span>
        </h3>
        <ul class="roster-list">
          {#each room.roster as participant (participant.window_id)}
            <li>
              <span class="roster-list__name">{participant.name}</span>
              <span class="roster-list__id">{participant.window_id}</span>
            </li>
          {/each}
        </ul>
      {/if}

      {#if room.messages.length === 0}
        <p class="chat-panel__empty">{chat.emptyMessages}</p>
      {/if}

      {#each room.messages as message (message.seq)}
        <article class="msg" data-from={message.from_kind}>
          <div class="msg__head">
            <span class="msg__name">{message.from_name}</span>
            <span class="msg__kind">{describeFromKind(message.from_kind)}</span>
            {#if message.broadcast}
              <span class="msg__broadcast">{chat.broadcastMarker}</span>
            {/if}
            <span class="msg__time">{formatPostedAt(message.at_ms)}</span>
          </div>
          <p class="msg__body">
            {#each bodyRuns(message.body) as run, index (index)}
              {#if run.isMention}<span class="msg__mention">{run.text}</span>{:else}{run.text}{/if}
            {/each}
          </p>
          {#if message.deliveries.length > 0}
            <div class="msg__deliveries">
              {#each message.deliveries as delivery (delivery.window_id)}
                <span class="delivery" data-tone={deliveryTone(delivery.state)}>
                  <span class="delivery__to">{deliveryTarget(delivery.window_id)}</span>
                  <span
                    >{delivery.state === 'undelivered_unknown'
                      ? describeUndeliveredUnknownReason(delivery)
                      : describeDeliveryState(delivery.state)}</span
                  >
                </span>
              {/each}
            </div>
          {/if}
        </article>
      {/each}
    </div>

    <div class="chat-panel__footer">
      <div class="chat-panel__mention-anchor">
        {#if isMentionOpen}
          <div class="mention-menu" aria-label={chat.rosterLabel}>
            <ul class="mention-menu__list">
              {#each candidates as candidate, index (candidate.windowId ?? candidate.name)}
                <li>
                  <button
                    type="button"
                    class="mention-menu__item"
                    data-selected={index === activeCandidate}
                    on:click={() => void applyCandidate(candidate)}
                  >
                    <span class="mention-menu__name">{candidate.name}</span>
                  </button>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>

      <textarea
        class="chat-input"
        rows="2"
        bind:this={inputElement}
        bind:value={draft}
        placeholder={chat.inputPlaceholder}
        on:input={syncMention}
        on:click={syncMention}
        on:keyup={syncMention}
        on:keydown={handleInputKeydown}></textarea>

      <div class="chat-actions">
        <span class="chat-actions__hint">{halt === null ? chat.sendHint : chat.haltedHint}</span>
        <button
          type="button"
          class="btn btn-secondary btn-small"
          disabled={!canPost}
          on:click={() => void post(true)}
        >
          {chat.broadcastButton}
        </button>
        <button
          type="button"
          class="btn btn-primary btn-small"
          disabled={!canPost}
          on:click={() => void post(false)}
        >
          {chat.sendButton}
        </button>
      </div>
    </div>
  </div>
</section>

<style>
  .chat-panel {
    position: fixed;
    z-index: 20;
    display: flex;
    flex-direction: row;
    overflow: hidden;
    resize: both;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    box-shadow: var(--shadow-md);
  }

  /* The right-hand column: everything in the panel except the room rail,
     which shares the floating shell with it. */
  .chat-panel__column {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .room-rail {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  /* A 1px line in a 5px hit area; it thickens while hovered or dragged. */
  .room-rail-divider {
    flex: 0 0 5px;
    padding-inline: 2px;
    background-color: var(--color-border);
    background-clip: content-box;
    cursor: col-resize;
    touch-action: none;
  }

  .room-rail-divider:hover,
  .room-rail-divider--dragging {
    padding-inline: 1px;
    background-color: var(--color-primary);
  }

  .room-rail__head {
    flex: 0 0 auto;
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--color-text-secondary);
  }

  .room-rail__list {
    flex: 1 1 auto;
    list-style: none;
    margin: 0;
    padding: var(--space-xs);
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
  }

  .room-row {
    border-radius: var(--radius-sm);
  }

  .room-row:hover {
    background-color: var(--color-border);
  }

  .room-row[data-selected='true'] {
    background-color: var(--color-primary);
    color: var(--color-background);
  }

  .room-row[data-empty='true'] {
    opacity: 0.55;
  }

  /* The row's click/keyboard target: an `<li>` cannot itself carry an
     interactive role (a11y_no_noninteractive_element_to_interactive_role),
     so the row's layout and the hit area both live here instead. */
  .room-row__hit {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    cursor: pointer;
  }

  .room-row__name {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    overflow: hidden;
    white-space: nowrap;
    font-weight: 600;
    font-size: var(--font-size-sm);
  }

  /* Shrinks first, but keeps its first grapheme and the ellipsis. */
  .room-row__name-head {
    flex: 0 100 auto;
    min-width: calc(1em + 1ch);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .room-row__name-tail {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }

  .room-row__meta {
    flex: 0 0 auto;
    color: var(--color-text-secondary);
    font-size: 11px;
  }

  .room-row[data-selected='true'] .room-row__meta {
    color: inherit;
    opacity: 0.85;
  }

  .room-row__stop {
    flex: 0 0 auto;
    font-size: 11px;
    line-height: 1;
    color: var(--color-error);
  }

  .room-row[data-selected='true'] .room-row__stop {
    color: var(--color-background);
  }

  .room-row__dot {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 999px;
    background-color: var(--color-warning);
  }

  .chat-panel__bar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    background-color: var(--color-surface);
    cursor: move;
    touch-action: none;
    user-select: none;
  }

  .chat-panel__title {
    flex: 1 1 auto;
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--font-size-sm);
    font-weight: 600;
  }

  .chat-panel__room {
    flex: 0 0 auto;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }

  :global(.chat-panel__close) {
    flex: 0 0 auto;
    padding: 0;
    border: none;
    background: none;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
    cursor: pointer;
  }

  .chat-panel__roster-row {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }

  .chat-panel__roster-label {
    flex: 0 0 auto;
    padding: 0;
    border: none;
    background: none;
    color: inherit;
    font-family: inherit;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    cursor: pointer;
  }

  .chat-panel__body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-sm);
    background-color: var(--color-background);
  }

  .chat-panel__empty {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .chat-panel__footer {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm);
    border-top: 1px solid var(--color-border);
  }

  .chat-input {
    width: 100%;
    min-height: 3.5rem;
    resize: none;
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    color: var(--color-text-primary);
    font-family: inherit;
    font-size: var(--font-size-sm);
  }

  .chat-input:focus {
    outline: none;
    border-color: var(--color-primary);
  }

  .chat-actions {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .chat-actions__hint {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-text-secondary);
    font-size: 11px;
  }

  .msg {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
  }

  .msg[data-from='user'] {
    border-left: 3px solid var(--color-primary);
    padding-left: calc(var(--space-sm) - 3px);
  }

  .msg[data-from='system'] {
    border-left: 3px solid var(--color-warning);
    padding-left: calc(var(--space-sm) - 3px);
    background-color: var(--color-background);
  }

  .msg__head {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    font-size: var(--font-size-xs);
  }

  .msg__name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .msg__kind {
    flex: 0 0 auto;
    padding: 0 var(--space-xs);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    color: var(--color-text-secondary);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .msg__broadcast {
    flex: 0 0 auto;
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-primary);
    color: var(--color-background);
    font-size: 10px;
    font-weight: 600;
  }

  .msg__time {
    flex: 1 1 auto;
    text-align: right;
    color: var(--color-text-secondary);
    font-size: 11px;
  }

  .msg__body {
    margin: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
    font-size: var(--font-size-sm);
  }

  .msg__mention {
    color: var(--color-primary);
    font-weight: 600;
  }

  .msg__deliveries {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
  }

  .delivery {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    max-width: 100%;
    padding: 0 var(--space-xs);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    font-size: 11px;
  }

  .delivery__to {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-family-mono);
  }

  .delivery[data-tone='active'] {
    background-color: var(--color-success);
    color: var(--color-background);
  }

  .delivery[data-tone='quiet'] {
    background-color: var(--color-text-secondary);
    color: var(--color-background);
  }

  .delivery[data-tone='halted'] {
    background-color: var(--color-error);
    color: var(--color-background);
  }

  .delivery[data-tone='unknown'] {
    border-color: var(--color-text-secondary);
    border-style: dashed;
    background-color: transparent;
    color: var(--color-text-secondary);
  }

  .halt-banner {
    flex: 0 0 auto;
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-xs);
  }

  .halt-banner__text {
    flex: 1 1 auto;
    min-width: 0;
  }

  .roster-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-border);
    color: var(--color-text-primary);
    font-size: 11px;
  }

  .roster-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .roster-list li {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
  }

  .roster-list__name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--font-size-sm);
    font-weight: 600;
  }

  .roster-list__id {
    flex: 0 0 auto;
    color: var(--color-text-secondary);
    font-family: var(--font-family-mono);
    font-size: 11px;
  }

  .section-heading {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin: 0;
    color: var(--color-text-secondary);
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .section-heading__rule {
    flex: 1 1 auto;
    height: 1px;
    background-color: var(--color-border);
  }

  /* The footer stays unpositioned: WebKit hit-tests positioned descendants above the native resize corner. */
  .chat-panel__mention-anchor {
    position: relative;
    height: 0;
    margin-bottom: calc(-1 * var(--space-xs));
  }

  .mention-menu {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 2;
    max-height: 11rem;
    overflow-y: auto;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    box-shadow: var(--shadow-modal);
  }

  .mention-menu__list {
    list-style: none;
    margin: 0;
    padding: var(--space-xs);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .mention-menu__item {
    width: 100%;
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: inherit;
    font-family: inherit;
    font-size: var(--font-size-sm);
    text-align: left;
    cursor: pointer;
  }

  .mention-menu__item[data-selected='true'] {
    background-color: var(--color-primary);
    color: var(--color-background);
  }

  .mention-menu__name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
</style>
