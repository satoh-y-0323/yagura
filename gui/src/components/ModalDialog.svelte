<script lang="ts">
  import { onDestroy } from 'svelte';

  import { isImeKey } from '../keymap/key-chord';
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import { modalFocusPort, type ModalFocusToken } from '../session/modal-focus-port';
  import { focusAutofocusDelegate, shouldRefocus } from './modal-refocus';

  export let isOpen: boolean = false;
  export let onCancel: () => void;
  /**
   * Overrides the dialog's max-width; `null`/unset keeps the
   * 400px default set in `.modal-dialog` below. It is inline, so it wins over
   * the width a `variant` sets.
   */
  export let maxWidth: string | null = null;
  /** The dialog's accessible name; `null` leaves the dialog without one. */
  export let label: UiText | DisplayLine | null = null;
  /** The id of the element inside that names the dialog, in place of `label`. */
  export let labelledBy: string | null = null;
  /** `picker` is the command picker's frame: wider, near the top, and edge to edge inside. */
  export let variant: 'form' | 'picker' = 'form';
  /** The window the dialog is about, or null; decides where focus goes once it closes. */
  export let subject: string | null = null;
  /**
   * Identifies what the open dialog is about; when it changes while the dialog
   * stays open, focus goes back to the dialog's autofocus element.
   */
  export let refocusKey: string | null = null;

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

  let lastRefocusKey: string | null = null;

  // Reads and writes its own state in here so the `$:` below depends on its
  // arguments only.
  function refocusOnKeyChange(element: HTMLDialogElement, key: string | null, open: boolean): void {
    const refocus = shouldRefocus(lastRefocusKey, key, open && element.open);
    lastRefocusKey = key;
    if (refocus) {
      focusAutofocusDelegate(element);
    }
  }

  // Must stay after the open/close block: it needs the dialog already shown.
  $: if (dialogElement !== null) {
    refocusOnKeyChange(dialogElement, refocusKey, isOpen);
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
  data-variant={variant}
  aria-label={label ?? undefined}
  aria-labelledby={labelledBy ?? undefined}
  bind:this={dialogElement}
  style:max-width={maxWidth}
  on:keydown={handleKeydown}
  on:cancel|preventDefault={handleCancel}
  on:click={handleBackdropClick}
>
  <div class="modal-body" data-variant={variant}>
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

  /* The top margin is fixed and the bottom one is not, so the frame stays put
     while the rows below it are filtered. */
  .modal-dialog[data-variant='picker'] {
    width: min(560px, 100vw - 32px);
    max-width: none;
    margin: 15vh auto auto;
    border: var(--line-width) solid var(--line);
    border-radius: var(--radius-lg);
    background-color: var(--bg-raised);
    color: var(--fg-default);
    box-shadow: var(--shadow-modal);
  }

  .modal-body[data-variant='picker'] {
    padding: 0;
  }
</style>
