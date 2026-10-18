<script lang="ts">
  import { buttons, describeOperationFailure, errors } from '../ui-messages';
  import ModalDialog from './ModalDialog.svelte';

  export let title: string;
  export let label: string;
  export let initialValue: string = '';
  export let isOpen: boolean = false;
  /** Renames to `value`; rejects with the daemon's reason when it refuses the name. */
  export let onConfirm: (value: string) => Promise<void>;
  export let onCancel: () => void;

  let value = initialValue;
  let formError: string | null = null;
  let isSubmitting = false;
  // Bumped on cancel, so a rename still in flight from a cancelled dialog
  // cannot close or annotate the dialog opened after it.
  let attempt = 0;

  // Reopening the dialog for another window must not show the previous entry.
  // `value` is not a dependency here, so typing does not reset the field.
  $: if (isOpen) {
    value = initialValue;
    formError = null;
  }

  // The dialog stays open on a refusal, so the name can be corrected in place.
  async function handleConfirm(): Promise<void> {
    const current = attempt;
    formError = null;
    isSubmitting = true;
    try {
      await onConfirm(value);
      if (current === attempt) {
        isOpen = false;
      }
    } catch (error) {
      if (current === attempt) {
        formError = describeOperationFailure(errors.renameWindowFailed, error);
      }
    } finally {
      if (current === attempt) {
        isSubmitting = false;
      }
    }
  }

  function handleCancel(): void {
    attempt += 1;
    isSubmitting = false;
    onCancel();
    isOpen = false;
  }
</script>

<ModalDialog {isOpen} onCancel={handleCancel}>
  <form class="dialog-content" on:submit|preventDefault={handleConfirm}>
    <h2 class="dialog-title">{title}</h2>
    <!-- The field precedes the buttons, so the modal's initial focus lands
         on it and the label can be typed over straight away. -->
    <label class="dialog-field">
      <span class="dialog-label">{label}</span>
      <input type="text" class="dialog-input" bind:value />
    </label>
    {#if formError}
      <div class="form-error">{formError}</div>
    {/if}
    <div class="dialog-actions">
      <button type="button" class="btn btn-secondary" on:click={handleCancel}>
        {buttons.cancel}
      </button>
      <button type="submit" class="btn btn-primary" disabled={isSubmitting}>
        {buttons.rename}
      </button>
    </div>
  </form>
</ModalDialog>
