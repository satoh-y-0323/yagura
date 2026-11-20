<script context="module" lang="ts">
  let instanceCount = 0;
</script>

<script lang="ts">
  /**
   * The Notifications list: a non-modal dialog opened next to the bell, drawn
   * in the overlay host. Display only: it draws the notices it is handed (the
   * feed's `center`) and calls back. Blocking notices stay on top without a
   * close button. It closes on a press outside, Escape, Tab out of it and a
   * window resize; `onClose` hands focus back where the shell wants it.
   */
  import { afterUpdate, tick } from 'svelte';

  import type { Notice } from '../notices/notice-feed';
  import type { NoticeAction } from '../notices/notice-model';
  import { banners, buttons, hints } from '../ui-messages';
  import { dismissOnOutside, type OutsideDismissReason } from './dismiss-on-outside';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import { placeMenu, type Rect } from './menu-position';
  import { noticeToneMark } from './notice-tone';
  import { overlayPortal } from './overlay-host';
  import { isPanelCloseKey } from './panel-focus';

  /** Why the list asks to be closed. */
  type CloseReason = 'escape' | 'tab' | 'close-button' | OutsideDismissReason;

  const FOCUSABLE = 'button:not(:disabled), [href], [tabindex]:not([tabindex="-1"])';
  // Space between the bell and the list.
  const ANCHOR_GAP = 8;

  export let isOpen = false;
  /** The bell's viewport-relative box; the list opens to its right. */
  export let anchor: Rect | null;
  /** The bell: a press on it is not outside, and Tab leaves from it. */
  export let keepOpenWithin: HTMLElement | undefined = undefined;
  export let notices: readonly Notice[];
  /** Notices numbered above this came after the list was last opened and are marked new; the default marks none. */
  export let newerThan: number = Number.POSITIVE_INFINITY;
  /** The row to start on when an opening comes from a notice: its close button, else the list's. */
  export let initialSeq: number | null = null;
  export let onAction: (action: NoticeAction) => void;
  export let onDismiss: (notice: Notice) => void;
  /** Clears the events; state notices stay. */
  export let onClear: () => void;
  export let onOpenSettings: () => void;
  export let onClose: (reason: CloseReason) => void;

  const titleId = `notice-center-title-${instanceCount++}`;

  let originMarker: HTMLSpanElement | undefined;
  let dialog: HTMLDialogElement | undefined;
  let closeButton: HTMLButtonElement | undefined;
  let position = { left: 0, top: 0 };
  let isPlaced = false;
  let hasFocused = false;

  $: pinned = notices.filter((notice) => notice.blocking);
  $: others = notices.filter((notice) => !notice.blocking);
  $: hasEvents = notices.some((notice) => notice.source === 'event');
  $: if (!isOpen) {
    isPlaced = false;
    hasFocused = false;
  }

  afterUpdate(() => {
    if (!isOpen || dialog === undefined) {
      return;
    }
    if (!dialog.open) {
      dialog.show();
    }
    if (anchor !== null) {
      const rect = dialog.getBoundingClientRect();
      // Anchored at the bell's bottom right, so the list grows upwards from the bell's foot.
      const next = placeMenu(
        {
          left: anchor.left + anchor.width + ANCHOR_GAP,
          top: anchor.top + anchor.height,
          width: 0,
          height: 0,
        },
        { width: rect.width, height: rect.height },
        { width: window.innerWidth, height: window.innerHeight }
      );
      if (!isPlaced || next.left !== position.left || next.top !== position.top) {
        position = next;
        isPlaced = true;
      }
    }
    // Once per opening, which is always the user's doing; later updates never move focus.
    if (!hasFocused && (isPlaced || anchor === null)) {
      hasFocused = true;
      // After `tick`, because a list that is still hidden cannot take focus.
      void tick().then(focusInitial);
    }
  });

  function focusables(): HTMLElement[] {
    return dialog === undefined ? [] : Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE));
  }

  /** The asked-for row's close button, else the first control after the close button, else the close button. */
  function focusInitial(): void {
    if (!isOpen || closeButton === undefined) {
      return;
    }
    if (initialSeq !== null) {
      const row = dialog?.querySelector<HTMLElement>(`[data-notice-seq="${initialSeq}"]`) ?? null;
      row?.scrollIntoView({ block: 'nearest' });
      (row?.querySelector<HTMLElement>('[data-notice-dismiss]') ?? closeButton).focus();
      return;
    }
    const elements = focusables();
    const next = elements[elements.indexOf(closeButton) + 1] ?? closeButton;
    next.focus();
  }

  function handleKeyDown(event: KeyboardEvent): void {
    if (isPanelCloseKey(event)) {
      event.preventDefault();
      onClose('escape');
      return;
    }
    if (event.key !== 'Tab' || event.ctrlKey || event.altKey || event.metaKey) {
      return;
    }
    const elements = focusables();
    const edge = event.shiftKey ? elements[0] : elements[elements.length - 1];
    if (document.activeElement !== edge) {
      return;
    }
    // The list lives at the end of the document, so the browser's own Tab would
    // leave from there; leaving from the bell keeps the order.
    if (event.shiftKey) {
      event.preventDefault();
    }
    keepOpenWithin?.focus();
    onClose('tab');
  }

  // Focus goes to another close button, never to a row's action, where the next key typed would act.
  async function dismiss(notice: Notice, index: number): Promise<void> {
    const neighbour =
      others.slice(index + 1).find((other) => other.dismissible) ??
      others
        .slice(0, index)
        .reverse()
        .find((other) => other.dismissible);
    onDismiss(notice);
    await tick();
    const row =
      neighbour === undefined
        ? null
        : (dialog?.querySelector<HTMLElement>(`[data-notice-seq="${neighbour.seq}"]`) ?? null);
    (row?.querySelector<HTMLElement>('[data-notice-dismiss]') ?? closeButton)?.focus();
  }

  async function clear(): Promise<void> {
    onClear();
    await tick();
    closeButton?.focus();
  }
