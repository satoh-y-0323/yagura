/**
 * Which addressee a request with no target window is shown with, and the facts
 * the approval dialog draws a request from. Expected wording is read from the
 * i18n resource, so these tests pin which message each request picks rather
 * than the message itself.
 */
import { describe, it, expect } from 'vitest';

import { approvalFactsOf, describeUntargeted } from './approval-target';
import { formatApprovalBody, sanitizeForDisplay } from '../display/untrusted-text';
import type { ApprovalRequest, WindowStatus } from '../ipc/session-boundary';
import { partyRuns, type HeadlineParty } from './approval-types';
import { describeStatus, dialogs } from '../ui-messages';

const REQUESTER_MARK = { cells: 0b010_101_011_100_110, hue: 3 };
const TARGET_MARK = { cells: 0b101_010_100_011_001, hue: 9 };

const WINDOW_REQUESTER = {
  kind: 'window',
  window_id: 'w-req',
  displayWindowId: sanitizeForDisplay('w-req'),
  name: sanitizeForDisplay('builder'),
  profile: sanitizeForDisplay('claude-code'),
  status: 'running',
  mark: REQUESTER_MARK,
} as const;

const TARGET = {
  window_id: 'w-target',
  displayWindowId: sanitizeForDisplay('w-target'),
  name: sanitizeForDisplay('reviewer'),
  profile: sanitizeForDisplay('codex'),
  status: 'waiting_input',
  mark: TARGET_MARK,
} as const;

function request(overrides: Partial<ApprovalRequest>): ApprovalRequest {
  const tool = overrides.tool ?? 'send_chat';
  return {
    request_id: 'a1-test',
    tool,
    displayTool: sanitizeForDisplay(tool),
    requester: WINDOW_REQUESTER,
    text: null,
    ...overrides,
  };
}

function external(kind: 'external' | 'unknown'): ApprovalRequest['requester'] {
  return { kind, window_id: null, name: null };
}

/** The text a party reads as in a sentence. */
function textOf(party: HeadlineParty): string {
  return partyRuns(party)
    .map((run) => run.text)
    .join('');
}

/** A handoff that opens its window from a profile, so it carries no target. */
const HANDOFF_TO_NEW_WINDOW = request({
  tool: 'send_handoff',
  text: formatApprovalBody('profile: codex\n\nenvelope'),
});

describe('describeUntargeted', () => {
  it('names the room a request carries by its name, set apart from the words around it', () => {
    const named = request({ room: { room_id: 'main', name: 'Main' } });

    expect(describeUntargeted(named)).toEqual({
      kind: 'room',
      parts: {
        before: dialogs.approvalRoomWords.before,
        name: 'Main',
        after: dialogs.approvalRoomWords.after,
        sentence: 'the chat room Main',
      },
    });
    // A room's name is the last name of a path, so a reordering control and a no-break space are written out.
    const reordered = request({ room: { room_id: 'r', name: 'ya\u202Egura' } });
    expect(describeUntargeted(reordered)).toMatchObject({
      kind: 'room',
      parts: { name: 'ya\\u{202E}gura' },
    });
    const spaced = request({ room: { room_id: 's', name: 'Ma\u00A0in' } });
    expect(describeUntargeted(spaced)).toMatchObject({
      kind: 'room',
      parts: { name: 'Ma\\u{00A0}in' },
    });
    const padded = request({ room: { room_id: 'p', name: ' Main ' } });
    expect(describeUntargeted(padded)).toMatchObject({
      kind: 'room',
      parts: { name: '\\u{0020}Main\\u{0020}' },
    });
  });

  it('names a new window for a request to open one', () => {
    const opening = request({ tool: 'open_window' });

    expect(describeUntargeted(opening)).toEqual({
      kind: 'words',
      words: dialogs.approvalTargetNewWindow,
    });
  });

  it('names a new window for a handoff that opens one from a profile', () => {
    const opening = request({
      tool: 'send_handoff',
      text: formatApprovalBody('profile: claude-code\n\nenvelope'),
    });

    expect(describeUntargeted(opening)).toEqual({
      kind: 'words',
      words: dialogs.approvalTargetNewWindow,
    });
  });

  it.each(['send_chat', 'broadcast_chat'])(
    'falls back to the chat room as a whole for a %s request that names no room',
    (tool) => {
      expect(describeUntargeted(request({ tool }))).toEqual({
        kind: 'words',
        words: dialogs.approvalTargetRoom,
      });
    }
  );
});

