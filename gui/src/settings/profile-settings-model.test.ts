/**
 * The pure model behind the Settings panel's Profiles tab: which roles still
 * point at a profile and what each row shows.
 */
import { describe, it, expect } from 'vitest';

import { profileSettingsRows, rolesUsingProfile } from './profile-settings-model';
import type { ProfileOption } from '../components/add-window-dialog-types';
import type { RoleEntry } from '../ipc/plan-boundary';
import type { ProfileGuidance, ProfileLimit } from '../ipc/session-client';
import {
  foldExternalText,
  formatCommandLine,
  formatPathForDisplay,
  sanitizeForDisplay,
} from '../display/untrusted-text';
import { settings } from '../ui-messages';

function option(overrides: Partial<ProfileOption> = {}): ProfileOption {
  const id = overrides.id ?? 'codex-cli';
  const command = overrides.command ?? ['codex'];
  return {
    id,
    displayId: sanitizeForDisplay(id),
    displayName: sanitizeForDisplay('Codex CLI'),
    detection: null,
    displayDetection: null,
    detectionWarning: null,
    displayDetectionWarning: null,
    cwd: null,
    command,
    displayCommand: formatCommandLine(command),
    available: true,
    reason: null,
    unavailableKind: null,
    guidance: null,
    model: null,
    effort: null,
    enabled: true,
    limits: [],
    ...overrides,
  };
}

function guidance(text: string, command: string | null, url: string | null): ProfileGuidance {
  return {
    text: foldExternalText(text),
    command,
    displayCommand: command === null ? null : formatPathForDisplay(command),
    url,
    displayUrl: url === null ? null : formatPathForDisplay(url),
  };
}

const GUIDANCE = guidance(
  'Install the standalone build.',
  'install-it',
  'https://example.com/install'
);

function role(name: string, profile: string): RoleEntry {
  return { name, profile, model: null, effort: null };
}

const ROLES: RoleEntry[] = [
  role('reviewer', 'codex-cli'),
  role('implementer', 'claude-code'),
  role('tester', 'codex-cli'),
  role('prefix-lookalike', 'codex-cli-wsl'),
];

describe('rolesUsingProfile', () => {
  it('names every role whose profile is exactly the given id, in table order', () => {
    expect(rolesUsingProfile(ROLES, 'codex-cli')).toEqual(['reviewer', 'tester']);
  });

  it('is empty when no role points at the profile', () => {
    expect(rolesUsingProfile(ROLES, 'shell')).toEqual([]);
  });

  it('is empty for an empty role table', () => {
    expect(rolesUsingProfile([], 'codex-cli')).toEqual([]);
  });
});

