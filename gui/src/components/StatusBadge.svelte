<script lang="ts">
  /**
   * The mark of one window status. Display only: the icon, the colour and the
   * motion all come from `STATUS_PRESENTATION`, and the badge never takes focus.
   * `icon` is a bare icon that names the status as an image; `label` is a pill
   * with the status word beside it; `count` is the small pill a group of windows
   * wears: the mark of its highest-ranked status and how many windows need the
   * user. `reason` follows the word (why an unknown status is unknown).
   */
  import type { UiText } from '../display/ui-text';
  import type { WindowStatus } from '../ipc/session-boundary';
  import { STATUS_PRESENTATION } from '../status-presentation';
  import { describeAttentionBadge, describeStatusLabel } from '../ui-messages';
  import Icon from './Icon.svelte';

  export let status: WindowStatus;
  export let variant: 'icon' | 'label' | 'count';
  /** The short reason the word is followed by, `null` for none. */
  export let reason: UiText | null = null;
  /** Windows that need the user; read by the `count` variant only. */
  export let count = 0;

  $: presentation = STATUS_PRESENTATION[status];
  $: text = describeStatusLabel(status, reason);
</script>

{#if variant === 'icon'}
  <span class="status-badge" data-role={presentation.role}>
    <span class="status-badge__icon" data-motion={presentation.motion}>
      <Icon name={presentation.icon} size="sm" label={text} />
    </span>
  </span>
{:else if variant === 'label'}
  <span class="status-badge status-badge--label" data-role={presentation.role}>
    <span class="status-badge__icon" data-motion={presentation.motion}>
      <Icon name={presentation.icon} size="sm" />
    </span>
    <span class="status-badge__text">{text}</span>
  </span>
{:else}
  <span
    class="status-badge status-badge--count"
    data-role={presentation.role}
    role="img"
    aria-label={describeAttentionBadge(count)}
  >
    <span class="status-badge__icon" data-motion={presentation.motion}>
      <Icon name={presentation.icon} size="xs" />
    </span>
    <span class="status-badge__count">{count}</span>
  </span>
{/if}

<style>
  .status-badge {
    --badge-color: var(--st-idle);
    --badge-tint: var(--st-idle-tint);
    display: inline-flex;
    align-items: center;
    flex: 0 0 auto;
    color: var(--badge-color);
  }

  .status-badge[data-role='approval'] {
    --badge-color: var(--st-approval);
    --badge-tint: var(--st-approval-tint);
  }

  .status-badge[data-role='input'] {
    --badge-color: var(--st-input);
    --badge-tint: var(--st-input-tint);
  }

  .status-badge[data-role='running'] {
    --badge-color: var(--st-running);
    --badge-tint: var(--st-running-tint);
  }

  .status-badge[data-role='halted'] {
    --badge-color: var(--st-halted);
    --badge-tint: var(--st-halted-tint);
  }

  .status-badge[data-role='unknown'] {
    --badge-color: var(--st-unknown);
    --badge-tint: var(--st-unknown-tint);
  }

  .status-badge--label {
    gap: var(--space-1);
    height: 20px;
    padding: 0 var(--space-2) 0 var(--space-1_5);
    border-radius: var(--radius-full);
    background-color: var(--badge-tint);
  }

  .status-badge__text {
    color: var(--fg-default);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    line-height: var(--leading-xs);
    white-space: nowrap;
  }

  /* A count never sits on a status-coloured ground: the pill is neutral and
     only the icon carries the colour. */
  .status-badge--count {
    gap: var(--space-0_5);
    min-width: 16px;
    height: 16px;
    padding: 0 var(--space-1);
    border-radius: var(--radius-full);
    background-color: var(--bg-selected);
  }

  .status-badge__count {
    color: var(--fg-default);
    font-size: var(--text-2xs);
    font-weight: var(--weight-semibold);
    font-variant-numeric: tabular-nums;
    line-height: var(--leading-2xs);
  }

  .status-badge__icon {
    display: inline-flex;
  }

  /* Eight steps keep the redraw rate low; only transform changes, so the
     spin stays on the compositor. A static arc is what remains when the
     user asks for less motion. */
  @media (prefers-reduced-motion: no-preference) {
    .status-badge__icon[data-motion='spin'] {
      animation: status-badge-spin 1s steps(8) infinite;
    }
  }

  @keyframes status-badge-spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
