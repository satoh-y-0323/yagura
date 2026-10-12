import { describe, it, expect } from 'vitest';
import {
  SESSION_EVENT,
  narrowStatusPayload,
  BoundaryError,
  type DetectedStatusView,
  type StatusPayload,
  type WindowStatus,
} from './session-boundary';

const WINDOW_STATUS_VALUES: WindowStatus[] = [
  'running',
  'waiting_input',
  'waiting_approval',
  'interrupted',
  'idle',
  'exited',
  'unknown',
];

/** The statuses that can never carry a reason. */
const KNOWN_STATUS_VALUES = WINDOW_STATUS_VALUES.filter((status) => status !== 'unknown');

/**
 * Sample of the window id the daemon assigns. The boundary treats it as an
 * opaque string, so only its type matters here, never its shape.
 */
const WINDOW_ID = 'w1-k3f9xa';

/** The pair a narrowed payload holds for a status that is not `'unknown'`. */
function knownPair(status: WindowStatus): DetectedStatusView {
  return { status } as DetectedStatusView;
}

describe('SESSION_EVENT.status', () => {
  it('defines the session://status event name', () => {
    expect(SESSION_EVENT.status).toBe('session://status');
  });
});

describe('narrowStatusPayload', () => {
  it.each(WINDOW_STATUS_VALUES)('accepts status value %s with a window id', (status) => {
    const payload: StatusPayload = narrowStatusPayload({ status, window_id: WINDOW_ID });
    const detected: DetectedStatusView =
      status === 'unknown' ? { status: 'unknown', reason: null } : knownPair(status);
    expect(payload).toEqual({ detected, window_id: WINDOW_ID });
  });

  it('keeps the window id the core reported', () => {
    // The window id must survive narrowing rather than being dropped.
    const payload = narrowStatusPayload({ status: 'running', window_id: 'w7-k3f9xa' });
    expect(payload.window_id).toBe('w7-k3f9xa');
  });

  it('rejects missing window_id field', () => {
    // A payload without window_id must not be accepted as if it named the
    // attached window.
    expect(() => narrowStatusPayload({ status: 'running' })).toThrow(BoundaryError);
  });

  it('rejects non-string window_id', () => {
    expect(() => narrowStatusPayload({ status: 'running', window_id: 1 })).toThrow(BoundaryError);
    expect(() => narrowStatusPayload({ status: 'running', window_id: null })).toThrow(
      BoundaryError
    );
    expect(() => narrowStatusPayload({ status: 'running', window_id: undefined })).toThrow(
      BoundaryError
    );
  });

  it('rejects unknown status literal instead of coercing it', () => {
    // Fail-loud: an unrecognized status must not be guessed into "unknown".
    expect(() => narrowStatusPayload({ status: 'connecting', window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
  });

  it('rejects empty string status', () => {
    expect(() => narrowStatusPayload({ status: '', window_id: WINDOW_ID })).toThrow(BoundaryError);
  });

  it('rejects non-string status', () => {
    expect(() => narrowStatusPayload({ status: 1, window_id: WINDOW_ID })).toThrow(BoundaryError);
    expect(() => narrowStatusPayload({ status: null, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
    expect(() => narrowStatusPayload({ status: undefined, window_id: WINDOW_ID })).toThrow(
      BoundaryError
    );
  });

  it('rejects missing status field', () => {
    expect(() => narrowStatusPayload({ window_id: WINDOW_ID })).toThrow(BoundaryError);
  });

  it('rejects non-object value', () => {
    expect(() => narrowStatusPayload('running')).toThrow(BoundaryError);
    expect(() => narrowStatusPayload(null)).toThrow(BoundaryError);
    expect(() => narrowStatusPayload([{ status: 'running', window_id: WINDOW_ID }])).toThrow(
      BoundaryError
    );
  });

  it('ignores extra fields', () => {
    const payload = narrowStatusPayload({
      status: 'idle',
      window_id: WINDOW_ID,
      extra: 'ignored',
    });
    expect(payload).toEqual({ detected: knownPair('idle'), window_id: WINDOW_ID });
  });
});

// ---------------------------------------------------------------------------
// The status and the reason leave this boundary as one value: the reason is
// reachable only from the `'unknown'` arm, so no reader downstream can hold a
// status that is not `'unknown'` together with a reason for it.
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

describe('narrowStatusPayload: the status and its reason narrow to one value', () => {
  it.each(KNOWN_STATUS_VALUES)('gives the %s arm no reason own property', (status) => {
    const payload = narrowStatusPayload({ status, window_id: WINDOW_ID });
    expect(payload.detected).toEqual({ status });
    expect(Object.prototype.hasOwnProperty.call(payload.detected, 'reason')).toBe(false);
  });

  it.each(UNKNOWN_REASON_VALUES)('puts reason %s on the unknown arm of the pair', (reason) => {
    const payload = narrowStatusPayload({
      status: 'unknown',
      window_id: WINDOW_ID,
      unknown_reason: reason,
    });
    expect(payload.detected).toEqual({ status: 'unknown', reason });
  });

  it('accepts status "unknown" with no reason, as a null-valued reason on the unknown arm', () => {
    const payload = narrowStatusPayload({ status: 'unknown', window_id: WINDOW_ID });
    expect(payload.detected).toEqual({ status: 'unknown', reason: null });
  });

  it('treats an explicit null unknown_reason the same as an omitted key', () => {
    const payload = narrowStatusPayload({
      status: 'unknown',
      window_id: WINDOW_ID,
      unknown_reason: null,
    });
    expect(payload.detected).toEqual({ status: 'unknown', reason: null });
  });

  it('treats an explicit null unknown_reason as absent for a known status too (null never trips the mismatch rejection)', () => {
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      unknown_reason: null,
    });
    expect(payload.detected).toEqual({ status: 'running' });
  });

  it('no longer exposes a top-level status or unknown_reason key', () => {
    const payload = narrowStatusPayload({
      status: 'unknown',
      window_id: WINDOW_ID,
      unknown_reason: 'screen_lost',
    });
    expect(Object.prototype.hasOwnProperty.call(payload, 'status')).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(payload, 'unknown_reason')).toBe(false);
  });
});

describe('narrowStatusPayload: the status-change route refuses a mismatched pair', () => {
  it.each(KNOWN_STATUS_VALUES)(
    'rejects a reason carried alongside status %s rather than dropping it',
    (status) => {
      expect(() =>
        narrowStatusPayload({ status, window_id: WINDOW_ID, unknown_reason: 'no_definition' })
      ).toThrow(BoundaryError);
    }
  );

  it('rejects an unrecognized unknown_reason string', () => {
    expect(() =>
      narrowStatusPayload({
        status: 'unknown',
        window_id: WINDOW_ID,
        unknown_reason: 'a_future_reason',
      })
    ).toThrow(BoundaryError);
  });

  it('rejects an unrecognized unknown_reason string alongside a known status too', () => {
    expect(() =>
      narrowStatusPayload({
        status: 'running',
        window_id: WINDOW_ID,
        unknown_reason: 'a_future_reason',
      })
    ).toThrow(BoundaryError);
  });

  it('rejects a non-string unknown_reason', () => {
    expect(() =>
      narrowStatusPayload({ status: 'unknown', window_id: WINDOW_ID, unknown_reason: 7 })
    ).toThrow(BoundaryError);
  });
});

// ---------------------------------------------------------------------------
// The generation the payload carries. The key set the boundary produces is
// pinned as a whole: nothing unknown is introduced and nothing known is
// dropped. The generation joins `window_id` and the pair as the third known
// key.
// ---------------------------------------------------------------------------

/** Every key a narrowed status payload may own. */
const KNOWN_STATUS_PAYLOAD_KEYS = ['window_id', 'detected', 'status_rev'];

describe('narrowStatusPayload: the key set it produces', () => {
  it('introduces no key outside the known set, for a payload carrying every field', () => {
    const payload = narrowStatusPayload({
      status: 'unknown',
      window_id: WINDOW_ID,
      unknown_reason: 'screen_lost',
      status_rev: 12,
      extra: 'ignored',
    });
    expect(Object.keys(payload).sort()).toEqual([...KNOWN_STATUS_PAYLOAD_KEYS].sort());
  });

  it('drops no known key of a payload carrying every field', () => {
    const payload = narrowStatusPayload({
      status: 'unknown',
      window_id: WINDOW_ID,
      unknown_reason: 'screen_lost',
      status_rev: 12,
    });
    expect(payload).toEqual({
      window_id: WINDOW_ID,
      detected: { status: 'unknown', reason: 'screen_lost' },
      status_rev: 12,
    });
  });

  it('omits the generation as an own property when the wire omits it', () => {
    const payload = narrowStatusPayload({ status: 'running', window_id: WINDOW_ID });
    expect(Object.prototype.hasOwnProperty.call(payload, 'status_rev')).toBe(false);
  });

  it('omits the generation as an own property for an explicit null', () => {
    const payload = narrowStatusPayload({
      status: 'running',
      window_id: WINDOW_ID,
      status_rev: null,
    });
    expect(Object.prototype.hasOwnProperty.call(payload, 'status_rev')).toBe(false);
  });

  it('carries a zero generation through rather than treating it as absent', () => {
    const payload = narrowStatusPayload({ status: 'running', window_id: WINDOW_ID, status_rev: 0 });
    expect(payload.status_rev).toBe(0);
  });

  it('rejects a non-integer generation', () => {
    expect(() =>
      narrowStatusPayload({ status: 'running', window_id: WINDOW_ID, status_rev: 1.5 })
    ).toThrow(BoundaryError);
    expect(() =>
      narrowStatusPayload({ status: 'running', window_id: WINDOW_ID, status_rev: 'seven' })
    ).toThrow(BoundaryError);
  });
});
