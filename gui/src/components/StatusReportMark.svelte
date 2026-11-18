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
    <path d="M3 7V5a2 2 0 0 1 2-2h2" />
    <path d="M17 3h2a2 2 0 0 1 2 2v2" />
    <path d="M21 17v2a2 2 0 0 1-2 2h-2" />
    <path d="M7 21H5a2 2 0 0 1-2-2v-2" />
    <path d="M7 8h8" />
    <path d="M7 12h10" />
    <path d="M7 16h6" />
  </svg>
{/if}

<style>
  .status-report-mark {
    flex: 0 0 auto;
    color: var(--color-warning);
  }
</style>
