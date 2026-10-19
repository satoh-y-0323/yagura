<script lang="ts">
  /**
   * The Models tab of the plans panel: the model each alias of a launch
   * profile stands for, as editable rows. The shell holds the model lists and
   * every request; nothing reaches the daemon until the rows are saved
   * through the callback the shell supplies.
   */
  import type { ProfileOption } from './add-window-dialog-types';
  import type {
    LaunchOptionAlias,
    ModelChoice,
    ModelChoiceSource,
    ShippedModelProfile,
    ShippedModelProfiles,
  } from '../ipc/model-boundary';
  import {
    aliasesToSave,
    buildAliasRows,
    choicesFailureLine,
    editableProfiles,
    isDirty,
    resetToShipped,
    type AliasRow,
    type ChoicesFailure,
    type ModelChoicesEntry,
  } from '../profile/model-aliases-model';
  import { plans } from '../ui-messages';
  import IconButton from './IconButton.svelte';

  /** The launch profiles the daemon offers, as the shell last read them. */
  export let profileOptions: ProfileOption[];
  /** The aliases of the profiles shipped with the application, keyed by profile id. */
  export let shipped: ShippedModelProfiles;
  /** Each profile's model list, keyed by profile id. */
  export let choices: ReadonlyMap<string, ModelChoicesEntry>;
  /** Whether the connected daemon is too old to take model-alias requests. */
  export let readOnly: boolean;
  export let onSelectProfile: (profile: string) => void;
  export let onRefreshChoices: (profile: string) => void;
  export let onSave: (profile: string, aliases: LaunchOptionAlias[]) => void;

  /**
   * A profile another panel asked to show; a new `seq` shows it again even
   * when the id is the same as last time.
   */
  export let requestedProfile: { id: string; seq: number } | null = null;

  let selectedId = '';
  let shownRequestSeq = 0;
  let rows: AliasRow[] = [];
  /** Aliases whose model is typed by hand after choosing Other…. */
  let typedAliases: string[] = [];

  $: editable = editableProfiles(profileOptions);
  $: if (!editable.some((profile) => profile.id === selectedId)) {
    const first = editable[0];
    if (first !== undefined) {
      showProfile(first.id);
      onSelectProfile(first.id);
    }
  }
  $: if (
    requestedProfile !== null &&
    requestedProfile.seq !== shownRequestSeq &&
    editable.some((profile) => profile.id === requestedProfile?.id)
  ) {
    shownRequestSeq = requestedProfile.seq;
    showProfile(requestedProfile.id);
    onSelectProfile(requestedProfile.id);
  }
  $: savedAliases = savedAliasesOf(profileOptions, selectedId);
  $: shippedProfile = shippedProfileOf(shipped, selectedId);
  let entry: ModelChoicesEntry | undefined;
  $: entry = choices.get(selectedId);
  let listedChoices: ModelChoice[] = [];
  $: listedChoices = entry?.kind === 'answered' ? entry.choices : [];
  let listedSource: ModelChoiceSource | null = null;
  $: listedSource = entry?.kind === 'answered' ? entry.source : null;
  let fetchedAtMs = 0;
  $: fetchedAtMs = entry?.kind === 'answered' ? entry.fetchedAtMs : 0;
  $: failure = failureOf(entry);
  // A reloaded profile list or an arriving model list refreshes the rows,
  // keeping the values of an unsaved edit rather than the saved ones.
  $: rows = buildAliasRows(
    isDirty(rows, savedAliases) ? aliasesToSave(rows) : savedAliases,
    shippedProfile?.aliases ?? [],
    listedChoices
  );
  $: dirty = isDirty(rows, savedAliases);

  function savedAliasesOf(profiles: ProfileOption[], id: string): LaunchOptionAlias[] {
    return profiles.find((profile) => profile.id === id)?.model?.aliases ?? [];
  }

  // Profile ids come from configuration, so the lookup must not reach the
  // prototype of the map (an id such as `constructor`).
  function shippedProfileOf(all: ShippedModelProfiles, id: string): ShippedModelProfile | null {
    return Object.prototype.hasOwnProperty.call(all, id) ? (all[id] ?? null) : null;
  }

  function failureOf(current: ModelChoicesEntry | undefined): ChoicesFailure | null {
    if (current?.kind === 'failed') {
      return current.failure;
    }
    if (current?.kind === 'answered' && current.error !== null) {
      return { kind: 'reported', reason: current.error };
    }
    return null;
  }

  function freshRows(id: string): AliasRow[] {
    const current = choices.get(id);
    return buildAliasRows(
      savedAliasesOf(profileOptions, id),
      shippedProfileOf(shipped, id)?.aliases ?? [],
      current?.kind === 'answered' ? current.choices : []
    );
  }

  function showProfile(id: string): void {
    selectedId = id;
    rows = freshRows(id);
    typedAliases = [];
  }

  function editValue(alias: string, value: string): void {
    rows = rows.map((row) => (row.alias === alias ? { ...row, value } : row));
  }

  function chooseValue(alias: string, value: string): void {
    if (value === '') {
      typedAliases = [...typedAliases, alias];
      return;
    }
    editValue(alias, value);
  }

  function resetRow(alias: string): void {
    rows = resetToShipped(rows, alias, listedChoices);
    typedAliases = typedAliases.filter((typed) => typed !== alias);
  }

  function save(): void {
    onSave(selectedId, aliasesToSave(rows));
  }

  function discard(): void {
    rows = freshRows(selectedId);
    typedAliases = [];
  }

  function fetchedTime(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<div class="model-aliases">
  <label class="model-aliases__picker">
    <span class="model-aliases__picker-label">{plans.modelsProfileLabel}</span>
    <select
      disabled={dirty}
      on:change={(event) => {
        showProfile(event.currentTarget.value);
        onSelectProfile(event.currentTarget.value);
      }}
    >
      {#each editableProfiles(profileOptions) as profile (profile.id)}
        <option value={profile.id} selected={profile.id === selectedId}
          >{profile.displayName}</option
        >
      {/each}
    </select>
  </label>

  {#if entry === undefined || entry.kind === 'fetching'}
    <p class="model-aliases__source">{plans.modelChoicesFetching}</p>
  {:else if failure !== null}
    <p class="model-aliases__failure">
      <span>{choicesFailureLine(failure)}</span>
      <IconButton
        class="model-aliases__icon"
        label={plans.refreshModelChoicesLabel}
        on:click={() => onRefreshChoices(selectedId)}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path d="M21 12a9 9 0 1 1-3-6.7L21 8" /><path d="M21 3v5h-5" /></svg
        >
      </IconButton>
    </p>
  {:else if listedSource?.kind === 'values'}
    <p class="model-aliases__source">{plans.modelChoicesFromProfile}</p>
  {:else if listedSource?.kind === 'command'}
    <p class="model-aliases__source">
      <span>{plans.modelChoicesFromCommand}</span>
      <code>{listedSource.argv.join(' ')}</code>
      <span>{plans.modelChoicesFetchedAt(fetchedTime(fetchedAtMs))}</span>
      <IconButton
        class="model-aliases__icon"
        label={plans.refreshModelChoicesLabel}
        on:click={() => onRefreshChoices(selectedId)}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"
          ><path d="M21 12a9 9 0 1 1-3-6.7L21 8" /><path d="M21 3v5h-5" /></svg
        >
      </IconButton>
    </p>
  {/if}

  <div class="model-aliases__table-scroll">
    <table class="model-aliases__table">
      <thead>
        <tr>
          <th>{plans.modelAliasColumn}</th>
          <th>{plans.modelValueColumn}</th>
          <th>{plans.modelShippedColumn}</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row (row.alias)}
          <tr>
            <td class="model-aliases__alias">
              {row.alias}
              {#if row.edited}
                <span class="model-aliases__edited">{plans.modelEditedMark}</span>
              {/if}
            </td>
            <td class="model-aliases__value">
              {#if listedChoices.length > 0 && !row.valueMissingFromChoices && !typedAliases.includes(row.alias)}
                <select
                  aria-label={plans.modelForAliasLabel(row.alias)}
                  disabled={readOnly}
                  on:change={(event) => chooseValue(row.alias, event.currentTarget.value)}
                >
                  {#each listedChoices as choice (choice.id)}
                    <option value={choice.id} selected={choice.id === row.value}
                      >{choice.label === null
                        ? choice.id
                        : `${choice.id} · ${choice.label}`}</option
                    >
                  {/each}
                  <option value="">{plans.modelOtherOption}</option>
                </select>
              {:else}
                <input
                  value={row.value}
                  aria-label={plans.modelForAliasLabel(row.alias)}
                  disabled={readOnly}
                  spellcheck="false"
                  on:input={(event) => editValue(row.alias, event.currentTarget.value)}
                />
              {/if}
              <IconButton
                class="model-aliases__icon"
                label={plans.resetToShippedLabel}
                disabled={!row.edited || readOnly}
                on:click={() => resetRow(row.alias)}
              >
                <svg viewBox="0 0 24 24" aria-hidden="true"
                  ><path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" /></svg
                >
              </IconButton>
            </td>
            <td class="model-aliases__shipped">{row.shippedValue ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <div class="model-aliases__actions">
    <button type="button" class="btn btn-secondary" disabled={!dirty} on:click={discard}
      >{plans.discardModelAliasesButton}</button
    >
    <button type="button" class="btn btn-primary" disabled={!dirty || readOnly} on:click={save}
      >{plans.saveModelAliasesButton}</button
    >
  </div>
</div>

<style>
  .model-aliases {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    min-height: 0;
    flex: 1 1 auto;
    overflow-y: auto;
  }

  .model-aliases__picker {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
  }

  .model-aliases__picker-label {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .model-aliases__source {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-sm);
    align-items: center;
    margin: 0;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .model-aliases__source code {
    font-family: var(--font-family-mono);
    color: var(--color-text-primary);
  }

  .model-aliases__failure {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-left: 3px solid var(--color-warning);
    border-radius: var(--radius-sm);
    font-size: var(--font-size-xs);
    color: var(--color-text-primary);
  }

  :global(.model-aliases__icon) {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--color-text-primary);
    cursor: pointer;
  }

  :global(.model-aliases__icon:disabled) {
    opacity: 0.4;
    cursor: not-allowed;
  }

  :global(.model-aliases__icon) svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .model-aliases__table-scroll {
    overflow-x: auto;
  }

  .model-aliases__table {
    width: 100%;
    min-width: 560px;
    border-collapse: collapse;
    font-size: var(--font-size-xs);
  }

  .model-aliases__table th,
  .model-aliases__table td {
    padding: var(--space-xs) var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    text-align: left;
    vertical-align: middle;
  }

  .model-aliases__table th {
    font-weight: 500;
    color: var(--color-text-secondary);
  }

  .model-aliases__alias {
    font-family: var(--font-family-mono);
    white-space: nowrap;
  }

  .model-aliases__edited {
    margin-left: var(--space-xs);
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-warning-action);
    color: var(--color-on-warning);
    font-family: var(--font-family-base);
    font-size: 10px;
    font-weight: 600;
  }

  .model-aliases__value {
    display: flex;
    gap: var(--space-xs);
    align-items: center;
  }

  .model-aliases__value select,
  .model-aliases__value input {
    flex: 1 1 200px;
    min-width: 0;
  }

  .model-aliases__shipped {
    font-family: var(--font-family-mono);
    color: var(--color-text-secondary);
    white-space: nowrap;
  }

  .model-aliases__actions {
    display: flex;
    gap: var(--space-sm);
    justify-content: flex-end;
  }
</style>
