/**
 * Contract of the MCP registration boundary at the narrowing functions
 * themselves: the closed state and action vocabularies, the optional parts
 * of a row, and which part of a refused reply the message names.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowMcpRegistrationRequested,
  narrowMcpRegistrationResultPayload,
  narrowMcpRegistrationStatusPayload,
  narrowMcpRegistrationsListed,
} from './mcp-registration-boundary';

function commandWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { argv: ['alpha', 'mcp', 'add'], program_at: 1, ...overrides };
}

function rowWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'alpha-cli',
    display_name: 'Alpha CLI',
    group: 'g',
    add: commandWire(),
    remove: commandWire({ argv: ['alpha', 'mcp', 'remove'] }),
    toml_set: { file: 'c.toml', table: ['t'], values: [{ key: 'k', value: 'v' }] },
    entries: [{ file: 'h.json', name: 'n', content: 'c' }],
    ...overrides,
  };
}

function listedWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { request_id: 3, bridge: 'b.exe', rows: [rowWire()], ...overrides };
}

function statusInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'alpha-cli',
    state: 'registered',
    differs_in: [],
    reason: null,
    add: null,
    remove: null,
    ...overrides,
  };
}

function resultWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 4,
    profile: 'alpha-cli',
    action: 'update',
    steps: [{ argv: ['a'], program_at: 0, exit_code: 2, output: 'o' }],
    error: 'boom',
    status: statusInfoWire(),
    ...overrides,
  };
}

function stateOf(state: string): string {
  return narrowMcpRegistrationStatusPayload({
    request_id: 1,
    status: statusInfoWire({ state }),
  }).status.state;
}

describe('the registration state of a status push', () => {
  it.each(['registered', 'differs', 'not_registered', 'not_installed', 'unknown'])(
    'keeps the known state %s',
    (state) => {
      expect(stateOf(state)).toBe(state);
    }
  );

  it('reads a state this build does not know as unknown', () => {
    expect(stateOf('quantum')).toBe('unknown');
    expect(stateOf('')).toBe('unknown');
  });

  it('keeps the commands, reason and differing parts the daemon reported', () => {
    const { status } = narrowMcpRegistrationStatusPayload({
      request_id: 1,
      status: statusInfoWire({
        state: 'differs',
        differs_in: ['command'],
        reason: 'why',
        add: commandWire(),
        remove: commandWire({ program_at: 0 }),
      }),
    });
    expect(status.differsIn).toEqual(['command']);
    expect(status.reason).toBe('why');
    expect(status.add).toEqual({ argv: ['alpha', 'mcp', 'add'], programAt: 1 });
    expect(status.remove).toEqual({ argv: ['alpha', 'mcp', 'add'], programAt: 0 });
  });

  it('keeps the claim pointer a refused Register or Update names', () => {
    const { status } = narrowMcpRegistrationStatusPayload({
      request_id: 1,
      status: statusInfoWire({ state: 'differs', refused_by_claim: '/managed_hooks_path' }),
    });
    expect(status.refusedByClaim).toBe('/managed_hooks_path');
  });

  it('reads an omitted or null claim refusal as none', () => {
    for (const absent of [null, undefined]) {
      const { status } = narrowMcpRegistrationStatusPayload({
        request_id: 1,
        status: statusInfoWire({ refused_by_claim: absent }),
      });
      expect(status.refusedByClaim).toBeNull();
    }
  });

  it('reads an omitted add or remove as none', () => {
    const info = statusInfoWire();
    delete info['add'];
    delete info['remove'];
    const { status } = narrowMcpRegistrationStatusPayload({ request_id: 1, status: info });
    expect(status.add).toBeNull();
    expect(status.remove).toBeNull();
  });

  it('names the part of the push that is wrong', () => {
    expect(() => narrowMcpRegistrationStatusPayload(1)).toThrow(
      /^session:\/\/mcp-registration-status: expected an object/
    );
    expect(() =>
      narrowMcpRegistrationStatusPayload({ request_id: 1, status: statusInfoWire({ add: 'x' }) })
    ).toThrow(/mcp-registration-status\.status\.add: expected an object/);
    expect(() => narrowMcpRegistrationStatusPayload({ request_id: 1, status: 5 })).toThrow(
      /mcp-registration-status\.status/
    );
  });
});

describe('narrowMcpRegistrationsListed', () => {
  it('keeps every part of a well-formed reply', () => {
    expect(narrowMcpRegistrationsListed(listedWire())).toEqual({
      requestId: 3,
      bridge: 'b.exe',
      rows: [
        {
          profile: 'alpha-cli',
          displayName: 'Alpha CLI',
          group: 'g',
          add: { argv: ['alpha', 'mcp', 'add'], programAt: 1 },
          remove: { argv: ['alpha', 'mcp', 'remove'], programAt: 1 },
          tomlSet: { file: 'c.toml', table: ['t'], values: [{ key: 'k', value: 'v' }] },
          entries: [{ file: 'h.json', name: 'n', content: 'c' }],
        },
      ],
    });
  });

  it('reads omitted or null entries and settings-file change as none', () => {
    for (const absent of [null, undefined]) {
      const row = rowWire({ entries: absent, toml_set: absent });
      const listed = narrowMcpRegistrationsListed(listedWire({ rows: [row] }));
      expect(listed.rows[0]?.entries).toEqual([]);
      expect(listed.rows[0]?.tomlSet).toBeNull();
    }
  });

  it('refuses an entry that lacks a field or is not an object', () => {
    for (const entry of [
      { name: 'n', content: 'c' },
      { file: 'f', content: 'c' },
      { file: 'f', name: 'n' },
      'x',
    ]) {
      const row = rowWire({ entries: [entry] });
      expect(() => narrowMcpRegistrationsListed(listedWire({ rows: [row] }))).toThrow(
        /rows\.entries/
      );
    }
  });

  it('refuses entries that are not an array', () => {
    const row = rowWire({ entries: {} });
    expect(() => narrowMcpRegistrationsListed(listedWire({ rows: [row] }))).toThrow(
      /rows\.entries/
    );
  });

  it('reads a row with no add or remove command as having none', () => {
    for (const absent of [null, undefined]) {
      const row = rowWire({ add: absent, remove: absent });
      const listed = narrowMcpRegistrationsListed(listedWire({ rows: [row] }));
      expect(listed.rows[0]?.add).toBeNull();
      expect(listed.rows[0]?.remove).toBeNull();
      expect(listed.rows[0]?.entries).toEqual([{ file: 'h.json', name: 'n', content: 'c' }]);
    }
  });

  it('names the part of the reply that is wrong', () => {
    const list = (overrides: Record<string, unknown>): unknown =>
      narrowMcpRegistrationsListed(listedWire(overrides));
    expect(() => narrowMcpRegistrationsListed(null)).toThrow(/^list_mcp_registrations/);
    expect(() => list({ rows: {} })).toThrow(/^list_mcp_registrations\.rows/);
    expect(() => list({ rows: [rowWire({ add: 'x' })] })).toThrow(/rows\.add: expected an object/);
    expect(() => list({ rows: [rowWire({ remove: 'x' })] })).toThrow(
      /rows\.remove: expected an object/
    );
    expect(() => list({ rows: [rowWire({ toml_set: 'x' })] })).toThrow(/rows\.toml_set/);
    expect(() =>
      list({ rows: [rowWire({ toml_set: { file: 'f', table: [], values: 1 } })] })
    ).toThrow(/rows\.toml_set\.values/);
    expect(() =>
      list({ rows: [rowWire({ toml_set: { file: 'f', table: [], values: [{ key: 'k' }] } })] })
    ).toThrow(/rows\.toml_set\.values/);
  });
});

describe('narrowMcpRegistrationRequested', () => {
  it.each(['register', 'update', 'remove'])('keeps the action %s', (action) => {
    expect(narrowMcpRegistrationRequested({ request_id: 1, profile: 'p', action }).action).toBe(
      action
    );
  });

  it('refuses an action outside the closed set, naming it', () => {
    expect(() =>
      narrowMcpRegistrationRequested({ request_id: 1, profile: 'p', action: 'purge' })
    ).toThrow(/^run_mcp_registration\.action: unrecognized action "purge"/);
  });

  it('names the command on any other refusal', () => {
    expect(() => narrowMcpRegistrationRequested(null)).toThrow(/^run_mcp_registration/);
  });
});

describe('narrowMcpRegistrationResultPayload', () => {
  it('keeps the steps, the error and the final status', () => {
    expect(narrowMcpRegistrationResultPayload(resultWire())).toEqual({
      requestId: 4,
      profile: 'alpha-cli',
      action: 'update',
      steps: [{ argv: ['a'], programAt: 0, exitCode: 2, output: 'o' }],
      error: 'boom',
      status: {
        profile: 'alpha-cli',
        state: 'registered',
        differsIn: [],
        reason: null,
        add: null,
        remove: null,
        refusedByClaim: null,
      },
    });
  });

  it('reads a step without an exit code as having none', () => {
    const step = { argv: [], program_at: 0, output: '' };
    expect(
      narrowMcpRegistrationResultPayload(resultWire({ steps: [step] })).steps[0]?.exitCode
    ).toBeNull();
  });

  it('names the part of the push that is wrong', () => {
    const result = (overrides: Record<string, unknown>): unknown =>
      narrowMcpRegistrationResultPayload(resultWire(overrides));
    expect(() => result({ steps: {} })).toThrow(/mcp-registration-result\.steps/);
    expect(() => result({ steps: [1] })).toThrow(/mcp-registration-result\.steps/);
    expect(() => result({ status: 1 })).toThrow(/mcp-registration-result\.status/);
    expect(() => result({ action: 'purge' })).toThrow(BoundaryError);
  });
});
