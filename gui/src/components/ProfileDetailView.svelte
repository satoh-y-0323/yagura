<script lang="ts">
  /**
   * One profile's details inside its Profiles-tab row: every value in use
   * with a Shipped / Changed chip, and the editor for this host's command.
   * No IPC: the shell reads and writes, and hands the results back here.
   */
  import type { ProfileDetail } from '../ipc/profile-detail-boundary';
  import { canSaveCommand, detailSections, isCommandDirty } from '../settings/profile-detail-model';
  import { settings } from '../ui-messages';
  import IconButton from './IconButton.svelte';

  export let detail: ProfileDetail | null;
  export let failure: string | null;
  export let platformName: string;
  export let isSavingCommand: boolean;
  export let commandFailure: string | null;
  export let onSaveCommand: (argv: string[]) => void;
  export let onResetCommand: () => void;
  export let onOpenModels: () => void;

  let edited: string[] = [];
  let savedKey = '';
  let showEnvironment = false;

  // A new saved argv (a save, a reset, a fresh read) replaces the edits.
  $: if (detail !== null && detail.command.argv.join('\u0000') !== savedKey) {
    savedKey = detail.command.argv.join('\u0000');
    edited = [...detail.command.argv];
  }
  $: sections = detail === null ? null : detailSections(detail);
  $: saved = detail?.command.argv ?? [];
  $: shippedCommand = detail?.command.shipped ?? null;

  function updateArgument(index: number, value: string): void {
    edited = edited.map((item, at) => (at === index ? value : item));
  }

  function removeArgument(index: number): void {
    edited = edited.filter((_, at) => at !== index);
  }
</script>

