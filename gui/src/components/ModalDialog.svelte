<script lang="ts">
  import { onDestroy } from 'svelte';

  import { isImeKey } from '../keymap/key-chord';
  import { modalFocusPort, type ModalFocusToken } from '../session/modal-focus-port';

  export let isOpen: boolean = false;
  export let onCancel: () => void;
  /**
   * Overrides the dialog's max-width; `null`/unset keeps the
   * 400px default set in `.modal-dialog` below.
   */
  export let maxWidth: string | null = null;
  /** The window the dialog is about, or null; decides where focus goes once it closes. */
  export let subject: string | null = null;

  let dialogElement: HTMLDialogElement | null = null;

  const focusPort = modalFocusPort();
  let focusToken: ModalFocusToken | null = null;

  // Reached from close() and from a teardown while open (a dialog shown under an
  // `{#if}`), which neither restores focus nor fires `close`; the token keeps it
  // to one report per opening.
  function reportClosed(): void {
    const token = focusToken;
    focusToken = null;
    if (token !== null) {
      focusPort?.closed(token);
    }
  }

  onDestroy(reportClosed);

  // showModal() rather than the `open` attribute: the browser then traps Tab
  // inside the dialog, makes the rest of the page inert and turns Escape into
  // a `cancel` event, so none of that has to be hand-written here. showModal()
  // on an already-open dialog throws InvalidStateError, hence the `open` guards.
  $: if (dialogElement !== null) {
    if (isOpen && !dialogElement.open) {
      // Before showModal(), which moves focus into the dialog.
      focusToken = focusPort?.opened(subject) ?? null;
      dialogElement.showModal();
    } else if (!isOpen && dialogElement.open) {
      dialogElement.close();
      reportClosed();
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
  <!-- Where tooltips and menus opened from inside this dialog are drawn: the dialog
       makes everything outside it inert, so they cannot live anywhere else. -->
  <div class="modal-overlay-host"></div>
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
    background-color: var(--backdrop);
  }

  .modal-body {
    padding: var(--space-md);
  }
</style>
