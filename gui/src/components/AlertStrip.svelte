<script lang="ts">
  /**
   * The strip of notices about a part of the app that does not work. Display
   * only: it draws the blocking notices it is handed and calls back. A row has
   * no close button, and it is not a live region: the announcer reads it.
   */
  import type { Notice } from '../notices/notice-feed';
  import type { NoticeAction } from '../notices/notice-model';
  import { banners, buttons } from '../ui-messages';
  import Icon from './Icon.svelte';
  import { noticeToneMark } from './notice-tone';

  export let notices: readonly Notice[];
  export let onAction: (action: NoticeAction) => void;
</script>

{#if notices.length > 0}
  <div class="alert-strip">
    {#each notices as notice (notice.seq)}
      {@const tone = noticeToneMark(notice.tone)}
      <div class="alert-strip__row" data-tone={notice.tone}>
        <span class="alert-strip__icon">
          <Icon name={tone.icon} label={tone.label} />
        </span>
        <span class="alert-strip__title">{notice.title}</span>
        {#if notice.action !== null}
          {@const action = notice.action}
          <button type="button" class="btn btn-secondary" on:click={() => onAction(action)}>
            {buttons[action.labelKey]}
          </button>
        {/if}
        {#if notice.detail !== null}
          <details class="alert-strip__details">
            <summary class="alert-strip__summary">
              {banners.noticeDetails}
              <Icon name="chevron-down" size="sm" />
            </summary>
            <p class="alert-strip__detail">{notice.detail}</p>
          </details>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .alert-strip {
    display: flex;
    flex-direction: column;
  }

  /* The details' body joins the row's flex lines, so it wraps to a line of its own beneath. */
  .alert-strip__row {
    --alert-tone: var(--fg-muted);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: var(--space-2);
    min-height: 36px;
    padding: 0 var(--space-3) 0 var(--space-2);
    border-bottom: var(--line-width) solid var(--line);
    border-left: 3px solid var(--alert-tone);
    background-color: var(--bg-raised);
    color: var(--fg-default);
    font-size: var(--text-sm);
  }

  .alert-strip__row[data-tone='warning'] {
    --alert-tone: var(--st-approval);
  }

  .alert-strip__row[data-tone='error'] {
    --alert-tone: var(--danger);
  }

  .alert-strip__icon {
    display: inline-flex;
    flex: 0 0 auto;
    color: var(--alert-tone);
  }

  .alert-strip__title {
    flex: 1 1 0;
    min-inline-size: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .alert-strip__details {
    display: contents;
  }

  .alert-strip__details::details-content {
    flex-basis: 100%;
  }

  .alert-strip__summary {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-1) var(--space-1_5);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    list-style: none;
    cursor: pointer;
  }

  .alert-strip__summary::-webkit-details-marker {
    display: none;
  }

  .alert-strip__summary:hover {
    color: var(--fg-default);
  }

  .alert-strip__summary:focus-visible {
    outline: var(--line-width-strong) solid var(--ring);
    outline-offset: 1px;
  }

  .alert-strip__details[open] .alert-strip__summary :global(.icon) {
    rotate: 180deg;
  }

  /* Indented to the title; the basis also covers engines without the details-content box. */
  .alert-strip__detail {
    flex-basis: 100%;
    margin: 0;
    padding: 0 0 var(--space-2) calc(var(--icon-md) + var(--space-2));
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-xs);
    overflow-wrap: anywhere;
  }

  @media (forced-colors: active) {
    .alert-strip__row {
      border-left-color: CanvasText;
    }
  }
</style>
