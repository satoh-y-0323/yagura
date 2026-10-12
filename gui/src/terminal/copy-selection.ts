/** What a copy needs from the window it copies out of. */
export interface SelectionSource {
  copySelection: () => Promise<string | null>;
  clearSelection: () => void;
}

/**
 * Puts the window's selected text on the clipboard and then clears the
 * selection, so the next right click pastes instead of copying again. Nothing
 * happens when nothing is selected; a failed write keeps the selection.
 *
 * @throws when the clipboard write fails.
 */
export async function copyAndClearSelection(
  source: SelectionSource,
  write: (text: string) => Promise<void>
): Promise<void> {
  const text = await source.copySelection();
  if (text === null || text === '') {
    return;
  }
  await write(text);
  source.clearSelection();
}
