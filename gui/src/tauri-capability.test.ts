/**
 * The fs-sealing invariant: what must hold is "no fs command exists at all", not
 * merely "no fs permission is listed", because a filesystem plugin can be registered
 * in Rust without ever being granted through `capabilities/main.json`.
 */
import { describe, it, expect } from 'vitest';

import capabilitiesJson from '../src-tauri/capabilities/main.json?raw';
import mainRsSource from '../src-tauri/src/main.rs?raw';

/** The `permissions` array of the parsed capabilities file, validated as string[]. */
function parsePermissions(raw: string): string[] {
  const parsed: unknown = JSON.parse(raw);
  const permissions =
    typeof parsed === 'object' && parsed !== null && 'permissions' in parsed
      ? parsed.permissions
      : undefined;
  if (!Array.isArray(permissions) || !permissions.every((entry) => typeof entry === 'string')) {
    throw new Error('capabilities/main.json permissions is not a string array');
  }
  return permissions;
}

describe('capabilities/main.json grants no fs: permission', () => {
  it('parses to a non-empty permissions array (sanity check against a vacuous pass)', () => {
    const permissions = parsePermissions(capabilitiesJson);
    expect(permissions.length).toBeGreaterThan(0);
  });

  it('has no permission starting with fs:', () => {
    const permissions = parsePermissions(capabilitiesJson);
    const fsPermissions = permissions.filter((entry) => entry.startsWith('fs:'));
    expect(fsPermissions).toEqual([]);
  });
});

describe('main.rs registers no tauri_plugin_fs plugin', () => {
  it('reads a non-empty main.rs (sanity check against a vacuous pass)', () => {
    expect(mainRsSource.length).toBeGreaterThan(0);
  });

  it('does not reference tauri_plugin_fs anywhere', () => {
    expect(mainRsSource).not.toMatch(/\btauri_plugin_fs\b/);
  });
});
