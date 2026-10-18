/**
 * What the session boundary refuses, and which part of the reply its refusal
 * names; plus the optional fields it keeps or drops. The message is the only
 * thing that tells an operator reading a log which reply from the daemon was
 * wrong, so each reader's wording is part of its contract.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';

import {
  BoundaryError,
  formatApprovalBody,
  MAX_DISPLAY_FIELD_CHARS,
  narrowApprovalRequest,
  narrowApprovalRequestList,
  narrowApprovalRequestedPayload,
  narrowApprovalWithdrawnPayload,
  narrowApprovalResetPayload,
  narrowApprovalSyncOutcome,
  narrowAttachResponse,
  narrowChatChanged,
  narrowChatMessages,
  narrowChatPosted,
  narrowChatRooms,
  narrowCloseOutcome,
  narrowDecideApprovalOutcome,
  narrowExitPayload,
  narrowRecoveryStatusReport,
  narrowRestoreCandidateList,
  narrowStatusPayload,
  narrowWindowInfo,
  narrowWindowInfoList,
  narrowWindowListPayload,
  narrowWindowOpenFailure,
  sanitizeForDisplay,
  WindowOpenFailureError,
} from './session-boundary';

type Wire = Record<string, unknown>;

afterEach(() => {
  vi.restoreAllMocks();
});

function messageOf(action: () => unknown): string {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(BoundaryError);
    return (error as Error).message;
  }
  throw new Error('the value was accepted');
}

function requesterWire(overrides: Wire = {}): Wire {
  return { kind: 'window', window_id: 'w1', name: 'one', ...overrides };
}

function requestWire(overrides: Wire = {}): Wire {
  return { request_id: 'r1', tool: 'close_window', requester: requesterWire(), ...overrides };
}

function windowInfoWire(overrides: Wire = {}): Wire {
  return {
    window_id: 'w1',
    name: 'one',
    cwd: '/repo',
    status: 'running',
    ...overrides,
  };
}

function deliveryWire(overrides: Wire = {}): Wire {
  return { window_id: 'w2', state: 'taken', ...overrides };
}

function chatMessageWire(overrides: Wire = {}): Wire {
  return {
    seq: 1,
    at_ms: 10,
    from_kind: 'window',
    from_name: 'one',
    body: 'hi',
    mentions: [],
    broadcast: false,
    deliveries: [deliveryWire()],
    ...overrides,
  };
}

function chatMessagesWire(overrides: Wire = {}): Wire {
  return {
    room_id: 'room',
    messages: [chatMessageWire()],
    next_after_seq: 1,
    halted: null,
    needs_user: false,
    roster: [{ window_id: 'w1', name: 'one' }],
    ...overrides,
  };
}

function roomRowWire(overrides: Wire = {}): Wire {
  return {
    room_id: 'room',
    name: 'Room',
    participant_count: 2,
    needs_user: false,
    halted: null,
    ...overrides,
  };
}

describe('a null reply is refused as a BoundaryError, not a crash', () => {
  it('refuses null where an object is expected', () => {
    expect(messageOf(() => narrowExitPayload(null))).toMatch(
      /^session:\/\/exit: expected an object/
    );
    expect(messageOf(() => narrowExitPayload([]))).toMatch(/expected an object/);
  });
});

describe('the reason an approval request was withdrawn', () => {
  it.each(['resolved', 'timeout', 'requester_disconnected'])('keeps the reason %s', (reason) => {
    expect(narrowApprovalWithdrawnPayload({ request_id: 'r', reason }).reason).toBe(reason);
  });

  it('reads a reason this build does not know as unknown', () => {
    expect(narrowApprovalWithdrawnPayload({ request_id: 'r', reason: 'abducted' }).reason).toBe(
      'unknown'
    );
  });
});

describe('a primitive is not a record', () => {
  it.each(['text', 5, true])('refuses %j where an object is expected', (value) => {
    expect(messageOf(() => narrowExitPayload(value))).toMatch(
      /^session:\/\/exit: expected an object/
    );
  });
});

describe('the field readers name the field they refuse', () => {
  it.each<[string, () => unknown, RegExp]>([
    [
      'a string',
      () => narrowExitPayload({ exit_code: 0, window_id: 4 }),
      /exit\.window_id: expected a string/,
    ],
    [
      'an integer',
      () => narrowExitPayload({ exit_code: 'x', window_id: 'w' }),
      /exit\.exit_code: expected an integer/,
    ],
    [
      'a fractional integer',
      () => narrowExitPayload({ exit_code: 1.5, window_id: 'w' }),
      /exit\.exit_code: expected an integer/,
    ],
    [
      'a boolean',
      () => narrowAttachResponse({ child_exited: 1 }),
      /attach\.child_exited: expected a boolean/,
    ],
    [
      'a non-negative integer',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: -1 }),
      /get_recovery_status\.rev: expected a non-negative integer/,
    ],
    [
      'a non-numeric non-negative integer',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: '1' }),
      /get_recovery_status\.rev: expected a non-negative integer/,
    ],
    [
      'a recovery status',
      () => narrowRecoveryStatusReport({ status: 'broken', rev: 1 }),
      /get_recovery_status\.status: unrecognized status "broken"/,
    ],
    [
      'a window status',
      () => narrowStatusPayload({ window_id: 'w', status: 'dancing' }),
      /session:\/\/status\.status: unrecognized status "dancing"/,
    ],
    [
      'a config failure',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: 1, config_failure: 'on_fire' }),
      /get_recovery_status\.config_failure: unrecognized config_failure \("on_fire"\)/,
    ],
    [
      'a config failure that is not text',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: 1, config_failure: 3 }),
      /config_failure: unrecognized config_failure \(3\)/,
    ],
    [
      'a startup env failure',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: 1, startup_env_failure: 'x' }),
      /get_recovery_status\.startup_env_failure: unrecognized startup_env_failure \("x"\)/,
    ],
    [
      'a startup env failure that is not text',
      () => narrowRecoveryStatusReport({ status: 'idle', rev: 1, startup_env_failure: false }),
      /startup_env_failure: unrecognized startup_env_failure \(false\)/,
    ],
    [
      'an array that is not an array',
      () => narrowChatPosted({ room_id: 'r', seq: 1, targets: 'x' }),
      /chat_posted\.targets: expected an array/,
    ],
    [
      'an array of strings that holds a number',
      () => narrowChatPosted({ room_id: 'r', seq: 1, targets: ['a', 2] }),
      /chat_posted\.targets: expected an array of strings/,
    ],
    [
      'a chat sender kind',
      () =>
        narrowChatMessages(chatMessagesWire({ messages: [chatMessageWire({ from_kind: 'bot' })] })),
      /chat_messages\.messages\.from_kind: unrecognized kind "bot"/,
    ],
    [
      'a delivery state',
      () =>
        narrowChatMessages(
          chatMessagesWire({
            messages: [chatMessageWire({ deliveries: [deliveryWire({ state: 'lost' })] })],
          })
        ),
      /deliveries\.state: unrecognized delivery state "lost"/,
    ],
  ])('names %s', (_name, action, pattern) => {
    expect(messageOf(action)).toMatch(pattern);
  });
});

describe('the unknown_reason and status generation of a record', () => {
  it('refuses a reason that is not text, on the status route', () => {
    expect(
      messageOf(() => narrowStatusPayload({ window_id: 'w', status: 'unknown', unknown_reason: 5 }))
    ).toMatch(/session:\/\/status\.unknown_reason: expected a string/);
  });

  it('refuses a reason that does not belong on a known status', () => {
    expect(
      messageOf(() =>
        narrowStatusPayload({ window_id: 'w', status: 'running', unknown_reason: 'screen_lost' })
      )
    ).toMatch(/unknown_reason: "screen_lost" does not belong on this arm/);
  });

  it('refuses a reason outside the vocabulary on the unknown status', () => {
    expect(
      messageOf(() =>
        narrowStatusPayload({ window_id: 'w', status: 'unknown', unknown_reason: 'x' })
      )
    ).toMatch(/unknown_reason: "x" does not belong on this arm/);
  });

  it('drops a misplaced reason from a listed window and says so', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const info = narrowWindowInfo(
      windowInfoWire({ status: 'idle', unknown_reason: 'screen_lost' })
    );
    expect(info.detected).toEqual({ status: 'idle' });
    expect(warn).toHaveBeenCalledWith(
      'window_info.unknown_reason: dropped "screen_lost", which does not belong on this arm'
    );
  });

  it('drops a misplaced reason from a chat delivery instead of refusing the message', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const room = narrowChatMessages(
      chatMessagesWire({
        messages: [
          chatMessageWire({
            deliveries: [deliveryWire({ state: 'taken', unknown_reason: 'screen_lost' })],
          }),
        ],
      })
    );
    expect(room.messages[0]?.deliveries[0]).toEqual({ window_id: 'w2', state: 'taken' });
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it('keeps the reason of an undelivered_unknown delivery', () => {
    const room = narrowChatMessages(
      chatMessagesWire({
        messages: [
          chatMessageWire({
            deliveries: [
              deliveryWire({ state: 'undelivered_unknown', unknown_reason: 'screen_lost' }),
            ],
          }),
        ],
      })
    );
    expect(room.messages[0]?.deliveries[0]).toEqual({
      window_id: 'w2',
      state: 'undelivered_unknown',
      reason: 'screen_lost',
    });
  });

  it('refuses a negative status generation on the status route and drops it from a listing', () => {
    expect(
      messageOf(() => narrowStatusPayload({ window_id: 'w', status: 'idle', status_rev: -1 }))
    ).toMatch(/session:\/\/status\.status_rev: expected a non-negative generation/);
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const info = narrowWindowInfo(windowInfoWire({ status_rev: -2 }));
    expect(info).not.toHaveProperty('status_rev');
    expect(warn).toHaveBeenCalledWith('window_info.status_rev: dropped a negative generation (-2)');
  });
});

describe('the pieces of an approval request', () => {
  it('names the request, requester, target and room that are wrong', () => {
    expect(messageOf(() => narrowApprovalRequest(5))).toMatch(
      /^approval_request: expected an object/
    );
    expect(messageOf(() => narrowApprovalRequest(requestWire({ requester: 5 })))).toMatch(
      /^approval_request\.requester: expected an object/
    );
    expect(messageOf(() => narrowApprovalRequest(requestWire({ target: 5 })))).toMatch(
      /^approval_request\.target: expected an object/
    );
    expect(messageOf(() => narrowApprovalRequest(requestWire({ room: 5 })))).toMatch(
      /^approval_request\.room: expected an object/
    );
  });

  it('refuses a room with an empty id or an empty name, naming which', () => {
    expect(
      messageOf(() => narrowApprovalRequest(requestWire({ room: { room_id: '', name: 'n' } })))
    ).toMatch(/room\.room_id: expected a non-empty string/);
    expect(
      messageOf(() => narrowApprovalRequest(requestWire({ room: { room_id: 'r', name: '' } })))
    ).toMatch(/room\.name: expected a non-empty string/);
  });

  it('keeps the target and room when present, and omits them when null or absent', () => {
    const full = narrowApprovalRequest(
      requestWire({
        target: { window_id: 'w2', name: 'two', status: 'idle' },
        room: { room_id: 'r', name: 'Room' },
        text: 'why',
      })
    );
    expect(full.target).toEqual({ window_id: 'w2', name: 'two', status: 'idle' });
    expect(full.room).toEqual({ room_id: 'r', name: 'Room' });
    expect(full.text).toBe('why');
    const bare = narrowApprovalRequest(requestWire({ target: null, room: null }));
    expect(bare).not.toHaveProperty('target');
    expect(bare).not.toHaveProperty('room');
    expect(bare.text).toBeNull();
  });

  it('reads a requester kind it does not know as unknown with no identity', () => {
    const request = narrowApprovalRequest(
      requestWire({ requester: { kind: 'robot', window_id: 'w9', name: 'n' } })
    );
    expect(request.requester).toEqual({ kind: 'unknown', window_id: null, name: null });
  });

  it('keeps an external requester without an identity', () => {
    const request = narrowApprovalRequest(requestWire({ requester: { kind: 'external' } }));
    expect(request.requester).toEqual({ kind: 'external', window_id: null, name: null });
  });

  it('names where a pushed request or a pending list went wrong', () => {
    expect(messageOf(() => narrowApprovalRequestedPayload({ request: 5 }))).toMatch(
      /session:\/\/approval-requested\.request: expected an object/
    );
    expect(messageOf(() => narrowApprovalResetPayload({ requests: 5 }))).toMatch(
      /session:\/\/approval-reset\.requests: expected an array/
    );
    expect(messageOf(() => narrowApprovalRequestList({}, 'pending'))).toMatch(
      /^pending: expected an array/
    );
  });

  it('names the sync outcome and its pending list', () => {
    expect(messageOf(() => narrowApprovalSyncOutcome({ status: 'maybe' }))).toMatch(
      /^sync_approvals: unrecognized status \("maybe"\)/
    );
    expect(
      messageOf(() => narrowApprovalSyncOutcome({ status: 'registered', pending: 5 }))
    ).toMatch(/^sync_approvals\.pending: expected an array/);
    expect(messageOf(() => narrowApprovalSyncOutcome(5))).toMatch(/^sync_approvals/);
  });

  it('names the decision and close outcomes it does not know', () => {
    expect(messageOf(() => narrowDecideApprovalOutcome('later'))).toBe(
      'decide_approval: unrecognized outcome ("later")'
    );
    expect(messageOf(() => narrowCloseOutcome(1))).toBe('close_window: unrecognized outcome (1)');
  });
});

describe('the chat replies', () => {
  it('keeps every part of a room history', () => {
    const room = narrowChatMessages(
      chatMessagesWire({ halted: { reason: 'loop', since_ms: 5 }, needs_user: true })
    );
    expect(room.halted).toEqual({ reason: 'loop', since_ms: 5 });
    expect(room.needs_user).toBe(true);
    expect(room.roster).toEqual([{ window_id: 'w1', name: 'one' }]);
    expect(room.messages[0]?.from_window_id).toBeNull();
  });

  it.each(['window', 'user', 'external', 'system'])('keeps the sender kind %s', (from_kind) => {
    const room = narrowChatMessages(
      chatMessagesWire({ messages: [chatMessageWire({ from_kind })] })
    );
    expect(room.messages[0]?.from_kind).toBe(from_kind);
  });

  it.each([
    'pending',
    'taken',
    'undelivered_halted',
    'undelivered_exhausted',
    'undelivered_restart',
  ])('keeps the delivery state %s', (state) => {
    const room = narrowChatMessages(
      chatMessagesWire({ messages: [chatMessageWire({ deliveries: [deliveryWire({ state })] })] })
    );
    expect(room.messages[0]?.deliveries[0]).toEqual({ window_id: 'w2', state });
  });

  it('names the part of a history that is wrong', () => {
    const history = (overrides: Wire) => () => narrowChatMessages(chatMessagesWire(overrides));
    expect(messageOf(history({ messages: 5 }))).toMatch(
      /^chat_messages\.messages: expected an array/
    );
    expect(messageOf(history({ roster: 5 }))).toMatch(/^chat_messages\.roster: expected an array/);
    expect(messageOf(history({ halted: 5 }))).toMatch(/^chat_messages\.halted: expected an object/);
    expect(messageOf(history({ messages: [chatMessageWire({ deliveries: 5 })] }))).toMatch(
      /chat_messages\.messages\.deliveries: expected an array/
    );
    expect(messageOf(() => narrowChatMessages(5))).toMatch(/^chat_messages: expected an object/);
  });

  it('names the part of a room list that is wrong', () => {
    expect(messageOf(() => narrowChatRooms({ rooms: 5 }))).toMatch(
      /^list_chat_rooms\.rooms: expected an array/
    );
    expect(messageOf(() => narrowChatRooms({ rooms: [roomRowWire({ halted: 5 })] }))).toMatch(
      /list_chat_rooms\.rooms\.halted: expected an object/
    );
    expect(messageOf(() => narrowChatRooms(5))).toMatch(/^list_chat_rooms/);
  });

  it('keeps the halt of a room row and reads a missing one as none', () => {
    const rooms = narrowChatRooms({
      rooms: [roomRowWire({ halted: { reason: 'r', since_ms: 1 }, last_at_ms: 9 }), roomRowWire()],
    });
    expect(rooms[0]).toMatchObject({ halted: { reason: 'r', since_ms: 1 }, last_at_ms: 9 });
    expect(rooms[1]).toMatchObject({ halted: null, last_at_ms: null });
  });

  it('names the chat reply and event that are not objects', () => {
    expect(messageOf(() => narrowChatPosted(5))).toMatch(/^chat_posted/);
    expect(messageOf(() => narrowChatChanged(5))).toMatch(/^session:\/\/chat-changed/);
  });
});

describe('the window list', () => {
  it('keeps the optional fields of a window when they are present', () => {
    const info = narrowWindowInfo(
      windowInfoWire({
        vendor: 'v',
        layout_json: '{}',
        room: { room_id: 'r', name: 'n' },
        detection_warning: 'warn',
        status_rev: 3,
        mcp_connected: true,
        status_reports_missing: true,
        exit_code: 2,
      })
    );
    expect(info).toMatchObject({
      vendor: 'v',
      layout_json: '{}',
      room: { room_id: 'r', name: 'n' },
      detection_warning: 'warn',
      status_rev: 3,
      mcp_connected: true,
      status_reports_missing: true,
      exit_code: 2,
    });
  });

  it('omits the optional fields that are null or absent', () => {
    const info = narrowWindowInfo(
      windowInfoWire({
        layout_json: null,
        room: null,
        detection_warning: null,
        status_rev: null,
        mcp_connected: null,
        status_reports_missing: null,
      })
    );
    for (const key of [
      'layout_json',
      'room',
      'detection_warning',
      'status_rev',
      'mcp_connected',
      'status_reports_missing',
    ]) {
      expect(info).not.toHaveProperty(key);
    }
    expect(narrowWindowInfo(windowInfoWire())).not.toHaveProperty('status_reports_missing');
  });

  it('keeps a false mcp_connected and a false status_reports_missing', () => {
    const info = narrowWindowInfo(
      windowInfoWire({ mcp_connected: false, status_reports_missing: false })
    );
    expect(info).toMatchObject({ mcp_connected: false, status_reports_missing: false });
  });

  it('refuses an optional field of the wrong type, naming it', () => {
    expect(messageOf(() => narrowWindowInfo(windowInfoWire({ layout_json: 5 })))).toMatch(
      /window_info\.layout_json: expected a string/
    );
    expect(messageOf(() => narrowWindowInfo(windowInfoWire({ mcp_connected: 'yes' })))).toMatch(
      /window_info\.mcp_connected: expected a boolean/
    );
    expect(
      messageOf(() => narrowWindowInfo(windowInfoWire({ status_reports_missing: 'yes' })))
    ).toMatch(/window_info\.status_reports_missing: expected a boolean/);
    expect(messageOf(() => narrowWindowInfo(windowInfoWire({ room: { room_id: 'r' } })))).toMatch(
      /window_info\.room\.name: expected a string/
    );
  });

  it('names the listing, its payload and the entries that are wrong', () => {
    expect(messageOf(() => narrowWindowInfoList({}))).toBe('list_windows: expected an array');
    expect(messageOf(() => narrowWindowInfoList([5]))).toMatch(/^list_windows: expected an object/);
    expect(messageOf(() => narrowWindowListPayload(5))).toMatch(/^session:\/\/window-list/);
    expect(
      messageOf(() => narrowWindowListPayload({ windows: [], protocol_revision: 'x' }))
    ).toMatch(/session:\/\/window-list\.protocol_revision: expected an integer/);
  });

  it('names the attach reply that is not an object', () => {
    expect(messageOf(() => narrowAttachResponse(5))).toMatch(/^attach: expected an object/);
  });
});

describe('the failure an open_window rejection carries', () => {
  it('reads a string, an Error and an untagged value for their text', () => {
    expect(narrowWindowOpenFailure('refused')).toEqual({ kind: 'bare', message: 'refused' });
    expect(narrowWindowOpenFailure(new Error('bridge'))).toEqual({
      kind: 'bare',
      message: 'bridge',
    });
    expect(narrowWindowOpenFailure(7)).toEqual({ kind: 'bare', message: '7' });
    expect(narrowWindowOpenFailure(null)).toEqual({ kind: 'bare', message: 'null' });
    expect(narrowWindowOpenFailure({ code: 1 })).toEqual({ kind: 'bare', message: '{"code":1}' });
  });

  it('reads an object that cannot be encoded as empty text', () => {
    expect(narrowWindowOpenFailure({ toJSON: () => undefined })).toEqual({
      kind: 'bare',
      message: '',
    });
    const cyclic: Wire = {};
    cyclic['self'] = cyclic;
    expect(narrowWindowOpenFailure(cyclic)).toEqual({ kind: 'bare', message: '' });
  });

  it('names a tag it does not know', () => {
    expect(messageOf(() => narrowWindowOpenFailure({ kind: 'weird' }))).toBe(
      'open_window.failure.kind: unrecognized failure kind "weird"'
    );
  });

  it('refuses a tagged failure missing its fields, naming the command', () => {
    expect(messageOf(() => narrowWindowOpenFailure({ kind: 'local' }))).toMatch(
      /^open_window\.failure\.message: expected a string/
    );
    expect(messageOf(() => narrowWindowOpenFailure({ kind: 'unsupported' }))).toMatch(
      /^open_window\.failure\.request_type: expected a string/
    );
  });

  it('keeps the unsupported request type and reads it into the error message', () => {
    const failure = narrowWindowOpenFailure({ kind: 'unsupported', request_type: 'OpenWindow' });
    expect(failure).toEqual({ kind: 'unsupported', requestType: 'OpenWindow' });
    const error = new WindowOpenFailureError(failure);
    expect(error.name).toBe('WindowOpenFailureError');
    expect(error.message).toBe(
      'the connected daemon has not settled a wire revision for OpenWindow'
    );
    expect(error.failure).toBe(failure);
  });

  it('carries the reason text of every other shape as the error message', () => {
    const rejected = narrowWindowOpenFailure({
      kind: 'rejected',
      code: 'bad_cwd',
      message: 'no such directory',
    });
    expect(rejected).toEqual({
      kind: 'rejected',
      code: 'bad_cwd',
      message: 'no such directory',
      rejectedArgument: null,
    });
    expect(new WindowOpenFailureError(rejected).message).toBe('no such directory');
    expect(new WindowOpenFailureError({ kind: 'bare', message: 'plain' }).message).toBe('plain');
  });
});

describe('the restore candidates', () => {
  function candidate(overrides: Wire = {}): Wire {
    return { position: 0, command: ['claude', '--x'], cwd: '/repo', name: 'one', ...overrides };
  }

  it('shows the command as its words joined by a space', () => {
    const [entry] = narrowRestoreCandidateList([candidate()]);
    expect(entry?.displayCommand).toBe('claude --x');
  });

  it('keeps the vendor and detection, and shows them sanitized', () => {
    const [entry] = narrowRestoreCandidateList([
      candidate({ vendor: 'v\u0007x', detection: 'd\u001bz' }),
    ]);
    expect(entry).toMatchObject({
      vendor: 'v\u0007x',
      detection: 'd\u001bz',
      displayVendor: 'vx',
      displayDetection: 'dz',
    });
    const [bare] = narrowRestoreCandidateList([candidate()]);
    expect(bare).toMatchObject({
      vendor: null,
      detection: null,
      displayVendor: null,
      displayDetection: null,
    });
  });

  it('drops a row whose execution fields have the wrong type, and keeps the others', () => {
    const rows = [
      candidate({ name: 'good' }),
      5,
      null,
      [],
      candidate({ position: '0' }),
      candidate({ position: 1.5 }),
      candidate({ command: 'claude' }),
      candidate({ command: ['claude', 2] }),
      candidate({ cwd: 3 }),
      candidate({ name: 3 }),
    ];
    expect(narrowRestoreCandidateList(rows).map((entry) => entry.name)).toEqual(['good']);
  });

  it('reads a layout that is not text as none', () => {
    const [entry] = narrowRestoreCandidateList([candidate({ layout_json: { cols: 1 } })]);
    expect(entry?.layout).toBeNull();
  });

  it('reads a list holding layout text as none, though the text alone is a layout', () => {
    const text = '{"v":1,"pos":null,"size":null,"z":0,"min":false,"max":false,"tab":0}';
    const [asText, asList] = narrowRestoreCandidateList([
      candidate({ layout_json: text }),
      candidate({ layout_json: [text] }),
    ]);
    expect(asText?.layout).not.toBeNull();
    expect(asList?.layout).toBeNull();
  });

  it('names the list that is not an array', () => {
    expect(messageOf(() => narrowRestoreCandidateList({}))).toBe(
      'list_restore_candidates.windows: expected an array'
    );
  });
});

describe('sanitizeForDisplay', () => {
  it('strips control characters and keeps everything else', () => {
    expect(sanitizeForDisplay('a\u0000b\u007fc\u009fd\ne')).toBe('abcde');
  });

  it('keeps a text of exactly the limit whole', () => {
    const text = 'x'.repeat(MAX_DISPLAY_FIELD_CHARS);
    expect(sanitizeForDisplay(text)).toBe(text);
  });

  it('cuts a longer text at the limit in code points and marks the cut', () => {
    const text = '😀'.repeat(MAX_DISPLAY_FIELD_CHARS + 1);
    const shown = sanitizeForDisplay(text);
    expect(shown).toBe(`${'😀'.repeat(MAX_DISPLAY_FIELD_CHARS)}…`);
  });

  it('keeps an empty text empty', () => {
    expect(sanitizeForDisplay('')).toBe('');
    expect(sanitizeForDisplay('\u0007')).toBe('');
  });
});

describe('formatApprovalBody', () => {
  it('keeps the lines of a body whole', () => {
    expect(formatApprovalBody('command: sh -c "rm -rf x"\ncwd: /work\nfor: handoff')).toBe(
      'command: sh -c "rm -rf x"\ncwd: /work\nfor: handoff'
    );
  });

  it('writes other control characters out instead of removing them', () => {
    expect(formatApprovalBody('a\u0000b\u007fc\u009fd\re')).toBe(
      'a\\u{0000}b\\u{007F}c\\u{009F}d\\u{000D}e'
    );
  });

  it('writes each bidirectional control out so the line reads as it is stored', () => {
    const controls = [
      '\u061C',
      '\u200E',
      '\u200F',
      '\u202A',
      '\u202B',
      '\u202C',
      '\u202D',
      '\u202E',
      '\u2066',
      '\u2067',
      '\u2068',
      '\u2069',
    ];
    for (const control of controls) {
      const hex = (control.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
      expect(formatApprovalBody(`cmd ${control}txt.exe`)).toBe(`cmd \\u{${hex}}txt.exe`);
    }
  });

  it('keeps text that is not a control, including other scripts and emoji', () => {
    expect(formatApprovalBody('コマンド: 日本語 😀')).toBe('コマンド: 日本語 😀');
  });

  it('cuts a longer body at the limit in code points before escaping', () => {
    const shown = formatApprovalBody(`${'😀'.repeat(MAX_DISPLAY_FIELD_CHARS)}\u202Etail`);
    expect(shown).toBe(`${'😀'.repeat(MAX_DISPLAY_FIELD_CHARS)}…`);
  });

  it('keeps an empty body empty', () => {
    expect(formatApprovalBody('')).toBe('');
  });
});
