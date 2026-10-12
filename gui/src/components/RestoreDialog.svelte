<script lang="ts">
  import { buttons, forms, hints, describeRestoreSelectionCount } from '../ui-messages';
  import type { RestoreEntry } from '../ipc/session-boundary';
  import { matchesOpenWindow } from '../session/restore-model';
  import IconButton from './IconButton.svelte';
  import ModalDialog from './ModalDialog.svelte';

  /** Whether the dialog is on screen. */
  export let isOpen: boolean = false;
  /** Called when the user dismisses the dialog without restoring. */
  export let onCancel: () => void;
  /** The previous boot's candidates, in the order the restore UI presents them. */
  export let candidates: RestoreEntry[] = [];
  /** Currently open windows, for the already-open hint (cwd + name match). */
  export let openWindows: readonly { cwd: string; name: string }[] = [];
  /** Positions currently checked. */
  export let selected: ReadonlySet<number> = new Set();
  /** Toggles one candidate's checkbox. */
  export let onToggle: (position: number) => void;
  /** Checks every candidate. */
  export let onSelectAll: () => void;
  /** Unchecks every candidate. */
  export let onClearAll: () => void;
  /** Restores the checked candidates. */
  export let onRestore: () => void;
</script>

<ModalDialog {isOpen} {onCancel} maxWidth="560px">
  <div class="restore-dialog-content">
    <div class="dialog-header">
      <h2 class="dialog-title">{buttons.chooseWindows}</h2>
      <IconButton class="dialog-close" label={forms.closeDialog} on:click={onCancel}>✕</IconButton>
    </div>

    <div class="restore-bulk-actions">
      <button type="button" class="btn btn-secondary" on:click={onSelectAll}>
        {buttons.selectAll}
      </button>
      <button type="button" class="btn btn-secondary" on:click={onClearAll}>
        {buttons.clearAll}
      </button>
      <span class="restore-selection-count">
        {describeRestoreSelectionCount(selected.size, candidates.length)}
      </span>
    </div>

    <ul class="restore-candidate-list">
      {#each candidates as candidate (candidate.position)}
        <li class="restore-row">
          <label class="restore-row__label">
            <input
              type="checkbox"
              checked={selected.has(candidate.position)}
              on:change={() => onToggle(candidate.position)}
            />
            <span class="restore-row__name">{candidate.displayName}</span>
            <span class="restore-row__cwd">{candidate.displayCwd}</span>
          </label>
          {#if matchesOpenWindow(candidate, openWindows)}
            <span class="restore-row__hint">{hints.restoreAlreadyOpen}</span>
          {/if}
        </li>
      {/each}
    </ul>

    <p class="restore-note">{hints.restoreNoteExecution}</p>
    <p class="restore-note">{hints.restoreNoteUnselected}</p>

    <div class="dialog-actions">
      <button type="button" class="btn btn-secondary" on:click={onCancel}>
        {buttons.cancel}
      </button>
      <button
        type="button"
        class="btn btn-primary"
        on:click={onRestore}
        disabled={selected.size === 0}
      >
        {buttons.restore}
      </button>
    </div>
  </div>
</ModalDialog>

<style>
  .restore-bulk-actions {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .restore-selection-count {
    margin-left: auto;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .restore-candidate-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    max-height: 320px;
    overflow-y: auto;
  }

  .restore-row {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }

  .restore-row__label {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    flex: 1 1 auto;
    min-width: 0;
  }

  .restore-row__cwd {
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .restore-row__hint {
    flex: 0 0 auto;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }

  .restore-note {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }
</style>
