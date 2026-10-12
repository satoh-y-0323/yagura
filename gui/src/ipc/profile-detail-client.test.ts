/**
 * The profile-detail commands: which Tauri command each call invokes with
 * which arguments, and that a reply reaches the caller only validated.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));

import { BoundaryError } from './session-boundary';
import { invokeGetProfileDetail, invokeSetProfileCommand } from './profile-detail-client';

const command = { argv: ['claude'], shipped: null, changed: false };

beforeEach(() => {
  invokeMock.mockReset();
});

describe('invokeGetProfileDetail', () => {
  it('asks get_profile_detail for the profile and returns the narrowed reply', async () => {
    invokeMock.mockResolvedValue({ profile: 'p', shipped_base: false, command, entries: [] });
    const detail = await invokeGetProfileDetail('p');
    expect(invokeMock).toHaveBeenCalledWith('get_profile_detail', { profile: 'p' });
    expect(detail).toEqual({
      profile: 'p',
      shippedBase: false,
      command: { argv: ['claude'], shipped: null, changed: false },
      entries: [],
    });
  });

  it('rejects a reply that fails validation', async () => {
    invokeMock.mockResolvedValue({ profile: 'p' });
    await expect(invokeGetProfileDetail('p')).rejects.toBeInstanceOf(BoundaryError);
  });

  it('passes a refusal from the core through', async () => {
    invokeMock.mockRejectedValue('unsupported');
    await expect(invokeGetProfileDetail('p')).rejects.toBe('unsupported');
  });
});

describe('invokeSetProfileCommand', () => {
  it('sends a copy of the argv and returns the narrowed reply', async () => {
    invokeMock.mockResolvedValue({ profile: 'p', command: { ...command, argv: ['a', 'b'] } });
    const argv: readonly string[] = ['a', 'b'];
    const set = await invokeSetProfileCommand('p', argv);
    expect(invokeMock).toHaveBeenCalledWith('set_profile_command', {
      profile: 'p',
      argv: ['a', 'b'],
    });
    const sent = invokeMock.mock.calls[0]?.[1] as { argv: unknown };
    expect(sent.argv).not.toBe(argv);
    expect(set.command.argv).toEqual(['a', 'b']);
  });

  it('sends null to put the shipped argv back', async () => {
    invokeMock.mockResolvedValue({ profile: 'p', command });
    await invokeSetProfileCommand('p', null);
    expect(invokeMock).toHaveBeenCalledWith('set_profile_command', { profile: 'p', argv: null });
  });

  it('rejects a reply that fails validation', async () => {
    invokeMock.mockResolvedValue({ command });
    await expect(invokeSetProfileCommand('p', null)).rejects.toBeInstanceOf(BoundaryError);
  });
});