</script>

<span class="notice-center__origin" hidden bind:this={originMarker}></span>

{#if isOpen}
  <!-- Not a menu: the rows hold buttons and Tab moves through them. -->
  <dialog
    class="notice-center"
    bind:this={dialog}
    aria-labelledby={titleId}
    style="left: {position.left}px; top: {position.top}px; visibility: {isPlaced || anchor === null
      ? 'visible'
      : 'hidden'};"
    use:overlayPortal={originMarker ?? null}
    use:dismissOnOutside={{ keepOpenWithin, onDismiss: onClose }}
    on:keydown={handleKeyDown}
  >
    <div class="notice-center__head">
      <h2 class="notice-center__title" id={titleId}>{banners.notifications}</h2>
      <button type="button" class="btn btn-ghost notice-center__settings" on:click={onOpenSettings}>
        {banners.notificationSettings}
      </button>
      <IconButton
        class="notice-center__close"
        label={buttons.close}
        bind:element={closeButton}
        on:click={() => onClose('close-button')}
      >
        <Icon name="x" size="sm" />
      </IconButton>
    </div>
    {#if notices.length === 0}
      <p class="notice-center__empty">{banners.noNotifications}</p>
    {:else}
      <ul class="notice-center__list">
        {#each pinned as notice (notice.seq)}
          {@const tone = noticeToneMark(notice.tone)}
          <li class="notice-center__row notice-center__row--pinned" data-tone={notice.tone}>
            <span class="notice-center__icon"><Icon name={tone.icon} label={tone.label} /></span>
            <div class="notice-center__body">
              <p class="notice-center__row-title">{notice.title}</p>
              {#if notice.detail !== null}
                <p class="notice-center__detail">{notice.detail}</p>
              {/if}
              {#if notice.action !== null}
                {@const action = notice.action}
                <div class="notice-center__actions">
                  <button type="button" class="btn btn-secondary" on:click={() => onAction(action)}>
                    {buttons[action.labelKey]}
                  </button>
                </div>
              {/if}
            </div>
          </li>
        {/each}
        {#each others as notice, index (notice.seq)}
          {@const tone = noticeToneMark(notice.tone)}
          <li class="notice-center__row" data-tone={notice.tone} data-notice-seq={notice.seq}>
            <span class="notice-center__icon"><Icon name={tone.icon} label={tone.label} /></span>
            <div class="notice-center__body">
              <p class="notice-center__row-title">
                {notice.title}
                {#if notice.seq > newerThan}
                  <span class="notice-center__new">{banners.noticeNew}</span>
                {/if}
              </p>
              {#if notice.detail !== null}
                <p class="notice-center__detail">{notice.detail}</p>
              {/if}
              {#if notice.at !== null}
                <time class="notice-center__at" datetime={notice.at}>{notice.at}</time>
              {/if}
              {#if notice.action !== null}
                {@const action = notice.action}
                <div class="notice-center__actions">
                  <button type="button" class="btn btn-secondary" on:click={() => onAction(action)}>
                    {buttons[action.labelKey]}
                  </button>
                </div>
              {/if}
            </div>
            {#if notice.dismissible}
              <IconButton
                class="notice-center__dismiss"
                label={hints.dismissNotice}
                data-notice-dismiss
                on:click={() => void dismiss(notice, index)}
              >
                <Icon name="x" size="sm" />
              </IconButton>
            {/if}
          </li>
        {/each}
      </ul>
      {#if hasEvents}
        <div class="notice-center__foot">
          <button type="button" class="btn btn-ghost" on:click={() => void clear()}>
            {banners.clearNotifications}
          </button>
        </div>
      {/if}
    {/if}
  </dialog>
{/if}

<style>
  .notice-center {
    position: fixed;
    inset: auto;
    width: min(360px, calc(100vw - var(--rail-width) - 16px));
    max-height: 70vh;
    margin: 0;
    padding: 0;
    overflow: hidden;
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-md);
    background-color: var(--bg-raised);
    box-shadow: var(--shadow-popover);
    color: var(--fg-default);
    font-size: var(--text-sm);
    z-index: var(--z-popover);
  }

  /* Scoped to the open state, so the closed dialog keeps its hidden display. */
  .notice-center[open] {
    display: flex;
    flex-direction: column;
  }

  .notice-center__head {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-1) var(--space-1) var(--space-1) var(--space-3);
    border-bottom: var(--line-width) solid var(--line);
  }

  .notice-center__title {
    flex: 1 1 auto;
    margin: 0;
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
  }

  .notice-center__settings {
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .notice-center :global(.notice-center__close),
  .notice-center :global(.notice-center__dismiss) {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: var(--icon-button-hit-min);
    height: var(--icon-button-hit-min);
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .notice-center :global(.notice-center__dismiss) {
    align-self: flex-start;
  }

  .notice-center :global(.notice-center__close:hover),
  .notice-center :global(.notice-center__dismiss:hover) {
    background-color: var(--bg-hover);
  }

  .notice-center :global(.notice-center__close:focus-visible),
  .notice-center :global(.notice-center__dismiss:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  .notice-center__list {
    flex: 1 1 auto;
    min-height: 0;
    margin: 0;
    padding: var(--space-1) 0;
    overflow: auto;
    list-style: none;
  }

  .notice-center__row {
    --notice-tone: var(--fg-muted);
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
    border-left: 3px solid var(--notice-tone);
  }

  .notice-center__row + .notice-center__row {
    border-top: var(--line-width) solid var(--line);
  }

  .notice-center__row--pinned {
    background-color: var(--bg-hover);
  }

  .notice-center__row[data-tone='warning'] {
    --notice-tone: var(--st-approval);
  }

  .notice-center__row[data-tone='error'] {
    --notice-tone: var(--danger);
  }

  .notice-center__icon {
    display: inline-flex;
    flex: 0 0 auto;
    margin-top: var(--space-0_5);
    color: var(--notice-tone);
  }

  .notice-center__body {
    flex: 1 1 auto;
    min-width: 0;
  }

  .notice-center__row-title {
    margin: 0;
    font-weight: var(--weight-semibold);
    line-height: var(--leading-sm);
    overflow-wrap: anywhere;
  }

  .notice-center__new {
    display: inline-block;
    margin-left: var(--space-1_5);
    padding: 0 var(--space-1);
    border: var(--line-width) solid var(--line-strong);
    border-radius: var(--radius-xs);
    color: var(--fg-default);
    font-size: var(--text-2xs);
    font-weight: var(--weight-semibold);
    vertical-align: middle;
  }

  .notice-center__detail {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-xs);
    overflow-wrap: anywhere;
  }

  .notice-center__at {
    display: block;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .notice-center__actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1_5);
    margin-top: var(--space-1_5);
  }

  .notice-center__empty {
    margin: 0;
    padding: var(--space-6) var(--space-3);
    color: var(--fg-muted);
    text-align: center;
  }

  .notice-center__foot {
    display: flex;
    flex: 0 0 auto;
    justify-content: flex-end;
    padding: var(--space-1) var(--space-2);
    border-top: var(--line-width) solid var(--line);
  }

  /* The shadow is dropped in forced colours, so the borders carry the shape and the tone. */
  @media (forced-colors: active) {
    .notice-center,
    .notice-center__row {
      border-color: CanvasText;
    }
  }
</style>
