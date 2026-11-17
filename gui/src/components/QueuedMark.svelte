<script lang="ts">
  /**
   * The envelope-and-count mark shown next to a window whose inbox holds
   * messages waiting to be read. Renders nothing while the count is absent;
   * see {@link import('../stores/window-registry').queuedCountOf}.
   */
  import { describeQueuedMessages } from '../ui-messages';
  import { tooltip } from './tooltip';

  export let count: number | undefined;
</script>

{#if count !== undefined}
  <span
    class="queued-mark"
    role="img"
    aria-label={describeQueuedMessages(count)}
    use:tooltip={{ text: describeQueuedMessages(count), announcedElsewhere: true }}
  >
    <svg
      class="queued-mark__icon"
      aria-hidden="true"
      focusable="false"
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <rect x="2" y="4" width="20" height="16" rx="2" />
      <path d="m22 7-10 6L2 7" />
    </svg>
    {count}
  </span>
{/if}
