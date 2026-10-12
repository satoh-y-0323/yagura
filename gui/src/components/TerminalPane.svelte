<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import { readClipboardText, writeClipboardText } from '../ipc/clipboard';
  import { forms } from '../ui-messages';
  import { CLIPBOARD_CHORDS } from '../keymap/clipboard-chords';
  import { formatChord } from '../keymap/key-chord';
  import { hostKeyboardPlatform } from '../keymap/keyboard-platform';
  import { appHidden } from '../session/app-visibility';
  import { effectiveVisibility } from '../session/effective-visibility';
  import { canApplyTerminalSize } from '../session/size-report';
  import type { SessionController } from '../session/session-controller';
  import { copyAndClearSelection } from '../terminal/copy-selection';
  import { completeTheme, wirePaletteOf } from '../terminal/terminal-colors';
  import { TerminalView } from '../terminal/terminal-view';
  import { bindTerminalTheme } from '../theme/theme-apply';
  import type { ThemeStore } from '../theme/theme-store';
  import TerminalContextMenu from './TerminalContextMenu.svelte';
  import { decideRightClick } from './terminal-right-click';
  import type { TerminalTheme } from './terminal-theme';

  export let controller: SessionController;
  /** The application theme store; the terminal follows what it resolves. */
  export let themeStore: ThemeStore;
  /** Id of the window this pane belongs to, for the per-window override. */
  export let windowId: string;
  /** Vendor of the window, for the per-vendor assignment; `null` when none. */
  export let vendor: string | null;
  export let isVisible = true;

  let root: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let input: HTMLTextAreaElement;
  let view: TerminalView | null = null;
  let observer: ResizeObserver | null = null;
  let unbindTheme: (() => void) | null = null;

  const platform = hostKeyboardPlatform();
  const clipboardKeys = {
    copy: formatChord(CLIPBOARD_CHORDS[platform].copy, platform).join('+'),
    paste: formatChord(CLIPBOARD_CHORDS[platform].paste, platform).join('+'),
  };
  let menuAt: { x: number; y: number } | null = null;
  let canCopy = false;

  $: view?.setVisible(isVisible);
  $: controller.setVisible(effectiveVisibility(isVisible, $appHidden));

  async function copySelection(): Promise<void> {
    await copyAndClearSelection(controller, writeClipboardText);
  }

  async function pasteClipboard(): Promise<void> {
    const text = await readClipboardText();
    view?.paste(text);
  }

  function runClipboardAction(action: 'copy' | 'paste'): void {
    const operation = action === 'copy' ? copySelection() : pasteClipboard();
    operation.catch((error: unknown) => {
      console.error(`failed to ${action} through the clipboard:`, error);
    });
  }

  function rightClickAction(event: MouseEvent): ReturnType<typeof decideRightClick> {
    return decideRightClick({
      platform,
      appCapturesMouse: view?.appCapturesMouse() ?? false,
      shiftKey: event.shiftKey,
      hasSelection: view?.hasSelection() ?? false,
    });
  }

  function handleContextMenu(event: MouseEvent): void {
    // The web view's own menu (reload, inspect, …) never belongs on a window.
    event.preventDefault();
    const action = rightClickAction(event);
    if (action === 'menu') {
      canCopy = view?.hasSelection() ?? false;
      menuAt = { x: event.clientX, y: event.clientY };
    } else if (action !== 'pass-to-app') {
      runClipboardAction(action);
    }
  }

  function closeMenu(): void {
    menuAt = null;
    view?.focus();
  }

  /**
   * The typography of the terminal. Its colours are not read here: they come
   * from the theme store, which resolves this window's palette.
   */
  function readTerminalFont(): { family: string; size: number } {
    const styles = getComputedStyle(document.documentElement);
    const family = styles.getPropertyValue('--font-family-mono').trim();
    const size = Number.parseFloat(styles.getPropertyValue('--font-size-sm'));
    return {
      family: family === '' ? 'monospace' : family,
      size: Number.isFinite(size) && size > 0 ? size : 14,
    };
  }

  function applyTheme(theme: Partial<TerminalTheme>): void {
    const complete = completeTheme(theme);
    view?.setTheme(complete);
    controller.setPalette(wirePaletteOf(complete));
  }

  function reportSize(): void {
    const size = view?.measure() ?? null;
    if (size !== null && canApplyTerminalSize(isVisible, size)) {
      controller.notifyResize(size);
    }
  }

  onMount(() => {
    let initialTheme: Partial<TerminalTheme> = {};
    const unbind = bindTerminalTheme(
      themeStore,
      { windowId, vendor },
      {
        setTheme: (theme) => {
          initialTheme = theme;
          applyTheme(theme);
        },
      }
    );
    view = new TerminalView(root, canvas, input, {
      controller,
      platform,
      font: readTerminalFont(),
      theme: completeTheme(initialTheme),
      onClipboard: runClipboardAction,
      passesRightPress: (event) => rightClickAction(event) === 'pass-to-app',
    });
    view.setVisible(isVisible);
    unbindTheme = unbind;

    observer = new ResizeObserver(reportSize);
    observer.observe(root);

    void controller.start();
  });

  onDestroy(() => {
    controller.setVisible(false);
    unbindTheme?.();
    unbindTheme = null;
    observer?.disconnect();
    observer = null;
    view?.dispose();
    view = null;
    controller.dispose();
  });
</script>

<!-- The right-click is a pointer shortcut; the keyboard has the clipboard chords. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="terminal-pane" bind:this={root} on:contextmenu={handleContextMenu}>
  <canvas class="terminal-pane__canvas" bind:this={canvas}></canvas>
  <textarea
    class="terminal-pane__input"
    bind:this={input}
    autocapitalize="off"
    autocomplete="off"
    aria-label={forms.terminalInput}
    spellcheck="false"></textarea>
</div>
<TerminalContextMenu
  isOpen={menuAt !== null}
  at={menuAt}
  {canCopy}
  copyKeys={clipboardKeys.copy}
  pasteKeys={clipboardKeys.paste}
  onCopy={() => {
    runClipboardAction('copy');
    closeMenu();
  }}
  onPaste={() => {
    closeMenu();
    runClipboardAction('paste');
  }}
  onClose={closeMenu}
/>

<style>
  .terminal-pane {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
    padding: var(--space-xs);
    background-color: var(--color-background);
  }

  .terminal-pane__canvas {
    display: block;
  }

  /* Takes the keyboard and the input method at the cursor's cell without being seen. */
  .terminal-pane__input {
    position: absolute;
    margin: 0;
    padding: 0;
    border: 0;
    outline: none;
    resize: none;
    overflow: hidden;
    opacity: 0;
    pointer-events: none;
    color: transparent;
    background: transparent;
    caret-color: transparent;
    white-space: nowrap;
  }
</style>
