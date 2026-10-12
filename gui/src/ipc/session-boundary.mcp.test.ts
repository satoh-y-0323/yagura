/**
 * Whether a listed window's child is connected to the daemon over MCP. The
 * fact travels only on the window list: the status push has no path that
 * carries it, so the status payload stays as it was.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowRestoreCandidateList,
  narrowStatusPayload,
  narrowWindowInfo,
} from './session-boundary';

const WINDOW_ID = 'w-mcp';

/** A well-formed `list_windows` entry that says nothing about MCP. */
function windowInfoWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    window_id: WINDOW_ID,
    name: 'main',
    vendor: null,
    cwd: '/x',
    status: 'running',
    exit_code: null,
    ...overrides,
  };
}

/** A well-formed restore candidate row that names no launch profile. */
function candidateWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    position: 0,
    command: ['claude'],
    cwd: '/home/user/project',
    name: 'main',
    vendor: null,
    detection: null,
    ...overrides,
  };
}

describe('narrowWindowInfo: mcp_connected', () => {
  it('reads an absent mcp_connected as unknown rather than as disconnected', () => {
    const info = narrowWindowInfo(windowInfoWire());

    expect(info.mcp_connected ?? null).toBeNull();
  });

  it('carries true through', () => {
    const info = narrowWindowInfo(windowInfoWire({ mcp_connected: true }));

    expect(info.mcp_connected).toBe(true);
  });

  it('carries false through', () => {
    const info = narrowWindowInfo(windowInfoWire({ mcp_connected: false }));

    expect(info.mcp_connected).toBe(false);
  });

  it.each([
    { label: 'a string', value: 'true' },
    { label: 'a number', value: 1 },
    { label: 'an object', value: {} },
    { label: 'an array', value: [true] },
  ])('refuses $label in place of a boolean', ({ value }) => {
    expect(() => narrowWindowInfo(windowInfoWire({ mcp_connected: value }))).toThrow(BoundaryError);
  });
});

describe('narrowStatusPayload: mcp_connected is not part of the status push', () => {
  it('does not carry mcp_connected even when the payload holds one', () => {
    const payload = narrowStatusPayload({
      window_id: WINDOW_ID,
      status: 'running',
      mcp_connected: true,
    });

    expect(Object.prototype.hasOwnProperty.call(payload, 'mcp_connected')).toBe(false);
  });
});

describe('narrowRestoreCandidateList: the launch profile of a candidate', () => {
  it('carries profile, model and effort through', () => {
    const [entry] = narrowRestoreCandidateList([
      candidateWire({ profile: 'claude-code', model: 'opus', effort: 'high' }),
    ]);

    expect(entry?.profile).toBe('claude-code');
    expect(entry?.model).toBe('opus');
    expect(entry?.effort).toBe('high');
  });

  it('reads absent profile, model and effort as null', () => {
    const [entry] = narrowRestoreCandidateList([candidateWire()]);

    expect(entry?.profile).toBeNull();
    expect(entry?.model).toBeNull();
    expect(entry?.effort).toBeNull();
  });

  it('keeps the row and degrades a non-string profile, model or effort to null', () => {
    const entries = narrowRestoreCandidateList([
      candidateWire({ profile: 7, model: ['opus'], effort: {} }),
    ]);

    expect(entries).toHaveLength(1);
    expect(entries[0]?.profile).toBeNull();
    expect(entries[0]?.model).toBeNull();
    expect(entries[0]?.effort).toBeNull();
  });
});
