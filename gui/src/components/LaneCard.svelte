<script context="module" lang="ts">
  let instanceCount = 0;
</script>

<script lang="ts">
  /**
   * One card on the notice lane, on one line. Display only: it draws the item
   * it is handed and calls back. The whole body is one button that does the
   * card's action; while it is hovered or holds keyboard focus its title gives
   * way to the sentence of that action. The close button is its sibling.
   * A timed item goes after eight seconds; the clock stands still while the
   * pointer is on it, while focus is inside it, and while `timersRunning` is
   * false. A new `restart` starts the time over. Nothing here moves focus.
   */
  import { onDestroy } from 'svelte';

  import type { LaneItem } from '../notices/notice-feed';
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import { banners, buttons, hints, type TitleParts } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { noticeToneMark } from './notice-tone';
  import { tooltip } from './tooltip';

  export let item: LaneItem;
  /** The tooltip's line under the whole sentence: a notice's detail or where a calling window is; the shell picks it. */
  export let secondLine: UiText | DisplayLine | null = null;
  /** False while the app is hidden or not focused; the shell decides. */
  export let timersRunning: boolean;
  /** Called when the body is pressed; what the card does is the shell's. */
  export let onPress: () => void;
  /** Called with the close button's `click`, which tells a key from a pointer. */
  export let onDismiss: (event: MouseEvent) => void;
  export let onExpire: () => void;

  /** A line the card draws: split around a name that alone is cut short, or one sentence cut at its end. */
  type Line = { kind: 'split'; parts: TitleParts } | { kind: 'whole'; text: UiText | DisplayLine };

  const LIFETIME_MS = 8000;
  const nameId = `lane-card-name-${instanceCount++}`;

  let hovered = false;
  let focusedWithin = false;
  let remaining = LIFETIME_MS;
  let startedAt = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let mark = item.restart;

  $: titleLine = item.parts === null ? whole(item.sentence) : split(item.parts);
  $: showsCount = item.folded > 0;
  $: actionLine = cardAction(item);
  $: name = banners.laneCardName(item.title, sentence(actionLine));
  $: running = item.timed && timersRunning && !hovered && !focusedWithin;
  $: schedule(running, item.restart);

  function cardAction(card: LaneItem): Line {
    if (card.kind === 'needs-you') {
      return split(banners.goToCallingWindowParts(card.parts.name));
    }
    return whole(
      card.action === null ? banners.showInNotifications : buttons[card.action.labelKey]
    );
  }

  function split(parts: TitleParts): Line {
    return { kind: 'split', parts };
  }

  function whole(text: UiText | DisplayLine): Line {
    return { kind: 'whole', text };
  }

  function sentence(line: Line): UiText | DisplayLine {
    return line.kind === 'whole' ? line.text : line.parts.sentence;
  }

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

<!-- The pointer only holds the clock still; the buttons inside act. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="lane-card"
  data-lane-card-id={item.id}
  data-tone={item.kind === 'notice' ? item.tone : undefined}
  data-status={item.kind === 'needs-you' ? item.status : undefined}
  on:pointerenter={() => (hovered = true)}
  on:pointerleave={() => (hovered = false)}
  on:focusin={() => (focusedWithin = true)}
  on:focusout={leaveFocus}
