/**
 * Whether a role table that just arrived may replace the rows being edited.
 * An ordinary load never erases an unsaved edit; the echo of the editor's
 * own save is always taken, so the rows settle on what was stored.
 *
 * @param hasUnsavedEdit - whether the rows differ from the table last taken
 * @param reason - `'saved'` when the arriving table is the one this editor
 *   saved, `'loaded'` otherwise
 */
export function acceptsRoleTableReload(
  hasUnsavedEdit: boolean,
  reason: 'loaded' | 'saved'
): boolean {
  return reason === 'saved' || !hasUnsavedEdit;
}

/**
 * The default role once the rows are edited: kept while a row still carries
 * its name, `null` once that row is deleted or renamed.
 */
export function defaultRoleAfterEdit(
  defaultRole: string | null,
  rows: readonly { name: string }[]
): string | null {
  return rows.some((row) => row.name === defaultRole) ? defaultRole : null;
}