<div class="profile-detail">
  {#if failure !== null}
    <p class="profile-detail__failure">{settings.profileDetailFailed(failure)}</p>
  {:else if detail === null || sections === null}
    <p class="profile-detail__hint">{settings.profileDetailLoading}</p>
  {:else}
    <section class="profile-detail__section">
      <h4 class="profile-detail__heading">
        {settings.profileCommandHeading(platformName)}
        {#if detail.shippedBase}
          <span
            class="profile-detail__chip"
            class:profile-detail__chip--changed={detail.command.changed}
            >{detail.command.changed ? settings.profileChanged : settings.profileShipped}</span
          >
        {/if}
      </h4>
      <div class="profile-detail__args">
        {#each edited as argument, index (index)}
          <div class="profile-detail__arg">
            <span class="profile-detail__index">{index}</span>
            <input
              class="profile-detail__input"
              value={argument}
              aria-label={settings.profileArgumentLabel(index)}
              disabled={isSavingCommand}
              spellcheck="false"
              on:input={(event) => updateArgument(index, event.currentTarget.value)}
            />
            {#if edited.length > 1}
              <IconButton
                class="profile-detail__remove"
                label={settings.profileRemoveArgument(index)}
                disabled={isSavingCommand}
                on:click={() => removeArgument(index)}>✕</IconButton
              >
            {/if}
          </div>
        {/each}
      </div>
      <div class="profile-detail__actions">
        <button
          type="button"
          class="btn btn-secondary"
          disabled={isSavingCommand}
          on:click={() => (edited = [...edited, ''])}>{settings.profileAddArgument}</button
        >
        <button
          type="button"
          class="btn btn-primary"
          disabled={isSavingCommand || !canSaveCommand(edited, saved)}
          on:click={() => onSaveCommand([...edited])}>{settings.profileSaveCommand}</button
        >
        <button
          type="button"
          class="btn btn-secondary"
          disabled={isSavingCommand || !isCommandDirty(edited, saved)}
          on:click={() => (edited = [...saved])}>{settings.profileDiscardCommand}</button
        >
        {#if detail.command.changed && shippedCommand !== null}
          <button
            type="button"
            class="profile-detail__link"
            disabled={isSavingCommand}
            on:click={onResetCommand}
            >{settings.profileResetCommand(shippedCommand.join(' '))}</button
          >
        {/if}
      </div>
      {#if commandFailure !== null}
        <p class="profile-detail__failure">{settings.profileCommandFailed(commandFailure)}</p>
      {/if}
    </section>

    {#each [{ heading: settings.profileLaunchHeading, rows: sections.launch }, { heading: settings.profileAliasesHeading, rows: sections.aliases }] as group (group.heading)}
      {#if group.rows.length > 0}
        <section class="profile-detail__section">
          <h4 class="profile-detail__heading">
            {group.heading}
            {#if group.rows === sections.aliases}
              <button type="button" class="profile-detail__link" on:click={onOpenModels}
                >{settings.profileEditInModels}</button
              >
            {/if}
          </h4>
          <div class="profile-detail__values">
            {#each group.rows as row (row.key)}
              <span class="profile-detail__key">{row.key}</span>
              <span class="profile-detail__value"
                >{row.text}{#if row.shippedText !== null}
                  <span class="profile-detail__hint">
                    ({settings.profileShippedValue(row.shippedText)})</span
                  >{/if}</span
              >
              <span>
                {#if detail.shippedBase}
                  <span
                    class="profile-detail__chip"
                    class:profile-detail__chip--changed={row.changed}
                    >{row.changed ? settings.profileChanged : settings.profileShipped}</span
                  >
                {/if}
              </span>
            {/each}
          </div>
        </section>
      {/if}
    {/each}

    {#if sections.environment.length > 0}
      <section class="profile-detail__section">
        <h4 class="profile-detail__heading">
          {settings.profileEnvironmentHeading}
          <button
            type="button"
            class="profile-detail__link"
            on:click={() => (showEnvironment = !showEnvironment)}
            >{showEnvironment ? settings.profileHideValues : settings.profileShowValues}</button
          >
        </h4>
        <div class="profile-detail__values">
          {#each sections.environment as row (row.key)}
            <span class="profile-detail__key">{row.key}</span>
            <span class="profile-detail__value">{showEnvironment ? row.text : '••••••••'}</span>
            <span>
              {#if detail.shippedBase}
                <span class="profile-detail__chip" class:profile-detail__chip--changed={row.changed}
                  >{row.changed ? settings.profileChanged : settings.profileShipped}</span
                >
              {/if}
            </span>
          {/each}
        </div>
      </section>
    {/if}

    {#if sections.provided.length > 0}
      <section class="profile-detail__section">
        <h4 class="profile-detail__heading">{settings.profileProvidedHeading}</h4>
        <div class="profile-detail__values">
          {#each sections.provided as row (row.key)}
            <span class="profile-detail__key">{settings.profileProvidedLabel(row.key)}</span>
            <span class="profile-detail__value profile-detail__value--dim"
              >{settings.profileProvidedIncluded}</span
            >
            <span>
              {#if detail.shippedBase}
                <span class="profile-detail__chip" class:profile-detail__chip--changed={row.changed}
                  >{row.changed ? settings.profileChanged : settings.profileShipped}</span
                >
              {/if}
            </span>
          {/each}
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .profile-detail {
    grid-column: 1 / -1;
    display: grid;
    gap: var(--space-sm);
    margin-top: var(--space-xs);
    padding-top: var(--space-sm);
    border-top: 1px dashed var(--color-border);
    min-width: 0;
  }

  .profile-detail__section {
    display: grid;
    gap: var(--space-xs);
    min-width: 0;
  }

  .profile-detail__heading {
    margin: 0;
    display: flex;
    gap: var(--space-sm);
    align-items: center;
    flex-wrap: wrap;
    font-size: var(--font-size-xs);
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-text-secondary);
  }

  .profile-detail__chip {
    font-size: var(--font-size-xs);
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    color: var(--color-text-secondary);
  }

  .profile-detail__chip--changed {
    font-weight: 600;
    background-color: var(--color-border);
    color: var(--color-primary);
  }

  .profile-detail__link {
    font: inherit;
    font-size: var(--font-size-xs);
    letter-spacing: 0;
    text-transform: none;
    background: none;
    border: none;
    padding: 0;
    color: var(--color-primary);
    cursor: pointer;
  }

  .profile-detail__link:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .profile-detail__args {
    display: grid;
    gap: var(--space-xs);
  }

  .profile-detail__arg {
    display: flex;
    gap: var(--space-xs);
    align-items: center;
  }

  .profile-detail__index {
    width: 1.4rem;
    text-align: right;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    font-variant-numeric: tabular-nums;
  }

  .profile-detail__input {
    flex: 1;
    min-width: 0;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-sm);
    background-color: var(--color-surface);
    color: var(--color-text-primary);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    padding: 2px var(--space-xs);
  }

  :global(.profile-detail__remove) {
    font: inherit;
    background: none;
    border: none;
    color: var(--color-text-secondary);
    cursor: pointer;
    padding: 0 var(--space-xs);
  }

  .profile-detail__actions {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
    flex-wrap: wrap;
  }

  .profile-detail__values {
    display: grid;
    grid-template-columns: minmax(6rem, auto) minmax(0, 1fr) auto;
    gap: 2px var(--space-sm);
    align-items: baseline;
    font-size: var(--font-size-sm);
  }

  .profile-detail__key {
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .profile-detail__value {
    font-family: var(--font-family-mono);
    overflow-wrap: anywhere;
    min-width: 0;
  }

  .profile-detail__value--dim,
  .profile-detail__hint {
    color: var(--color-text-secondary);
  }

  .profile-detail__hint {
    margin: 0;
    font-size: var(--font-size-xs);
  }

  .profile-detail__failure {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }
</style>
