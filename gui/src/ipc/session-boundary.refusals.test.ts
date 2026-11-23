/**
 * What the session boundary refuses, and which part of the reply its refusal
 * names; plus the optional fields it keeps or drops. The message is the only
 * thing that tells an operator reading a log which reply from the daemon was
 * wrong, so each reader's wording is part of its contract.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';

import {
  formatApprovalBody,
  formatChatBody,
  formatCommandLine,
  formatOutputBlock,
  formatPathForDisplay,
  joinDisplayBlocks,
  MAX_DISPLAY_FIELD_CHARS,
  sanitizeForDisplay,
} from '../display/untrusted-text';
import {
  BoundaryError,
  narrowApprovalRequest,
  narrowApprovalRequestList,
  narrowApprovalRequestedPayload,
  narrowApprovalWithdrawnPayload,
  narrowApprovalResetPayload,
  narrowApprovalSyncOutcome,
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
  return { window_id: 'w2', state: 'delivered', ...overrides };
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
      () => narrowChatPosted({ room_id: 'r', seq: 1, targets: [], user: 1 }),
      /chat_posted\.user: expected a boolean/,
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
            deliveries: [deliveryWire({ state: 'delivered', unknown_reason: 'screen_lost' })],
          }),
        ],
      })
    );
    expect(room.messages[0]?.deliveries[0]).toEqual({ window_id: 'w2', state: 'delivered' });
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
    expect(room.roster).toEqual([{ window_id: 'w1', name: 'one', displayName: 'one' }]);
    expect(room.messages[0]?.from_window_id).toBeNull();

    const spoken = narrowChatMessages(
      chatMessagesWire({
        messages: [
          chatMessageWire({
            body: 'a\r\nb\u202Ec\u200Bd\u{E0041}e\u200Cf\u26A0\uFE0Fg',
          }),
        ],
      })
    );
    expect(spoken.messages[0]?.body).toBe(
      'a\r\nb\\u{202E}c\\u{200B}d\\u{E0041}e\\u{200C}f\u26A0\uFE0Fg'
    );
  });

  it.each(['window', 'user', 'external', 'system'])('keeps the sender kind %s', (from_kind) => {
    const room = narrowChatMessages(
      chatMessagesWire({ messages: [chatMessageWire({ from_kind })] })
    );
    expect(room.messages[0]?.from_kind).toBe(from_kind);
  });

  it.each([
    'queued',
    'delivered',
    'undelivered_halted',
    'undelivered_exhausted',
    'undelivered_restart',
  ])('keeps the delivery state %s', (state) => {
    const room = narrowChatMessages(
      chatMessagesWire({ messages: [chatMessageWire({ deliveries: [deliveryWire({ state })] })] })
    );
    expect(room.messages[0]?.deliveries[0]).toEqual(
      state === 'queued' ? { window_id: 'w2', state, note: null } : { window_id: 'w2', state }
    );
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
        status_boot: { origin: 7, seq: 3 },
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
      status_boot: { origin: 7, seq: 3 },
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

/**
 * Characters a path or an approval body writes out: the label set the daemon
 * refuses, plus every default ignorable code point (ZWNJ and ZWJ included).
 */
const HIDDEN_WRITTEN_OUT = [
  '\u00AD',
  '\u034F',
  '\u061C',
  '\u115F',
  '\u180E',
  '\u200B',
  '\u200C',
  '\u200D',
  '\u200E',
  '\u200F',
  '\u2028',
  '\u2029',
  '\u202A',
  '\u202B',
  '\u202C',
  '\u202D',
  '\u202E',
  '\u2060',
  '\u2061',
  '\u2064',
  '\u2066',
  '\u2067',
  '\u2068',
  '\u2069',
  '\u206A',
  '\u3164',
  '\uFE0F',
  '\uFEFF',
  '\u{E0041}',
];

