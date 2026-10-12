<script lang="ts">
  import { buttons } from '../ui-messages';
  import ModalDialog from './ModalDialog.svelte';

  export let title: string;
  export let label: string;
  export let initialValue: string = '';
  export let isOpen: boolean = false;
  export let onConfirm: (value: string) => void;
  export let onCancel: () => void;

  let value = initialValue;

  // Reopening the dialog for another window must not show the previous entry.
  // `value` is not a dependency here, so typing does not reset the field.
  $: if (isOpen) {
    value = initialValue;
  }

  function handleConfirm(): void {
    onConfirm(value);
    isOpen = false;
  }

  function handleCancel(): void {
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
    <div class="dialog-actions">
      <button type="button" class="btn btn-secondary" on:click={handleCancel}>
        {buttons.cancel}
      </button>
      <button type="submit" class="btn btn-primary">
        {buttons.rename}
      </button>
    </div>
  </form>
</ModalDialog>
