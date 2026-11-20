/**
 * The notification log of things that happened: each event is its own entry, so a
 * failure that repeats, or a window that ends while another is in Focus, is still
 * there to read after the next one arrives. Pure and in memory only.
 */
import type { DaemonRecoveryStatus } from '../session/recovery-types';
import type { WindowGoneEvent } from '../session/shell-state';
import { banners, describeExitedWindow } from '../ui-messages';
import { classifyNotice } from './notice-model';

/** The kinds of event the log records. */
export type EventKey =
  | 'daemon-reconnected'
  | 'operation-failure'
  | 'window-exit'
  | 'restore-load-failed'
  | 'restore-not-restored';

/** An event before the log numbers it. */
export interface NewEvent {
  key: EventKey;
  tone: 'info' | 'warning' | 'error';
  /** The sentence shown for the event, already resolved for the active locale. */
  text: string;
  /** Whether the event may pop up as a toast; `false` when something else already shows it. */
  toast: boolean;
  /** The window the event is about, `null` when it is about no window. */
  windowId: string | null;
}

/** An event as the log keeps it. */
export interface LoggedEvent extends NewEvent {
  /** Unique across the log's lifetime, so a dismissed number is never handed out again. */
  seq: number;
  /** When it was recorded, `HH:mm`, built once by whoever records it; this module reads no clock. */
  at: string;
}

/** The recorded events, oldest first. */
export type EventLog = readonly LoggedEvent[];

/** How many events the log keeps; the oldest goes first. */
const EVENT_LOG_LIMIT = 50;

/**
 * Records one event, numbering it from the counter.
 * @param log the events recorded so far
 * @param memory the counter, with whatever else its owner keeps beside it
 * @param event the event to record
 * @param at when it is recorded, `HH:mm`
 * @returns the log with the event, dropping the oldest past the limit, and the counter moved past it
 */
export function appendEvent<M extends { nextSeq: number }>(
  log: EventLog,
  memory: M,
  event: NewEvent,
  at: string
): { log: EventLog; memory: M } {
  const entry: LoggedEvent = { ...event, seq: memory.nextSeq, at };
  return {
    log: [...log, entry].slice(-EVENT_LOG_LIMIT),
    memory: { ...memory, nextSeq: memory.nextSeq + 1 },
  };
}

/**
 * Removes the one event the user dismissed.
 * @param log the events recorded so far
 * @param seq the number of the dismissed event
 */
export function dismissEvent(log: EventLog, seq: number): EventLog {
  return log.filter((entry) => entry.seq !== seq);
}

/** A window that left the sidebar, with what the shell knows about why. */
export type GoneWindow = Omit<WindowGoneEvent, 'type'> & {
  /** Whether this GUI closed it, which the user already knows about. */
  goneByUser: boolean;
  /** Whether Focus shows a card for it, which already tells the user it ended. */
  carded: boolean;
};

/**
 * Makes the event for one window that ended.
 * @param gone the window that left the sidebar
 * @returns the event, or `null` when this GUI closed the window
 */
export function windowExitEvent(gone: GoneWindow): NewEvent | null {
  if (gone.goneByUser) {
    return null;
  }
  const name =
    gone.orderBefore.find((entry) => entry.windowId === gone.windowId)?.name ?? gone.windowId;
  return {
    key: 'window-exit',
    tone: classifyNotice('window-exit', { exitCode: gone.exitCode }).tone,
    text: describeExitedWindow(name, gone.exitCode),
    toast: !gone.carded,
    windowId: gone.windowId,
  };
}

/** The kinds of event that report an action that did not go through. */
export type FailureEventKey = Extract<
  EventKey,
  'operation-failure' | 'restore-load-failed' | 'restore-not-restored'
>;

/**
 * Makes the event for an action that did not go through.
 * @param key which kind of failure it is
 * @param text the sentence to show, already resolved for the active locale
 */
export function failureEvent(key: FailureEventKey, text: string): NewEvent {
  return {
    key,
    tone: classifyNotice(key, {}).tone,
    text,
    toast: true,
    windowId: null,
  };
}

/**
 * Makes the event for the connection to the daemon coming back.
 * @param before the recovery status before the update
 * @param after the recovery status after the update
 * @returns the event, or `null` unless recovery went from trying back to connected
 */
export function daemonReconnectedEvent(
  before: DaemonRecoveryStatus,
  after: DaemonRecoveryStatus
): NewEvent | null {
  // idle to idle is also the first connection, which reconnects nothing.
  if (before === 'idle' || after !== 'idle') {
    return null;
  }
  return {
    key: 'daemon-reconnected',
    tone: classifyNotice('daemon-reconnected', {}).tone,
    text: banners.daemonReconnected,
    toast: true,
    windowId: null,
  };
}
