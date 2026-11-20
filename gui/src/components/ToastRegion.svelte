<script lang="ts">
  /**
   * The one container pop-ups appear in, a named region at the bottom right of
   * the positioned box it is placed in. Display only: what shows, in what
   * order, and how many wait is the feed's; this draws it and calls back.
   * Focus moves only when the user closes the pop-up holding it: to the next
   * pop-up left, or through `onReturnFocus` when none is.
   */
  import { tick } from 'svelte';

  import type { NoticeAction } from '../notices/notice-model';
  import type { NoticeFeedOutput, ToastItem } from '../notices/notice-feed';
  import { banners } from '../ui-messages';
  import Toast from './Toast.svelte';

  export let toasts: Pick<
    NoticeFeedOutput['toasts'],
    'visible' | 'overflowWaiting' | 'overflowNotices'
  >;
  /** The line beneath each pop-up's title, by pop-up id; the shell decides what it says. */
  export let secondLines: ReadonlyMap<string, string> = new Map();
  /** False while the app is hidden or not focused; the shell decides. */
  export let timersRunning: boolean;
  export let onAction: (action: NoticeAction) => void;
  export let onGoToWindow: (windowId: string) => void;
  export let onDismiss: (item: ToastItem) => void;
  export let onExpire: (item: ToastItem) => void;
  /** Opens the sidebar's Needs you filter. */
  export let onShowWaiting: () => void;
  /** Opens the Notifications list. */
  export let onShowNotifications: () => void;
  /** Gives focus back to the current window's terminal. */
  export let onReturnFocus: () => void;

  let region: HTMLElement;

  function toastElement(id: string): HTMLElement | null {
    return region.querySelector<HTMLElement>(`[data-toast-id="${CSS.escape(id)}"]`);
  }

  async function dismiss(item: ToastItem, index: number): Promise<void> {
    const held = toastElement(item.id)?.contains(document.activeElement) ?? false;
    const neighbour = toasts.visible[index + 1] ?? toasts.visible[index - 1];
    onDismiss(item);
    if (!held) {
      return;
    }
    await tick();
    const next =
      (neighbour === undefined ? null : toastElement(neighbour.id)) ??
      region.querySelector<HTMLElement>('[data-toast-id]');
    const control = next?.querySelector<HTMLElement>('button') ?? null;
    if (control === null) {
      onReturnFocus();
    } else {
      control.focus();
    }
  }
</script>

<!-- A section with an accessible name is a region landmark. -->
<section class="toast-region" aria-label={banners.notificationPopUps} bind:this={region}>
  {#each toasts.visible as item, index (item.id)}
    <Toast
      {item}
      secondLine={secondLines.get(item.id) ?? null}
      {timersRunning}
      {onAction}
      {onGoToWindow}
      onDismiss={() => void dismiss(item, index)}
      onExpire={() => onExpire(item)}
    />
  {/each}
  {#if toasts.overflowWaiting > 0 || toasts.overflowNotices > 0}
    <div class="toast-region__more">
      {#if toasts.overflowWaiting > 0}
        <button type="button" class="toast-region__count" on:click={onShowWaiting}>
          {banners.waitingForYouMore(toasts.overflowWaiting)}
        </button>
      {/if}
      {#if toasts.overflowNotices > 0}
        <button type="button" class="toast-region__count" on:click={onShowNotifications}>
          {banners.notificationsMore(toasts.overflowNotices)}
        </button>
      {/if}
    </div>
  {/if}
</section>

<style>
  /* The box between the pop-ups stays clickable, so an empty region blocks nothing. */
  .toast-region {
    position: absolute;
    right: var(--space-3);
    bottom: var(--space-3);
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: min(360px, 100% - 2 * var(--space-3));
    pointer-events: none;
  }

  .toast-region__more {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-1_5);
  }

  .toast-region__count {
    padding: var(--space-0_5) var(--space-2);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-full);
    background-color: var(--bg-raised);
    box-shadow: var(--shadow-popover);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-xs);
    line-height: var(--leading-xs);
    cursor: pointer;
    pointer-events: auto;
  }

  .toast-region__count:hover {
    color: var(--fg-default);
  }

  .toast-region__count:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  @media (forced-colors: active) {
    .toast-region__count {
      border-color: ButtonText;
    }
  }
</style>
