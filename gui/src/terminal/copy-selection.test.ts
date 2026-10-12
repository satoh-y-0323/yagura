import { describe, expect, it, vi } from 'vitest';

import { copyAndClearSelection } from './copy-selection';

function fakes(selected: string | null) {
  const order: string[] = [];
  return {
    order,
    source: {
      copySelection: vi.fn(() => {
        order.push('read');
        return Promise.resolve(selected);
      }),
      clearSelection: vi.fn(() => {
        order.push('clear');
      }),
    },
    write: vi.fn((_text: string) => {
      order.push('write');
      return Promise.resolve();
    }),
  };
}

describe('copyAndClearSelection', () => {
  it('writes the selected text to the clipboard and then clears the selection', async () => {
    const { order, source, write } = fakes('hello');

    await copyAndClearSelection(source, write);

    expect(write).toHaveBeenCalledWith('hello');
    expect(order).toEqual(['read', 'write', 'clear']);
  });

  it.each([null, ''])('leaves the clipboard and the selection alone for %j', async (selected) => {
    const { source, write } = fakes(selected);

    await copyAndClearSelection(source, write);

    expect(write).not.toHaveBeenCalled();
    expect(source.clearSelection).not.toHaveBeenCalled();
  });

  it('keeps the selection when the clipboard write fails', async () => {
    const { source } = fakes('hello');
    const failing = vi.fn(() => Promise.reject(new Error('denied')));

    await expect(copyAndClearSelection(source, failing)).rejects.toThrow('denied');

    expect(source.clearSelection).not.toHaveBeenCalled();
  });
});
