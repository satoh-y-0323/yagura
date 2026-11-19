<script context="module" lang="ts">
  let instanceCount = 0;

  /** A run of a shown text, marked or not. */
  interface TextPiece {
    text: string;
    marked: boolean;
  }
</script>

<script lang="ts">
  /**
   * The go-to-window picker and the command palette: one dialog, two modes.
   * Display only. Which rows there are, their order, what is marked and where
   * the selection goes all come from `picker-model`; the shell decides when the
   * dialog is open and what a chosen row does.
   */
  import { onDestroy, tick } from 'svelte';

  import {
    chosenRow,
    moveSelection,
    openPicker,
    openingInput,
    pointAt,
    pressOpenKey,
    refreshPicker,
    typeInput,
    type MarkSpan,
    type PickerAction,
    type PickerHeading,
    type PickerMode,
    type PickerSource,
    type PickerState,
    type PickerWindow,
  } from '../command-picker/picker-model';
  import { splitForMiddleEllipsis } from '../chat/middle-ellipsis';
  import type { ActionVerdict } from '../keymap/action-availability';
  import { defaultKeysFor } from '../keymap/default-keymap';
  import type { ActionId } from '../keymap/key-binding';
  import { chordOfEvent, formatChord, isImeKey } from '../keymap/key-chord';
  import type { KeyboardPlatform } from '../keymap/keyboard-platform';
  import type { Keys } from '../keymap/keymap-model';
  import { keyboardRows } from '../settings/keyboard-settings-model';
  import { roomNameOf } from '../sidebar/room-groups';
  import { isProminentWindow } from '../status-tone';
  import type { WindowEntry } from '../stores/window-registry';
  import {
    buttons,
    describeActionBlock,
    describePickerActionRow,
    describePickerEmpty,
    describePickerResults,
    describePickerWindowRow,
    describeStatus,
    picker,
    settings,
  } from '../ui-messages';
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';
  import ModalDialog from './ModalDialog.svelte';
  import QueuedMark from './QueuedMark.svelte';
  import StatusBadge from './StatusBadge.svelte';

  export let isOpen: boolean;
  /** The mode the picker opens in. */
  export let mode: PickerMode;
  /** The open windows, in sidebar order. */
  export let windows: readonly WindowEntry[];
  export let currentId: string | null;
  /** The windows whose permission request waits for the user. */
  export let awaitingPermission: ReadonlySet<string>;
  /** The effective keys: the ones shown, and the two that switch modes. */
  export let keys: Keys;
  export let platform: KeyboardPlatform;
  /** Whether an action can run now; asked each time the rows are built. */
  export let actionVerdict: (action: ActionId) => ActionVerdict;
  export let onChooseWindow: (entry: WindowEntry) => void;
  export let onChooseAction: (action: ActionId) => void;
  /** Escape, the backdrop or the close button. */
  export let onCancel: () => void;

  /** One window as the picker shows it, with the entry a choice hands back. */
  interface PickerEntry extends PickerWindow {
    entry: WindowEntry;
  }

  // The list shows this many rows before it scrolls, and Page Up / Page Down move by it.
  const PAGE_ROWS = 10;
  // Read once typing pauses; the delay paces the screen reader and nothing else.
  const ANNOUNCE_DELAY_MS = 500;

  const instance = instanceCount++;
  const listId = `command-picker-${instance}-list`;

  let inputElement: HTMLInputElement | null = null;
  let listElement: HTMLDivElement | null = null;
  let state: PickerState<PickerEntry> | null = null;
  let wasOpen = false;
  let announcement = '';
  let announceTimer: ReturnType<typeof setTimeout> | null = null;

  function pickerWindowOf(entry: WindowEntry): PickerEntry {
    return {
      windowId: entry.windowId,
      name: entry.displayName,
      place: roomNameOf(entry),
      cwd: entry.displayCwd,
      vendor: entry.displayVendor,
      statusWords: describeStatus(entry.detected.status),
      detected: entry.detected,
      ...(entry.taskReport === undefined ? {} : { taskReport: entry.taskReport }),
      entry,
    };
  }

  // The Keyboard tab's rows name each action's section, so the palette's
  // sections are the tab's own; the defaults only feed what the tab marks changed.
  function actionsNow(): PickerAction[] {
    return keyboardRows(keys, defaultKeysFor(platform)).map((row) => ({
      id: row.action,
      name: settings.keyboardActionName(row.action),
      keywords: picker.actionKeywords(row.action),
      section: settings.keyboardGroupHeading(row.group),
      verdict: actionVerdict(row.action),
    }));
  }

  function sourceNow(): PickerSource<PickerEntry> {
    return { windows: windows.map(pickerWindowOf), currentId, actions: actionsNow() };
  }

  function followOpen(open: boolean): void {
    if (open === wasOpen) {
      return;
    }
    wasOpen = open;
    stopAnnouncing();
    if (!open) {
      state = null;
      return;
    }
    state = openPicker(mode, sourceNow());
    void showInput(mode);
  }

  $: followOpen(isOpen);

  // Takes every source of the rows as an argument, so a pushed change reaches the
  // open picker and the picker's own state is not one of the `$:`'s dependencies.
  function refreshFor(_windows: readonly WindowEntry[], _currentId: string | null): void {
    if (state !== null) {
      state = refreshPicker(state, sourceNow());
    }
  }

  $: refreshFor(windows, currentId);

  /** Writes the input a mode opens with and puts the caret where that mode wants it. */
  async function showInput(shownMode: PickerMode): Promise<void> {
    const { input, caret } = openingInput(shownMode);
    // After the flush that shows the dialog, so the caret is set on the focused field.
    await tick();
    if (inputElement !== null) {
      inputElement.value = input;
      inputElement.setSelectionRange(caret, caret);
    }
    await revealSelected();
  }

  /**
   * Shows `shownMode` as its open key does while the picker is open; the key of
   * the mode already shown does nothing.
   */
  export function showMode(shownMode: PickerMode): void {
    if (state === null) {
      return;
    }
    const next = pressOpenKey(state, shownMode, sourceNow());
    if (next === null) {
      return;
    }
    state = next;
    void showInput(shownMode);
    announceLater();
  }

  function rebuild(input: string): void {
    if (state === null) {
      return;
    }
    state = typeInput(state, input, sourceNow());
    void revealSelected();
    announceLater();
  }

  function handleInput(event: Event & { currentTarget: HTMLInputElement }): void {
    // An input method's text is matched once it is committed, not as it is composed.
    if (event instanceof InputEvent && event.isComposing) {
      return;
    }
    rebuild(event.currentTarget.value);
  }

  function handleCompositionEnd(
    event: CompositionEvent & { currentTarget: HTMLInputElement }
  ): void {
    rebuild(event.currentTarget.value);
  }

  function openModeOf(event: KeyboardEvent): PickerMode | null {
    const chord = chordOfEvent(event);
    if (chord === null) {
      return null;
    }
    if (chord === keys['open-window-picker']) {
      return 'windows';
    }
    return chord === keys['open-command-palette'] ? 'commands' : null;
  }

  function move(direction: 'down' | 'up', distance: number): void {
    if (state === null) {
      return;
    }
    state = moveSelection(state, direction, distance);
    void revealSelected();
  }

  function choose(): void {
    const row = state === null ? null : chosenRow(state);
    if (row === null) {
      return;
    }
    if (row.kind === 'window') {
      onChooseWindow(row.window.entry);
    } else {
      onChooseAction(row.action.id);
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (state === null || isImeKey(event)) {
      return;
    }
    const openMode = openModeOf(event);
    if (openMode !== null) {
      event.preventDefault();
      showMode(openMode);
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) {
      return;
    }
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        move('down', 1);
        break;
      case 'ArrowUp':
        event.preventDefault();
        move('up', 1);
        break;
      case 'PageDown':
        event.preventDefault();
        move('down', PAGE_ROWS);
        break;
      case 'PageUp':
        event.preventDefault();
        move('up', PAGE_ROWS);
        break;
      case 'Enter':
        event.preventDefault();
        choose();
        break;
    }
  }

  function point(key: string): void {
    if (state !== null && state.selected !== key) {
      state = pointAt(state, key);
    }
  }

  function chooseByPointer(key: string): void {
    point(key);
    choose();
  }

  async function revealSelected(): Promise<void> {
    await tick();
    listElement?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }

  function announceLater(): void {
    stopAnnouncing();
    announceTimer = setTimeout(() => {
      announceTimer = null;
      if (state !== null) {
        announcement = describePickerResults(state.mode, rowCount, state.empty);
      }
    }, ANNOUNCE_DELAY_MS);
  }

  function stopAnnouncing(): void {
    if (announceTimer !== null) {
      clearTimeout(announceTimer);
      announceTimer = null;
    }
    announcement = '';
  }

  onDestroy(stopAnnouncing);

  /** Cuts the code points `[start, end)` of a shown text into marked and plain runs. */
  function piecesOf(
    points: readonly string[],
    marks: readonly MarkSpan[],
    start: number,
    end: number
  ): TextPiece[] {
    const pieces: TextPiece[] = [];
    let at = start;
    for (const mark of marks) {
      const from = Math.max(mark.start, at);
      const to = Math.min(mark.end, end);
      if (from >= to) {
        continue;
      }
      if (from > at) {
        pieces.push({ text: points.slice(at, from).join(''), marked: false });
      }
      pieces.push({ text: points.slice(from, to).join(''), marked: true });
      at = to;
    }
    if (at < end) {
      pieces.push({ text: points.slice(at, end).join(''), marked: false });
    }
    return pieces;
  }

  function markedText(text: string, marks: readonly MarkSpan[]): TextPiece[] {
    const points = Array.from(text);
    return piecesOf(points, marks, 0, points.length);
  }

  /** The cwd split for an ellipsis in its middle, each half cut into its runs. */
  function markedCwd(
    cwd: string,
    marks: readonly MarkSpan[]
  ): { head: TextPiece[]; tail: TextPiece[] } {
    const points = Array.from(cwd);
    const headLength = Array.from(splitForMiddleEllipsis(cwd).head).length;
    return {
      head: piecesOf(points, marks, 0, headLength),
      tail: piecesOf(points, marks, headLength, points.length),
    };
  }

  function keyLabelOf(currentKeys: Keys, action: ActionId): string | null {
    const chord = currentKeys[action];
    return chord === null ? null : formatChord(chord, platform).join('+');
  }

  function optionId(key: string): string {
    return `command-picker-${instance}-option-${key}`;
  }

  function headingText(heading: NonNullable<PickerHeading>): string {
    switch (heading.kind) {
      case 'needs-you':
        return picker.needsYouHeading;
      case 'other-windows':
        return picker.otherWindowsHeading;
      case 'actions':
        return heading.name;
    }
  }

  $: shownMode = state?.mode ?? mode;
  $: dialogName = shownMode === 'commands' ? picker.commandsTitle : picker.windowsTitle;
  $: rowCount = state === null ? 0 : state.sections.reduce((sum, s) => sum + s.rows.length, 0);
  $: activeId = state?.selected == null ? undefined : optionId(state.selected);
  $: empty = state?.empty == null ? null : describePickerEmpty(state.empty);
