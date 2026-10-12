import { describe, it, expect, vi } from 'vitest';
import {
  SESSION_EVENT,
  MIN_PTY_COLS,
  MIN_PTY_ROWS,
  MAX_PTY_DIMENSION,
  narrowExitPayload,
  narrowDisconnectedPayload,
  narrowSupersededPayload,
  narrowDaemonSpawnWarningPayload,
  narrowAttachResponse,
  narrowWindowInfo,
  narrowWindowInfoList,
  isValidSize,
  BoundaryError,
  narrowRestoreCandidateList,
  MAX_DISPLAY_FIELD_CHARS,
  narrowApprovalRequest,
  narrowApprovalRequestedPayload,
  narrowApprovalWithdrawnPayload,
  narrowApprovalResetPayload,
  narrowApprovalRequestList,
  narrowApprovalSyncOutcome,
  narrowDecideApprovalOutcome,
  narrowApprovalRegistrationRefusedPayload,
  narrowCloseOutcome,
  narrowChatMessages,
  narrowWindowOpenFailure,
  narrowWindowListPayload,
} from './session-boundary';

/**
 * Sample of the window id the daemon assigns. The boundary treats it as an
 * opaque string, so only its type matters here, never its shape.
 */
const WINDOW_ID = 'w1-k3f9xa';

describe('SESSION_EVENT', () => {
  it('defines the five canonical session events of a window', () => {
    expect(SESSION_EVENT.exit).toBe('session://exit');
    expect(SESSION_EVENT.disconnected).toBe('session://disconnected');
    expect(SESSION_EVENT.superseded).toBe('session://superseded');
    expect(SESSION_EVENT.daemonSpawnWarning).toBe('session://daemon-spawn-warning');
  });

  it('adds the three approval events', () => {
    expect(SESSION_EVENT.approvalRequested).toBe('session://approval-requested');
    expect(SESSION_EVENT.approvalWithdrawn).toBe('session://approval-withdrawn');
    expect(SESSION_EVENT.approvalReset).toBe('session://approval-reset');
  });

  it('adds the approver-registration-refused event', () => {
    expect(SESSION_EVENT.approvalRegistrationRefused).toBe(
      'session://approval-registration-refused'
    );
  });
});

