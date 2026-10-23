<script lang="ts">
  import { mappedLaunchOptionValues } from '../profile/launch-profile-choices';
  import { buttons, errors, forms } from '../ui-messages';
  import { describeWindowOpenFailure, windowOpenFailureOf } from '../window-open-failure-model';
  import type { OpenWindowSubmission, ProfileOption } from './add-window-dialog-types';
  import IconButton from './IconButton.svelte';
  import LaunchProfileFields from './LaunchProfileFields.svelte';
  import ModalDialog from './ModalDialog.svelte';

  /** Whether the dialog is on screen. */
  export let isOpen: boolean = false;
  /** Called when the user dismisses the dialog without submitting. */
  export let onCancel: () => void;
  /**
   * Why the detection definitions could not be enumerated, `null` when
   * nothing is wrong. Owned by the caller: a warning raised while this dialog
   * is closed must survive until it is opened again, so it is only displayed
   * here, never set or cleared.
   */
  export let warning: string | null = null;
  /** The working directory currently entered, held by the caller. */
  export let cwdValue: string = '';
  /** Reports a new working directory back to the caller. */
  export let onCwdChange: (value: string) => void;
  /** Asks the caller to run the native folder picker. */
  export let onBrowse: () => void;
  /** Whether a folder is being dragged over the window right now. */
  export let isDropTarget: boolean = false;
  /** The detection definition currently picked, held by the caller. */
  export let detectionValue: string = '';
  /** Reports a new detection definition selection back to the caller. */
  export let onDetectionChange: (value: string) => void;
  /** The detection definition names to offer. */
  export let detectionOptions: string[] = [];
  /** Asks the caller to refresh `detectionOptions`. */
  export let onLoadDetections: () => void;
  /** The launch profiles to offer, in the order they are shown. */
  export let profileOptions: ProfileOption[] = [];
  /** The launch profile currently picked, held by the caller; `''` is Custom. */
  export let profileValue: string = '';
  /** Reports a new launch profile selection back to the caller. */
  export let onProfileChange: (value: string) => void;
  /** Asks the caller to refresh `profileOptions`. */
  export let onLoadProfiles: () => void;
  /**
   * Why the launch profiles could not be enumerated, `null` when nothing is
   * wrong. A channel of its own, separate from `warning`: a profile list that
   * could not be read says nothing about the detection definitions, and the
   * Custom form stays usable either way.
   */
  export let profileWarning: string | null = null;
  /** The `model` value currently entered, held by the caller. */
  export let modelValue: string = '';
  /** Reports a new `model` value back to the caller. */
  export let onModelChange: (value: string) => void;
  /** The `effort` value currently entered, held by the caller. */
  export let effortValue: string = '';
  /** Reports a new `effort` value back to the caller. */
  export let onEffortChange: (value: string) => void;
  /** Opens the window. Rejects with the reason the attempt was refused. */
  export let onSubmit: (input: OpenWindowSubmission) => Promise<void>;

  let commandInput = '';
  let nameInput = '';
  let isSubmitting = false;
  let formError: string | null = null;

  /** The profile the selection names, `null` while Custom is selected. */
  $: selectedProfile = profileOptions.find((entry) => entry.id === profileValue) ?? null;

  /**
   * Self-close, matching ConfirmDialog and RenameDialog: so
   * `bind:isOpen` at the call site works the same way for all three
   * siblings, whichever one triggers the dismissal. Also the single route
   * all three dismissals (✕, Cancel, Escape/backdrop via ModalDialog) go
   * through, so a submission in flight can be guarded once instead of per
   * control: `onSubmit` already reached the daemon by the time
   * this could run, so closing early would abandon the window it opens
   * without telling the user, or a failure with nowhere left to show
   * `formError`. Guarding here keeps the dialog open until that settles.
   * Dismissing discards what was typed, so the next open starts
   * from a blank form; a failed submission keeps the dialog open instead,
   * leaving the input to be corrected.
   */
  function handleCancel(): void {
    if (isSubmitting) {
      return;
    }
    formError = null;
    commandInput = '';
    nameInput = '';
    onCancel();
    isOpen = false;
  }

  /**
   * What the form submits: the selected profile, which the daemon expands
   * into a command of its own, or the Custom form's argument vector.
   *
   * A blank directory or name is left out: the core tells "not provided"
   * apart from an empty value, and validates both. `null` means the Custom
   * form has no command yet, the one refusal this dialog makes by itself.
   */
  function buildSubmission(): OpenWindowSubmission | null {
    const cwd = cwdValue || undefined;
    const name = nameInput.trim() || undefined;
    if (profileValue !== '') {
      // A field is sent only for an axis the profile maps and can take here,
      // and only when it holds text: a value typed then hidden or disabled by
      // switching profiles must not slip into a launch that no longer offers it.
      const mapped = mappedLaunchOptionValues(profileOptions, profileValue, {
        model: modelValue,
        effort: effortValue,
      });
      const model = mapped.model !== '' ? mapped.model : undefined;
      const effort = mapped.effort !== '' ? mapped.effort : undefined;
      return { kind: 'profile', profile: profileValue, cwd, name, model, effort };
    }
    // One line is one token, so a path containing spaces needs no quoting.
    const tokens = commandInput
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line !== '');
    return tokens.length === 0 ? null : { kind: 'custom', tokens, cwd, name };
  }

  async function handleSubmit(): Promise<void> {
    formError = null;
    const submission = buildSubmission();
    if (submission === null) {
      formError = errors.missingCommand;
      return;
    }

    isSubmitting = true;
    try {
      await onSubmit(submission);
      commandInput = '';
      nameInput = '';
      isOpen = false;
    } catch (error) {
      formError = describeWindowOpenFailure(errors.openWindowFailed, windowOpenFailureOf(error));
    } finally {
      isSubmitting = false;
    }
  }

  /** The text the input now holds, `''` when the event has no input behind it. */
  function enteredText(event: Event): string {
    const target = event.currentTarget;
    return target instanceof HTMLInputElement ? target.value : '';
  }

  /** The value the select now shows, `''` when the event has no select behind it. */
  function selectedValue(event: Event): string {
    const target = event.currentTarget;
    return target instanceof HTMLSelectElement ? target.value : '';
  }