>
  <button
    type="button"
    class="lane-card__body"
    aria-labelledby={nameId}
    use:tooltip={{
      text: item.title,
      details: secondLine === null ? [] : [secondLine],
      announcedElsewhere: true,
    }}
    on:click={onPress}
  >
    <span id={nameId} hidden>{name}</span>
    <span class="lane-card__icon">
      {#if item.kind === 'needs-you'}
        <StatusBadge status={item.status} variant="icon" />
      {:else}
        {@const tone = noticeToneMark(item.tone)}
        <Icon name={tone.icon} size="sm" label={tone.label} />
      {/if}
    </span>
    <!-- Both lines share one cell, so the swap cross-fades in place without moving the card. -->
    <span class="lane-card__lines">
      <span class="lane-card__line lane-card__line--title">
        {#if titleLine.kind === 'split'}
          <span class="lane-card__keep">{titleLine.parts.before}</span><span class="lane-card__name"
            >{titleLine.parts.name}</span
          ><span class="lane-card__rest">{titleLine.parts.after}</span>
        {:else if titleLine.kind === 'whole'}
          <!-- Named rather than `else`: the lint's type reader does not narrow the union in an `else`. -->
          <span class="lane-card__sentence">{titleLine.text}</span>
        {/if}
        {#if showsCount}
          <span class="lane-card__count">{banners.laneCount(item.folded)}</span>
        {/if}
      </span>
      <span class="lane-card__line lane-card__line--action">
        {#if actionLine.kind === 'split'}
          <span class="lane-card__keep">{actionLine.parts.before}</span><span
            class="lane-card__name">{actionLine.parts.name}</span
          ><span class="lane-card__rest">{actionLine.parts.after}</span>
        {:else if actionLine.kind === 'whole'}
          <span class="lane-card__sentence">{actionLine.text}</span>
        {/if}
        <span class="lane-card__arrow" aria-hidden="true">→</span>
      </span>
    </span>
  </button>
  <IconButton
    class="lane-card__close"
    label={hints.dismissNotice}
    data-lane-close
    on:click={onDismiss}
  >
    <Icon name="x" size="sm" />
  </IconButton>
</div>

<style>
  .lane-card {
    --lane-card-tone: var(--fg-muted);
    display: flex;
    align-items: center;
    gap: var(--space-0_5);
    box-sizing: border-box;
    width: 100%;
    height: 32px;
    padding: var(--space-0_5) var(--space-1) var(--space-0_5) var(--space-0_5);
    border: var(--line-width) solid var(--line);
    border-left: 3px solid var(--lane-card-tone);
    border-radius: var(--radius-md);
    background-color: var(--bg-raised);
    box-shadow: var(--shadow-md);
    color: var(--fg-default);
  }

  .lane-card[data-tone='warning'],
  .lane-card[data-status='waiting_approval'] {
    --lane-card-tone: var(--st-approval);
  }

  .lane-card[data-tone='error'] {
    --lane-card-tone: var(--danger);
  }

  .lane-card[data-status='waiting_input'] {
    --lane-card-tone: var(--st-input);
  }

  .lane-card__body {
    display: flex;
    flex: 1 1 auto;
    align-items: center;
    gap: var(--space-1_5);
    min-width: 0;
    height: 100%;
    padding: 0 var(--space-1) 0 var(--space-1_5);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: inherit;
    font: inherit;
    font-size: var(--text-sm);
    line-height: var(--leading-sm);
    text-align: start;
    cursor: pointer;
  }

  .lane-card__body:hover {
    background-color: var(--bg-hover);
  }

  .lane-card__body:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 0;
  }

  .lane-card__icon {
    display: inline-flex;
    flex: 0 0 auto;
    color: var(--lane-card-tone);
  }

  .lane-card__lines {
    display: grid;
    flex: 1 1 auto;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
  }

  .lane-card__line {
    display: flex;
    grid-area: 1 / 1;
    align-items: baseline;
    min-width: 0;
    overflow: hidden;
    font-weight: var(--weight-semibold);
    white-space: nowrap;
  }

  .lane-card__line--action {
    color: var(--accent);
    opacity: 0;
  }

  .lane-card__body:hover .lane-card__line--title,
  .lane-card__body:focus-visible .lane-card__line--title {
    opacity: 0;
  }

  .lane-card__body:hover .lane-card__line--action,
  .lane-card__body:focus-visible .lane-card__line--action {
    opacity: 1;
  }

  .lane-card__keep,
  .lane-card__count,
  .lane-card__arrow {
    flex: 0 0 auto;
  }

  /* The name gives way first: its weight makes the rest's share of any shortfall too small to draw. */
  .lane-card__name {
    flex: 0 100000 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .lane-card__rest,
  .lane-card__sentence {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .lane-card__count {
    margin-inline-start: var(--space-1_5);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    font-weight: var(--weight-medium);
  }

  .lane-card__arrow {
    margin-inline-start: var(--space-1);
  }

  .lane-card :global(.lane-card__close) {
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

  .lane-card :global(.lane-card__close:hover) {
    background-color: var(--bg-hover);
    color: var(--fg-default);
  }

  .lane-card :global(.lane-card__close:focus-visible) {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 0;
  }

  /* Only the arrival and the swap move; a replaced sentence or a new count stays still. */
  @media (prefers-reduced-motion: no-preference) {
    .lane-card {
      animation: lane-card-enter 150ms var(--ease-out);
    }

    .lane-card__line {
      transition: opacity var(--dur-fast) var(--ease-out);
    }
  }

  @keyframes lane-card-enter {
    from {
      opacity: 0;
      translate: 0 4px;
    }
  }

  /* The shadow is dropped in forced colours, so the edge carries the shape. */
  @media (forced-colors: active) {
    .lane-card {
      border-color: CanvasText;
    }

    .lane-card__body:hover .lane-card__line--action,
    .lane-card__body:focus-visible .lane-card__line--action {
      color: LinkText;
    }
  }
</style>
