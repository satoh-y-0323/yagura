/**
 * The route by which the app moves keyboard focus into a window's terminal:
 * through the controller the app already reaches by window id, to the sink
 * the terminal view connected.
 */
import { describe, it, expect, vi } from 'vitest';

vi.mock('../ipc/session-client', () => ({
  invokeSendWindowInput: vi.fn().mockResolvedValue(undefined),
  invokeAttachWindow: vi.fn(),
  invokeResize: vi.fn().mockResolvedValue(undefined),
}));

import { createWindowSessionStore } from '../stores/session-store';
import { SessionController, type TerminalSink } from './session-controller';

const WINDOW_ID = 'w1-k3f9xa';

function recordingSink(): { sink: TerminalSink; calls: string[] } {
  const calls: string[] = [];
  const sink: TerminalSink = {
    invalidate: () => {
      calls.push('invalidate');
    },
    measure: () => {
      calls.push('measure');
      return null;
    },
    focus: () => {
      calls.push('focus');
    },
  };
  return { sink, calls };
}

describe('SessionController.focusTerminal', () => {
  it('focuses the connected terminal exactly once and touches nothing else on it', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    const { sink, calls } = recordingSink();
    controller.setSink(sink);

    controller.focusTerminal();

    expect(calls).toEqual(['focus']);
  });

  it('focuses the terminal most recently connected, not one it replaced', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());
    const first = recordingSink();
    const second = recordingSink();
    controller.setSink(first.sink);
    controller.setSink(second.sink);

    controller.focusTerminal();

    expect(first.calls).toEqual([]);
    expect(second.calls).toEqual(['focus']);
  });

  it('does nothing before a terminal is connected, and focuses it once one is', () => {
    const controller = new SessionController(WINDOW_ID, createWindowSessionStore());

    expect(() => controller.focusTerminal()).not.toThrow();

    const { sink, calls } = recordingSink();
    controller.setSink(sink);
    controller.focusTerminal();

    expect(calls).toEqual(['focus']);
  });
});
