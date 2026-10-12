/**
 * Session state of one window, created per window so several panes can be
 * open at once.
 *
 * The store carries state, not wording: display strings are resolved in the
 * UI layer.
 */

import { writable, type Readable } from 'svelte/store';

/** Lifecycle phase of a session as far as the WebView can observe it. */
export type SessionPhase =
  /** Waiting for the attach handshake to complete. */
  | 'connecting'
  /** Attached to a live child process. */
  | 'attached'
  /** The child process has exited; the terminal is read-only. */
  | 'exited'
  /** Another window took the session over; no reconnect is attempted. */
  | 'superseded'
  /** The connection failed or was lost; the user can retry. */
  | 'failed';

/** Observable state of the session one window shows. */
export interface SessionState {
  /** Current lifecycle phase. */
  phase: SessionPhase;
  /** Exit code, set once the child process has exited. */
  exitCode: number | null;
  /** Technical detail of the failure, for display next to the banner. */
  failureDetail: string | null;
}

/** Store of [`SessionState`] with the transitions the session can make. */
export interface SessionStore extends Readable<SessionState> {
  /** Enters `connecting`, clearing any previous failure. */
  markConnecting: () => void;
  /** Enters `attached`. */
  markAttached: () => void;
  /** Enters `exited`, recording the exit code when it is known. */
  markExited: (exitCode: number | null) => void;
  /** Enters `superseded`. */
  markSuperseded: () => void;
  /** Enters `failed`, recording the technical detail of the failure. */
  markFailed: (detail: string) => void;
}

/**
 * Reports whether the session accepts interaction: only an attached session
 * with a live child process takes input and resizes (the daemon answers both
 * with an error once the child has exited).
 */
export function isSessionInteractive(state: SessionState): boolean {
  return state.phase === 'attached';
}

/** Creates the session store of one window. */
export function createWindowSessionStore(): SessionStore {
  const { subscribe, set } = writable<SessionState>({
    phase: 'connecting',
    exitCode: null,
    failureDetail: null,
  });
  return {
    subscribe,
    markConnecting: () => {
      set({ phase: 'connecting', exitCode: null, failureDetail: null });
    },
    markAttached: () => {
      set({ phase: 'attached', exitCode: null, failureDetail: null });
    },
    markExited: (exitCode) => {
      set({ phase: 'exited', exitCode, failureDetail: null });
    },
    markSuperseded: () => {
      set({ phase: 'superseded', exitCode: null, failureDetail: null });
    },
    markFailed: (detail) => {
      set({ phase: 'failed', exitCode: null, failureDetail: detail });
    },
  };
}
