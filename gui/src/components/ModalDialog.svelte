<script lang="ts">
  import { isImeKey } from '../keymap/key-chord';

  export let isOpen: boolean = false;
  export let onCancel: () => void;
  /**
   * Overrides the dialog's max-width; `null`/unset keeps the
   * 400px default set in `.modal-dialog` below.
   */
  export let maxWidth: string | null = null;

  let dialogElement: HTMLDialogElement | null = null;

  // showModal() rather than the `open` attribute: the browser then traps Tab
  // inside the dialog, makes the rest of the page inert and turns Escape into
  // a `cancel` event, so none of that has to be hand-written here. showModal()
  // on an already-open dialog throws InvalidStateError, hence the `open` guards.
  $: if (dialogElement !== null) {
    if (isOpen && !dialogElement.open) {
      dialogElement.showModal();
    } else if (!isOpen && dialogElement.open) {
      dialogElement.close();
    }
  }

  // The browser turns an Escape into `cancel` even when the input method owns
  // it (the Esc cancelling a composition), so the keydown before it decides.
  let imeOwnsKey = false;

  function handleKeydown(event: KeyboardEvent): void {
    imeOwnsKey = isImeKey(event);
  }

  function handleCancel(): void {
    if (imeOwnsKey) {
      imeOwnsKey = false;
      return;
    }
    onCancel();
  }

  /**
   * A modal dialog is itself the target of clicks on its backdrop, because the
   * backdrop is painted by the dialog and has no node of its own; a click on
   * any content reports that descendant instead. The dialog element must
   * therefore stay padding-free (padding lives on .modal-body): padding on the
   * dialog draws a ring inside the frame that still reports the dialog as the
   * target, so clicking it would cancel even though the click was inside.
   */
  function handleBackdropClick(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      onCancel();
    }
  }
</script>

<dialog
  class="modal-dialog"
  bind:this={dialogElement}
  style:max-width={maxWidth}
  on:keydown={handleKeydown}
  on:cancel|preventDefault={handleCancel}
  on:click={handleBackdropClick}
>
  <div class="modal-body">
    <slot />
  </div>
</dialog>

<style>
  .modal-dialog {
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background-color: var(--color-surface);
    color: var(--color-text-primary);
    padding: 0;
    box-shadow: var(--shadow-modal);
    max-width: 400px;
    width: 90%;
  }

  .modal-dialog::backdrop {
    background-color: var(--overlay-backdrop, rgba(0, 0, 0, 0.5));
  }

  .modal-body {
    padding: var(--space-md);
  }
</style>
