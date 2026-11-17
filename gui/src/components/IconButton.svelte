<script context="module" lang="ts">
  let reasonCount = 0;
</script>

<script lang="ts">
  /**
   * A button whose face is an icon, a glyph or a bare count. The required
   * `label` is both its accessible name and its hover tooltip, so the two
   * cannot drift apart; `hint` replaces the tooltip alone when a longer
   * explanation helps. Other attributes (`aria-pressed`, `data-*`) fall
   * through to the `<button>`, and `click` is forwarded.
   *
   * With `disabledReason` the button cannot act now but stays focusable
   * (`aria-disabled`, not `disabled`): the reason is tied to it by
   * `aria-describedby`, and a press never reaches the caller's `click` handler.
   *
   * The `<button>` belongs to this component, so a parent's scoped style
   * reaches it only through `:global(.class)`.
   */
  export let label: string;
  export let hint: string | undefined = undefined;
  export let disabled = false;
  /** Why the button cannot act now; set it instead of `disabled` to keep focus and say why. */
  export let disabledReason: string | undefined = undefined;
  /** The underlying element, for callers that anchor a popup to it. */
  export let element: HTMLButtonElement | undefined = undefined;
  let className = '';
  export { className as class };

  const reasonId = `icon-button-reason-${reasonCount++}`;

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
  aria-describedby={disabledReason === undefined ? undefined : reasonId}
  title={disabledReason ?? hint ?? label}
  {disabled}
  on:click|capture={swallowWhenUnavailable}
  on:click
>
  <slot />
  {#if disabledReason !== undefined}
    <span id={reasonId} hidden>{disabledReason}</span>
  {/if}
</button>
