<script lang="ts">
  /**
   * The Keyboard tab of the Settings panel: renders the rows `keyboardRows`
   * derives and hands every press to the tab controller. No IPC and no
   * decision of its own -- the shell owns the keys, how the keymap load ended
   * and the calls that record and save.
   */
  import { onDestroy } from 'svelte';

  import { defaultKeysFor } from '../keymap/default-keymap';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import { formatChord } from '../keymap/key-chord';
  import type { KeymapSaveFailureKind } from '../keymap/keymap-store';
  import type { Keys } from '../keymap/keymap-model';
  import {
    createKeyboardTabController,
    keyboardRows,
    keymapEditBlock,
    keymapFileLocation,
    type KeymapLoadStatus,
  } from '../settings/keyboard-settings-model';
  import { settings } from '../ui-messages';

  export let keys: Keys;
  export let platform: KeyboardPlatform;
  export let loadStatus: KeymapLoadStatus;
  export let saveFailure: KeymapSaveFailureKind | null;
  export let keyFilePath: string | null;
  export let onBeginRecording: (onResult: (chord: string | null) => void) => void;
  export let onCancelRecording: () => void;
  export let onCommit: (edit: (keys: Keys) => Keys) => Promise<boolean>;

  const controller = createKeyboardTabController({
    platform,
    keys: () => keys,
    editBlock: () => keymapEditBlock(loadStatus),
    beginRecording: (onResult) => onBeginRecording(onResult),
    cancelRecording: () => onCancelRecording(),
    commit: (edit) => onCommit(edit),
  });

  onDestroy(() => controller.dispose());

  $: rows = keyboardRows(keys, defaultKeysFor(platform));
  $: block = keymapEditBlock(loadStatus);

  /** A chord as the sentences of the notices quote it. */
  function chordText(chord: string): string {
    return formatChord(chord, platform).join('+');
  }
</script>

<div class="keyboard-settings">
  <p class="keyboard-settings__description">{settings.keyboardDescription}</p>
  {#if block !== null}
    <p class="keyboard-settings__blocked">
      {settings.keyboardEditBlocked(block, keymapFileLocation(platform, keyFilePath))}
    </p>
  {/if}
  {#if saveFailure !== null}
    <p class="keyboard-settings__failure">{settings.keyboardSaveFailure(saveFailure)}</p>
  {/if}
  <div class="keyboard-settings__rows">
    {#each rows as row, index (row.action)}
      {#if index === 0 || rows[index - 1]?.group !== row.group}
        <div class="keyboard-settings__group-head">{settings.keyboardGroupHeading(row.group)}</div>
      {/if}
      {@const note = settings.keyboardActionNote(row.action)}
      {@const notice = $controller.notices[row.action]}
      <div
        class="keyboard-settings__row"
        class:keyboard-settings__row--recording={$controller.recording === row.action}
      >
        <div class="keyboard-settings__name">
          <span>{settings.keyboardActionName(row.action)}</span>
          {#if note !== null}
            <span class="keyboard-settings__note">{note}</span>
          {/if}
        </div>
        <div class="keyboard-settings__chord">
          {#if $controller.recording === row.action}
            <span class="keyboard-settings__recording">{settings.keyboardRecording}</span>
          {:else if row.chord === null}
            <span class="keyboard-settings__none">{settings.keyboardNotAssigned}</span>
          {:else}
            {#each formatChord(row.chord, platform) as part, partIndex (partIndex)}
              {#if partIndex > 0}<span class="keyboard-settings__plus">+</span>{/if}<kbd>{part}</kbd
              >
            {/each}
            {#if row.isChanged}
              <span class="keyboard-settings__changed">{settings.keyboardChangedMark}</span>
            {/if}
          {/if}
        </div>
        <div class="keyboard-settings__actions">
          <button
            type="button"
            class="btn btn-secondary keyboard-settings__button"
            disabled={block !== null}
            on:click={() => controller.change(row.action)}>{settings.keyboardChange}</button
          >
          <button
            type="button"
            class="btn btn-secondary keyboard-settings__button"
            disabled={block !== null || !row.canReset}
            on:click={() => void controller.reset(row.action)}>{settings.keyboardReset}</button
          >
          <button
            type="button"
            class="btn btn-secondary keyboard-settings__button"
            disabled={block !== null || !row.canRemove}
            on:click={() => void controller.remove(row.action)}>{settings.keyboardRemove}</button
          >
        </div>
        {#if notice !== undefined}
          {#if notice.kind === 'rejected'}
            <p class="keyboard-settings__refusal">
              {settings.keyboardRejection(notice.reason, chordText(notice.chord))}
            </p>
          {:else if notice.kind === 'took'}
            <p class="keyboard-settings__move">
              {settings.keyboardTook(
                chordText(notice.chord),
                settings.keyboardActionName(notice.from)
              )}
            </p>
          {:else}
            <p class="keyboard-settings__move">{settings.keyboardLost(chordText(notice.chord))}</p>
          {/if}
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .keyboard-settings {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    overflow-y: auto;
  }

  .keyboard-settings__description {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: var(--font-size-sm);
  }

  .keyboard-settings__blocked,
  .keyboard-settings__failure {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    border: 1px solid var(--color-warning);
    color: var(--color-warning);
    font-size: var(--font-size-sm);
  }

  .keyboard-settings__rows {
    display: flex;
    flex-direction: column;
  }

  .keyboard-settings__group-head {
    padding: var(--space-sm) var(--space-xs) var(--space-xs);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .keyboard-settings__row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: var(--space-xs) var(--space-sm);
    align-items: center;
    padding: var(--space-xs);
    border-bottom: 1px solid var(--color-border);
  }

  .keyboard-settings__row--recording {
    background-color: var(--color-background);
  }

  .keyboard-settings__name {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .keyboard-settings__note {
    color: var(--color-text-secondary);
    font-size: var(--font-size-xs);
  }

  .keyboard-settings__chord {
    min-width: 150px;
    text-align: right;
    font-size: var(--font-size-sm);
  }

  .keyboard-settings__chord kbd {
    padding: 1px 6px;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    font-family: inherit;
    font-size: var(--font-size-xs);
    white-space: nowrap;
  }

  .keyboard-settings__plus {
    padding: 0 2px;
  }

  .keyboard-settings__none {
    color: var(--color-text-secondary);
    font-style: italic;
  }

  .keyboard-settings__recording,
  .keyboard-settings__changed {
    color: var(--color-primary);
  }

  .keyboard-settings__changed {
    margin-left: var(--space-xs);
    font-size: var(--font-size-xs);
  }

  .keyboard-settings__actions {
    display: flex;
    gap: var(--space-xs);
  }

  .keyboard-settings__refusal,
  .keyboard-settings__move {
    grid-column: 1 / -1;
    margin: 0;
    font-size: var(--font-size-xs);
  }

  .keyboard-settings__refusal {
    color: var(--color-error);
  }

  .keyboard-settings__move {
    color: var(--color-warning);
  }
</style>
