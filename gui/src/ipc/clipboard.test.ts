/**
 * The clipboard bridge: text reads come back as text, a clipboard holding
 * something else reads as empty (and says why), and writes reach the plugin.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const { readTextMock, writeTextMock } = vi.hoisted(() => ({
  readTextMock: vi.fn(),
  writeTextMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({
  readText: readTextMock,
  writeText: writeTextMock,
}));

import { readClipboardText, writeClipboardText } from './clipboard';

beforeEach(() => {
  readTextMock.mockReset();
  writeTextMock.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('readClipboardText', () => {
  it('returns the text the plugin read', async () => {
    readTextMock.mockResolvedValue('hello');
    await expect(readClipboardText()).resolves.toBe('hello');
  });

  it('returns an empty string and logs the cause when the clipboard holds no text', async () => {
    const cause = new Error('not text');
    readTextMock.mockRejectedValue(cause);
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(readClipboardText()).resolves.toBe('');
    expect(warn).toHaveBeenCalledWith('failed to read text from the clipboard:', cause);
  });
});

describe('writeClipboardText', () => {
  it('hands the text to the plugin', async () => {
    writeTextMock.mockResolvedValue(undefined);
    await writeClipboardText('copied');
    expect(writeTextMock).toHaveBeenCalledWith('copied');
  });

  it('rejects when the plugin cannot write', async () => {
    writeTextMock.mockRejectedValue(new Error('denied'));
    await expect(writeClipboardText('x')).rejects.toThrow('denied');
  });
});
