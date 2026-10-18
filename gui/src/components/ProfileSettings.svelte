<script lang="ts">
  /**
   * The Profiles tab of the Settings panel: renders the rows
   * `profileSettingsRows` derives and reports a switch press. No IPC, no
   * decision of its own -- the shell owns the save state, the failure line
   * and every call.
   */
  import type { ProfileOption } from './add-window-dialog-types';
  import type { RoleEntry } from '../ipc/plan-boundary';
  import ProfileDetailView from './ProfileDetailView.svelte';
  import SettingsSwitch from './SettingsSwitch.svelte';
  import type { ProfileDetailActions, ProfileDetailsState } from '../settings/profile-detail-model';
  import { profileSettingsRows } from '../settings/profile-settings-model';
  import { settings } from '../ui-messages';

  export let profileOptions: readonly ProfileOption[];
  export let roles: readonly RoleEntry[];
  export let isReadOnly: boolean;
  export let savingProfileId: string | null;
  export let updateFailure: string | null;
  export let onSetEnabled: (id: string, enabled: boolean) => void;
  export let details: ProfileDetailsState;
  export let detailActions: ProfileDetailActions;

  // The shell opens a row's details to point at it, so that row is brought into view.
  function revealWhenOpen(node: HTMLElement, isOpen: boolean): { update: (next: boolean) => void } {
    const reveal = (open: boolean): void => {
      if (open) {
        node.scrollIntoView({ block: 'nearest' });
      }
    };
    reveal(isOpen);
    return { update: reveal };
  }
</script>

<div class="profile-settings">
  <p class="profile-settings__description">{settings.profilesDescription}</p>
  {#if isReadOnly}
    <p class="profile-settings__read-only">{settings.profileSettingsReadOnly}</p>
  {/if}
  {#if updateFailure}
    <p class="profile-settings__failure">{updateFailure}</p>
  {/if}
  <div class="profile-settings__list">
    {#each profileSettingsRows(profileOptions, roles) as row (row.id)}
      <div
        class="profile-settings__row"
        class:profile-settings__row--off={!row.enabled}
        use:revealWhenOpen={details.openId === row.id}
      >
        <div class="profile-settings__name">
          <b>{row.displayName}</b>
          <span class="profile-settings__id">{row.id}</span>
          {#if !row.enabled}
            <span class="profile-settings__badge">{settings.profileDisabled}</span>
          {/if}
        </div>
        <div class="profile-settings__switch-slot">
          {#if details.isAvailable}
            <button
              type="button"
              class="profile-settings__details"
              aria-expanded={details.openId === row.id}
              on:click={() => detailActions.onToggle(row.id)}
              >{details.openId === row.id
                ? settings.profileHideDetails
                : settings.profileShowDetails}</button
            >
          {/if}
          <SettingsSwitch
            checked={row.enabled}
            label={settings.profileSwitchLabel(row.displayName)}
            text={row.enabled ? settings.profileEnabled : settings.profileDisabled}
            disabled={isReadOnly || savingProfileId === row.id}
            onToggle={() => onSetEnabled(row.id, !row.enabled)}
          />
        </div>
        <div class="profile-settings__command">{row.commandLine}</div>
        <div class="profile-settings__status">
          {#if row.isReady}
            <span class="profile-settings__status-ok">{settings.profileReady}</span>
          {:else}
            <span class="profile-settings__status-missing">{row.unavailableLine}</span>
          {/if}
        </div>
        {#if row.guidance !== null}
          {@const guidance = row.guidance}
          <div class="profile-settings__guidance">
            <p class="profile-settings__guidance-text">{guidance.text}</p>
            {#if guidance.command !== null}
              {@const command = guidance.command}
              <div class="profile-settings__copyable">
                <code class="profile-settings__copy-value">{command}</code>
                <button
                  type="button"
                  class="btn btn-secondary profile-settings__copy"
                  on:click={() => detailActions.onCopyText(command)}>{settings.profileCopy}</button
                >
              </div>
            {/if}
            {#if guidance.url !== null}
              {@const url = guidance.url}
              <span class="profile-settings__guide-label">{settings.profileInstallGuide}</span>
              <div class="profile-settings__copyable">
                <code class="profile-settings__copy-value">{url}</code>
                <button
                  type="button"
                  class="btn btn-secondary profile-settings__copy"
                  on:click={() => detailActions.onCopyText(url)}>{settings.profileCopy}</button
                >
              </div>
            {/if}
          </div>
        {/if}
        {#if row.rolesUsing.length > 0}
          <p class="profile-settings__roles">{settings.profileUsedByRoles(row.rolesUsing)}</p>
        {/if}
        {#if details.openId === row.id}
          <ProfileDetailView
            detail={details.detail}
            failure={details.failure}
            platformName={details.platformName}
            isSavingCommand={details.isSavingCommand}
            commandFailure={details.commandFailure}
            onSaveCommand={(argv: string[]) => detailActions.onSaveCommand(row.id, argv)}
            onResetCommand={() => detailActions.onResetCommand(row.id)}
            onOpenModels={() => detailActions.onOpenModels(row.id)}
          />
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .profile-settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    overflow-y: auto;
  }

  .profile-settings__description {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .profile-settings__read-only,
  .profile-settings__failure {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }

  .profile-settings__list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }

  .profile-settings__row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-xs) var(--space-sm);
    padding: var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    align-items: center;
  }

  .profile-settings__row--off .profile-settings__name b,
  .profile-settings__row--off .profile-settings__command {
    color: var(--color-text-secondary);
    opacity: 0.6;
  }

  .profile-settings__row:last-child {
    border-bottom: none;
  }

  .profile-settings__name {
    display: flex;
    align-items: baseline;
    gap: var(--space-xs);
    flex-wrap: wrap;
    min-width: 0;
  }

  .profile-settings__id {
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .profile-settings__badge {
    font-size: var(--font-size-xs);
    font-weight: 600;
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-border);
    color: var(--color-text-secondary);
  }

  .profile-settings__switch-slot {
    grid-column: 2;
    grid-row: 1 / span 3;
    display: flex;
    gap: var(--space-sm);
    align-items: center;
  }

  .profile-settings__details {
    font: inherit;
    font-size: var(--font-size-sm);
    background: none;
    border: none;
    padding: 0;
    color: var(--color-primary);
    cursor: pointer;
  }

  .profile-settings__command {
    grid-column: 1;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-sm);
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }

  .profile-settings__status {
    grid-column: 1;
    font-size: var(--font-size-sm);
  }

  .profile-settings__status-ok {
    color: var(--color-success);
  }

  .profile-settings__status-missing {
    color: var(--color-warning);
  }

  .profile-settings__guidance {
    grid-column: 1 / -1;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    font-size: var(--font-size-sm);
  }

  .profile-settings__guidance-text {
    margin: 0;
    overflow-wrap: anywhere;
  }

  .profile-settings__guide-label {
    color: var(--color-text-secondary);
  }

  .profile-settings__copyable {
    display: flex;
    gap: var(--space-sm);
    align-items: stretch;
  }

  /* 44px is the smallest comfortable pointer target. */
  .profile-settings__copy-value {
    flex: 1;
    min-width: 0;
    min-height: 44px;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    padding: 0 var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    font-family: var(--font-family-mono);
    overflow-wrap: anywhere;
    user-select: text;
  }

  .profile-settings__copy {
    flex-shrink: 0;
    min-height: 44px;
    white-space: nowrap;
  }

  .profile-settings__roles {
    grid-column: 1 / -1;
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }
</style>
