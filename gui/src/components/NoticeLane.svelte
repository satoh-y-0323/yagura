<script lang="ts">
  /**
   * The notice lane: one line at the foot of the work surface, in the layout
   * rather than over it. Display only: which cards show, in what order, and
   * how many are left over is the feed's; this draws them left to right and
   * calls back. While it holds nothing it stays as an empty band of the same
   * height and is no landmark.
   *
   * Focus moves only off a card that leaves. Closed with a pointer, it goes
   * back through `onReturnFocus`; closed from the keyboard, or gone for any
   * other reason while it held focus, to the close button of the next card
   * (else the one before), or back through `onReturnFocus` when none is left.
   * A card whose body was pressed is left alone, since its action moves focus.
   * Focus never lands on a card's body, where the next key typed would act.
   */
  import { afterUpdate, beforeUpdate, tick } from 'svelte';
  import { cubicOut } from 'svelte/easing';

  import type { LaneItem, NoticeFeedOutput } from '../notices/notice-feed';
  import { STATUS_PRESENTATION } from '../status-presentation';
  import { banners } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import LaneCard from './LaneCard.svelte';
  import { pressedByKeyboard } from './press-source';

  /** The waiting count's mark is `moreCallsStatus`: the most urgent status among the calls that did not fit. */
  export let lane: Pick<
    NoticeFeedOutput['lane'],
    'visible' | 'moreCalls' | 'moreCallsStatus' | 'moreNotices'
  >;
  /** The tooltip's line under each card's sentence, by card id; the shell decides what it says. */
  export let secondLines: ReadonlyMap<string, string> = new Map();
  /** False while the app is hidden or not focused; the shell decides. */
  export let timersRunning: boolean;
  /** A card's body was pressed; what that does is the shell's. */
  export let onPress: (item: LaneItem) => void;
  export let onDismiss: (item: LaneItem) => void;
  export let onExpire: (item: LaneItem) => void;
  /** Opens the sidebar's Needs you filter. */
  export let onShowWaiting: () => void;
  /** Opens the Notifications list. */
  export let onShowNotifications: () => void;
  /** Gives focus back to the current window's terminal. */
  export let onReturnFocus: () => void;
  /** The lane's element, for the shell to measure. */
  export let element: HTMLElement | undefined = undefined;

  const GLIDE_MS = 150;

  /** The card that held focus as this update began, and the order the cards had then. */
  let held: { id: string; order: string[] } | null = null;
  /** A card whose own press took it off; its action moves focus. */
  let pressedId: string | null = null;

  $: empty = lane.visible.length === 0 && lane.moreCalls === 0 && lane.moreNotices === 0;
  $: alone = lane.visible.length === 1;

  function cardElement(id: string): HTMLElement | null {
    return element?.querySelector<HTMLElement>(`[data-lane-card-id="${CSS.escape(id)}"]`) ?? null;
  }

  /** The cards as they are drawn now, which before an update is still the order they had. */
  function drawnOrder(): string[] {
    return Array.from(
      element?.querySelectorAll<HTMLElement>('[data-lane-card-id]') ?? [],
      (card) => card.dataset.laneCardId ?? ''
    );
  }

  function cardHoldingFocus(): string | null {
    const active = document.activeElement;
    const card = active?.closest<HTMLElement>('[data-lane-card-id]') ?? null;
    return card !== null && element?.contains(card) === true
      ? (card.dataset.laneCardId ?? null)
      : null;
  }

  function focusCloseNear(order: readonly string[], index: number): void {
    const after = order.slice(index + 1);
    const before = order.slice(0, Math.max(0, index)).reverse();
    const next = [...after, ...before].map(cardElement).find((card) => card !== null) ?? null;
    const close = next?.querySelector<HTMLElement>('[data-lane-close]') ?? null;
    if (close === null) {
      onReturnFocus();
    } else {
      close.focus();
    }
  }

  // Taken before the cards are redrawn: once a card is gone, nothing tells it held focus.
  beforeUpdate(() => {
    const id = element === undefined ? null : cardHoldingFocus();
    held = id === null ? null : { id, order: drawnOrder() };
  });

  afterUpdate(() => {
    if (pressedId !== null && cardElement(pressedId) === null) {
      const pressed = pressedId;
      pressedId = null;
      if (held?.id === pressed) {
        held = null;
      }
    }
    if (held === null || cardElement(held.id) !== null) {
      held = null;
      return;
    }
    const { id, order } = held;
    held = null;
    focusCloseNear(order, order.indexOf(id));
  });

  function forgetPress(event: FocusEvent): void {
    const card =
      event.target instanceof Element
        ? event.target.closest<HTMLElement>('[data-lane-card-id]')
        : null;
    if (card?.dataset.laneCardId === pressedId) {
      pressedId = null;
    }
  }

  function press(item: LaneItem): void {
    pressedId = item.id;
    onPress(item);
  }

  async function dismiss(item: LaneItem, event: MouseEvent): Promise<void> {
    const holds = cardElement(item.id)?.contains(document.activeElement) ?? false;
    const order = drawnOrder();
    // The close press decides where focus goes, so the update it causes does not.
    pressedId = item.id;
    onDismiss(item);
    if (!holds) {
      return;
    }
    if (!pressedByKeyboard(event)) {
      onReturnFocus();
      return;
    }
    await tick();
    focusCloseNear(order, order.indexOf(item.id));
  }

  /** Slides a card to where it now sits; only the position moves, so the text never stretches. */
  function glide(node: Element, { from, to }: { from: DOMRect; to: DOMRect }) {
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const dx = from.left - to.left;
    const dy = from.top - to.top;
    return {
      duration: reduced || (dx === 0 && dy === 0) ? 0 : GLIDE_MS,
      easing: cubicOut,
      css: (_t: number, u: number) => `transform: translate(${u * dx}px, ${u * dy}px);`,
    };
  }
