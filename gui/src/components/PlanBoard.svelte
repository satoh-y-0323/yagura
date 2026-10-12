<script lang="ts">
  /**
   * The Board tab of the plans panel: the plan picker, the owner banner and
   * nomination, and one column per wave with a card per task.
   *
   * It owns no IPC: the plan and the window list are handed in, and every
   * change goes back out through the callbacks the shell supplies. Liveness
   * and the owner candidates come from the window list, not from the plan.
   */
  import {
    assignmentFromEditor,
    assignmentSourceKey,
    buildWaveColumns,
    editorFieldsOf,
    isUnassigned,
    ownerCandidates,
    planOwner,
    reportOutcomeKey,
    reviewBannerKind,
    unappliedTaskIds,
    type BoardWindow,
    type EditorFields,
    type TaskCard,
  } from '../plans/plan-board-model';
  import { mappedLaunchOptionValues } from '../profile/launch-profile-choices';
  import { describeStatus, hints, nav, plans } from '../ui-messages';
  import type { ProfileOption } from './add-window-dialog-types';
  import IconButton from './IconButton.svelte';
  import LaunchProfileFields from './LaunchProfileFields.svelte';

  type PlanView = Parameters<typeof buildWaveColumns>[0];
  type TaskView = TaskCard['task'];
  type AssignmentView = ReturnType<typeof assignmentFromEditor>;

  /** The least of a plan-list entry the picker reads. */
  interface PlanChoice {
    plan_id: string;
    title: string;
  }

  export let planList: PlanChoice[];
  export let selectedPlanId: string | null;
  export let plan: PlanView | null;
  export let windows: BoardWindow[];
  export let onSelectPlan: (planId: string) => void;
  export let onDeletePlan: (planId: string) => void;
  export let onSetTaskAssignment: (taskId: string, assignment: AssignmentView) => void;
  export let onClearTaskAssignment: (taskId: string) => void;
  /** Names a window as the owner; resolves false when that was refused. */
  export let onSetPlanOwner: (windowId: string) => Promise<boolean>;
  /** Approves the selected plan. */
  export let onApprovePlan: () => void;
  /** The launch profiles an assignment's profile is chosen from. */
  export let profileOptions: ProfileOption[];

  // Editor text per task, kept only while the same plan is shown: task ids
  // are unique within one plan, not across plans.
  let drafts: Record<string, EditorFields> = {};
  let draftsPlanId: string | null = null;
  let isOwnerAssignmentRefused = false;

  $: forgetDraftsOnPlanSwitch(plan === null ? null : plan.plan_id);
  $: owner = plan === null ? null : planOwner(plan, windows);

  function forgetDraftsOnPlanSwitch(planId: string | null): void {
    if (planId === draftsPlanId) {
      return;
    }
    draftsPlanId = planId;
    drafts = {};
    isOwnerAssignmentRefused = false;
  }

  function draftFor(current: Record<string, EditorFields>, task: TaskView): EditorFields {
    return current[task.task_id] ?? editorFieldsOf(task.resolved.assignment);
  }

  function editDraft(task: TaskView, field: keyof EditorFields, value: string): void {
    drafts = { ...drafts, [task.task_id]: { ...draftFor(drafts, task), [field]: value } };
  }

  function assignmentOf(fields: EditorFields): AssignmentView {
    return assignmentFromEditor({
      profile: fields.profile,
      ...mappedLaunchOptionValues(profileOptions, fields.profile, fields),
    });
  }

  function forgetDraft(taskId: string): void {
    drafts = Object.fromEntries(Object.entries(drafts).filter(([key]) => key !== taskId));
  }

  function describeAssignment(assignment: AssignmentView | null): string {
    if (assignment === null) {
      return '';
    }
    return [assignment.profile, assignment.model, assignment.effort]
      .filter((part): part is string => part !== null && part !== '')
      .join(' · ');
  }

  function isPlansKey(key: string): key is keyof typeof plans {
    return Object.prototype.hasOwnProperty.call(plans, key);
  }

  // `plans` also holds function-valued entries (e.g. the sidebar badge's
  // tooltip), so a key looked up dynamically here is not always a string.
  function plansLabel(key: string): string {
    if (!isPlansKey(key)) {
      return '';
    }
    const value = plans[key];
    return typeof value === 'string' ? value : '';
  }

  async function assignOwner(windowId: string): Promise<void> {
    const accepted = await onSetPlanOwner(windowId);
    isOwnerAssignmentRefused = !accepted;
  }

  function deleteSelectedPlan(): void {
    if (selectedPlanId !== null) {
      onDeletePlan(selectedPlanId);
    }
  }
</script>

