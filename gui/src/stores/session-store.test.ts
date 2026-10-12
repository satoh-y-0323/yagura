import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';

import { createWindowSessionStore, isSessionInteractive, type SessionState } from './session-store';

describe('the window session store', () => {
  it('starts connecting, with no exit code and no failure', () => {
    expect(get(createWindowSessionStore())).toEqual({
      phase: 'connecting',
      exitCode: null,
      failureDetail: null,
    });
  });

  it('records each transition with the fields that belong to it and clears the rest', () => {
    const store = createWindowSessionStore();

    store.markFailed('pipe closed');
    expect(get(store)).toEqual({ phase: 'failed', exitCode: null, failureDetail: 'pipe closed' });

    store.markConnecting();
    expect(get(store)).toEqual({ phase: 'connecting', exitCode: null, failureDetail: null });

    store.markAttached();
    expect(get(store)).toEqual({ phase: 'attached', exitCode: null, failureDetail: null });

    store.markExited(7);
    expect(get(store)).toEqual({ phase: 'exited', exitCode: 7, failureDetail: null });

    store.markExited(null);
    expect(get(store)).toEqual({ phase: 'exited', exitCode: null, failureDetail: null });

    store.markFailed('again');
    store.markSuperseded();
    expect(get(store)).toEqual({ phase: 'superseded', exitCode: null, failureDetail: null });
  });

  it('keeps stores of different windows apart', () => {
    const first = createWindowSessionStore();
    const second = createWindowSessionStore();

    first.markAttached();

    expect(get(second).phase).toBe('connecting');
  });
});

describe('isSessionInteractive', () => {
  const base: SessionState = { phase: 'connecting', exitCode: null, failureDetail: null };

  it('accepts interaction only while attached to a live child', () => {
    expect(isSessionInteractive({ ...base, phase: 'attached' })).toBe(true);
    for (const phase of ['connecting', 'exited', 'superseded', 'failed'] as const) {
      expect(isSessionInteractive({ ...base, phase }), phase).toBe(false);
    }
  });
});
