/**
 * The sole place in the GUI that touches the Tauri dialog/webview APIs:
 * `Browse…` and drag-drop both funnel through `pickFolder` /
 * `subscribeFolderDrop`. Raw drop events are normalized by
 * `folder-drop-model.ts` rather than switched on here.
 *
 * Neither function checks the returned path against the filesystem: the
 * daemon validates cwd when the window is opened.
 */
import { open } from '@tauri-apps/plugin-dialog';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { UnlistenFn } from '@tauri-apps/api/event';

import { normalizeDrop, type DropSignal } from './folder-drop-model';

/** Receives a normalized drag-drop signal. */
type FolderDropHandler = (signal: DropSignal) => void;

/** Opens the native directory picker. `null` when the user cancels. */
export async function pickFolder(): Promise<string | null> {
  return open({ directory: true });
}

/**
 * Subscribes to this window's drag-drop events, normalizing each raw event
 * before forwarding it to `handler`.
 */
export async function subscribeFolderDrop(handler: FolderDropHandler): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((event) => {
    handler(normalizeDrop(event.payload));
  });
}
