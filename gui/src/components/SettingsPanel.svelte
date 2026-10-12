<script lang="ts">
  /**
   * The Settings panel: a tab strip over a tab body. It
   * owns no tab state, no IPC and no decision of its own -- the shell holds
   * the active tab, the save state, the failure line and every call, and
   * this only renders what it is handed.
   */
  import type { ProfileOption } from './add-window-dialog-types';
  import IconButton from './IconButton.svelte';
  import ProfileSettings from './ProfileSettings.svelte';
  import ApprovalSettings from './ApprovalSettings.svelte';
  import IntegrationSettings from './IntegrationSettings.svelte';
  import KeyboardSettings from './KeyboardSettings.svelte';
  import { closeOnEscape } from './panel-focus';
  import type { RoleEntry } from '../ipc/plan-boundary';
  import type { ApprovalSettingsInfo } from '../ipc/session-client';
  import type {
    McpRegistrationAction,
    McpRegistrationResultPayload,
    McpRegistrationsListed,
    McpRegistrationStatusInfo,
  } from '../ipc/mcp-registration-boundary';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import type { Keys } from '../keymap/keymap-model';
  import type { KeymapSaveFailureKind } from '../keymap/keymap-store';
  import type { KeymapLoadStatus } from '../settings/keyboard-settings-model';
  import type { ProfileDetailActions, ProfileDetailsState } from '../settings/profile-detail-model';
  import { SETTINGS_TABS, type SettingsTab } from '../settings/settings-tabs';
  import { observeSize } from './observe-size';
  import { PANEL_MIN_SIZE, type PanelSize } from '../settings/panel-size';
  import { buttons, settings } from '../ui-messages';

  export let activeTab: SettingsTab;
  export let onSelectTab: (tab: SettingsTab) => void;
  export let onClose: () => void;
  export let profileOptions: readonly ProfileOption[];
  export let roles: readonly RoleEntry[];
  export let isReadOnly: boolean;
  export let savingProfileId: string | null;
  export let updateFailure: string | null;
  export let onSetProfileEnabled: (id: string, enabled: boolean) => void;
  export let profileDetails: ProfileDetailsState;
  export let profileDetailActions: ProfileDetailActions;
  export let approvalSettings: ApprovalSettingsInfo | null;
  export let approvalNotice: string | null;
  export let isApprovalReadOnly: boolean;
  export let isApprovalSaving: boolean;
  export let approvalUpdateFailure: string | null;
  export let onSetAutoApprove: (tool: string, autoApprove: boolean) => void;
  export let onSetPlanApprovalRequired: (required: boolean) => void;
  export let integrationsListed: McpRegistrationsListed | null;
  export let integrationStatuses: ReadonlyMap<string, McpRegistrationStatusInfo>;
  export let integrationRunning: string | null;
  export let integrationResults: ReadonlyMap<string, McpRegistrationResultPayload>;
  export let integrationsNotice: string | null;
  export let integrationsFailure: string | null;
  export let onRunMcpRegistration: (profile: string, action: McpRegistrationAction) => void;
  export let keys: Keys;
  export let platform: KeyboardPlatform;
  export let loadStatus: KeymapLoadStatus;
  export let saveFailure: KeymapSaveFailureKind | null;
  export let keyFilePath: string | null;
  export let onBeginRecording: (onResult: (chord: string | null) => void) => void;
  export let onCancelRecording: () => void;
  export let onCommit: (edit: (keys: Keys) => Keys) => Promise<boolean>;
  export let size: PanelSize;
  export let maxSize: PanelSize;
  export let onResize: (size: PanelSize) => void;

  /** The tab strip's display text, keyed by tab id. */
  const TAB_LABELS: Record<SettingsTab, string> = {
    profiles: settings.profilesTab,
    approvals: settings.approvalsTab,
    integrations: settings.integrationsTab,
    keyboard: settings.keyboardTab,
  };
</script>

<section
  class="settings-panel"
  use:closeOnEscape={onClose}
  aria-label={settings.panelTitle}
  use:observeSize={{ size, onResize }}
  style:min-width="min({PANEL_MIN_SIZE.width}px, 100vw)"
  style:min-height="min({PANEL_MIN_SIZE.height}px, 100vh)"
  style:max-width="{maxSize.width}px"
  style:max-height="{maxSize.height}px"
>
  <header class="settings-panel__header">
    <div class="settings-panel__tabs">
      {#each SETTINGS_TABS as tab (tab)}
        <button
          type="button"
          class="settings-panel__tab"
          aria-pressed={activeTab === tab}
          on:click={() => onSelectTab(tab)}>{TAB_LABELS[tab]}</button
        >
      {/each}
    </div>
    <IconButton class="settings-panel__close" label={buttons.close} on:click={onClose}>✕</IconButton
    >
  </header>

  <div class="settings-panel__body">
    {#if activeTab === 'profiles'}
      <ProfileSettings
        {profileOptions}
        {roles}
        {isReadOnly}
        {savingProfileId}
        {updateFailure}
        onSetEnabled={onSetProfileEnabled}
        details={profileDetails}
        detailActions={profileDetailActions}
      />
    {/if}
    {#if activeTab === 'approvals'}
      <ApprovalSettings
        info={approvalSettings}
        notice={approvalNotice}
        isReadOnly={isApprovalReadOnly}
        isSaving={isApprovalSaving}
        updateFailure={approvalUpdateFailure}
        {onSetAutoApprove}
        {onSetPlanApprovalRequired}
      />
    {/if}
    {#if activeTab === 'integrations'}
      <IntegrationSettings
        listed={integrationsListed}
        statuses={integrationStatuses}
        running={integrationRunning}
        results={integrationResults}
        notice={integrationsNotice}
        failure={integrationsFailure}
        onRun={onRunMcpRegistration}
      />
    {/if}
    {#if activeTab === 'keyboard'}
      <KeyboardSettings
        {keys}
        {platform}
        {loadStatus}
        {saveFailure}
        {keyFilePath}
        {onBeginRecording}
        {onCancelRecording}
        {onCommit}
      />
    {/if}
  </div>
</section>

<style>
  .settings-panel {
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

  .settings-panel__header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
  }

  .settings-panel__tabs {
    display: flex;
    gap: var(--space-xs);
  }

  .settings-panel__tab {
    padding: var(--space-xs) var(--space-sm);
    border: none;
    border-bottom: 2px solid transparent;
    background: none;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
    cursor: pointer;
  }

  .settings-panel__tab[aria-pressed='true'] {
    border-bottom-color: var(--color-primary);
    color: var(--color-text-primary);
  }

  :global(.settings-panel__close) {
    border: none;
    background: none;
    color: var(--color-text-secondary);
    cursor: pointer;
  }

  .settings-panel__body {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: var(--space-sm);
    overflow-y: auto;
  }
</style>