describe('profileSettingsRows', () => {
  it('builds one row per profile with every field the row shows', () => {
    const rows = profileSettingsRows(
      [
        option({
          id: 'codex-cli',
          displayName: sanitizeForDisplay('Codex CLI'),
          command: ['codex', '--sandbox', 'workspace-write'],
          enabled: false,
        }),
      ],
      ROLES
    );

    expect(rows).toEqual([
      {
        id: 'codex-cli',
        displayId: 'codex-cli',
        displayName: 'Codex CLI',
        commandLine: 'codex --sandbox workspace-write',
        isReady: true,
        unavailableLine: null,
        guidance: null,
        enabled: false,
        rolesUsing: ['reviewer', 'tester'],
        limitNote: null,
      },
    ]);
  });

  it('lists the roles only for a disabled profile', () => {
    const rows = profileSettingsRows([option({ id: 'codex-cli', enabled: true })], ROLES);

    expect(rows[0]?.enabled).toBe(true);
    expect(rows[0]?.rolesUsing).toEqual([]);
  });

  it('falls back to the reason the daemon gave when it names no kind', () => {
    const rows = profileSettingsRows(
      [
        option({
          id: 'wsl-codex',
          available: false,
          reason: foldExternalText('executable not found'),
        }),
      ],
      []
    );

    expect(rows[0]).toMatchObject({
      isReady: false,
      unavailableLine: settings.profileUnavailable(foldExternalText('executable not found')),
    });
  });

  it('falls back to the reason the daemon gave for a kind this build does not know', () => {
    const rows = profileSettingsRows(
      [
        option({
          available: false,
          reason: foldExternalText('some new reason'),
          unavailableKind: 'unknown',
        }),
      ],
      []
    );

    expect(rows[0]?.unavailableLine).toBe(
      settings.profileUnavailable(foldExternalText('some new reason'))
    );
  });

  it('heads only a program that could not be found as not found', () => {
    const rows = profileSettingsRows(
      [
        option({
          id: 'a',
          available: false,
          reason: foldExternalText('x'),
          unavailableKind: 'not_found',
        }),
        option({
          id: 'b',
          available: false,
          reason: foldExternalText('y'),
          unavailableKind: 'no_command',
        }),
      ],
      []
    );

    expect(rows.map((row) => row.unavailableLine)).toEqual([
      settings.profileNotFound(settings.profileUnavailableReason.not_found),
      settings.profileUnavailable(settings.profileUnavailableReason.no_command),
    ]);
  });

  it('shows a native-program reason in its own words with the guidance apart, never twice', () => {
    const rows = profileSettingsRows(
      [
        option({
          available: false,
          reason: foldExternalText('daemon sentence. Install the standalone build.'),
          unavailableKind: 'needs_native_program',
          guidance: GUIDANCE,
        }),
      ],
      []
    );

    expect(rows[0]).toMatchObject({
      unavailableLine: settings.profileUnavailable(
        settings.profileUnavailableReason.needs_native_program
      ),
      guidance: GUIDANCE,
    });
    expect(rows[0]?.unavailableLine).not.toContain('Install the standalone build.');
  });

  it('shows no guidance for a ready profile', () => {
    const rows = profileSettingsRows([option({ guidance: GUIDANCE })], []);

    expect(rows[0]?.guidance).toBeNull();
  });

  it('shows the argv the daemon returned for a profile with no command table of its own', () => {
    const rows = profileSettingsRows(
      [option({ id: 'shell', command: ['C:\\Windows\\System32\\cmd.exe'] })],
      []
    );

    expect(rows[0]?.commandLine).toBe('C:\\Windows\\System32\\cmd.exe');
  });

  it('shows an empty command line when the daemon returned no argv', () => {
    const rows = profileSettingsRows([option({ id: 'shell', command: [] })], []);

    expect(rows[0]?.commandLine).toBe('');
  });

  it('keeps the order the profiles arrived in', () => {
    const rows = profileSettingsRows(
      [
        option({ id: 'zeta', displayName: sanitizeForDisplay('Zeta') }),
        option({ id: 'alpha', displayName: sanitizeForDisplay('Alpha') }),
        option({ id: 'mid', displayName: sanitizeForDisplay('Mid') }),
      ],
      []
    );

    expect(rows.map((row) => row.id)).toEqual(['zeta', 'alpha', 'mid']);
  });

  it('is empty for no profiles', () => {
    expect(profileSettingsRows([], ROLES)).toEqual([]);
  });
});

const SHIM_GUIDANCE = guidance('Use the WSL profile for those.', null, 'https://example.com/muse');

function limit(capability: ProfileLimit['capability'], reason: string): ProfileLimit {
  return { capability, reason: foldExternalText(reason), guidance: SHIM_GUIDANCE };
}

describe('profileSettingsRows: what a ready profile cannot do here', () => {
  it('stays ready and notes one line per limit, then the guidance text and page once', () => {
    const [row] = profileSettingsRows(
      [
        option({
          limits: [
            limit('effort', 'the effort option cannot pass a .cmd file unchanged'),
            limit('launch_prompt', 'a request at launch cannot pass a .cmd file unchanged'),
          ],
        }),
      ],
      ROLES
    );

    expect(row?.isReady).toBe(true);
    expect(row?.limitNote?.items).toHaveLength(2);
    for (const item of row?.limitNote?.items ?? []) {
      expect(item.length).toBeGreaterThan(0);
    }
    expect(row?.limitNote?.guidance).toMatchObject({
      text: SHIM_GUIDANCE.text,
      url: SHIM_GUIDANCE.url,
    });
  });

  it("shows the daemon's own reason for a capability this build does not know", () => {
    const [row] = profileSettingsRows(
      [option({ limits: [limit('unknown', 'a newer capability is not carried')] })],
      ROLES
    );

    expect(row?.limitNote?.items).toEqual(['a newer capability is not carried']);
  });

  it('links only an https:// page', () => {
    const [row] = profileSettingsRows(
      [
        option({
          limits: [
            {
              capability: 'effort',
              reason: foldExternalText('no effort here'),
              guidance: guidance('See the page.', null, 'http://example.com/muse'),
            },
          ],
        }),
      ],
      ROLES
    );

    expect(row?.limitNote?.guidance).toMatchObject({ text: 'See the page.', url: null });
  });

  it('has no note for a profile without limits', () => {
    const [row] = profileSettingsRows([option()], ROLES);

    expect(row?.limitNote).toBeNull();
  });
});
