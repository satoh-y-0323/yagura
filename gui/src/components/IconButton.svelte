<script context="module" lang="ts">
  let instanceCount = 0;
</script>

<script lang="ts">
  /**
   * A button whose face is an icon, a glyph or a bare count. The required
   * `label` is its accessible name and, unless something longer is given, its
   * tooltip, so the two cannot drift apart. `hint` is a longer explanation: the
   * tooltip shows it and `aria-describedby` carries it, so it is not tooltip-only.
   * Other attributes (`aria-pressed`, `data-*`) fall through to the `<button>`,
   * and `click` is forwarded.
   *
   * With `disabledReason` the button cannot act now but stays focusable
   * (`aria-disabled`, not `disabled`): the reason is tied to it by
   * `aria-describedby`, and a press never reaches the caller's `click` handler.
   *
   * With `toggletip` a press opens (and the next press closes) a chip with that
   * text, drawn as visible text rather than a hover tooltip.
   *
   * The `<button>` belongs to this component, so a parent's scoped style
   * reaches it only through `:global(.class)`.
   */
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import { tooltip } from './tooltip';

  export let label: UiText | DisplayLine;
  export let hint: UiText | DisplayLine | undefined = undefined;
  export let disabled = false;
  /** Why the button cannot act now; set it instead of `disabled` to keep focus and say why. */
  export let disabledReason: UiText | DisplayLine | undefined = undefined;
  /** Text of a press-to-open chip; replaces the hover tooltip. */
  export let toggletip: UiText | DisplayLine | undefined = undefined;
  /** The underlying element, for callers that anchor a popup to it. */
  export let element: HTMLButtonElement | undefined = undefined;
  let className = '';
  export { className as class };

  const instance = instanceCount++;
  const reasonId = `icon-button-reason-${instance}`;
  const hintId = `icon-button-hint-${instance}`;

  $: describedHint = disabledReason === undefined && hint !== undefined && hint !== label;
  $: describedBy = disabledReason !== undefined ? reasonId : describedHint ? hintId : undefined;
  $: tooltipOptions =
    toggletip !== undefined
      ? { text: toggletip, mode: 'toggle' as const }
      : { text: disabledReason ?? hint ?? label, announcedElsewhere: true };

  // Runs in the capture phase, before the forwarded `click` and before any
  // ancestor listener, so a press on an unavailable button reaches no caller.
  function swallowWhenUnavailable(event: MouseEvent): void {
    if (disabledReason !== undefined) {
      event.stopImmediatePropagation();
      event.preventDefault();
    }
  }
</script>

<!-- eslint-disable-next-line no-restricted-syntax -- the one place a button carries aria-label itself -->
<button
  bind:this={element}
  type="button"
  {...$$restProps}
  class={className}
  aria-label={label}
  aria-disabled={disabledReason === undefined ? undefined : 'true'}
  aria-describedby={describedBy}
  {disabled}
  use:tooltip={tooltipOptions}
  on:click|capture={swallowWhenUnavailable}
  on:click
>
  <slot />
  {#if disabledReason !== undefined}
    <span id={reasonId} hidden>{disabledReason}</span>
  {:else if describedHint}
    <span id={hintId} hidden>{hint}</span>
  {/if}
</button>