function escapeOf(char: string): string {
  const hex = (char.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0');
  return `\\u{${hex}}`;
}

describe('sanitizeForDisplay', () => {
  it('strips control characters and the characters a label may not hold, and keeps everything else', () => {
    expect(sanitizeForDisplay('a\u0000b\u007fc\u009fd\ne')).toBe('abcde');
    // The set the daemon refuses in a label (`is_forbidden_label_char`).
    const forbidden = [
      '\u061c',
      '\u2028',
      '\u2029',
      '\u200B',
      '\u200E',
      '\u200F',
      '\u2060',
      '\uFEFF',
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
    for (const char of forbidden) {
      expect(sanitizeForDisplay(`exe.${char}txt`)).toBe('exe.txt');
    }
    expect(sanitizeForDisplay('a\u200Cb\u200Dc')).toBe('a\u200Cb\u200Dc');
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

  it('writes each hidden character out so the line reads as it is stored', () => {
    for (const hidden of HIDDEN_WRITTEN_OUT) {
      expect(formatApprovalBody(`cmd ${hidden}txt.exe`)).toBe(`cmd ${escapeOf(hidden)}txt.exe`);
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

describe('formatPathForDisplay', () => {
  it('writes every hidden character and line break out, and cuts before escaping', () => {
    for (const hidden of HIDDEN_WRITTEN_OUT) {
      expect(formatPathForDisplay(`C:\\work\\a${hidden}b`)).toBe(`C:\\work\\a${escapeOf(hidden)}b`);
    }
    expect(formatPathForDisplay('C:\\re\u0000po\n\t')).toBe('C:\\re\\u{0000}po\\u{000A}\\u{0009}');
    expect(formatPathForDisplay('C:\\work\\yagura')).toBe('C:\\work\\yagura');
    const shown = formatPathForDisplay(`${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\u202Etail`);
    expect(shown).toBe(`${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\\u{202E}…`);
  });
});

describe('formatCommandLine', () => {
  it('writes a line break out and does not quote for it', () => {
    expect(formatCommandLine(['a\nb'])).toBe('a\\u{000A}b');
  });

  it('writes a double quote inside an element out, and quotes the element when it holds a space', () => {
    expect(formatCommandLine(['say "hi"'])).toBe('"say \\u{0022}hi\\u{0022}"');
  });

  it('quotes an element that holds a space, an ideographic space, or nothing', () => {
    expect(formatCommandLine(['C:\\Program Files\\x.exe'])).toBe('"C:\\Program Files\\x.exe"');
    expect(formatCommandLine(['a\u3000b'])).toBe('"a\u3000b"');
    expect(formatCommandLine([''])).toBe('""');
  });

  it('joins the elements with one space', () => {
    expect(formatCommandLine(['claude', '--x', 'a b'])).toBe('claude --x "a b"');
  });

  it('cuts at the limit counting the separators, closes the quote of a cut element, and drops the rest', () => {
    const shown = formatCommandLine(['ab', `c ${'d'.repeat(5000)}`, 'e']);
    expect(shown).toBe(`ab "c ${'d'.repeat(4091)}"…`);
  });
});

describe('formatChatBody', () => {
  it('keeps line feeds and carriage returns', () => {
    expect(formatChatBody('a\r\nb\nc')).toBe('a\r\nb\nc');
  });

  it('writes the tab, the reordering and zero-width controls and the line separator out', () => {
    expect(formatChatBody('x\u202Ey')).toBe('x\\u{202E}y');
    expect(formatChatBody('x\u200By')).toBe('x\\u{200B}y');
    expect(formatChatBody('x\u2028y')).toBe('x\\u{2028}y');
    expect(formatChatBody('x\ty')).toBe('x\\u{0009}y');
  });

  it('writes out the invisible characters that carry no emoji shape', () => {
    expect(formatChatBody('a\u{E0041}b')).toBe('a\\u{E0041}b');
    expect(formatChatBody('a\u3164b')).toBe('a\\u{3164}b');
    expect(formatChatBody('a\u200Db')).toBe('a\\u{200D}b');
    expect(formatChatBody('a\u200Cb')).toBe('a\\u{200C}b');
    expect(formatChatBody('a\uFE00')).toBe('a\\u{FE00}');
    expect(formatChatBody('\u{1F468}\u200Db')).toBe('\u{1F468}\\u{200D}b');
  });

  it('writes a variation selector out unless it follows a pictograph or completes a keycap', () => {
    expect(formatChatBody('1\uFE0F')).toBe('1\\u{FE0F}');
    expect(formatChatBody('#\uFE0F')).toBe('#\\u{FE0F}');
    expect(formatChatBody('*\uFE0E')).toBe('*\\u{FE0E}');
    expect(formatChatBody('1\uFE0E\u20E3')).toBe('1\\u{FE0E}\u20E3');
    expect(formatChatBody('1\uFE0F\uFE0F\u20E3')).toBe('1\\u{FE0F}\\u{FE0F}\u20E3');
    expect(formatChatBody('\u26A0\uFE0F\uFE0F')).toBe('\u26A0\uFE0F\\u{FE0F}');
  });

  it('draws the emoji shapes: a joined sequence, a presentation selector and a keycap', () => {
    expect(formatChatBody('\u{1F468}\u200D\u{1F469}')).toBe('\u{1F468}\u200D\u{1F469}');
    expect(formatChatBody('\u{1F468}\u{1F3FB}\u200D\u{1F469}')).toBe(
      '\u{1F468}\u{1F3FB}\u200D\u{1F469}'
    );
    expect(formatChatBody('\u26A0\uFE0F')).toBe('\u26A0\uFE0F');
    expect(formatChatBody('1\uFE0F\u20E3')).toBe('1\uFE0F\u20E3');
  });

  it('cuts at the display limit in code points before writing out', () => {
    expect(formatChatBody(`${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\u202Etail`)).toBe(
      `${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\\u{202E}…`
    );
  });
});

describe('formatOutputBlock', () => {
  it('keeps line feeds and tabs', () => {
    expect(formatOutputBlock('a\n\tb')).toBe('a\n\tb');
  });

  it('writes the reordering and zero-width controls, the carriage return and tag characters out', () => {
    expect(formatOutputBlock('x\u202Ey')).toBe('x\\u{202E}y');
    expect(formatOutputBlock('x\u200By')).toBe('x\\u{200B}y');
    expect(formatOutputBlock('x\ry')).toBe('x\\u{000D}y');
    expect(formatOutputBlock('x\u{E0041}y')).toBe('x\\u{E0041}y');
  });

  it('draws a pictograph with its presentation selector', () => {
    expect(formatOutputBlock('\u26A0\uFE0F')).toBe('\u26A0\uFE0F');
  });

  it('cuts at the display limit in code points before writing out', () => {
    expect(formatOutputBlock(`${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\u202Etail`)).toBe(
      `${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 1)}\\u{202E}…`
    );
  });
});

describe('joinDisplayBlocks', () => {
  it('joins a line and a block with one line feed each', () => {
    expect(joinDisplayBlocks([sanitizeForDisplay('a'), formatApprovalBody('b\nc')])).toBe(
      'a\nb\nc'
    );
  });

  it('cuts the whole at the display limit and marks the cut', () => {
    const joined = joinDisplayBlocks([
      sanitizeForDisplay('x'.repeat(MAX_DISPLAY_FIELD_CHARS)),
      sanitizeForDisplay('tail'),
    ]);
    expect(joined).toBe(`${'x'.repeat(MAX_DISPLAY_FIELD_CHARS)}…`);
  });

  it('never cuts a written-out character in half: the cut moves before it', () => {
    const joined = joinDisplayBlocks([
      sanitizeForDisplay('a'.repeat(MAX_DISPLAY_FIELD_CHARS - 3)),
      formatPathForDisplay('\u202E'),
    ]);
    expect(joined).toBe(`${'a'.repeat(MAX_DISPLAY_FIELD_CHARS - 3)}\n…`);
  });
});

describe('sliceDisplayText', () => {
  // The arguments after the text are not fixed by any design line; this reads them as the
  // start and end of String.prototype.slice, which the chat body split uses today.
  async function loadSlice(): Promise<(text: string, start: number, end?: number) => string> {
    const mod: Record<string, unknown> = await import('../display/untrusted-text');
    return mod['sliceDisplayText'] as (text: string, start: number, end?: number) => string;
  }

  it('slices ordinary text like String.prototype.slice', async () => {
    const slice = await loadSlice();
    expect(slice('hello @bob', 6)).toBe('@bob');
    expect(slice('hello @bob', 0, 5)).toBe('hello');
  });

  it('never leaves half of a written-out character at either end of a slice', async () => {
    const slice = await loadSlice();
    const text = 'a\\u{202E}b';
    const head = slice(text, 0, 4);
    expect(['a', 'a\\u{202E}']).toContain(head);
    const tail = slice(text, 4);
    expect(['\\u{202E}b', 'b']).toContain(tail);
  });
});
