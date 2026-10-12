<script lang="ts">
  /**
   * The mark shown next to a window whose CLI was given hooks but whose
   * status reports are not arriving. Renders nothing otherwise; see
   * {@link statusReportMarkOf}.
   */
  import { statusReportMarkOf } from './status-report-mark';
  import { hints } from '../ui-messages';

  export let statusReportsMissing: boolean | null;

  $: mark = statusReportMarkOf(statusReportsMissing);
</script>

{#if mark !== null}
  <svg
    class="status-report-mark"
    role="img"
    aria-label={hints[mark.tooltipKey]}
    focusable="false"
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <title>{hints[mark.tooltipKey]}</title>
    <path d="M2 20h.01" />
    <path d="M7 20v-4" />
    <path d="M12 20v-8" />
    <path d="M17 20V8" />
    <path d="m2 2 20 20" />
  </svg>
{/if}

<style>
  .status-report-mark {
    flex: 0 0 auto;
    color: var(--color-warning);
  }
</style>
