<script lang="ts">
  import {
    buttons,
    forms,
    hints,
    describeRestoreOrigin,
    describeRestoreSelectionCount,
    describeRestoreConfirm,
  } from '../ui-messages';
  import type { RestoreEntry } from '../ipc/session-boundary';
  import { splitPreparedForMiddleEllipsis } from '../chat/middle-ellipsis';
  import { matchesOpenWindow } from '../session/restore-model';
  import Icon from './Icon.svelte';
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
      <h2 class="dialog-title">{forms.reopenTitle}</h2>
      <IconButton class="dialog-close" label={forms.closeDialog} on:click={onCancel}
        ><Icon name="x" /></IconButton
      >
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
        {@const origin = describeRestoreOrigin(candidate.displayProfile, candidate.displayVendor)}
        {@const cwd = splitPreparedForMiddleEllipsis(candidate.displayCwd)}
        <li class="restore-row">
          <label class="restore-row__label">
            <input
              type="checkbox"
              checked={selected.has(candidate.position)}
              on:change={() => onToggle(candidate.position)}
            />
            <span class="restore-row__name">{candidate.displayName}</span>
            {#if matchesOpenWindow(candidate, openWindows)}
              <span class="restore-row__chip">{hints.restoreAlreadyOpen}</span>
            {/if}
            <span class="restore-row__meta">
              {#if origin !== null}
                <span class="restore-row__origin">{origin}</span>
              {/if}
              <span class="restore-row__cwd">
                <span class="restore-row__cwd-head">{cwd.head}</span><span
                  class="restore-row__cwd-tail">{cwd.tail}</span
                >
              </span>
            </span>
          </label>
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
        {describeRestoreConfirm(selected.size)}
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
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }

  /* Checkbox | name | chip on the first line; the meta line spans the rest. */
  .restore-row__label {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 2px var(--space-sm);
  }

  .restore-row__name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .restore-row__chip {
    padding: 0 var(--space-xs);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    font-size: var(--font-size-xs);
    white-space: nowrap;
  }

  .restore-row__meta {
    grid-column: 2 / 4;
    display: flex;
    gap: var(--space-sm);
    min-width: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
    white-space: nowrap;
  }

  .restore-row__origin {
    flex: 0 0 auto;
  }

  .restore-row__cwd {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    overflow: hidden;
  }

  /* Shrinks first, so the end of the path stays readable. */
  .restore-row__cwd-head {
    flex: 0 100 auto;
    min-width: calc(1em + 1ch);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .restore-row__cwd-tail {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
  }

  .restore-note {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }
</style>