describe('approvalFactsOf: who asks', () => {
  const toWindow = request({ tool: 'close_window', target: TARGET });

  it('shows a listed window requester by its name, launch profile, status, ID and mark', () => {
    const { from, tool } = approvalFactsOf(toWindow, true);

    expect(from).toEqual({
      kind: 'window',
      label: { kind: 'name', name: WINDOW_REQUESTER.name },
      profile: WINDOW_REQUESTER.profile,
      status: describeStatus(WINDOW_REQUESTER.status),
      window: { id: WINDOW_REQUESTER.displayWindowId, mark: REQUESTER_MARK },
    });
    expect(tool).toBe(toWindow.displayTool);
  });

  it('leaves the launch profile out for a window opened from a command', () => {
    const fromCommand = request({
      tool: 'close_window',
      target: { ...TARGET, profile: null },
      requester: { ...WINDOW_REQUESTER, profile: null },
    });

    const facts = approvalFactsOf(fromCommand, true);

    expect(facts.from.profile).toBeNull();
    expect(facts.to.profile).toBeNull();
  });

  it('keeps a closed requester window by its name, ID and mark, with a status no live window has', () => {
    const { from } = approvalFactsOf(toWindow, false);

    expect(from.kind).toBe('window');
    expect(from.label).toEqual({ kind: 'name', name: WINDOW_REQUESTER.name });
    expect(from).toMatchObject({
      window: { id: WINDOW_REQUESTER.displayWindowId, mark: REQUESTER_MARK },
    });
    expect(from.status).not.toBeNull();
    const liveStatuses: readonly WindowStatus[] = [
      'running',
      'waiting_input',
      'waiting_approval',
      'interrupted',
      'idle',
      'exited',
      'unknown',
    ];
    for (const status of liveStatuses) {
      expect(from.status).not.toBe(describeStatus(status));
    }
  });

  it.each(['external', 'unknown'] as const)(
    'shows a %s requester as an external client with no mark, ID, profile or status',
    (kind) => {
      const { from } = approvalFactsOf({ ...toWindow, requester: external(kind) }, true);

      expect(from).toEqual({
        kind: 'external',
        label: { kind: 'words', words: dialogs.approvalRequesterExternal },
        profile: null,
        status: null,
      });
    }
  );
});

describe('approvalFactsOf: what it acts on', () => {
  it('shows a target window by its name, launch profile, status when asked, ID and mark', () => {
    const { to } = approvalFactsOf(request({ tool: 'send_message', target: TARGET }), true);

    expect(to).toEqual({
      kind: 'window',
      label: { kind: 'name', name: TARGET.name },
      profile: TARGET.profile,
      status: describeStatus(TARGET.status),
      window: { id: TARGET.displayWindowId, mark: TARGET_MARK },
    });
  });

  it.each([
    ['a new window', request({ tool: 'open_window' })],
    ['a new window for a handoff', HANDOFF_TO_NEW_WINDOW],
    ['the room it names', request({ tool: 'send_chat', room: { room_id: 'm', name: 'Main' } })],
    ['the chat room as a whole', request({ tool: 'broadcast_chat' })],
  ])('shows %s as an empty card with no mark', (_label, untargeted) => {
    const { to } = approvalFactsOf(untargeted, true);

    expect(to).toEqual({
      kind: 'untargeted',
      label: describeUntargeted(untargeted),
      profile: null,
      status: null,
    });
  });
});

const MAIN_ROOM = { room_id: 'm', name: 'Main' };

/** Every form of request whose heading the dialog words differently, from the same requester. */
const HEADED_FORMS: readonly [string, ApprovalRequest][] = [
  ['send_message', request({ tool: 'send_message', target: TARGET })],
  ['send_handoff to a window', request({ tool: 'send_handoff', target: TARGET })],
  ['send_handoff to a new window', HANDOFF_TO_NEW_WINDOW],
  ['interrupt_window', request({ tool: 'interrupt_window', target: TARGET })],
  ['close_window', request({ tool: 'close_window', target: TARGET })],
  ['open_window', request({ tool: 'open_window' })],
  ['send_chat', request({ tool: 'send_chat', room: MAIN_ROOM })],
  ['broadcast_chat', request({ tool: 'broadcast_chat', room: MAIN_ROOM })],
  ['a tool this build does not know', request({ tool: 'future_tool', target: TARGET })],
];

