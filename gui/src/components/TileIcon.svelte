<script lang="ts">
  // Shared tile-arrange glyph: used by both the header's
  // tile-all button and each room's tile action, so the icon is not
  // duplicated inline.
  export let size = 18;
</script>

<svg
  aria-hidden="true"
  focusable="false"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
>
  <rect x="3" y="3" width="18" height="18" rx="1" />
  <line x1="12" y1="3" x2="12" y2="21" />
  <line x1="3" y1="12" x2="21" y2="12" />
</svg>