</script>

<ModalDialog {isOpen} onCancel={handleCancel}>
  <div class="dialog-content">
    <div class="dialog-header">
      <h2 class="dialog-title">{forms.addWindowTitle}</h2>
      <IconButton class="dialog-close" label={forms.closeDialog} on:click={handleCancel}>
        ✕
      </IconButton>
    </div>

    {#if warning}
      <div class="dialog-warning" role="alert">{warning}</div>
    {/if}

    {#if profileWarning}
      <div class="dialog-warning" role="alert">{profileWarning}</div>
    {/if}

    <form class="open-window-form" on:submit|preventDefault={handleSubmit}>
      <LaunchProfileFields
        {profileOptions}
        {profileValue}
        {onProfileChange}
        {modelValue}
        {onModelChange}
        {effortValue}
        {onEffortChange}
        {onLoadProfiles}
        customOptionLabel={forms.customOption}
        disabled={isSubmitting}
      />
      {#if profileValue !== ''}
        <div class="profile-summary">
          <span>
            {forms.profileSummaryCommandLabel}
            {selectedProfile !== null && selectedProfile.command.length > 0
              ? selectedProfile.command.join(' ')
              : forms.profileSummaryCommandUnavailable}
          </span>
          <span>
            {forms.profileSummaryDetectionLabel}
            {selectedProfile?.detection ?? forms.detectionDefaultOption}
          </span>
          {#if selectedProfile?.detectionWarning}
            <span class="profile-summary-warning">{selectedProfile.detectionWarning}</span>
          {/if}
        </div>
      {/if}
      {#if profileValue === ''}
        <label class="dialog-field">
          <span class="dialog-label">{forms.commandLabel}</span>
          <textarea
            class="dialog-input form-textarea"
            placeholder={forms.commandPlaceholder}
            bind:value={commandInput}
            disabled={isSubmitting}
            spellcheck="false"
            rows="3"></textarea>
          <span class="form-help">{forms.commandHelp}</span>
        </label>
      {/if}
      <label class="dialog-field">
        <span class="dialog-label">
          {forms.cwdLabel}
          <span class="form-optional">{forms.optionalSuffix}</span>
        </span>
        <div class="cwd-row">
          <input
            type="text"
            class="dialog-input"
            placeholder={forms.cwdPlaceholder}
            value={cwdValue}
            on:input={(event) => onCwdChange(enteredText(event))}
            disabled={isSubmitting}
            spellcheck="false"
          />
          <button
            type="button"
            class="btn btn-secondary"
            on:click={() => onBrowse()}
            disabled={isSubmitting}
          >
            {forms.browseButton}
          </button>
        </div>
        {#if isDropTarget}
          <div class="folder-drop-hint">{forms.folderDropHint}</div>
        {/if}
      </label>
      <label class="dialog-field">
        <span class="dialog-label">
          {forms.nameLabel}
          <span class="form-optional">{forms.optionalSuffix}</span>
        </span>
        <input
          type="text"
          class="dialog-input"
          placeholder={forms.namePlaceholder}
          bind:value={nameInput}
          disabled={isSubmitting}
          spellcheck="false"
        />
      </label>
      {#if profileValue === ''}
        <label class="dialog-field">
          <span class="dialog-label">
            {forms.detectionLabel}
            <span class="form-optional">{forms.optionalSuffix}</span>
          </span>
          <select
            class="detection-select"
            value={detectionValue}
            on:change={(event) => onDetectionChange(selectedValue(event))}
            on:focus={onLoadDetections}
            disabled={isSubmitting}
          >
            <option value="">{forms.detectionDefaultOption}</option>
            {#each detectionOptions as name (name)}
              <option value={name}>{name}</option>
            {/each}
          </select>
        </label>
      {/if}
      {#if formError}
        <div class="form-error">{formError}</div>
      {/if}
      <div class="dialog-actions">
        <button type="button" class="btn btn-secondary" on:click={handleCancel}>
          {buttons.cancel}
        </button>
        <button type="submit" class="btn btn-primary" disabled={isSubmitting}>
          {buttons.submit}
        </button>
      </div>
    </form>
  </div>
</ModalDialog>

<style>
  /* justify-content: space-between for .dialog-header is consolidated into
     components.css (shared with .status-panel__header). */
  .dialog-warning {
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-sm);
  }

  .open-window-form {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }

  .form-optional {
    font-size: var(--font-size-xs);
    font-weight: 400;
    color: var(--color-text-secondary);
  }

  .cwd-row {
    display: flex;
    gap: var(--space-sm);
  }

  .cwd-row .dialog-input {
    flex: 1 1 auto;
    min-width: 0;
  }

  /* No tinted fill: a tint needs color-mix() or a new token, and color-mix()
     is avoided for webview support. The dashed border marks the drop target. */
  .folder-drop-hint {
    padding: var(--space-xs) var(--space-sm);
    border: 1px dashed var(--color-primary);
    border-radius: var(--radius-sm);
    color: var(--color-primary);
    font-size: var(--font-size-xs);
  }

  .form-textarea {
    resize: vertical;
    font-family: var(--font-family-mono);
    min-height: 80px;
  }

  .form-help {
    display: block;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  /* Left otherwise unstyled: a native select draws itself from the
     `color-scheme` the tokens set, which is what keeps it legible. */
  .detection-select {
    font-size: var(--font-size-sm);
  }

  /* Read-only recap of what the selected profile launches: the fields it
     replaces are not rendered while it is shown. */
  .profile-summary {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }

  .profile-summary-warning {
    color: var(--color-warning);
  }

  /* components.css gives .dialog-input no disabled state, and every field is
     disabled while a submission is in flight. */
  .dialog-input:disabled,
  .detection-select:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
</style>