describe('approvalFactsOf: the heading', () => {
  it('words each tool with a sentence of its own', () => {
    const sentences = HEADED_FORMS.map(([, form]) => approvalFactsOf(form, true).headline.sentence);

    expect(new Set(sentences).size).toBe(HEADED_FORMS.length);
    // A room's name stands as a name in the sentence, with no quotes around it.
    const toRoom = request({ tool: 'broadcast_chat', room: MAIN_ROOM });
    expect(approvalFactsOf(toRoom, true).headline.sentence).toBe(
      'builder wants to post to everyone in the chat room Main'
    );
  });

  it.each(HEADED_FORMS)(
    'starts the %s sentence with the requester, sets the names apart without quotes, and ends without a period',
    (_label, form) => {
      const facts = approvalFactsOf(form, true);
      const { sentence } = facts.headline;

      // The requester window's name goes in the part drawn as a name, never in one drawn as words.
      expect(facts.headline.from).toEqual({ kind: 'name', name: WINDOW_REQUESTER.name });
      expect(facts.headline.from).toEqual(facts.from.label);
      expect(sentence.startsWith(WINDOW_REQUESTER.name)).toBe(true);
      expect(sentence).not.toMatch(/\.$/);
      for (const name of [WINDOW_REQUESTER.name, TARGET.name, MAIN_ROOM.name]) {
        expect(sentence).not.toContain(`“${name}”`);
        expect(sentence).not.toContain(`"${name}"`);
      }
    }
  );

  it.each(HEADED_FORMS.filter(([, form]) => form.target !== undefined))(
    'names the target window after the requester in the %s sentence',
    (_label, form) => {
      const facts = approvalFactsOf(form, true);
      const { sentence } = facts.headline;

      expect(facts.headline.to).toEqual(facts.to.label);
      expect(facts.headline.to).toEqual({ kind: 'name', name: TARGET.name });
      expect(sentence.indexOf(TARGET.name, WINDOW_REQUESTER.name.length)).toBeGreaterThan(0);
    }
  );

  it.each(HEADED_FORMS.filter(([, form]) => form.target === undefined))(
    'names what the %s request is addressed to in its sentence',
    (_label, form) => {
      const facts = approvalFactsOf(form, true);

      // The heading and the card name the addressee by the same value: a room by its name, a
      // window that does not exist yet by words.
      expect(facts.headline.to).toEqual(facts.to.label);
      expect(facts.headline.to?.kind).toBe(form.room === undefined ? 'words' : 'room');
      expect(facts.headline.sentence).toContain(textOf(describeUntargeted(form)));
    }
  );

  it('puts the name of a tool this build does not know in its sentence', () => {
    const onWindow = request({ tool: 'future_tool', target: TARGET });
    const onNothing = request({ tool: 'future_tool' });

    // Whether ` on ` links the tool to a target is decided by the English resource, so it is spelled out.
    expect(approvalFactsOf(onWindow, true).headline.sentence).toBe(
      'builder wants to use future_tool on reviewer'
    );
    expect(approvalFactsOf(onNothing, true).headline.sentence).toBe(
      'builder wants to use future_tool'
    );
    expect(approvalFactsOf(onNothing, true).headline.to).toBeNull();
  });

  it.each(HEADED_FORMS)(
    'opens the %s sentence with the same words for an external client and a requester of an unknown kind',
    (_label, form) => {
      const fromWindow = approvalFactsOf(form, true).headline.sentence;
      const rest = fromWindow.slice(WINDOW_REQUESTER.name.length);
      const clientFacts = approvalFactsOf({ ...form, requester: external('external') }, true);
      const unknownFacts = approvalFactsOf({ ...form, requester: external('unknown') }, true);
      const fromExternal = clientFacts.headline.sentence;
      const fromUnknown = unknownFacts.headline.sentence;

      expect(fromExternal.endsWith(rest)).toBe(true);
      const opening = fromExternal.slice(0, fromExternal.length - rest.length);
      expect(opening).not.toBe('');
      expect(opening).not.toBe(WINDOW_REQUESTER.name);
      expect(fromUnknown).toBe(fromExternal);
      // A client that is no window has no name to show, so the heading opens with words.
      expect(clientFacts.headline.from).toEqual({ kind: 'words', words: opening });
      expect(unknownFacts.headline.from).toEqual({ kind: 'words', words: opening });
    }
  );
});

describe('approvalFactsOf: the heading of the body', () => {
  function bodyHeadingOf(form: ApprovalRequest): unknown {
    return approvalFactsOf(form, true).bodyHeading;
  }

  it('gives a tool that carries no body no heading for one', () => {
    expect(bodyHeadingOf(request({ tool: 'interrupt_window', target: TARGET }))).toBeNull();
    expect(bodyHeadingOf(request({ tool: 'close_window', target: TARGET }))).toBeNull();
  });

  it('heads a message, a handoff, a command and the body of an unknown tool each its own way', () => {
    const message = bodyHeadingOf(request({ tool: 'send_message', target: TARGET }));
    const handoff = bodyHeadingOf(request({ tool: 'send_handoff', target: TARGET }));
    const command = bodyHeadingOf(request({ tool: 'open_window' }));
    const details = bodyHeadingOf(request({ tool: 'future_tool', target: TARGET }));

    for (const heading of [message, handoff, command, details]) {
      expect(heading).not.toBeNull();
    }
    expect(new Set([message, handoff, command, details]).size).toBe(4);
    // A post to a room is a message; a handoff that opens a window is still a handoff.
    expect(bodyHeadingOf(request({ tool: 'send_chat', room: MAIN_ROOM }))).toBe(message);
    expect(bodyHeadingOf(request({ tool: 'broadcast_chat', room: MAIN_ROOM }))).toBe(message);
    expect(bodyHeadingOf(HANDOFF_TO_NEW_WINDOW)).toBe(handoff);
  });
});
