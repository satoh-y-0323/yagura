<script lang="ts">
  import { buttons } from '../ui-messages';
  import ModalDialog from './ModalDialog.svelte';
  import type { ConfirmOperation } from '../session/confirm-target';

  export let title: string;
  export let message: string;
  export let operation: ConfirmOperation;
  export let confirmLabel: string;
  export let isOpen: boolean = false;
  export let onConfirm: () => void;
  export let onCancel: () => void;

  function handleConfirm(): void {
    onConfirm();
    isOpen = false;
  }

  function handleCancel(): void {
    onCancel();
    isOpen = false;
  }
</script>

<ModalDialog {isOpen} onCancel={handleCancel}>
  <div class="dialog-content">
    <h2 class="dialog-title">{title}</h2>
    <p class="dialog-message">{message}</p>
    <div class="dialog-actions">
      <!-- Cancel comes first because the browser puts the modal's initial
           focus on the first focusable descendant: every operation behind
           this dialog is destructive, so a reflexive Enter must not carry
           one out. -->
      <button type="button" class="btn btn-secondary" on:click={handleCancel}>
        {buttons.cancel}
      </button>
      <button
        type="button"
        class="btn btn-danger"
        data-operation={operation}
        on:click={handleConfirm}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</ModalDialog>
