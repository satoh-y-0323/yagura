<script lang="ts">
  /**
   * One pop-up. Display only: it draws the item it is handed and calls back.
   * A timed item goes after eight seconds; the clock
   * stands still while the pointer is on it, while focus is inside it, and
   * while `timersRunning` is false. A new `restart` starts the time over.
   * Nothing here moves focus.
   */
  import { onDestroy } from 'svelte';

  import type { NoticeAction } from '../notices/notice-model';
  import type { LaneItem } from '../notices/notice-feed';
  import { banners, buttons, hints } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { noticeToneMark } from './notice-tone';

  export let item: LaneItem;
  /** The line beneath the title: a notice's detail or where a calling window is; the shell picks it. */
  export let secondLine: string | null = null;
  /** False while the app is hidden or not focused; the shell decides. */
  export let timersRunning: boolean;
  export let onAction: (action: NoticeAction) => void;
  export let onGoToWindow: (windowId: string) => void;
  /** Called with the close button's `click`, which tells a key from a pointer. */
  export let onDismiss: (event: MouseEvent) => void;
  export let onExpire: () => void;

  const LIFETIME_MS = 8000;

  let hovered = false;
  let focusedWithin = false;
  let remaining = LIFETIME_MS;
  let startedAt = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let mark = item.restart;

  $: noticeAction = item.kind === 'notice' ? item.action : null;
  $: running = item.timed && timersRunning && !hovered && !focusedWithin;
  $: schedule(running, item.restart);

  function schedule(run: boolean, restart: number): void {
    if (restart !== mark) {
      mark = restart;
      cancel();
      remaining = LIFETIME_MS;
    }
    if (run) {
      resume();
    } else {
      pause();
    }
  }

  function resume(): void {
    if (timer === undefined) {
      startedAt = performance.now();
      timer = setTimeout(expire, Math.max(0, remaining));
    }
  }

  function pause(): void {
    if (timer !== undefined) {
      cancel();
      remaining -= performance.now() - startedAt;
    }
  }

  function cancel(): void {
    clearTimeout(timer);
    timer = undefined;
  }

  function expire(): void {
    timer = undefined;
    onExpire();
  }

  function leaveFocus(event: FocusEvent): void {
    const next = event.relatedTarget;
    const root = event.currentTarget;
    focusedWithin = next instanceof Node && root instanceof Node && root.contains(next);
  }

  onDestroy(cancel);
</script>

<!-- The pointer only holds the clock still; nothing here acts on it. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="toast"
  data-toast-id={item.id}
  data-tone={item.kind === 'notice' ? item.tone : undefined}
  data-status={item.kind === 'needs-you' ? item.status : undefined}
  on:pointerenter={() => (hovered = true)}
  on:pointerleave={() => (hovered = false)}
  on:focusin={() => (focusedWithin = true)}
  on:focusout={leaveFocus}
>
  <span class="toast__icon">
    {#if item.kind === 'needs-you'}
      <StatusBadge status={item.status} variant="icon" />
    {:else}
      {@const tone = noticeToneMark(item.tone)}
      <Icon name={tone.icon} label={tone.label} />
    {/if}
  </span>
  <div class="toast__body">
    <p class="toast__title">{item.title}</p>
    {#if secondLine !== null}
      <p class="toast__second">{secondLine}</p>
    {/if}
    {#if item.kind === 'needs-you'}
      {@const windowId = item.windowId}
      <div class="toast__actions">
        <button type="button" class="btn btn-secondary" on:click={() => onGoToWindow(windowId)}>
          {banners.goToWindow}
        </button>
      </div>
    {:else if noticeAction !== null}
      {@const action = noticeAction}
      <div class="toast__actions">
        <button type="button" class="btn btn-secondary" on:click={() => onAction(action)}>
          {buttons[action.labelKey]}
        </button>
      </div>
    {/if}
  </div>
  <IconButton
    class="toast__close"
    label={hints.dismissNotice}
    data-toast-close
    on:click={onDismiss}
  >
    <Icon name="x" size="sm" />
  </IconButton>
</div>

<style>
  .toast {
    --toast-tone: var(--fg-muted);
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
    border: var(--line-width) solid var(--line);
    border-left: 3px solid var(--toast-tone);
    border-radius: var(--radius-md);
    background-color: var(--bg-raised);
    box-shadow: var(--shadow-popover);
    color: var(--fg-default);
    pointer-events: auto;
  }

  .toast[data-tone='warning'],
  .toast[data-status='waiting_approval'] {
    --toast-tone: var(--st-approval);
  }

  .toast[data-tone='error'] {
    --toast-tone: var(--danger);
  }

  .toast[data-status='waiting_input'] {
    --toast-tone: var(--st-input);
  }

  .toast__icon {
    display: inline-flex;
    flex: 0 0 auto;
    margin-top: var(--space-0_5);
    color: var(--toast-tone);
  }

  .toast__body {
    flex: 1 1 auto;
    min-width: 0;
  }

  .toast__title {
    margin: 0;
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
    line-height: var(--leading-sm);
    overflow-wrap: anywhere;
  }

  .toast__second {
    display: -webkit-box;
    margin: 0;
    overflow: hidden;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-xs);
    overflow-wrap: anywhere;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
  }

  .toast__actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1_5);
    margin-top: var(--space-1_5);
  }

  .toast :global(.toast__close) {
    display: inline-flex;
    flex: 0 0 auto;
    align-self: flex-start;
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

  .toast :global(.toast__close:hover) {
    background-color: var(--bg-hover);
  }

  .toast :global(.toast__close:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  /* Only the arrival moves; a replaced sentence or a new count stays still. */
  @media (prefers-reduced-motion: no-preference) {
    .toast {
      animation: toast-enter 150ms var(--ease-out);
    }
  }

  @keyframes toast-enter {
    from {
      opacity: 0;
      translate: 0 4px;
    }
  }

  /* The shadow is dropped in forced colours, so the edge carries the shape. */
  @media (forced-colors: active) {
    .toast {
      border-color: CanvasText;
    }
  }
</style>
