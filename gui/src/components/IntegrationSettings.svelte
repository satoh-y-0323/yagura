<script lang="ts">
  /**
   * The Integrations tab of the Settings panel: renders the rows
   * `integrationGroups` derives and reports a button press. No IPC, no
   * decision of its own -- the shell owns the list, the per-row status, the
   * operation in flight, its result and every call.
   */
  import type {
    McpRegistrationAction,
    McpRegistrationResultPayload,
    McpRegistrationsListed,
    McpRegistrationStatusInfo,
  } from '../ipc/mcp-registration-boundary';
  import { integrationGroups, resultLines } from '../settings/integration-settings-model';
  import { settings } from '../ui-messages';

  export let listed: McpRegistrationsListed | null;
  export let statuses: ReadonlyMap<string, McpRegistrationStatusInfo>;
  export let running: string | null;
  export let results: ReadonlyMap<string, McpRegistrationResultPayload>;
  export let notice: string | null;
  export let failure: string | null;
  export let onRun: (profile: string, action: McpRegistrationAction) => void;

  /** The primary button's text, keyed by the action it starts. */
  const PRIMARY_LABELS: Record<'register' | 'update', string> = {
    register: settings.integrationRegisterButton,
    update: settings.integrationUpdateButton,
  };
</script>

<div class="integration-settings">
  <p class="integration-settings__description">{settings.integrationsDescription}</p>
  {#if notice}
    <p class="integration-settings__notice">{notice}</p>
  {/if}
  {#if failure}
    <p class="integration-settings__failure">{failure}</p>
  {/if}
  {#if listed}
    {#each integrationGroups(listed, statuses, running) as group (group.heading)}
      <div class="integration-settings__group-head">{group.heading}</div>
      <div class="integration-settings__list">
        {#each group.rows as row (row.profile)}
          <div class="integration-settings__row">
            <div class="integration-settings__name">
              <span>{row.name}</span>
              <span
                class="integration-settings__state integration-settings__state--{row.stateTone}"
              >
                <span class="integration-settings__dot" aria-hidden="true"></span>
                {row.stateLabel}
              </span>
            </div>
            <div class="integration-settings__command" title={row.commandTitle}>{row.command}</div>
            {#if row.tomlSetNote}
              <div class="integration-settings__extra">{row.tomlSetNote}</div>
            {/if}
            {#if row.hookEntriesNote}
              <div class="integration-settings__extra" title={row.hookEntriesTitle}>
                {row.hookEntriesNote}
              </div>
            {/if}
            {#if row.reason}
              <div class="integration-settings__extra">{row.reason}</div>
            {/if}
            <div class="integration-settings__actions">
              {#if row.primaryAction}
                <button
                  type="button"
                  class="btn btn-primary integration-settings__button"
                  disabled={row.isPrimaryDisabled}
                  on:click={() => {
                    // A closure loses the narrowing the enclosing if block gave this property.
                    if (row.primaryAction !== null) onRun(row.profile, row.primaryAction);
                  }}>{PRIMARY_LABELS[row.primaryAction]}</button
                >
              {/if}
              {#if row.canRemove}
                <button
                  type="button"
                  class="btn btn-secondary integration-settings__button"
                  disabled={row.isRemoveDisabled}
                  on:click={() => onRun(row.profile, 'remove')}
                  >{settings.integrationRemoveButton}</button
                >
              {/if}
            </div>
            {#if results.has(row.profile)}
              {@const outcome = resultLines(
                results.get(row.profile) as McpRegistrationResultPayload
              )}
              <pre
                class="integration-settings__result"
                class:integration-settings__result--failure={outcome.isFailure}>{outcome.text}</pre>
            {/if}
          </div>
        {/each}
      </div>
    {/each}
    {#if listed.bridge}
      <p class="integration-settings__footer">{settings.integrationsFooter(listed.bridge)}</p>
    {/if}
  {/if}
</div>

<style>
  .integration-settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    overflow-y: auto;
  }

  .integration-settings__description {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .integration-settings__notice,
  .integration-settings__failure {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }

  .integration-settings__group-head {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .integration-settings__list {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }

  .integration-settings__row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-xs) var(--space-sm);
    padding: var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    align-items: center;
  }

  .integration-settings__row:last-child {
    border-bottom: none;
  }

  .integration-settings__name {
    display: flex;
    align-items: baseline;
    gap: var(--space-sm);
    flex-wrap: wrap;
    min-width: 0;
  }

  .integration-settings__state {
    display: inline-flex;
    align-items: center;
    gap: var(--space-xs);
    font-size: var(--font-size-sm);
  }

  .integration-settings__dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background-color: var(--color-text-secondary);
  }

  .integration-settings__state--ok .integration-settings__dot {
    background-color: var(--color-success);
  }

  .integration-settings__state--warn {
    color: var(--color-warning);
  }

  .integration-settings__state--warn .integration-settings__dot {
    background-color: var(--color-warning);
  }

  .integration-settings__command {
    grid-column: 1;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .integration-settings__extra {
    grid-column: 1;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .integration-settings__actions {
    grid-column: 2;
    grid-row: 1 / span 3;
    display: flex;
    gap: var(--space-xs);
    align-items: center;
  }

  .integration-settings__button {
    white-space: nowrap;
  }

  .integration-settings__result {
    grid-column: 1 / -1;
    margin: var(--space-xs) 0 0;
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    white-space: pre-wrap;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .integration-settings__result--failure {
    border-color: var(--color-error);
    color: var(--color-error);
  }

  .integration-settings__footer {
    margin: 0;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }
</style>
