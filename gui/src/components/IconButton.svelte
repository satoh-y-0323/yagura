<script lang="ts">
  /**
   * A button whose face is an icon, a glyph or a bare count. The required
   * `label` is both its accessible name and its hover tooltip, so the two
   * cannot drift apart; `hint` replaces the tooltip alone when a longer
   * explanation helps. Other attributes (`aria-pressed`, `data-*`) fall
   * through to the `<button>`, and `click` is forwarded.
   *
   * The `<button>` belongs to this component, so a parent's scoped style
   * reaches it only through `:global(.class)`.
   */
  export let label: string;
  export let hint: string | undefined = undefined;
  export let disabled = false;
  /** The underlying element, for callers that anchor a popup to it. */
  export let element: HTMLButtonElement | undefined = undefined;
  let className = '';
  export { className as class };
</script>

<!-- eslint-disable-next-line no-restricted-syntax -- the one place a button carries aria-label itself -->
<button
  bind:this={element}
  type="button"
  {...$$restProps}
  class={className}
  aria-label={label}
  title={hint ?? label}
  {disabled}
  on:click
>
  <slot />
</button>
