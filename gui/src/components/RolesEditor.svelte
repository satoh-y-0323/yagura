<script lang="ts">
  /**
   * The Roles tab of the plans panel: the role-to-assignment table as
   * editable rows. Nothing reaches the daemon until the rows are saved
   * through the callback the shell supplies.
   */
  import LaunchProfileFields from './LaunchProfileFields.svelte';
  import type { ProfileOption } from './add-window-dialog-types';
  import { assignmentFromEditor, type EditorFields } from '../plans/plan-board-model';
  import { mappedLaunchOptionValues } from '../profile/launch-profile-choices';
  import { acceptsRoleTableReload, defaultRoleAfterEdit } from '../plans/roles-editor-model';
  import { forms, plans } from '../ui-messages';

  /** One row of the role table, as the daemon stores it. */
  interface RoleRow {
    name: string;
    profile: string;
    model: string | null;
    effort: string | null;
  }

  /** A row being edited: every field is text, blank meaning not set. */
  interface EditableRow extends EditorFields {
    name: string;
  }

  export let roles: RoleRow[];
  /** The role a task without a role of its own falls back to, `null` for none. */
  export let defaultRole: string | null;
  export let onSave: (roles: RoleRow[], defaultRole: string | null) => void;
  /** The launch profiles a row's profile is chosen from. */
  export let profileOptions: ProfileOption[];

  let rows: EditableRow[] = [];
  /** The rows as they stood when a table was last taken. */
  let takenRows: EditableRow[] = [];
  /** The rows last handed to `onSave`, until the saved table comes back. */
  let savedRows: EditableRow[] | null = null;
  /** The Default role picker's own editable choice. */
  let selectedDefaultRole: string | null = defaultRole;

  // A table from the shell restarts the edit unless that would discard an unsaved change.
  $: if (acceptsRoleTableReload(hasUnsavedEdit(), arrivalReason(roles))) resetRows(roles);

  // A row the choice named getting deleted or renamed falls it back to none,
  // the same rule a save applies, kept live so the picker never keeps
  // pointing at a row that is no longer there.
  $: selectedDefaultRole = defaultRoleAfterEdit(selectedDefaultRole, rows);

  function editableRows(roles: RoleRow[]): EditableRow[] {
    return roles.map((role) => ({
      name: role.name,
      profile: role.profile,
      model: role.model ?? '',
      effort: role.effort ?? '',
    }));
  }

  function sameRows(left: EditableRow[], right: EditableRow[]): boolean {
    return (
      left.length === right.length &&
      left.every(
        (row, index) =>
          row.name === right[index]?.name &&
          row.profile === right[index]?.profile &&
          row.model === right[index]?.model &&
          row.effort === right[index]?.effort
      )
    );
  }

  function hasUnsavedEdit(): boolean {
    return !sameRows(rows, takenRows);
  }

  function arrivalReason(roles: RoleRow[]): 'loaded' | 'saved' {
    return savedRows !== null && sameRows(editableRows(roles), savedRows) ? 'saved' : 'loaded';
  }

  function resetRows(roles: RoleRow[]): void {
    rows = editableRows(roles);
    takenRows = editableRows(roles);
    savedRows = null;
    selectedDefaultRole = defaultRole;
  }

  function addRow(): void {
    rows = [...rows, { name: '', profile: '', model: '', effort: '' }];
  }

  function editRow(row: EditableRow, field: keyof EditorFields, value: string): void {
    row[field] = value;
    rows = rows;
  }

  function save(): void {
    const table = rows.map((row) => ({
      name: row.name,
      ...assignmentFromEditor({
        profile: row.profile,
        ...mappedLaunchOptionValues(profileOptions, row.profile, row),
      }),
    }));
    savedRows = editableRows(table);
    onSave(table, selectedDefaultRole);
  }
</script>

<div class="roles-editor">
  <ul class="roles-editor__rows">
    {#each rows as row (row)}
      <li class="roles-editor__row">
        <label class="roles-editor__field">{forms.nameLabel}<input bind:value={row.name} /></label>
        <LaunchProfileFields
          {profileOptions}
          profileValue={row.profile}
          onProfileChange={(value: string) => editRow(row, 'profile', value)}
          modelValue={row.model}
          onModelChange={(value: string) => editRow(row, 'model', value)}
          effortValue={row.effort}
          onEffortChange={(value: string) => editRow(row, 'effort', value)}
          customOptionLabel={plans.sourceUnassigned}
        />
        <button
          type="button"
          class="btn btn-secondary"
          on:click={() => (rows = rows.filter((other) => other !== row))}
          >{plans.removeRoleButton}</button
        >
      </li>
    {/each}
  </ul>
  <label class="roles-editor__default">
    {plans.defaultRoleLabel}
    <!-- eslint-disable-next-line svelte/no-reactive-reassign -- bind:value only ever picks an existing row name or none, both already accepted by defaultRoleAfterEdit, so the reactive statement's own overwrite is a no-op except when a row disappeared -->
    <select bind:value={selectedDefaultRole}>
      <option value={null}>{plans.defaultRoleNone}</option>
      {#each rows as row (row)}
        <option value={row.name}>{row.name}</option>
      {/each}
    </select>
  </label>
  <span class="roles-editor__hint">{plans.defaultRoleHint}</span>
  <div class="roles-editor__actions">
    <button type="button" class="btn btn-secondary" on:click={addRow}>{plans.addRoleButton}</button>
    <button type="button" class="btn btn-primary" on:click={save}>{plans.saveRolesButton}</button>
  </div>
</div>

<style>
  .roles-editor {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    min-height: 0;
    flex: 1 1 auto;
  }

  .roles-editor__rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    overflow-y: auto;
  }

  .roles-editor__row {
    display: flex;
    gap: var(--space-xs);
    align-items: flex-end;
  }

  .roles-editor__field {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .roles-editor__actions {
    display: flex;
    gap: var(--space-sm);
    justify-content: flex-end;
  }

  .roles-editor__default {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .roles-editor__hint {
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }
</style>
