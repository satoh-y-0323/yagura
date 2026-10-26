/**
 * TS-boundary contract for the ticket board: the narrowing functions that
 * turn a daemon-shaped reply into typed ticket values, the
 * `session://ticket-changed` payload, and the display functions that make
 * ticket text safe to draw. Ticket titles, bodies and results are written by
 * an AI and read by a person, so they are untrusted text.
 *
 * Lives next to `plan-boundary.test.ts`, which covers the plan ledger the
 * same way.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  MAX_DISPLAY_FIELD_CHARS,
  formatApprovalBody,
  sanitizeForDisplay,
} from './session-boundary';
import {
  MAX_TICKET_TEXT_CHARS,
  displayTicketSummary,
  displayTicketWorkspace,
  formatTicketText,
  narrowTicketChanged,
  narrowTicketDetail,
  narrowTicketList,
  narrowTicketWorkspaceList,
} from './ticket-boundary';

type Wire = Record<string, unknown>;

function workspaceWire(overrides: Wire = {}): Wire {
  return { workspace_id: 1, kind: 'git', path: 'C:\\repo', ...overrides };
}

function summaryWire(overrides: Wire = {}): Wire {
  return {
    number: 12,
    title: 'Add the ticket board',
    state: 'todo',
    milestone: 'v1',
    updated_at: 1_790_000_000,
    ...overrides,
  };
}

function listWire(overrides: Wire = {}): Wire {
  return {
    workspace_id: 1,
    config: {
      guide: 'One ticket, one purpose.',
      states: [
        { name: 'todo', description: 'Not started.', requires: [] },
        { name: 'on-hold', description: 'Paused.', requires: ['resume_trigger'] },
      ],
      fields: [{ name: 'resume_trigger', description: 'What brings it back.' }],
    },
    milestones: [{ name: 'v1', position: 1, title: 'First release', description: 'Ship it.' }],
    tickets: [summaryWire()],
    truncated: false,
    ...overrides,
  };
}

function detailWire(overrides: Wire = {}): Wire {
  return {
    number: 12,
    title: 'Add the ticket board',
    body: 'line one\nline two',
    state: 'on-hold',
    milestone: 'v1',
    fields: { resume_trigger: 'when the daemon lands' },
    results: [{ seq: 1, at: 1_790_000_100, text: 'merged' }],
    plans: ['p-abc123'],
    created_at: 1_790_000_000,
    updated_at: 1_790_000_200,
    ...overrides,
  };
}

describe('narrowTicketWorkspaceList', () => {
  it('reads the workspaces of a well-formed reply', () => {
    const rows = narrowTicketWorkspaceList({ workspaces: [workspaceWire()] });
    expect(rows).toEqual([{ workspace_id: 1, kind: 'git', path: 'C:\\repo' }]);
  });

  it('rejects a row whose id is not an integer', () => {
    expect(() =>
      narrowTicketWorkspaceList({ workspaces: [workspaceWire({ workspace_id: '1' })] })
    ).toThrow(BoundaryError);
  });

  it('rejects a reply that is not an object with a workspaces array', () => {
    expect(() => narrowTicketWorkspaceList(null)).toThrow(BoundaryError);
    expect(() => narrowTicketWorkspaceList({ workspaces: 'nope' })).toThrow(BoundaryError);
  });
});

describe('narrowTicketList', () => {
  it('reads the settings, milestones and ticket summaries', () => {
    const list = narrowTicketList(listWire());

    expect(list.workspace_id).toBe(1);
    expect(list.config?.states.map((state) => state.name)).toEqual(['todo', 'on-hold']);
    expect(list.config?.states[1]?.requires).toEqual(['resume_trigger']);
    expect(list.config?.fields[0]?.name).toBe('resume_trigger');
    expect(list.config_error).toBeNull();
    expect(list.milestones[0]?.title).toBe('First release');
    expect(list.tickets[0]?.number).toBe(12);
    expect(list.truncated).toBe(false);
  });

  it('reads a frame with no settings as settings-less and keeps the reason', () => {
    const wire = listWire({ config_error: 'tickets.toml is missing' });
    delete wire['config'];
    delete wire['workspace_id'];

    const list = narrowTicketList(wire);

    expect(list.config).toBeNull();
    expect(list.config_error).toBe('tickets.toml is missing');
    expect(list.workspace_id).toBeNull();
  });

  it('reads an absent milestone as none', () => {
    const wire = summaryWire();
    delete wire['milestone'];

    expect(narrowTicketList(listWire({ tickets: [wire] })).tickets[0]?.milestone).toBeNull();
  });

  it('keeps the truncated flag', () => {
    expect(narrowTicketList(listWire({ truncated: true })).truncated).toBe(true);
  });

  it('rejects frames of the wrong shape', () => {
    const cases: Wire[] = [
      listWire({ tickets: 'nope' }),
      listWire({ truncated: 'yes' }),
      listWire({ tickets: [summaryWire({ number: '12' })] }),
      listWire({ tickets: [summaryWire({ number: -1 })] }),
      listWire({ tickets: [summaryWire({ state: 3 })] }),
      listWire({ milestones: [{ name: 'v1', position: 'a', title: 't' }] }),
      listWire({ config: { guide: 'g', states: 'nope', fields: [] } }),
      listWire({
        config: { guide: 'g', states: [{ name: 'todo', description: 'd' }], fields: [] },
      }),
    ];
    for (const wire of cases) {
      expect(() => narrowTicketList(wire)).toThrow(BoundaryError);
    }
    expect(() => narrowTicketList('nope')).toThrow(BoundaryError);
  });
});

describe('narrowTicketDetail', () => {
  it('reads a ticket in full, with its fields, results and plans', () => {
    const ticket = narrowTicketDetail(detailWire());

    expect(ticket.number).toBe(12);
    expect(ticket.body).toBe('line one\nline two');
    expect(ticket.state).toBe('on-hold');
    expect(ticket.fields).toEqual({ resume_trigger: 'when the daemon lands' });
    expect(ticket.results).toEqual([{ seq: 1, at: 1_790_000_100, text: 'merged' }]);
    expect(ticket.plans).toEqual(['p-abc123']);
  });

  it('keeps a state name the settings no longer know', () => {
    expect(narrowTicketDetail(detailWire({ state: 'retired-state' })).state).toBe('retired-state');
  });

  it('rejects frames of the wrong shape', () => {
    const cases: Wire[] = [
      detailWire({ body: 5 }),
      detailWire({ fields: { reason: 7 } }),
      detailWire({ fields: ['reason'] }),
      detailWire({ results: [{ seq: -1, at: 1, text: 't' }] }),
      detailWire({ results: [{ seq: 1, at: 1 }] }),
      detailWire({ plans: [3] }),
      detailWire({ updated_at: 'now' }),
    ];
    for (const wire of cases) {
      expect(() => narrowTicketDetail(wire)).toThrow(BoundaryError);
    }
    const missing = detailWire();
    delete missing['title'];
    expect(() => narrowTicketDetail(missing)).toThrow(BoundaryError);
  });

  it('does not read an inherited field as if the frame owned it', () => {
    const inherited = Object.create({ title: 'inherited' }) as Wire;
    expect(() => narrowTicketDetail(inherited)).toThrow(BoundaryError);
  });
});

describe('narrowTicketChanged', () => {
  it('reads the workspace id of the push', () => {
    expect(narrowTicketChanged({ workspace_id: 3 })).toEqual({ workspace_id: 3 });
  });

  it('rejects a payload of the wrong shape', () => {
    expect(() => narrowTicketChanged(null)).toThrow(BoundaryError);
    expect(() => narrowTicketChanged({})).toThrow(BoundaryError);
    expect(() => narrowTicketChanged({ workspace_id: 'a' })).toThrow(BoundaryError);
  });
});

describe('formatTicketText', () => {
  it('keeps line feeds and tabs', () => {
    expect(formatTicketText('a\n\tb\n')).toBe('a\n\tb\n');
  });

  it('writes other control characters out instead of removing them', () => {
    expect(formatTicketText('a\u0000b\u007fc\rd')).toBe('a\\u{0000}b\\u{007F}c\\u{000D}d');
  });

  it('writes each bidirectional control out so the text reads as it is stored', () => {
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
      expect(formatTicketText(`x${control}y`)).toBe(`x\\u{${hex}}y`);
    }
  });

  it('keeps other scripts and emoji', () => {
    expect(formatTicketText('日本語 😀')).toBe('日本語 😀');
  });

  it('holds a body as long as the body limit and cuts beyond it in code points', () => {
    expect(MAX_TICKET_TEXT_CHARS).toBeGreaterThan(MAX_DISPLAY_FIELD_CHARS);
    const whole = '😀'.repeat(MAX_TICKET_TEXT_CHARS);
    expect(formatTicketText(whole)).toBe(whole);
    expect(formatTicketText(`${whole}\u202Etail`)).toBe(`${whole}…`);
  });

  it('never cuts an escape in half at the limit', () => {
    const shown = formatTicketText(`${'a'.repeat(MAX_TICKET_TEXT_CHARS - 1)}\u202Etail`);
    expect(shown).toBe(`${'a'.repeat(MAX_TICKET_TEXT_CHARS - 1)}\\u{202E}…`);
  });

  it('escapes everything formatApprovalBody does, so the rule has one source', () => {
    const sample = 'a\u0000\u202E\u2067b\u0085c\u009fd';
    expect(formatTicketText(sample)).toBe(formatApprovalBody(sample));
  });

  it('differs from the approval body only in keeping the tab', () => {
    expect(formatApprovalBody('a\tb')).toBe('a\\u{0009}b');
    expect(formatTicketText('a\tb')).toBe('a\tb');
  });
});

describe('the one-line fields of a ticket', () => {
  it('shows a summary through sanitizeForDisplay', () => {
    const wire = summaryWire({ title: 'a\u0000b\u202Ec', state: 'to\ndo', milestone: 'v\u00071' });
    const summary = narrowTicketList(listWire({ tickets: [wire] })).tickets[0];
    if (summary === undefined) {
      throw new Error('the summary was dropped');
    }

    const shown = displayTicketSummary(summary);

    expect(shown.title).toBe(sanitizeForDisplay('a\u0000b\u202Ec'));
    expect(shown.title).not.toContain('\u0000');
    expect(shown.state).toBe('todo');
    expect(shown.milestone).toBe('v1');
  });

  it('labels a ticket by its number', () => {
    const shown = displayTicketSummary({
      number: 12,
      title: 't',
      state: 's',
      milestone: null,
      updated_at: 1,
    });
    expect(shown.label).toBe('T-12');
    expect(shown.milestone).toBeNull();
  });

  it('caps a long title at the display limit', () => {
    const shown = displayTicketSummary({
      number: 1,
      title: 'x'.repeat(MAX_DISPLAY_FIELD_CHARS + 5),
      state: 's',
      milestone: null,
      updated_at: 1,
    });
    expect(shown.title).toBe(`${'x'.repeat(MAX_DISPLAY_FIELD_CHARS)}…`);
  });

  it('strips control characters from a workspace path', () => {
    const shown = displayTicketWorkspace({
      workspace_id: 1,
      kind: 'git',
      path: 'C:\\re\u0000po\n',
    });
    expect(shown.path).toBe('C:\\repo');
  });
});