<div class="plan-board">
  <div class="plan-board__picker">
    <select
      class="plan-board__select"
      aria-label={nav.plans}
      bind:value={selectedPlanId}
      on:change={(event) => onSelectPlan(event.currentTarget.value)}
    >
      {#each planList as entry (entry.plan_id)}
        <option value={entry.plan_id}>{entry.title}</option>
      {/each}
    </select>
    <button
      type="button"
      class="btn btn-danger"
      disabled={selectedPlanId === null}
      on:click={deleteSelectedPlan}>{plans.deletePlanButton}</button
    >
  </div>

  {#if plan !== null}
    {#if reviewBannerKind(plan) === 'pending'}
      <div class="plan-board__review" data-tone="pending">
        <p class="plan-board__review-title">{plans.reviewPendingTitle}</p>
        <p class="plan-board__review-detail">{plans.reviewPendingDetail}</p>
        <button
          type="button"
          class="btn btn-primary"
          disabled={unappliedTaskIds(plan, drafts).length > 0}
          on:click={() => onApprovePlan()}>{plans.approvePlanButton}</button
        >
        {#if unappliedTaskIds(plan, drafts).length > 0}
          <p class="plan-board__review-hint">{plans.approvePlanBlockedHint}</p>
        {/if}
      </div>
    {/if}
    {#if reviewBannerKind(plan) === 'approved'}
      <div class="plan-board__review" data-tone="approved">
        <p class="plan-board__review-title">{plans.reviewApprovedTitle}</p>
        <p class="plan-board__review-detail">{plans.reviewApprovedDetail}</p>
      </div>
    {/if}
    {#if reviewBannerKind(plan) === 'changed'}
      <div class="plan-board__review" data-tone="changed">
        <p class="plan-board__review-title">{plans.reviewChangedTitle}</p>
        <p class="plan-board__review-detail">{plans.reviewChangedDetail}</p>
      </div>
    {/if}

    {#if planOwner(plan, windows).kind === 'none'}
      <p class="plan-board__banner" role="status">{plans.noOwnerBanner}</p>
    {/if}

    <div class="plan-board__owner" role="group" aria-label={plans.assignOwnerButton}>
      <span class="plan-board__owner-label">{plans.assignOwnerButton}</span>
      {#each ownerCandidates(plan, windows) as candidate (candidate.windowId)}
        <button
          type="button"
          class="plan-board__candidate"
          aria-pressed={owner !== null &&
            owner.kind === 'live' &&
            owner.windowId === candidate.windowId}
          on:click={() => void assignOwner(candidate.windowId)}>{candidate.name}</button
        >
      {:else}
        <span class="plan-board__muted">{hints.noWindows}</span>
      {/each}
    </div>
    {#if isOwnerAssignmentRefused}
      <p class="plan-board__refused" role="alert">{plans.assignOwnerFailed}</p>
    {/if}

    <div class="plan-board__columns">
      {#each buildWaveColumns(plan, windows) as column (column.wave)}
        <section class="plan-board__column">
          <h3 class="plan-board__wave">{column.wave + 1}</h3>
          <ul class="plan-board__cards">
            {#each column.cards as card (card.task.task_id)}
              {@const fields = draftFor(drafts, card.task)}
              <li
                class="plan-card"
                data-unassigned={isUnassigned(card.task.resolved)}
                data-review-mark={card.task.review_mark}
              >
                {#if card.task.review_mark === 'new'}
                  <span class="plan-card__mark" data-mark="new">{plans.reviewMarkNew}</span>
                {:else if card.task.review_mark === 'changed'}
                  <span class="plan-card__mark" data-mark="changed">{plans.reviewMarkChanged}</span>
                {/if}
                <div class="plan-card__head">
                  <span class="plan-card__title">{card.task.title}</span>
                  {#if card.task.role !== null}
                    <span class="plan-card__role">{card.task.role}</span>
                  {/if}
                </div>
                <div class="plan-card__assignment">
                  <span>{describeAssignment(card.task.resolved.assignment)}</span>
                  <span class="plan-card__source" data-source={card.task.resolved.source}
                    >{plansLabel(assignmentSourceKey(card.task.resolved.source))}</span
                  >
                </div>
                {#if card.task.writes.length > 0}
                  <div class="plan-card__writes">{card.task.writes.join(', ')}</div>
                {/if}
                {#if card.task.depends_on.length > 0}
                  <div class="plan-card__depends">← {card.task.depends_on.join(', ')}</div>
                {/if}
                {#if card.window.kind === 'live'}
                  <div class="plan-card__window">
                    {card.window.name} · {describeStatus(card.window.detected.status)}
                  </div>
                {:else if card.task.window_id !== null}
                  <div class="plan-card__window plan-board__muted">—</div>
                {/if}
                {#if card.task.report !== null}
                  <div class="plan-card__report" data-outcome={card.task.report.outcome}>
                    <span class="plan-card__outcome"
                      >{plansLabel(reportOutcomeKey(card.task.report.outcome))}</span
                    >
                    <p class="plan-card__summary">{card.task.report.summary}</p>
                    {#if card.task.report.path !== null}
                      <div class="plan-card__path">
                        {plans.reportPathLabel}: {card.task.report.path}
                      </div>
                    {/if}
                  </div>
                {/if}
                <div class="plan-card__editor">
                  <LaunchProfileFields
                    {profileOptions}
                    profileValue={fields.profile}
                    onProfileChange={(value: string) => editDraft(card.task, 'profile', value)}
                    modelValue={fields.model}
                    onModelChange={(value: string) => editDraft(card.task, 'model', value)}
                    effortValue={fields.effort}
                    onEffortChange={(value: string) => editDraft(card.task, 'effort', value)}
                    customOptionLabel={plans.sourceUnassigned}
                  />
                  <div class="plan-card__actions">
                    <IconButton
                      class="btn btn-primary plan-card__icon-button"
                      label={plans.applyAssignmentButton}
                      disabled={fields.profile === ''}
                      on:click={() => {
                        onSetTaskAssignment(card.task.task_id, assignmentOf(fields));
                        forgetDraft(card.task.task_id);
                      }}
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                        <path d="M20 6 9 17l-5-5" />
                      </svg>
                    </IconButton>
                    <IconButton
                      class="btn btn-secondary plan-card__icon-button"
                      label={plans.clearAssignmentButton}
                      disabled={card.task.user === null}
                      on:click={() => {
                        onClearTaskAssignment(card.task.task_id);
                        forgetDraft(card.task.task_id);
                      }}
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                        <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
                        <path d="M3 3v5h5" />
                      </svg>
                    </IconButton>
                    <IconButton
                      class="btn btn-secondary plan-card__icon-button"
                      label={plans.discardAssignmentButton}
                      disabled={!unappliedTaskIds(plan, drafts).includes(card.task.task_id)}
                      on:click={() => forgetDraft(card.task.task_id)}
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                        <path d="M18 6 6 18" />
                        <path d="m6 6 12 12" />
                      </svg>
                    </IconButton>
                  </div>
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {/if}
</div>

<style>
  .plan-board {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    min-height: 0;
    flex: 1 1 auto;
  }

  .plan-board__picker {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
  }

  .plan-board__select {
    flex: 1 1 auto;
    min-width: 0;
  }

  .plan-board__banner {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-sm);
  }

  .plan-board__review {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    border-left: 3px solid var(--color-border);
    background-color: var(--color-surface);
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    font-size: var(--font-size-sm);
  }

  .plan-board__review[data-tone='pending'] {
    border-left-color: var(--color-warning);
  }

  .plan-board__review[data-tone='approved'] {
    border-left-color: var(--color-success);
  }

  .plan-board__review[data-tone='changed'] {
    border-left-color: var(--color-primary);
  }

  .plan-board__review-title {
    margin: 0;
    font-weight: 600;
    color: var(--color-text-primary);
  }

  .plan-board__review-detail {
    margin: 0;
    color: var(--color-text-secondary);
  }

  .plan-board__review-hint {
    margin: 0;
    color: var(--color-warning-action);
  }

  .plan-board__owner {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
    align-items: center;
    font-size: var(--font-size-sm);
  }

  .plan-board__owner-label {
    color: var(--color-text-secondary);
  }

  .plan-board__candidate {
    padding: 2px var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    color: var(--color-text-primary);
    cursor: pointer;
  }

  .plan-board__candidate[aria-pressed='true'] {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }

  .plan-board__refused {
    margin: 0;
    color: var(--color-error);
    font-size: var(--font-size-sm);
  }

  .plan-board__muted {
    color: var(--color-text-secondary);
  }

  .plan-board__columns {
    flex: 1 1 auto;
    display: flex;
    gap: var(--space-sm);
    overflow: auto;
    min-height: 0;
  }

  .plan-board__column {
    flex: 0 0 240px;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .plan-board__wave {
    margin: 0;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .plan-board__cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .plan-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    font-size: var(--font-size-sm);
  }

  .plan-card[data-unassigned='true'] {
    border-left: 3px solid var(--color-warning);
  }

  .plan-card[data-review-mark] {
    border-left: 3px solid var(--color-warning-action);
  }

  .plan-card__mark {
    align-self: flex-start;
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-warning-action);
    color: var(--color-on-warning-action);
    font-size: var(--font-size-xs);
    font-weight: 600;
  }

  .plan-card__head {
    display: flex;
    gap: var(--space-xs);
    align-items: baseline;
    justify-content: space-between;
  }

  .plan-card__title {
    font-weight: 600;
    color: var(--color-text-primary);
  }

  .plan-card__role,
  .plan-card__source {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .plan-card__assignment {
    display: flex;
    gap: var(--space-xs);
    justify-content: space-between;
  }

  .plan-card__writes,
  .plan-card__depends {
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }

  .plan-card__report {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-left: var(--space-xs);
    border-left: 3px solid var(--color-border);
  }

  .plan-card__report[data-outcome='succeeded'] {
    border-left-color: var(--color-success);
  }

  .plan-card__report[data-outcome='failed'] {
    border-left-color: var(--color-error);
  }

  .plan-card__outcome {
    font-size: var(--font-size-xs);
    font-weight: 600;
    color: var(--color-text-secondary);
  }

  .plan-card__summary {
    margin: 0;
    color: var(--color-text-primary);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .plan-card__path {
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }

  .plan-card__editor {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .plan-card__actions {
    display: flex;
    gap: var(--space-xs);
  }

  /* Icon-only so the three actions always fit one row and a card keeps
     the same height whether or not it holds an unapplied edit. */
  :global(.btn.plan-card__icon-button) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-xs) var(--space-sm);
  }

  :global(.btn.plan-card__icon-button) svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