</script>

{#snippet marked(pieces: TextPiece[])}{#each pieces as piece, index (index)}{#if piece.marked}<mark
        >{piece.text}</mark
      >{:else}{piece.text}{/if}{/each}{/snippet}

<ModalDialog {isOpen} {onCancel} label={dialogName} variant="picker">
  <div class="command-picker">
    <div class="command-picker__search">
      <Icon name="search" size="md" />
      <!-- The dialog focusing steps pick the autofocus element, so typing starts at once. -->
      <!-- svelte-ignore a11y_autofocus -->
      <input
        bind:this={inputElement}
        class="command-picker__input"
        type="text"
        role="combobox"
        aria-expanded={rowCount > 0 ? 'true' : 'false'}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={activeId}
        aria-label={dialogName}
        placeholder={picker.placeholder}
        spellcheck="false"
        autocomplete="off"
        autofocus
        on:input={handleInput}
        on:compositionend={handleCompositionEnd}
        on:keydown={handleKeydown}
      />
      <IconButton
        class="btn btn-ghost command-picker__close"
        label={buttons.close}
        on:click={onCancel}
      >
        <Icon name="x" size="sm" />
      </IconButton>
    </div>

    {#if empty !== null}
      <div class="command-picker__empty">
        <p class="command-picker__empty-title">{empty.title}</p>
        {#if empty.hint !== null}
          <p class="command-picker__empty-hint">{empty.hint}</p>
        {/if}
      </div>
    {/if}

    <div
      bind:this={listElement}
      id={listId}
      class="command-picker__list"
      style:--picker-page-rows={PAGE_ROWS}
      role="listbox"
      aria-label={dialogName}
    >
      {#if state !== null}
        {#each state.sections as section, sectionIndex (sectionIndex)}
          {@const headingId = `command-picker-${instance}-heading-${sectionIndex}`}
          <div role="group" aria-labelledby={section.heading === null ? undefined : headingId}>
            {#if section.heading !== null}
              <div id={headingId} class="command-picker__heading" role="presentation">
                {headingText(section.heading)}
              </div>
            {/if}
            {#each section.rows as row (row.key)}
              {@const isSelected = row.key === state.selected}
              {#if row.kind === 'window'}
                {@const w = row.window}
                {@const isAwaitingPermission = awaitingPermission.has(w.windowId)}
                <!-- The combobox's input takes the keys; an option is only pointed at, never focused. -->
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <div
                  id={optionId(row.key)}
                  class="command-picker__option"
                  role="option"
                  tabindex="-1"
                  aria-selected={isSelected}
                  aria-labelledby="{optionId(row.key)}-name"
                  data-prominent={isProminentWindow(w)}
                  on:mousedown|preventDefault
                  on:pointermove={() => point(row.key)}
                  on:click={() => chooseByPointer(row.key)}
                >
                  <span id="{optionId(row.key)}-name" hidden
                    >{describePickerWindowRow({
                      name: w.name,
                      status: w.detected.status,
                      isAwaitingPermission,
                      place: w.place,
                      isCurrent: row.isCurrent,
                    })}</span
                  >
                  <StatusBadge status={w.detected.status} variant="icon" />
                  <span class="command-picker__name"
                    >{@render marked(markedText(w.name, row.marks.name))}</span
                  >
                  <span class="command-picker__status">{w.statusWords}</span>
                  <span class="command-picker__place">
                    <Icon name={w.entry.room === null ? 'folder' : 'messages-square'} size="xs" />
                    <span class="command-picker__place-name"
                      >{@render marked(markedText(w.place, row.marks.place))}</span
                    >
                  </span>
                  <!-- A window in no room is placed by its folder already, so its cwd is not repeated. -->
                  {#if w.entry.room !== null}
                    {@const cwd = markedCwd(w.cwd, row.marks.cwd)}
                    <span class="command-picker__cwd">
                      <span class="command-picker__cwd-head">{@render marked(cwd.head)}</span><span
                        class="command-picker__cwd-tail">{@render marked(cwd.tail)}</span
                      >
                    </span>
                  {/if}
                  {#if w.vendor !== null}
                    <span class="command-picker__vendor"
                      >{@render marked(markedText(w.vendor, row.marks.vendor))}</span
                    >
                  {/if}
                  <span class="command-picker__end">
                    {#if isAwaitingPermission}
                      <span class="command-picker__permission">
                        <Icon name="shield-exclamation-point" size="xs" />
                      </span>
                    {/if}
                    <QueuedMark count={w.entry.queuedCount} />
                    {#if row.isCurrent}
                      <span>{picker.current}</span>
                    {/if}
                  </span>
                </div>
              {:else if row.kind === 'action'}
                {@const keyLabel = keyLabelOf(keys, row.action.id)}
                <!-- The combobox's input takes the keys; an option is only pointed at, never focused. -->
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <div
                  id={optionId(row.key)}
                  class="command-picker__option"
                  role="option"
                  tabindex="-1"
                  aria-selected={isSelected}
                  aria-labelledby="{optionId(row.key)}-name"
                  aria-describedby={row.blocked === null ? undefined : `${optionId(row.key)}-why`}
                  aria-disabled={row.blocked === null ? undefined : 'true'}
                  data-blocked={row.blocked !== null}
                  on:mousedown|preventDefault
                  on:pointermove={() => point(row.key)}
                  on:click={() => chooseByPointer(row.key)}
                >
                  <span id="{optionId(row.key)}-name" hidden
                    >{describePickerActionRow(row.action.name, keyLabel)}</span
                  >
                  <span class="command-picker__name"
                    >{@render marked(markedText(row.action.name, row.marks.name))}</span
                  >
                  {#if row.blocked !== null}
                    <span id="{optionId(row.key)}-why" class="command-picker__why"
                      >{describeActionBlock(row.blocked)}</span
                    >
                  {/if}
                  {#if row.showsSection}
                    <span class="command-picker__section">{row.action.section}</span>
                  {/if}
                  <span class="command-picker__end">
                    {#if keyLabel !== null}
                      <kbd class="key-hint">{keyLabel}</kbd>
                    {/if}
                  </span>
                </div>
              {/if}
            {/each}
          </div>
        {/each}
      {/if}
    </div>

    <div class="command-picker__footer">
      {shownMode === 'commands' ? picker.commandsFooter : picker.windowsFooter}
    </div>
    <div class="command-picker__announcement" role="status">{announcement}</div>
  </div>
</ModalDialog>

<style>
  .command-picker {
    --picker-row-height: 32px;

    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .command-picker__search {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 40px;
    padding: 0 var(--space-1_5) 0 var(--space-3);
    border-bottom: var(--line-width) solid var(--line);
    color: var(--fg-muted);
  }

  /* The caret alone is a faint sign of focus, so the field's edge takes the ring colour too. */
  .command-picker__search:focus-within {
    border-bottom-color: var(--ring);
  }

  .command-picker__input {
    flex: 1 1 auto;
    min-width: 0;
    height: 100%;
    padding: 0;
    border: none;
    background-color: transparent;
    color: var(--fg-default);
    font-family: inherit;
    font-size: var(--text-md);
  }

  /* Transparent until forced colours paint it, where the field's edge colour is not kept. */
  .command-picker__input:focus-visible {
    outline: var(--line-width-strong) solid transparent;
  }

  .command-picker__input::placeholder {
    color: var(--fg-faint);
  }

  .command-picker :global(.command-picker__close) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--icon-button-hit-min);
    min-height: var(--icon-button-hit-min);
    padding: 0;
    color: var(--fg-muted);
  }

  .command-picker__empty {
    padding: var(--space-5) var(--space-4) var(--space-2);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    text-align: center;
    overflow-wrap: anywhere;
  }

  .command-picker__empty p {
    margin: 0;
  }

  .command-picker__empty-title {
    color: var(--fg-default);
    font-weight: var(--weight-medium);
  }

  .command-picker__list {
    max-height: calc(var(--picker-row-height) * var(--picker-page-rows) + var(--space-1) * 2);
    padding: var(--space-1) var(--space-1_5);
    overflow-y: auto;
  }

  .command-picker__heading {
    display: flex;
    align-items: center;
    height: 24px;
    padding: 0 var(--space-2);
    color: var(--fg-muted);
    font-size: var(--text-2xs);
    font-weight: var(--weight-semibold);
  }

  .command-picker__option {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    height: var(--picker-row-height);
    padding: 0 var(--space-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    white-space: nowrap;
    cursor: pointer;
  }

  .command-picker__option[aria-selected='true'] {
    background-color: var(--bg-selected);
  }

  /* Each part gives up its width in turn: the cwd first, the name last. */
  .command-picker__name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .command-picker__option[data-prominent='true'] .command-picker__name {
    color: var(--fg-default);
    font-weight: var(--weight-medium);
  }

  .command-picker__option[data-blocked='false'] .command-picker__name {
    color: var(--fg-default);
  }

  .command-picker__status {
    flex: 0 2 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--text-xs);
  }

  .command-picker__place {
    display: inline-flex;
    flex: 0 4 auto;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    font-size: var(--text-xs);
  }

  .command-picker__place-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .command-picker__vendor {
    flex: 0 8 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .command-picker__cwd {
    display: flex;
    flex: 0 16 auto;
    min-width: 0;
    overflow: hidden;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  /* The head shrinks behind the ellipsis; the tail, the folder's own name, stays. */
  .command-picker__cwd-head {
    flex: 0 100 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .command-picker__cwd-tail {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
  }

  .command-picker__why {
    flex: 0 0 auto;
    font-size: var(--text-xs);
  }

  .command-picker__section {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .command-picker__end {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1_5);
    margin-left: auto;
    color: var(--fg-muted);
    font-size: var(--text-2xs);
  }

  .command-picker__option[data-blocked='true'] .key-hint {
    color: var(--fg-faint);
  }

  .command-picker__permission {
    display: inline-flex;
    color: var(--st-approval);
  }

  /* In a row the count sits bare beside its envelope, as it does in the sidebar. */
  .command-picker__end :global(.queued-mark) {
    padding: 0;
    border: none;
    color: inherit;
    font-size: inherit;
  }

  mark {
    background-color: transparent;
    color: var(--fg-default);
    font-weight: var(--weight-semibold);
  }

  .command-picker__footer {
    display: flex;
    align-items: center;
    height: 28px;
    padding: 0 var(--space-3);
    border-top: var(--line-width) solid var(--line);
    color: var(--fg-faint);
    font-size: var(--text-2xs);
  }

  /* Read by a screen reader, not drawn. */
  .command-picker__announcement {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  @media (forced-colors: active) {
    /* A transparent mark keeps MarkText on the Canvas color, which can hide the text. */
    mark {
      background-color: Mark;
      color: MarkText;
    }

    .command-picker__option[aria-selected='true'] {
      outline: var(--line-width-strong) solid Highlight;
      outline-offset: -2px;
    }
  }
</style>
