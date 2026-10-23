<script context="module" lang="ts">
  let instanceCount = 0;
</script>

<script lang="ts">
  /**
   * The launch-profile picker: a profile chosen from the daemon's profile
   * list, and a model / effort field for each axis that profile maps. It holds
   * no state of its own; every edit is reported through the callbacks.
   */
  import {
    launchOptionAxes,
    launchOptionLimit,
    profileChoiceLabel,
    profileChoices,
  } from '../profile/launch-profile-choices';
  import { forms } from '../ui-messages';
  import type { ProfileOption } from './add-window-dialog-types';

  /** The launch profiles to offer, in the order they are shown. */
  export let profileOptions: ProfileOption[] = [];
  /** The profile id currently chosen, held by the caller. */
  export let profileValue: string = '';
  /** Reports a new profile choice back to the caller. */
  export let onProfileChange: (value: string) => void;
  /** The `model` text currently entered, held by the caller. */
  export let modelValue: string = '';
  /** Reports new `model` text back to the caller. */
  export let onModelChange: (value: string) => void;
  /** The `effort` text currently entered, held by the caller. */
  export let effortValue: string = '';
  /** Reports new `effort` text back to the caller. */
  export let onEffortChange: (value: string) => void;
  /** The label of a trailing `value=""` option; `null` offers none. */
  export let customOptionLabel: string | null = null;
  /** Asks the caller to refresh `profileOptions`. */
  export let onLoadProfiles: () => void = () => undefined;
  /** Whether the fields are disabled. */
  export let disabled: boolean = false;

  // Pickers repeat per row and per card, so each datalist id must be unique on the page.
  instanceCount += 1;
  const listPrefix = `launch-option-${String(instanceCount)}-`;

  function labelOf(key: 'model' | 'effort'): string {
    return key === 'model' ? forms.modelLabel : forms.effortLabel;
  }

  function valueOf(key: 'model' | 'effort', model: string, effort: string): string {
    return key === 'model' ? model : effort;
  }

  function reportText(key: 'model' | 'effort', event: Event): void {
    const target = event.currentTarget;
    const text = target instanceof HTMLInputElement ? target.value : '';
    if (key === 'model') {
      onModelChange(text);
    } else {
      onEffortChange(text);
    }
  }

  function selectedValue(event: Event): string {
    const target = event.currentTarget;
    return target instanceof HTMLSelectElement ? target.value : '';
  }
</script>

<div class="launch-profile-fields">
  <label class="dialog-field">
    <span class="dialog-label">{forms.profileLabel}</span>
    <select
      class="profile-select"
      value={profileValue}
      on:change={(event) => onProfileChange(selectedValue(event))}
      on:focus={onLoadProfiles}
      {disabled}
    >
      {#each profileChoices(profileOptions, profileValue) as choice (choice.id)}
        <option value={choice.id} disabled={!choice.available}>
          {profileChoiceLabel(choice)}
        </option>
      {/each}
      {#if customOptionLabel !== null}
        <option value="">{customOptionLabel}</option>
      {/if}
    </select>
  </label>
  {#each launchOptionAxes(profileOptions, profileValue) as axis (axis.key)}
    {@const limit = launchOptionLimit(profileOptions, profileValue, axis.key)}
    <label class="dialog-field">
      <span class="dialog-label">
        {labelOf(axis.key)}
        <span class="form-optional">{forms.optionalSuffix}</span>
      </span>
      <input
        type="text"
        class="dialog-input"
        list={listPrefix + axis.key}
        value={limit === null ? valueOf(axis.key, modelValue, effortValue) : ''}
        on:input={(event) => reportText(axis.key, event)}
        disabled={disabled || limit !== null}
        spellcheck="false"
      />
      <datalist id={listPrefix + axis.key}>
        {#each axis.aliases as entry (entry.alias)}
          <option value={entry.alias}>{entry.alias} — {entry.value}</option>
        {/each}
      </datalist>
      {#if limit !== null}
        <span class="launch-option-limit">
          {limit.reason}
          {#if limit.guidance !== null}{limit.guidance.text}{/if}
        </span>
      {/if}
    </label>
  {/each}
</div>

<style>
  /* The fields sit directly in the caller's own layout, as if written there. */
  .launch-profile-fields {
    display: contents;
  }

  .form-optional {
    font-size: var(--font-size-xs);
    font-weight: 400;
    color: var(--color-text-secondary);
  }

  .launch-option-limit {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }

  /* Left otherwise unstyled: a native select draws itself from the
     `color-scheme` the tokens set, which is what keeps it legible. */
  .profile-select {
    font-size: var(--font-size-sm);
  }

  .dialog-input:disabled,
  .profile-select:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
</style>