</script>

<!-- A section with an accessible name is a region landmark; an empty lane has no name. -->
<section
  class="notice-lane"
  aria-label={empty ? undefined : banners.noticeLane}
  bind:this={element}
  on:focusin={forgetPress}
>
  {#each lane.visible as item (item.id)}
    <div class="notice-lane__slot" class:notice-lane__slot--alone={alone} animate:glide>
      <LaneCard
        {item}
        secondLine={secondLines.get(item.id) ?? null}
        {timersRunning}
        onPress={() => press(item)}
        onDismiss={(event: MouseEvent) => void dismiss(item, event)}
        onExpire={() => onExpire(item)}
      />
    </div>
  {/each}
  {#if lane.moreCalls > 0 || lane.moreNotices > 0}
    <div class="notice-lane__more">
      {#if lane.moreCalls > 0}
        {@const presentation = STATUS_PRESENTATION[lane.moreCallsStatus]}
        <IconButton
          class="notice-lane__count"
          label={banners.waitingForYouMore(lane.moreCalls)}
          on:click={onShowWaiting}
        >
          <span class="notice-lane__mark" data-role={presentation.role}
            ><Icon name={presentation.icon} size="xs" /></span
          >
          {banners.laneCount(lane.moreCalls)}
        </IconButton>
      {/if}
      {#if lane.moreNotices > 0}
        <IconButton
          class="notice-lane__count"
          label={banners.notificationsMore(lane.moreNotices)}
          on:click={onShowNotifications}
        >
          <span class="notice-lane__mark"><Icon name="bell" size="xs" /></span>
          {banners.laneCount(lane.moreNotices)}
        </IconButton>
      {/if}
    </div>
  {/if}
</section>

<style>
  .notice-lane {
    display: flex;
    flex: 0 0 var(--notice-lane-height);
    align-items: center;
    gap: var(--lane-gap);
    box-sizing: border-box;
    height: var(--notice-lane-height);
    padding: 0 var(--lane-padding-inline);
    overflow: hidden;
    border-top: var(--line-width) solid var(--line);
    background-color: var(--bg-chrome);
  }

  /* The feed shows only as many cards as fit, so several never need to go below the minimum. */
  .notice-lane__slot {
    flex: 0 1 var(--lane-card-width);
    min-width: var(--lane-card-min-width);
  }

  /* A lone card takes what the counts leave, down to nothing: the counts reach every item. */
  .notice-lane__slot--alone {
    min-width: 0;
  }

  .notice-lane__more {
    display: flex;
    flex: 0 0 auto;
    gap: var(--lane-gap);
    margin-inline-start: auto;
  }

  .notice-lane :global(.notice-lane__count) {
    display: inline-flex;
    flex: 0 0 var(--lane-chip-width);
    align-items: center;
    justify-content: center;
    gap: var(--space-1);
    box-sizing: border-box;
    width: var(--lane-chip-width);
    height: var(--icon-button-hit-min);
    padding: 0 var(--space-1_5);
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-full);
    background-color: var(--bg-raised);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    font-weight: var(--weight-medium);
    line-height: var(--leading-xs);
    white-space: nowrap;
    cursor: pointer;
  }

  .notice-lane :global(.notice-lane__count:hover) {
    background-color: var(--bg-hover);
    color: var(--fg-default);
  }

  .notice-lane :global(.notice-lane__count:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  .notice-lane__mark {
    display: inline-flex;
    flex: 0 0 auto;
  }

  .notice-lane__mark[data-role='approval'] {
    color: var(--st-approval);
  }

  .notice-lane__mark[data-role='input'] {
    color: var(--st-input);
  }

  @media (forced-colors: active) {
    .notice-lane {
      border-top-color: CanvasText;
    }

    .notice-lane :global(.notice-lane__count) {
      border-color: ButtonText;
    }
  }
</style>
