<script lang="ts">
  /**
   * The Approvals tab of the Settings panel: renders the rows
   * `approvalToolRows` and `planReviewRow` derive and reports a switch press.
   * No IPC, no decision of its own -- the shell owns the save state, the
   * failure line, the notice line and every call.
   */
  import type { ApprovalSettingsInfo } from '../ipc/session-client';
  import SettingsSwitch from './SettingsSwitch.svelte';
  import {
    approvalToolRows,
    planReviewRow,
    PLAN_REVIEW_GATE_ID,
  } from '../settings/approval-settings-model';
  import { settings } from '../ui-messages';

  export let info: ApprovalSettingsInfo | null;
  export let notice: string | null;
  export let isReadOnly: boolean;
  export let isSaving: boolean;
  export let updateFailure: string | null;
  export let onSetAutoApprove: (tool: string, autoApprove: boolean) => void;
  export let onSetPlanApprovalRequired: (required: boolean) => void;

  $: frozen = isReadOnly || isSaving;
</script>

<div class="approval-settings">
  <p class="approval-settings__description">{settings.approvalsDescription}</p>
  {#if notice}
    <p class="approval-settings__notice">{notice}</p>
  {/if}
  {#if updateFailure}
    <p class="approval-settings__failure">{updateFailure}</p>
  {/if}
  {#if info}
    {@const toolRows = approvalToolRows(info)}
    {@const plan = planReviewRow(info)}
    <div class="approval-settings__group-head">{settings.approvalToolsHeading}</div>
    <div class="approval-settings__list">
      {#each toolRows as row (row.id)}
        <div class="approval-settings__row">
          <div class="approval-settings__name">
            <b>{row.name}</b>
            <span class="approval-settings__id">{row.id}</span>
          </div>
          <div class="approval-settings__switch-slot">
            <SettingsSwitch
              checked={row.autoApprove}
              label={settings.approvalToolSwitchLabel(row.name)}
              text={row.stateLabel}
              disabled={frozen}
              onToggle={() => onSetAutoApprove(row.id, !row.autoApprove)}
            />
          </div>
          <div class="approval-settings__what">{row.description}</div>
          {#if row.risk}
            <div
              class="approval-settings__risk"
              class:approval-settings__risk--emphasized={row.isRiskEmphasized}
            >
              {row.risk}
            </div>
          {/if}
        </div>
      {/each}
    </div>
    <div class="approval-settings__group-head">{settings.approvalPlansHeading}</div>
    <div class="approval-settings__list">
      <div class="approval-settings__row">
        <div class="approval-settings__name">
          <b>{settings.planReviewSwitchLabel}</b>
          <span class="approval-settings__id">{PLAN_REVIEW_GATE_ID}</span>
        </div>
        <div class="approval-settings__switch-slot">
          <SettingsSwitch
            checked={plan.required}
            label={settings.planReviewSwitchLabel}
            text={plan.stateLabel}
            disabled={frozen}
            onToggle={() => onSetPlanApprovalRequired(!plan.required)}
          />
        </div>
        <div class="approval-settings__what">{settings.planReviewDescription}</div>
        {#if plan.risk}
          <div class="approval-settings__risk">{plan.risk}</div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .approval-settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    overflow-y: auto;
  }

  .approval-settings__description {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .approval-settings__notice,
  .approval-settings__failure {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }

  .approval-settings__group-head {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .approval-settings__list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }

  .approval-settings__row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-xs) var(--space-sm);
    padding: var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    align-items: center;
  }

  .approval-settings__row:last-child {
    border-bottom: none;
  }

  .approval-settings__name {
    display: flex;
    align-items: baseline;
    gap: var(--space-xs);
    flex-wrap: wrap;
    min-width: 0;
  }

  .approval-settings__id {
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .approval-settings__switch-slot {
    grid-column: 2;
    grid-row: 1 / span 3;
    display: flex;
    align-items: center;
  }

  .approval-settings__what {
    grid-column: 1;
    font-size: var(--font-size-sm);
    color: var(--color-text-secondary);
  }

  .approval-settings__risk {
    grid-column: 1;
    font-size: var(--font-size-sm);
    color: var(--color-text-secondary);
  }

  .approval-settings__risk--emphasized {
    color: var(--color-warning);
  }
</style>
