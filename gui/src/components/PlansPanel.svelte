<script lang="ts">
  /**
   * The plans panel: a floating panel with a Board, a Roles and a Models tab.
   *
   * It owns no IPC and no store: the shell holds the plan ledger state and
   * hands it in, and every change goes back out through its callbacks.
   */
  import type { ComponentProps } from 'svelte';

  import IconButton from './IconButton.svelte';
  import ModelAliasesEditor from './ModelAliasesEditor.svelte';
  import PlanBoard from './PlanBoard.svelte';
  import RolesEditor from './RolesEditor.svelte';
  import { closeOnEscape } from './panel-focus';
  import { observeSize } from './observe-size';
  import { PANEL_MIN_SIZE, type PanelSize } from '../settings/panel-size';
  import { buttons, nav, plans } from '../ui-messages';

  type BoardProps = ComponentProps<typeof PlanBoard>;
  type EditorProps = ComponentProps<typeof RolesEditor>;
  type ModelEditorProps = ComponentProps<typeof ModelAliasesEditor>;

  export let planList: BoardProps['planList'];
  export let selectedPlanId: BoardProps['selectedPlanId'];
  export let plan: BoardProps['plan'];
  export let roles: EditorProps['roles'];
  export let defaultRole: EditorProps['defaultRole'];
  export let windows: BoardProps['windows'];
  export let onClose: () => void;
  export let onSelectPlan: BoardProps['onSelectPlan'];
  export let onDeletePlan: BoardProps['onDeletePlan'];
  export let onSetTaskAssignment: BoardProps['onSetTaskAssignment'];
  export let onClearTaskAssignment: BoardProps['onClearTaskAssignment'];
  export let onSetPlanOwner: BoardProps['onSetPlanOwner'];
  export let onSaveRoles: EditorProps['onSave'];
  export let onApprovePlan: BoardProps['onApprovePlan'];
  /** Notified when the Board tab is selected, one of the three "seen" triggers. */
  export let onBoardTabSelected: () => void;
  export let profileOptions: BoardProps['profileOptions'];
  export let shippedModels: ModelEditorProps['shipped'];
  export let modelChoices: ModelEditorProps['choices'];
  export let isModelAliasesReadOnly: ModelEditorProps['readOnly'];
  export let onSelectModelProfile: ModelEditorProps['onSelectProfile'];
  export let onRefreshModelChoices: ModelEditorProps['onRefreshChoices'];
  export let onSaveModelAliases: ModelEditorProps['onSave'];
  export let requestedModelProfile: { id: string; seq: number } | null = null;
  export let size: PanelSize;
  export let maxSize: PanelSize;
  export let onResize: (size: PanelSize) => void;

  let activeTab: 'board' | 'roles' | 'models' = 'board';
  let switchedForSeq = 0;
  $: if (requestedModelProfile !== null && requestedModelProfile.seq !== switchedForSeq) {
    switchedForSeq = requestedModelProfile.seq;
    activeTab = 'models';
  }
</script>

<section
  class="plans-panel"
  use:closeOnEscape={onClose}
  aria-label={nav.plans}
  use:observeSize={{ size, onResize }}
  style:min-width="min({PANEL_MIN_SIZE.width}px, 100vw)"
  style:min-height="min({PANEL_MIN_SIZE.height}px, 100vh)"
  style:max-width="{maxSize.width}px"
  style:max-height="{maxSize.height}px"
>
  <header class="plans-panel__header">
    <div class="plans-panel__tabs">
      <button
        type="button"
        class="plans-panel__tab"
        aria-pressed={activeTab === 'board'}
        on:click={() => {
          activeTab = 'board';
          onBoardTabSelected();
        }}>{plans.boardTab}</button
      >
      <button
        type="button"
        class="plans-panel__tab"
        aria-pressed={activeTab === 'roles'}
        on:click={() => (activeTab = 'roles')}>{plans.rolesTab}</button
      >
      <button
        type="button"
        class="plans-panel__tab"
        aria-pressed={activeTab === 'models'}
        on:click={() => (activeTab = 'models')}>{plans.modelsTab}</button
      >
    </div>
    <IconButton class="plans-panel__close" label={buttons.close} on:click={onClose}>✕</IconButton>
  </header>

  <div class="plans-panel__body">
    {#if activeTab === 'board'}
      <PlanBoard
        {planList}
        {selectedPlanId}
        {plan}
        {windows}
        {onSelectPlan}
        {onDeletePlan}
        {onSetTaskAssignment}
        {onClearTaskAssignment}
        {onSetPlanOwner}
        {onApprovePlan}
        {profileOptions}
      />
    {:else if activeTab === 'roles'}
      <RolesEditor {roles} {defaultRole} onSave={onSaveRoles} {profileOptions} />
    {:else if activeTab === 'models'}
      <ModelAliasesEditor
        {profileOptions}
        shipped={shippedModels}
        choices={modelChoices}
        readOnly={isModelAliasesReadOnly}
        onSelectProfile={onSelectModelProfile}
        onRefreshChoices={onRefreshModelChoices}
        onSave={onSaveModelAliases}
        requestedProfile={requestedModelProfile}
      />
    {/if}
  </div>
</section>

<style>
  .plans-panel {
    position: fixed;
    z-index: 20;
    top: 48px;
    left: 64px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    resize: both;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    box-shadow: var(--shadow-md);
  }

  .plans-panel__header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
  }

  .plans-panel__tabs {
    display: flex;
    gap: var(--space-xs);
  }

  .plans-panel__tab {
    padding: var(--space-xs) var(--space-sm);
    border: none;
    border-bottom: 2px solid transparent;
    background: none;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
    cursor: pointer;
  }

  .plans-panel__tab[aria-pressed='true'] {
    border-bottom-color: var(--color-primary);
    color: var(--color-text-primary);
  }

  :global(.plans-panel__close) {
    border: none;
    background: none;
    color: var(--color-text-secondary);
    cursor: pointer;
  }

  .plans-panel__body {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: var(--space-sm);
    overflow: hidden;
  }
</style>
