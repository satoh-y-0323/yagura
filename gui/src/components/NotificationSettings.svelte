<script lang="ts">
  /**
   * The Notifications tab of the Settings panel: one row per notice kind with
   * an in-app switch. No storage, no decision of its own -- the shell owns
   * the preferences and what a switch does.
   */
  import SettingsSwitch from './SettingsSwitch.svelte';
  import { NOTICE_CATEGORIES, type NoticeCategory } from '../notices/notice-model';
  import type { NotificationPreferences } from '../notices/notification-preferences';
  import { settings } from '../ui-messages';

  export let preferences: NotificationPreferences;
  export let onSetPreference: (category: NoticeCategory, enabled: boolean) => void;
</script>

<div class="notification-settings">
  <p class="notification-settings__description">{settings.notificationsDescription}</p>
  <table class="notification-settings__table">
    <thead>
      <tr>
        <th scope="col">{settings.notificationsColumnName}</th>
        <th scope="col" class="notification-settings__switch-cell"
          >{settings.notificationsColumnInApp}</th
        >
      </tr>
    </thead>
    <tbody>
      {#each NOTICE_CATEGORIES as category (category)}
        {@const name = settings.notificationsRowName(category)}
        <tr>
          <th scope="row">
            <span class="notification-settings__name">{name}</span>
            <span class="notification-settings__what"
              >{settings.notificationsRowDescription(category)}</span
            >
          </th>
          <td class="notification-settings__switch-cell">
            <SettingsSwitch
              checked={preferences.inApp[category]}
              label={settings.notificationsSwitchLabel(name)}
              text={null}
              disabled={false}
              onToggle={() => onSetPreference(category, !preferences.inApp[category])}
            />
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p class="notification-settings__always">{settings.notificationsAlwaysShown}</p>
</div>

<style>
  .notification-settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    overflow-y: auto;
  }

  .notification-settings__description,
  .notification-settings__always {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .notification-settings__table {
    width: 100%;
    border-collapse: collapse;
  }

  .notification-settings__table th,
  .notification-settings__table td {
    padding: var(--space-sm) var(--space-xs);
    border-bottom: 1px solid var(--color-border);
    text-align: left;
    vertical-align: middle;
  }

  .notification-settings__table thead th {
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
    font-weight: 600;
  }

  .notification-settings__table .notification-settings__switch-cell {
    width: 86px;
    text-align: center;
  }

  .notification-settings__table td.notification-settings__switch-cell :global(.settings-switch) {
    margin-inline: auto;
  }

  .notification-settings__name {
    display: block;
    font-weight: 500;
  }

  .notification-settings__what {
    display: block;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
    font-weight: 400;
  }
</style>