describe('narrowExitPayload', () => {
  it('accepts valid exit payload with positive exit code', () => {
    const payload = narrowExitPayload({ exit_code: 0, window_id: WINDOW_ID });
    expect(payload).toEqual({ exit_code: 0, window_id: WINDOW_ID });
  });

  it('accepts negative exit code', () => {
    const payload = narrowExitPayload({ exit_code: -1, window_id: WINDOW_ID });
    expect(payload).toEqual({ exit_code: -1, window_id: WINDOW_ID });
  });

  it('accepts positive exit code', () => {
    const payload = narrowExitPayload({ exit_code: 42, window_id: WINDOW_ID });
    expect(payload).toEqual({ exit_code: 42, window_id: WINDOW_ID });
  });

  it('rejects non-integer exit code', () => {
    expect(() => narrowExitPayload({ exit_code: 3.14, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
    expect(() => narrowExitPayload({ exit_code: '0', window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
  });

  it('rejects missing exit_code field', () => {
    expect(() => narrowExitPayload({ window_id: WINDOW_ID })).toThrow(BoundaryError);
  });

  it('ignores extra fields', () => {
    const payload = narrowExitPayload({ exit_code: 1, window_id: WINDOW_ID, extra: 'ignored' });
    expect(payload).toEqual({ exit_code: 1, window_id: WINDOW_ID });
  });

  it('keeps the window id the core reported', () => {
    const payload = narrowExitPayload({ exit_code: 1, window_id: 'w7-k3f9xa' });
    expect(payload.window_id).toBe('w7-k3f9xa');
  });

  it('rejects missing window_id field', () => {
    expect(() => narrowExitPayload({ exit_code: 1 })).toThrow(BoundaryError);
  });

  it('rejects non-string window_id', () => {
    expect(() => narrowExitPayload({ exit_code: 1, window_id: 1 })).toThrow(BoundaryError);
    expect(() => narrowExitPayload({ exit_code: 1, window_id: null })).toThrow(BoundaryError);
    expect(() => narrowExitPayload({ exit_code: 1, window_id: undefined })).toThrow(BoundaryError);
  });
});

describe('narrowDisconnectedPayload', () => {
  it('accepts valid disconnected payload', () => {
    const payload = narrowDisconnectedPayload({ reason: 'pipe_closed', window_id: WINDOW_ID });
    expect(payload).toEqual({ reason: 'pipe_closed', window_id: WINDOW_ID });
  });

  it('accepts empty reason string', () => {
    const payload = narrowDisconnectedPayload({ reason: '', window_id: WINDOW_ID });
    expect(payload).toEqual({ reason: '', window_id: WINDOW_ID });
  });

  it('rejects missing reason field', () => {
    expect(() => narrowDisconnectedPayload({ window_id: WINDOW_ID })).toThrow(BoundaryError);
  });

  it('rejects non-string reason', () => {
    expect(() => narrowDisconnectedPayload({ reason: 123, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
    expect(() => narrowDisconnectedPayload({ reason: null, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
  });

  it('ignores extra fields', () => {
    const payload = narrowDisconnectedPayload({
      reason: 'pipe_closed',
      window_id: WINDOW_ID,
      extra: 'ignored',
    });
    expect(payload).toEqual({ reason: 'pipe_closed', window_id: WINDOW_ID });
  });

  it('keeps the window id the core reported', () => {
    const payload = narrowDisconnectedPayload({ reason: 'pipe_closed', window_id: 'w7-k3f9xa' });
    expect(payload.window_id).toBe('w7-k3f9xa');
  });

  it('rejects missing window_id field', () => {
    expect(() => narrowDisconnectedPayload({ reason: 'pipe_closed' })).toThrow(BoundaryError);
  });

  it('rejects non-string window_id', () => {
    expect(() => narrowDisconnectedPayload({ reason: 'pipe_closed', window_id: 1 })).toThrow(
      BoundaryError
    );
    expect(() => narrowDisconnectedPayload({ reason: 'pipe_closed', window_id: null })).toThrow(
      BoundaryError
    );
    expect(() =>
      narrowDisconnectedPayload({ reason: 'pipe_closed', window_id: undefined })
    ).toThrow(BoundaryError);
  });
});

describe('narrowSupersededPayload', () => {
  it('accepts valid superseded payload', () => {
    const payload = narrowSupersededPayload({
      reason: 'new_session_attached',
      window_id: WINDOW_ID,
    });
    expect(payload).toEqual({ reason: 'new_session_attached', window_id: WINDOW_ID });
  });

  it('accepts empty reason string', () => {
    const payload = narrowSupersededPayload({ reason: '', window_id: WINDOW_ID });
    expect(payload).toEqual({ reason: '', window_id: WINDOW_ID });
  });

  it('rejects missing reason field', () => {
    expect(() => narrowSupersededPayload({ window_id: WINDOW_ID })).toThrow(BoundaryError);
  });

  it('rejects non-string reason', () => {
    expect(() => narrowSupersededPayload({ reason: 123, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
  });

  it('ignores extra fields', () => {
    const payload = narrowSupersededPayload({
      reason: 'new_session_attached',
      window_id: WINDOW_ID,
      extra: 'ignored',
    });
    expect(payload).toEqual({ reason: 'new_session_attached', window_id: WINDOW_ID });
  });

  it('keeps the window id the core reported', () => {
    const payload = narrowSupersededPayload({
      reason: 'new_session_attached',
      window_id: 'w7-k3f9xa',
    });
    expect(payload.window_id).toBe('w7-k3f9xa');
  });

  it('rejects missing window_id field', () => {
    expect(() => narrowSupersededPayload({ reason: 'new_session_attached' })).toThrow(
      BoundaryError
    );
  });

  it('rejects non-string window_id', () => {
    expect(() => narrowSupersededPayload({ reason: 'new_session_attached', window_id: 1 })).toThrow(
      BoundaryError
    );
    expect(() =>
      narrowSupersededPayload({ reason: 'new_session_attached', window_id: null })
    ).toThrow(BoundaryError);
    expect(() =>
      narrowSupersededPayload({ reason: 'new_session_attached', window_id: undefined })
    ).toThrow(BoundaryError);
  });
});

describe('narrowDaemonSpawnWarningPayload', () => {
  it('accepts valid daemon spawn warning payload', () => {
    const payload = narrowDaemonSpawnWarningPayload({ message: 'Failed to start daemon' });
    expect(payload).toEqual({ message: 'Failed to start daemon' });
  });

  it('accepts empty message string', () => {
    const payload = narrowDaemonSpawnWarningPayload({ message: '' });
    expect(payload).toEqual({ message: '' });
  });

  it('rejects missing message field', () => {
    expect(() => narrowDaemonSpawnWarningPayload({})).toThrow(BoundaryError);
  });

  it('rejects non-string message', () => {
    expect(() => narrowDaemonSpawnWarningPayload({ message: 123 })).toThrow(BoundaryError);
  });

  it('ignores extra fields', () => {
    const payload = narrowDaemonSpawnWarningPayload({ message: 'Warning', extra: 'ignored' });
    expect(payload).toEqual({ message: 'Warning' });
  });
});

describe('narrowAttachResponse', () => {
  it('accepts valid attach response', () => {
    const response = narrowAttachResponse({
      child_exited: false,
      exit_code: null,
    });
    expect(response).toEqual({
      child_exited: false,
      exit_code: null,
    });
  });

  it('accepts child_exited with exit code', () => {
    const response = narrowAttachResponse({
      child_exited: true,
      exit_code: 42,
    });
    expect(response).toEqual({
      child_exited: true,
      exit_code: 42,
    });
  });

  it('accepts undefined exit_code as null', () => {
    const response = narrowAttachResponse({
      child_exited: false,
      exit_code: undefined,
    });
    expect(response).toEqual({
      child_exited: false,
      exit_code: null,
    });
  });

  it('rejects missing child_exited', () => {
    expect(() =>
      narrowAttachResponse({
        exit_code: null,
      })
    ).toThrow(BoundaryError);
  });

  it('accepts missing exit_code field and treats as null', () => {
    const response = narrowAttachResponse({
      child_exited: false,
    });
    expect(response).toEqual({
      child_exited: false,
      exit_code: null,
    });
  });

  it('rejects non-boolean child_exited', () => {
    expect(() =>
      narrowAttachResponse({
        child_exited: 'false',
        exit_code: null,
      })
    ).toThrow(BoundaryError);
  });

  it('rejects non-integer exit_code when not null', () => {
    expect(() =>
      narrowAttachResponse({
        child_exited: true,
        exit_code: 3.14,
      })
    ).toThrow(BoundaryError);
  });

  it('ignores extra fields', () => {
    const response = narrowAttachResponse({
      child_exited: false,
      exit_code: null,
      extra: 'ignored',
    });
    expect(response).toEqual({
      child_exited: false,
      exit_code: null,
    });
  });
});

describe('isValidSize', () => {
  const size = (cols: number, rows: number) => ({ cols, rows });

  it('accepts the smallest valid size', () => {
    expect(isValidSize(size(MIN_PTY_COLS, MIN_PTY_ROWS))).toBe(true);
    expect(isValidSize(size(2, 1))).toBe(true);
  });

  it('rejects one column, which the core cannot model', () => {
    expect(isValidSize(size(1, 24))).toBe(false);
  });

  it('accepts the largest valid size', () => {
    expect(isValidSize(size(MAX_PTY_DIMENSION, MAX_PTY_DIMENSION))).toBe(true);
    expect(isValidSize(size(1000, 1000))).toBe(true);
  });

  it('accepts mid-range sizes', () => {
    expect(isValidSize(size(80, 24))).toBe(true);
    expect(isValidSize(size(500, 999))).toBe(true);
  });

  it('rejects zero', () => {
    expect(isValidSize(size(0, 24))).toBe(false);
    expect(isValidSize(size(80, 0))).toBe(false);
  });

  it('rejects negative dimensions', () => {
    expect(isValidSize(size(-1, 24))).toBe(false);
    expect(isValidSize(size(80, -100))).toBe(false);
  });

  it('rejects above maximum dimension', () => {
    expect(isValidSize(size(MAX_PTY_DIMENSION + 1, 24))).toBe(false);
    expect(isValidSize(size(80, 1001))).toBe(false);
  });

  it('rejects non-integer values', () => {
    expect(isValidSize(size(3.14, 24))).toBe(false);
    expect(isValidSize(size(80, 1.5))).toBe(false);
  });

  it('rejects infinity and NaN', () => {
    expect(isValidSize(size(Infinity, 24))).toBe(false);
    expect(isValidSize(size(80, -Infinity))).toBe(false);
    expect(isValidSize(size(NaN, 24))).toBe(false);
  });
});

describe('BoundaryError', () => {
  it('creates error with name and message', () => {
    const error = new BoundaryError('test message');
    expect(error).toBeInstanceOf(Error);
    expect(error.name).toBe('BoundaryError');
    expect(error.message).toBe('test message');
  });
});

// narrowRestoreCandidateList is the untrusted-store choke point for the
// restore UI.

describe('narrowWindowInfo: layout_json (conditional field)', () => {
  const base = {
    window_id: WINDOW_ID,
    name: 'main',
    vendor: null,
    cwd: '/x',
    status: 'running',
  };

  it('does not add a layout_json own property when the wire entry omits it', () => {
    const info = narrowWindowInfo({ ...base, exit_code: null });
    expect(Object.prototype.hasOwnProperty.call(info, 'layout_json')).toBe(false);
  });

  it('carries layout_json through unchanged when the wire entry has a string value', () => {
    const raw = '{"v":1,"pos":null,"size":null,"z":0,"min":false,"max":false,"tab":0}';
    const info = narrowWindowInfo({ ...base, exit_code: null, layout_json: raw });
    expect(info.layout_json).toBe(raw);
  });
});

// ---------------------------------------------------------------------------
// The room a listed window belongs to. A window the core has no room for
// omits the field from the wire, so the narrowed entry has no such own
// property either (exactOptionalPropertyTypes), the same discipline
// `layout_json` follows. The shapes the field may take are the ones a pending
// request's room may take, so the two records are read the same way.
// ---------------------------------------------------------------------------

/** A well-formed `list_windows` entry, before a room form is applied. */
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

/** What `windowInfoWire()` narrows to when it carries no room. */
const ROOMLESS_WINDOW_INFO = {
  window_id: WINDOW_ID,
  name: 'main',
  vendor: null,
  cwd: '/x',
  detected: { status: 'running' },
  exit_code: null,
};

/** A room both records accept. */
const WELL_FORMED_ROOM = { room_id: 'r-2b7c1d', name: 'yagura' };

/** Room values neither record accepts. */
const MALFORMED_ROOMS: readonly { label: string; room: unknown }[] = [
  { label: 'a string in place of an object', room: 'yagura' },
  { label: 'an array in place of an object', room: [] },
  { label: 'no room_id', room: { name: 'yagura' } },
  { label: 'no name', room: { room_id: 'r-2b7c1d' } },
  { label: 'an empty room_id', room: { room_id: '', name: 'yagura' } },
  { label: 'an empty name', room: { room_id: 'r-2b7c1d', name: '' } },
  { label: 'a numeric room_id', room: { room_id: 7, name: 'yagura' } },
  { label: 'a numeric name', room: { room_id: 'r-2b7c1d', name: 7 } },
];

describe('narrowWindowInfo: the room the window belongs to', () => {
  it('does not add a room own property when the wire entry omits it', () => {
    const info = narrowWindowInfo(windowInfoWire());

    expect(Object.prototype.hasOwnProperty.call(info, 'room')).toBe(false);
    expect(info).toEqual(ROOMLESS_WINDOW_INFO);
  });

  it('does not add a room own property when the wire entry carries an explicit null', () => {
    const info = narrowWindowInfo(windowInfoWire({ room: null }));

    expect(Object.prototype.hasOwnProperty.call(info, 'room')).toBe(false);
    expect(info).toEqual(ROOMLESS_WINDOW_INFO);
  });

  it('carries a well-formed room through unchanged', () => {
    const info = narrowWindowInfo(windowInfoWire({ room: WELL_FORMED_ROOM }));

    expect(info).toEqual({ ...ROOMLESS_WINDOW_INFO, room: WELL_FORMED_ROOM });
  });

  it('carries a room and a layout_json side by side', () => {
    const raw = '{"v":1,"pos":null,"size":null,"z":0,"min":false,"max":false,"tab":0}';

    const info = narrowWindowInfo(windowInfoWire({ room: WELL_FORMED_ROOM, layout_json: raw }));

    expect(info).toEqual({ ...ROOMLESS_WINDOW_INFO, room: WELL_FORMED_ROOM, layout_json: raw });
  });
});

// ---------------------------------------------------------------------------
// The status and the reason of a listed window leave this boundary as one
// value. The window-list route is the tolerant one: a pair that does not hold
// together, and a reason this build does not know, cost the reason alone --
// the entry survives, and one warning says so. detection_warning is not part
// of the pair and keeps its own optional key.
// ---------------------------------------------------------------------------

/** The six wire values `UnknownReason` narrows. */
const UNKNOWN_REASON_VALUES = [
  'no_definition',
  'no_rule_matched',
  'screen_lost',
  'detector_stopped',
  'not_yet_judged',
  'declared_unknown',
];

/** The statuses a listed window may report that can never carry a reason. */
const KNOWN_WINDOW_STATUS_VALUES = [
  'running',
  'waiting_input',
  'waiting_approval',
  'interrupted',
  'idle',
  'exited',
];

describe('narrowWindowInfo: the status and its reason narrow to one value', () => {
  it('does not add an unknown_reason own property when the wire entry omits it', () => {
    const info = narrowWindowInfo(windowInfoWire());
    expect(Object.prototype.hasOwnProperty.call(info, 'unknown_reason')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(info, 'status')).toBe(false);
  });

  it('does not add a detection_warning own property when the wire entry omits it', () => {
    const info = narrowWindowInfo(windowInfoWire());
    expect(Object.prototype.hasOwnProperty.call(info, 'detection_warning')).toBe(false);
  });

  it.each(KNOWN_WINDOW_STATUS_VALUES)('gives the %s arm no reason own property', (status) => {
    const info = narrowWindowInfo(windowInfoWire({ status }));
    expect(info.detected).toEqual({ status });
    expect(Object.prototype.hasOwnProperty.call(info.detected, 'reason')).toBe(false);
  });

  it.each(UNKNOWN_REASON_VALUES)('puts reason %s on the unknown arm of the pair', (reason) => {
    const info = narrowWindowInfo(windowInfoWire({ status: 'unknown', unknown_reason: reason }));
    expect(info).toEqual({
      ...ROOMLESS_WINDOW_INFO,
      detected: { status: 'unknown', reason },
    });
  });

  it('accepts status "unknown" with no reason, as a null-valued reason on the unknown arm', () => {
    const info = narrowWindowInfo(windowInfoWire({ status: 'unknown' }));
    expect(info.detected).toEqual({ status: 'unknown', reason: null });
  });

  it('carries detection_warning through unchanged', () => {
    const info = narrowWindowInfo(
      windowInfoWire({ detection_warning: 'could not read the definition file' })
    );
    expect(info).toEqual({
      ...ROOMLESS_WINDOW_INFO,
      detection_warning: 'could not read the definition file',
    });
  });

  it('treats an explicit null unknown_reason the same as an omitted key', () => {
    const info = narrowWindowInfo(windowInfoWire({ status: 'unknown', unknown_reason: null }));
    expect(info.detected).toEqual({ status: 'unknown', reason: null });
  });

  it('treats an explicit null detection_warning the same as an omitted key', () => {
    const info = narrowWindowInfo(windowInfoWire({ detection_warning: null }));
    expect(Object.prototype.hasOwnProperty.call(info, 'detection_warning')).toBe(false);
  });

  it('carries an unknown_reason and a detection_warning side by side', () => {
    const info = narrowWindowInfo(
      windowInfoWire({
        status: 'unknown',
        unknown_reason: 'no_definition',
        detection_warning: 'the profile did not resolve a definition',
      })
    );
    expect(info).toEqual({
      ...ROOMLESS_WINDOW_INFO,
      detected: { status: 'unknown', reason: 'no_definition' },
      detection_warning: 'the profile did not resolve a definition',
    });
  });

  it('rejects a non-string unknown_reason', () => {
    expect(() => narrowWindowInfo(windowInfoWire({ unknown_reason: 7 }))).toThrow(BoundaryError);
  });

  it('rejects a non-string detection_warning', () => {
    expect(() => narrowWindowInfo(windowInfoWire({ detection_warning: 7 }))).toThrow(BoundaryError);
  });
});

describe('narrowWindowInfo: the window-list route drops the reason instead of the window', () => {
  it.each(KNOWN_WINDOW_STATUS_VALUES)(
    'accepts a reason carried alongside status %s, keeping the entry and dropping the reason',
    (status) => {
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
      try {
        const info = narrowWindowInfo(
          windowInfoWire({ status, unknown_reason: 'no_definition', detection_warning: 'kept' })
        );
        expect(info).toEqual({
          ...ROOMLESS_WINDOW_INFO,
          detected: { status },
          detection_warning: 'kept',
        });
        expect(warn).toHaveBeenCalledTimes(1);
      } finally {
        warn.mockRestore();
      }
    }
  );

  it('accepts a reason this build does not know, keeping the unknown arm with a null reason', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      const info = narrowWindowInfo(
        windowInfoWire({ status: 'unknown', unknown_reason: 'a_future_reason' })
      );
      expect(info).toEqual({
        ...ROOMLESS_WINDOW_INFO,
        detected: { status: 'unknown', reason: null },
      });
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it('accepts a reason this build does not know alongside a known status too, warning once', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      const info = narrowWindowInfo(
        windowInfoWire({ status: 'running', unknown_reason: 'a_future_reason' })
      );
      expect(info.detected).toEqual({ status: 'running' });
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it('warns nothing for an entry whose pair holds together', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      narrowWindowInfo(windowInfoWire({ status: 'unknown', unknown_reason: 'screen_lost' }));
      narrowWindowInfo(windowInfoWire({ status: 'running' }));
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it('keeps every other window of a list when one entry carries a reason it may not', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      const list = narrowWindowInfoList([
        windowInfoWire({ window_id: 'w-a' }),
        windowInfoWire({ window_id: 'w-b', status: 'idle', unknown_reason: 'no_definition' }),
        windowInfoWire({ window_id: 'w-c', status: 'unknown', unknown_reason: 'screen_lost' }),
      ]);
      expect(list.map((entry) => entry.window_id)).toEqual(['w-a', 'w-b', 'w-c']);
      expect(list.map((entry) => entry.detected)).toEqual([
        { status: 'running' },
        { status: 'idle' },
        { status: 'unknown', reason: 'screen_lost' },
      ]);
    } finally {
      warn.mockRestore();
    }
  });
});

describe('narrowWindowInfo: the generation of the reported pair', () => {
  it('does not add a status_rev own property when the wire entry omits it', () => {
    const info = narrowWindowInfo(windowInfoWire());
    expect(Object.prototype.hasOwnProperty.call(info, 'status_rev')).toBe(false);
  });

  it('does not add a status_rev own property for an explicit null', () => {
    const info = narrowWindowInfo(windowInfoWire({ status_rev: null }));
    expect(Object.prototype.hasOwnProperty.call(info, 'status_rev')).toBe(false);
  });

  it('carries the generation through unchanged, zero included', () => {
    expect(narrowWindowInfo(windowInfoWire({ status_rev: 0 })).status_rev).toBe(0);
    expect(narrowWindowInfo(windowInfoWire({ status_rev: 31 })).status_rev).toBe(31);
  });

  it('rejects a non-integer generation', () => {
    expect(() => narrowWindowInfo(windowInfoWire({ status_rev: 1.5 }))).toThrow(BoundaryError);
    expect(() => narrowWindowInfo(windowInfoWire({ status_rev: 'seven' }))).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// A chat delivery record's unknown_reason must reach
// the view, not be silently dropped -- the delivery state alone
// ("undelivered_unknown") does not say why.
// ---------------------------------------------------------------------------

/** A minimal `read_chat`-shaped reply carrying one message with one delivery. */
function chatMessagesWireWithDelivery(delivery: Record<string, unknown>): Record<string, unknown> {
  return {
    room_id: 'r-main',
    messages: [
      {
        seq: 1,
        at_ms: 0,
        from_kind: 'window',
        from_window_id: 'w-a',
        from_name: 'sender',
        body: 'hello',
        mentions: [],
        broadcast: false,
        deliveries: [delivery],
      },
    ],
    next_after_seq: 2,
    halted: null,
    needs_user: false,
    roster: [],
  };
}

/** The delivery states that can never carry a reason. */
const REASONLESS_DELIVERY_STATES = [
  'pending',
  'confirmed',
  'unconfirmed',
  'not_observed',
  'not_written',
  'write_failed',
  'undelivered_halted',
  'undelivered_exhausted',
];

/** The single delivery record of a narrowed one-message reply. */
function soleDelivery(wire: Record<string, unknown>): unknown {
  const decoded = narrowChatMessages(chatMessagesWireWithDelivery(wire));
  return decoded.messages[0]?.deliveries[0];
}

describe('narrowChatMessages: a delivery record narrows to one value', () => {
  it.each(REASONLESS_DELIVERY_STATES)('gives the %s arm no reason own property', (state) => {
    const delivery = soleDelivery({ window_id: 'w-b', state });
    expect(delivery).toEqual({ window_id: 'w-b', state });
  });

  it('keeps the delivery state readable as a top-level key of the record', () => {
    const delivery = soleDelivery({ window_id: 'w-b', state: 'confirmed' });
    expect(Object.prototype.hasOwnProperty.call(delivery ?? {}, 'state')).toBe(true);
  });

  it('carries a delivery reason through on the undelivered-unknown arm', () => {
    const delivery = soleDelivery({
      window_id: 'w-b',
      state: 'undelivered_unknown',
      unknown_reason: 'screen_lost',
    });
    expect(delivery).toEqual({
      window_id: 'w-b',
      state: 'undelivered_unknown',
      reason: 'screen_lost',
    });
  });

  it('accepts the undelivered-unknown arm with no reason, as a null-valued reason', () => {
    const delivery = soleDelivery({ window_id: 'w-b', state: 'undelivered_unknown' });
    expect(delivery).toEqual({ window_id: 'w-b', state: 'undelivered_unknown', reason: null });
  });

  it('no longer exposes an unknown_reason key on the record', () => {
    const delivery = soleDelivery({
      window_id: 'w-b',
      state: 'undelivered_unknown',
      unknown_reason: 'screen_lost',
    });
    expect(Object.prototype.hasOwnProperty.call(delivery ?? {}, 'unknown_reason')).toBe(false);
  });
});

describe('narrowChatMessages: the history route drops the reason instead of the record', () => {
  it.each(REASONLESS_DELIVERY_STATES)(
    'accepts a reason carried alongside state %s, keeping the record and dropping the reason',
    (state) => {
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
      try {
        const delivery = soleDelivery({
          window_id: 'w-b',
          state,
          unknown_reason: 'screen_lost',
        });
        expect(delivery).toEqual({ window_id: 'w-b', state });
        expect(warn).toHaveBeenCalledTimes(1);
      } finally {
        warn.mockRestore();
      }
    }
  );

  it('accepts a reason this build does not know, keeping the arm with a null reason', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      const delivery = soleDelivery({
        window_id: 'w-b',
        state: 'undelivered_unknown',
        unknown_reason: 'a_future_reason',
      });
      expect(delivery).toEqual({ window_id: 'w-b', state: 'undelivered_unknown', reason: null });
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it('warns nothing for a record whose pair holds together', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    try {
      soleDelivery({
        window_id: 'w-b',
        state: 'undelivered_unknown',
        unknown_reason: 'screen_lost',
      });
      soleDelivery({ window_id: 'w-b', state: 'confirmed' });
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it('still rejects a delivery state this build does not know', () => {
    expect(() =>
      narrowChatMessages(
        chatMessagesWireWithDelivery({ window_id: 'w-b', state: 'delivered_by_carrier_pigeon' })
      )
    ).toThrow(BoundaryError);
  });
});

function candidate(overrides: Record<string, unknown> = {}): Record<string, unknown> {
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

describe('narrowRestoreCandidateList', () => {
  it('accepts a well-formed list and preserves the execution-value fields verbatim', () => {
    const entries = narrowRestoreCandidateList([candidate()]);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      position: 0,
      command: ['claude'],
      cwd: '/home/user/project',
      name: 'main',
      vendor: null,
      detection: null,
      layout: null,
    });
  });

  it.each([
    ['position', { position: 'zero' }],
    ['command', { command: 'claude' }],
    ['cwd', { cwd: 42 }],
    ['name', { name: null }],
  ])('drops (only) the row whose %s has the wrong type, keeping the rest', (_label, override) => {
    const good = candidate({ position: 0, name: 'good' });
    const bad = candidate({ position: 1, name: 'bad', ...override });
    const entries = narrowRestoreCandidateList([good, bad]);
    expect(entries).toHaveLength(1);
    expect(entries[0]?.name).toBe('good');
  });

  it('maps a missing layout_json key to layout: null, without dropping the row', () => {
    const entries = narrowRestoreCandidateList([candidate()]);
    expect(entries).toHaveLength(1);
    expect(entries[0]?.layout).toBeNull();
  });

  it('maps a JSON-null layout_json to layout: null, without dropping the row', () => {
    const entries = narrowRestoreCandidateList([candidate({ layout_json: null })]);
    expect(entries).toHaveLength(1);
    expect(entries[0]?.layout).toBeNull();
  });

  it('maps an unparseable layout_json to layout: null, without dropping the row (parseLayoutJson contract)', () => {
    const entries = narrowRestoreCandidateList([candidate({ layout_json: '{not json' })]);
    expect(entries).toHaveLength(1);
    expect(entries[0]?.layout).toBeNull();
  });

  it('parses a well-formed layout_json into the layout field', () => {
    const raw =
      '{"v":1,"pos":{"x":1,"y":2},"size":{"width":3,"height":4},"z":0,"min":false,"max":false,"tab":0}';
    const entries = narrowRestoreCandidateList([candidate({ layout_json: raw })]);
    expect(entries[0]?.layout).toEqual({
      v: 1,
      pos: { x: 1, y: 2 },
      size: { width: 3, height: 4 },
      z: 0,
      min: false,
      max: false,
      tab: 0,
    });
  });

  it('exports MAX_DISPLAY_FIELD_CHARS as 4096', () => {
    expect(MAX_DISPLAY_FIELD_CHARS).toBe(4096);
  });

  it('strips a C0 control character from displayName but keeps it in the raw name', () => {
    const controlChar = String.fromCharCode(7); // BEL, a C0 control character
    const withControl = 'main' + controlChar + 'tab';
    const entries = narrowRestoreCandidateList([candidate({ name: withControl })]);
    expect(entries[0]?.name).toBe(withControl);
    expect(entries[0]?.displayName).not.toContain(controlChar);
  });

  it('caps displayCwd at MAX_DISPLAY_FIELD_CHARS code points (with an ellipsis), keeping the raw cwd intact', () => {
    const longCwd = '/' + 'a'.repeat(MAX_DISPLAY_FIELD_CHARS + 50);
    const entries = narrowRestoreCandidateList([candidate({ cwd: longCwd })]);
    expect(entries[0]?.cwd).toBe(longCwd);
    expect(Array.from(entries[0]?.displayCwd ?? '').length).toBeLessThanOrEqual(
      MAX_DISPLAY_FIELD_CHARS + 1
    );
    expect(entries[0]?.displayCwd).not.toBe(longCwd);
  });
});

// ---------------------------------------------------------------------------
// Approval protocol: ApprovalRequest and the three session events
// that carry it. The shape is snake_case, copied verbatim from the Rust
// struct; requester.kind decides which fields are required ("window" always carries window_id+name,
// "external" carries neither).
// ---------------------------------------------------------------------------

/** A well-formed ApprovalRequest wire value with a "window" requester. */
function approvalRequestWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 'appr1',
    tool: 'close_window',
    requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
    target: { window_id: 'w-target', name: 'target window', status: 'running' },
    text: null,
    ...overrides,
  };
}

describe('narrowApprovalRequest', () => {
  it('accepts a well-formed request with a "window" requester', () => {
    const request = narrowApprovalRequest(approvalRequestWire(), 'test');
    expect(request).toEqual({
      request_id: 'appr1',
      tool: 'close_window',
      requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
      target: { window_id: 'w-target', name: 'target window', status: 'running' },
      text: null,
    });
  });

  it('accepts a well-formed request with an "external" requester (window_id and name both absent)', () => {
    const request = narrowApprovalRequest(
      approvalRequestWire({ requester: { kind: 'external', window_id: null, name: null } }),
      'test'
    );
    expect(request.requester).toEqual({ kind: 'external', window_id: null, name: null });
  });

  it('accepts a non-null text field (send_message full body)', () => {
    const request = narrowApprovalRequest(
      approvalRequestWire({ tool: 'send_message', text: 'hello from the requester' }),
      'test'
    );
    expect(request.text).toBe('hello from the requester');
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalRequest('nope', 'test')).toThrow(BoundaryError);
    expect(() => narrowApprovalRequest(null, 'test')).toThrow(BoundaryError);
  });

  it('rejects missing request_id', () => {
    const wire = approvalRequestWire();
    delete wire['request_id'];
    expect(() => narrowApprovalRequest(wire, 'test')).toThrow(BoundaryError);
  });

  it('rejects non-string request_id', () => {
    expect(() => narrowApprovalRequest(approvalRequestWire({ request_id: 42 }), 'test')).toThrow(
      BoundaryError
    );
  });

  it('rejects missing tool', () => {
    const wire = approvalRequestWire();
    delete wire['tool'];
    expect(() => narrowApprovalRequest(wire, 'test')).toThrow(BoundaryError);
  });

  it('rejects non-string tool', () => {
    expect(() => narrowApprovalRequest(approvalRequestWire({ tool: 7 }), 'test')).toThrow(
      BoundaryError
    );
  });

  it('rejects missing requester', () => {
    const wire = approvalRequestWire();
    delete wire['requester'];
    expect(() => narrowApprovalRequest(wire, 'test')).toThrow(BoundaryError);
  });

  it('rejects a non-object requester', () => {
    expect(() =>
      narrowApprovalRequest(approvalRequestWire({ requester: 'window' }), 'test')
    ).toThrow(BoundaryError);
  });

  it('rejects a non-string requester.kind', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({ requester: { kind: 1, window_id: 'w-req', name: 'x' } }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('normalizes a requester.kind this build does not recognize to "unknown" rather than throwing (fail-safe)', () => {
    const request = narrowApprovalRequest(
      approvalRequestWire({
        requester: { kind: 'a_future_kind', window_id: null, name: null },
      }),
      'test'
    );
    expect(request.requester.kind).toBe('unknown');
  });

  it('rejects requester.kind "window" missing window_id', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({ requester: { kind: 'window', name: 'requester window' } }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('rejects requester.kind "window" missing name', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({ requester: { kind: 'window', window_id: 'w-req' } }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('rejects requester.kind "window" with a non-string window_id', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({
          requester: { kind: 'window', window_id: 7, name: 'requester window' },
        }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('accepts a missing target, producing a request with no target property', () => {
    const wire = approvalRequestWire();
    delete wire['target'];
    const request = narrowApprovalRequest(wire, 'test');
    expect('target' in request).toBe(false);
  });

  it('rejects target missing window_id', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({ target: { name: 'target window', status: 'running' } }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('rejects target missing name', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({ target: { window_id: 'w-target', status: 'running' } }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('rejects an unrecognized target.status', () => {
    expect(() =>
      narrowApprovalRequest(
        approvalRequestWire({
          target: { window_id: 'w-target', name: 'target window', status: 'not_a_status' },
        }),
        'test'
      )
    ).toThrow(BoundaryError);
  });

  it('rejects a non-string, non-null text field', () => {
    expect(() => narrowApprovalRequest(approvalRequestWire({ text: 42 }), 'test')).toThrow(
      BoundaryError
    );
  });

  it('accepts an absent text field as null', () => {
    const wire = approvalRequestWire();
    delete wire['text'];
    const request = narrowApprovalRequest(wire, 'test');
    expect(request.text).toBeNull();
  });
});

describe('narrowApprovalRequestedPayload (session://approval-requested)', () => {
  it('accepts a valid payload and narrows the nested request', () => {
    const payload = narrowApprovalRequestedPayload({ request: approvalRequestWire() });
    expect(payload.request.request_id).toBe('appr1');
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalRequestedPayload('nope')).toThrow(BoundaryError);
  });

  it('rejects a missing request field', () => {
    expect(() => narrowApprovalRequestedPayload({})).toThrow(BoundaryError);
  });

  it('rejects a malformed nested request (propagates the same validation as narrowApprovalRequest)', () => {
    const wire = approvalRequestWire();
    delete wire['tool'];
    expect(() => narrowApprovalRequestedPayload({ request: wire })).toThrow(BoundaryError);
  });
});

describe('narrowApprovalWithdrawnPayload (session://approval-withdrawn)', () => {
  it('accepts a valid payload', () => {
    const payload = narrowApprovalWithdrawnPayload({
      request_id: 'appr1',
      reason: 'timeout',
    });
    expect(payload).toEqual({ request_id: 'appr1', reason: 'timeout' });
  });

  it('normalizes an empty reason string to "unknown" (it is not one of the known 3 values)', () => {
    const payload = narrowApprovalWithdrawnPayload({ request_id: 'appr1', reason: '' });
    expect(payload.reason).toBe('unknown');
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalWithdrawnPayload(null)).toThrow(BoundaryError);
  });

  it('rejects missing request_id', () => {
    expect(() => narrowApprovalWithdrawnPayload({ reason: 'timeout' })).toThrow(BoundaryError);
  });

  it('rejects non-string request_id', () => {
    expect(() => narrowApprovalWithdrawnPayload({ request_id: 1, reason: 'timeout' })).toThrow(
      BoundaryError
    );
  });

  it('rejects missing reason', () => {
    expect(() => narrowApprovalWithdrawnPayload({ request_id: 'appr1' })).toThrow(BoundaryError);
  });

  it('rejects non-string reason', () => {
    expect(() => narrowApprovalWithdrawnPayload({ request_id: 'appr1', reason: 3 })).toThrow(
      BoundaryError
    );
  });

  it('normalizes a reason this build does not recognize to "unknown" rather than throwing (fail-safe, forward compatibility with the daemon\'s serde `other`)', () => {
    const payload = narrowApprovalWithdrawnPayload({
      request_id: 'appr1',
      reason: 'a_future_reason',
    });
    expect(payload.reason).toBe('unknown');
  });
});

describe('narrowApprovalResetPayload (session://approval-reset, full replace)', () => {
  it('accepts a valid payload with multiple requests, preserving order', () => {
    const payload = narrowApprovalResetPayload({
      requests: [
        approvalRequestWire({ request_id: 'appr1' }),
        approvalRequestWire({ request_id: 'appr2' }),
      ],
    });
    expect(payload.requests.map((r) => r.request_id)).toEqual(['appr1', 'appr2']);
  });

  it('accepts an empty requests array', () => {
    const payload = narrowApprovalResetPayload({ requests: [] });
    expect(payload.requests).toEqual([]);
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalResetPayload('nope')).toThrow(BoundaryError);
  });

  it('rejects a missing requests field', () => {
    expect(() => narrowApprovalResetPayload({})).toThrow(BoundaryError);
  });

  it('rejects a requests field that is not an array', () => {
    expect(() => narrowApprovalResetPayload({ requests: 'nope' })).toThrow(BoundaryError);
  });

  it('rejects a malformed entry inside requests (propagates the same validation as narrowApprovalRequest)', () => {
    const wire = approvalRequestWire();
    delete wire['request_id'];
    expect(() => narrowApprovalResetPayload({ requests: [wire] })).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// The pending list arrives on two routes -- the reset event and the
// sync_approvals command's registered outcome -- so the array validation is
// one function both of them call. `narrowApprovalSyncOutcome`
// is a discriminated union on `status`: an unrecognized value is a contract
// violation for a request this side made itself, not a forward-compatible
// fallback.
// ---------------------------------------------------------------------------

describe('narrowApprovalRequestList', () => {
  it('accepts an array of well-formed requests, preserving order', () => {
    const requests = narrowApprovalRequestList(
      [approvalRequestWire({ request_id: 'appr1' }), approvalRequestWire({ request_id: 'appr2' })],
      'test'
    );
    expect(requests.map((entry) => entry.request_id)).toEqual(['appr1', 'appr2']);
  });

  it('accepts an empty array', () => {
    expect(narrowApprovalRequestList([], 'test')).toEqual([]);
  });

  it('rejects a value that is not an array', () => {
    expect(() => narrowApprovalRequestList('nope', 'test')).toThrow(BoundaryError);
    expect(() => narrowApprovalRequestList({ requests: [] }, 'test')).toThrow(BoundaryError);
    expect(() => narrowApprovalRequestList(null, 'test')).toThrow(BoundaryError);
  });

  it('rejects a malformed entry, applying the same validation as narrowApprovalRequest', () => {
    const wire = approvalRequestWire();
    delete wire['request_id'];
    expect(() => narrowApprovalRequestList([wire], 'test')).toThrow(BoundaryError);
  });
});

describe('narrowApprovalSyncOutcome (the sync_approvals command result)', () => {
  it('accepts a registered outcome and narrows its pending list', () => {
    const outcome = narrowApprovalSyncOutcome({
      status: 'registered',
      pending: [
        approvalRequestWire({ request_id: 'appr1' }),
        approvalRequestWire({ request_id: 'appr2' }),
      ],
    });
    expect(outcome.status).toBe('registered');
    expect(outcome.status === 'registered' && outcome.pending.map((e) => e.request_id)).toEqual([
      'appr1',
      'appr2',
    ]);
  });

  it('accepts a registered outcome carrying an empty pending list', () => {
    const outcome = narrowApprovalSyncOutcome({ status: 'registered', pending: [] });
    expect(outcome).toEqual({ status: 'registered', pending: [] });
  });

  it('accepts a refused outcome', () => {
    expect(narrowApprovalSyncOutcome({ status: 'refused' })).toEqual({ status: 'refused' });
  });

  it('rejects an unrecognized status', () => {
    expect(() => narrowApprovalSyncOutcome({ status: 'pending' })).toThrow(BoundaryError);
    expect(() => narrowApprovalSyncOutcome({ status: 'registered_later', pending: [] })).toThrow(
      BoundaryError
    );
  });

  it('rejects a missing status', () => {
    expect(() => narrowApprovalSyncOutcome({ pending: [] })).toThrow(BoundaryError);
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalSyncOutcome(null)).toThrow(BoundaryError);
    expect(() => narrowApprovalSyncOutcome('registered')).toThrow(BoundaryError);
  });

  it('rejects a registered outcome whose pending is not an array', () => {
    expect(() => narrowApprovalSyncOutcome({ status: 'registered', pending: 'nope' })).toThrow(
      BoundaryError
    );
    expect(() => narrowApprovalSyncOutcome({ status: 'registered' })).toThrow(BoundaryError);
  });

  it('rejects a registered outcome carrying a malformed pending entry', () => {
    const wire = approvalRequestWire();
    delete wire['request_id'];
    expect(() => narrowApprovalSyncOutcome({ status: 'registered', pending: [wire] })).toThrow(
      BoundaryError
    );
  });
});

// ---------------------------------------------------------------------------
// decide_approval outcome: the command's own return
// value is a request this side made itself, so -- unlike the fail-safe
// `reason` / `kind` normalization above -- an unrecognized value here is a
// contract violation, not a forward-compatible fallback.
// ---------------------------------------------------------------------------

describe('narrowDecideApprovalOutcome', () => {
  it('accepts "decided"', () => {
    expect(narrowDecideApprovalOutcome('decided')).toBe('decided');
  });

  it('accepts "not_pending"', () => {
    expect(narrowDecideApprovalOutcome('not_pending')).toBe('not_pending');
  });

  it('rejects an unrecognized string', () => {
    expect(() => narrowDecideApprovalOutcome('approved')).toThrow(BoundaryError);
  });

  it('rejects a non-string value', () => {
    expect(() => narrowDecideApprovalOutcome(1)).toThrow(BoundaryError);
    expect(() => narrowDecideApprovalOutcome(null)).toThrow(BoundaryError);
    expect(() => narrowDecideApprovalOutcome(undefined)).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// session://approval-registration-refused: a fieldless
// payload, so the daemon's `auth_failed` refusal always serializes to `{}`
// rather than `null` (a unit struct/`()` would serialize to `null` and fail
// `requireRecord`).
// ---------------------------------------------------------------------------

describe('narrowApprovalRegistrationRefusedPayload', () => {
  it('accepts an empty object', () => {
    expect(narrowApprovalRegistrationRefusedPayload({})).toEqual({});
  });

  it('ignores extra fields', () => {
    expect(narrowApprovalRegistrationRefusedPayload({ extra: 'ignored' })).toEqual({});
  });

  it('rejects a non-object value', () => {
    expect(() => narrowApprovalRegistrationRefusedPayload(null)).toThrow(BoundaryError);
    expect(() => narrowApprovalRegistrationRefusedPayload('nope')).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// close_window's own return value: a bare 3-value string, like
// narrowDecideApprovalOutcome above. Not found and unreachable are both
// resolved values here, never thrown -- the caller treats either as a signal
// to remove the pane.
// ---------------------------------------------------------------------------

describe('narrowCloseOutcome', () => {
  it('accepts "closed"', () => {
    expect(narrowCloseOutcome('closed')).toBe('closed');
  });

  it('accepts "not_found"', () => {
    expect(narrowCloseOutcome('not_found')).toBe('not_found');
  });

  it('accepts "unreachable"', () => {
    expect(narrowCloseOutcome('unreachable')).toBe('unreachable');
  });

  it('rejects an empty object', () => {
    expect(() => narrowCloseOutcome({})).toThrow(BoundaryError);
  });

  it('rejects an object shaped like a status wrapper', () => {
    expect(() => narrowCloseOutcome({ status: 'closed' })).toThrow(BoundaryError);
  });

  it('rejects a non-string value', () => {
    expect(() => narrowCloseOutcome(1)).toThrow(BoundaryError);
  });

  it('rejects an unrecognized string', () => {
    expect(() => narrowCloseOutcome('Closed')).toThrow(BoundaryError);
  });

  it('rejects undefined', () => {
    expect(() => narrowCloseOutcome(undefined)).toThrow(BoundaryError);
  });

  it('rejects null', () => {
    expect(() => narrowCloseOutcome(null)).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// The chat room a pending request concerns. Only a chat post carries one; for
// every other gated operation the field is omitted from the wire, and the
// narrowed request has no such own property (exactOptionalPropertyTypes), the
// same discipline `target` already follows.
// ---------------------------------------------------------------------------

/** A well-formed chat-post request: a room instead of a target window. */
function chatPostRequestWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const wire = approvalRequestWire({
    tool: 'send_chat',
    text: 'hello everyone',
    room: { room_id: 'r-2b7c1d', name: 'yagura' },
    ...overrides,
  });
  delete wire['target'];
  return wire;
}

describe('narrowApprovalRequest: the room a chat post is addressed to', () => {
  it('accepts a chat post that carries a room and no target window', () => {
    const request = narrowApprovalRequest(chatPostRequestWire(), 'test');

    expect(request).toEqual({
      request_id: 'appr1',
      tool: 'send_chat',
      requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
      text: 'hello everyone',
      room: { room_id: 'r-2b7c1d', name: 'yagura' },
    });
    expect('target' in request).toBe(false);
  });

  it('produces a request with no room property when the wire carries none', () => {
    const request = narrowApprovalRequest(approvalRequestWire(), 'test');
    expect('room' in request).toBe(false);
  });

  it('produces a request with no room property when the wire carries an explicit null', () => {
    const request = narrowApprovalRequest(chatPostRequestWire({ room: null }), 'test');
    expect('room' in request).toBe(false);
  });

  it('rejects a room that is not an object', () => {
    expect(() => narrowApprovalRequest(chatPostRequestWire({ room: 'yagura' }), 'test')).toThrow(
      BoundaryError
    );
  });

  it('rejects a room missing room_id', () => {
    expect(() =>
      narrowApprovalRequest(chatPostRequestWire({ room: { name: 'yagura' } }), 'test')
    ).toThrow(BoundaryError);
  });

  it('rejects a room missing name', () => {
    expect(() =>
      narrowApprovalRequest(chatPostRequestWire({ room: { room_id: 'r-2b7c1d' } }), 'test')
    ).toThrow(BoundaryError);
  });

  it('rejects a room whose name is not a string', () => {
    expect(() =>
      narrowApprovalRequest(chatPostRequestWire({ room: { room_id: 'r-2b7c1d', name: 7 } }), 'test')
    ).toThrow(BoundaryError);
  });

  it('carries the room through the requested payload as well', () => {
    const payload = narrowApprovalRequestedPayload({ request: chatPostRequestWire() });
    expect(payload.request).toEqual({
      request_id: 'appr1',
      tool: 'send_chat',
      requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
      text: 'hello everyone',
      room: { room_id: 'r-2b7c1d', name: 'yagura' },
    });
  });
});

describe('a listed window and a pending request read a room the same way', () => {
  it.each(MALFORMED_ROOMS)('rejects $label in both records', ({ room }) => {
    expect(() => narrowWindowInfo(windowInfoWire({ room }))).toThrow(BoundaryError);
    expect(() => narrowApprovalRequest(chatPostRequestWire({ room }), 'test')).toThrow(
      BoundaryError
    );
  });

  it('accepts the same well-formed room in both records', () => {
    const info = narrowWindowInfo(windowInfoWire({ room: WELL_FORMED_ROOM }));
    const request = narrowApprovalRequest(chatPostRequestWire({ room: WELL_FORMED_ROOM }), 'test');

    expect(info).toEqual({ ...ROOMLESS_WINDOW_INFO, room: WELL_FORMED_ROOM });
    expect(request).toEqual({
      request_id: 'appr1',
      tool: 'send_chat',
      requester: { kind: 'window', window_id: 'w-req', name: 'requester window' },
      text: 'hello everyone',
      room: WELL_FORMED_ROOM,
    });
  });
});

// ---------------------------------------------------------------------------
// The gate's rejection arriving as a new
// tagged arm of `WindowOpenFailure`, and the window-list push carrying the
// connection's wire revision alongside the windows it always carried.
// ---------------------------------------------------------------------------

describe('narrowWindowOpenFailure: the "unsupported" arm', () => {
  it('narrows a well-formed unsupported failure, reading the wire tag under requestType', () => {
    const result = narrowWindowOpenFailure({
      kind: 'unsupported',
      request_type: 'open_window_from_profile',
    });
    expect(result).toEqual({ kind: 'unsupported', requestType: 'open_window_from_profile' });
  });

  it('carries whatever wire tag the daemon reports, not a fixed one', () => {
    const result = narrowWindowOpenFailure({ kind: 'unsupported', request_type: 'send_handoff' });
    expect(result).toEqual({ kind: 'unsupported', requestType: 'send_handoff' });
  });

  it('rejects a payload that claims the "unsupported" tag but omits request_type', () => {
    expect(() => narrowWindowOpenFailure({ kind: 'unsupported' })).toThrow(BoundaryError);
  });

  it('rejects a payload whose request_type is not a string', () => {
    expect(() => narrowWindowOpenFailure({ kind: 'unsupported', request_type: 7 })).toThrow(
      BoundaryError
    );
  });

  it('rejects a payload whose request_type is spelled otherwise (camelCase)', () => {
    expect(() =>
      narrowWindowOpenFailure({ kind: 'unsupported', requestType: 'open_window_from_profile' })
    ).toThrow(BoundaryError);
  });
});

describe('narrowWindowListPayload (session://window-list)', () => {
  it('narrows the windows field the same way narrowWindowInfoList does', () => {
    const result = narrowWindowListPayload({ windows: [windowInfoWire()], protocol_revision: 2 });
    expect(result.windows).toEqual([ROOMLESS_WINDOW_INFO]);
  });

  it('reads the connection wire revision under protocol_revision', () => {
    const result = narrowWindowListPayload({ windows: [], protocol_revision: 3 });
    expect(result.protocol_revision).toBe(3);
  });

  it('reads an explicit null protocol_revision as null (not yet settled)', () => {
    const result = narrowWindowListPayload({ windows: [], protocol_revision: null });
    expect(result.protocol_revision).toBeNull();
  });

  it('reads an absent protocol_revision as null, the same as an explicit null', () => {
    const result = narrowWindowListPayload({ windows: [] });
    expect(result.protocol_revision).toBeNull();
  });

  it('rejects a protocol_revision that is present but not a number', () => {
    expect(() => narrowWindowListPayload({ windows: [], protocol_revision: 'two' })).toThrow(
      BoundaryError
    );
  });

  it('rejects a payload missing the windows field, the same as narrowWindowInfoList does', () => {
    expect(() => narrowWindowListPayload({ protocol_revision: 1 })).toThrow(BoundaryError);
  });

  it('rejects a payload that is not a record', () => {
    expect(() => narrowWindowListPayload('not a record')).toThrow(BoundaryError);
  });
});
