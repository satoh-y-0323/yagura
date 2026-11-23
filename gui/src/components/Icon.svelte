<script lang="ts">
  /**
   * One inline SVG icon from `icons/icon-paths`. Without `label` the icon is
   * decoration and hidden from assistive technology; with `label` it is a
   * standalone image that carries its meaning as its accessible name.
   */
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import { ICON_SHAPES, type IconName } from '../icons/icon-paths';

  export let name: IconName;
  export let size: 'xs' | 'sm' | 'md' | 'xl' = 'md';
  export let label: UiText | DisplayLine | undefined = undefined;
</script>

<svg
  class="icon icon--{size}"
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  focusable="false"
  role={label === undefined ? undefined : 'img'}
  aria-label={label}
  aria-hidden={label === undefined ? 'true' : undefined}
>
  {#each ICON_SHAPES[name] as shape, index (index)}
    <svelte:element this={shape.tag} {...shape.attrs} />
  {/each}
</svg>

<style>
  .icon {
    flex: 0 0 auto;
  }

  .icon--xs {
    width: var(--icon-xs);
    height: var(--icon-xs);
  }

  .icon--sm {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }

  .icon--md {
    width: var(--icon-md);
    height: var(--icon-md);
  }

  .icon--xl {
    width: var(--icon-xl);
    height: var(--icon-xl);
  }
</style>
