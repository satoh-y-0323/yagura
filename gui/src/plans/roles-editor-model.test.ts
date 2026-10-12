/**
 * The one decision that guards RolesEditor's reactive `roles` reload: a
 * table that just arrived must not silently replace rows the user has
 * started editing, unless the arrival is the echo of this editor's own
 * save. Also the default role the editor keeps: it follows the rows, so a
 * default whose row is gone is dropped before the table is saved. Measured
 * as pure functions so the property holds independent of
 * Svelte's reactivity, which this project's `environment: 'node'` vitest
 * config cannot exercise.
 */
import { describe, expect, it } from 'vitest';

import { acceptsRoleTableReload, defaultRoleAfterEdit } from './roles-editor-model';

describe('acceptsRoleTableReload', () => {
  it('accepts an ordinary load when the rows carry no unsaved edit', () => {
    expect(acceptsRoleTableReload(false, 'loaded')).toBe(true);
  });

  it('refuses an ordinary load that would erase an edit still unsaved', () => {
    expect(acceptsRoleTableReload(true, 'loaded')).toBe(false);
  });

  it('always accepts the table this editor just saved, even with an unsaved edit made meanwhile', () => {
    expect(acceptsRoleTableReload(true, 'saved')).toBe(true);
  });

  it('accepts a save echo when there was nothing unsaved either', () => {
    expect(acceptsRoleTableReload(false, 'saved')).toBe(true);
  });
});

describe('defaultRoleAfterEdit', () => {
  const rows = [{ name: 'review' }, { name: 'implement' }];

  it('keeps the default role while its row is still in the table', () => {
    expect(defaultRoleAfterEdit('review', rows)).toBe('review');
    expect(defaultRoleAfterEdit('implement', rows)).toBe('implement');
  });

  it('clears the default role once its row is deleted', () => {
    expect(defaultRoleAfterEdit('review', [{ name: 'implement' }])).toBeNull();
    expect(defaultRoleAfterEdit('review', [])).toBeNull();
  });

  it('clears the default role once its row is renamed to another name', () => {
    const renamed = [{ name: 'reviewer' }, { name: 'implement' }];
    expect(defaultRoleAfterEdit('review', renamed)).toBeNull();
  });

  it('leaves no default role as none', () => {
    expect(defaultRoleAfterEdit(null, rows)).toBeNull();
  });
});
