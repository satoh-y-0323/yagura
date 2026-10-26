/**
 * The ticket board's display logic, measured as pure functions: which lists
 * the board reads (one per state column), how the columns are laid out from
 * the user's settings, where a ticket in a state the settings do not name
 * goes, the order inside a column, which columns were cut short, what a
 * missing or broken settings file shows, and what a ticket-changed push reads
 * again.
 */
import { describe, expect, it } from 'vitest';

import type {
  TicketConfig,
  TicketList,
  TicketMilestone,
  TicketSummary,
} from '../ipc/ticket-boundary';

import { buildTicketBoard, ticketColumnQueries, ticketReloadFor } from './ticket-board-model';

function summary(number: number, state: string, milestone: string | null = null): TicketSummary {
  return {
    number,
    title: `ticket ${number}`,
    state,
    milestone,
    updated_at: 0,
    plan_count: 0,
    fields: {},
  };
}

function config(states: string[]): TicketConfig {
  return {
    guide: 'guide',
    states: states.map((name) => ({ name, description: '', requires: [] })),
    fields: [],
  };
}

function list(
  tickets: TicketSummary[],
  options: {
    config?: TicketConfig | null;
    configError?: string | null;
    milestones?: TicketMilestone[];
    truncated?: boolean;
  } = {}
): TicketList {
  return {
    workspace_id: 1,
    config: options.config === undefined ? config(['todo', 'in-progress', 'done']) : options.config,
    config_error: options.configError ?? null,
    milestones: options.milestones ?? [],
    tickets,
    truncated: options.truncated ?? false,
  };
}

function milestone(name: string, position: number): TicketMilestone {
  return { name, position, title: name, description: null };
}

function readyColumns(board: ReturnType<typeof buildTicketBoard>) {
  if (board.kind !== 'ready') {
    throw new Error(`expected a ready board, got ${board.kind}`);
  }
  return board.columns;
}

describe('ticketColumnQueries', () => {
  it('reads each state column on its own, in the order the settings list the states', () => {
    const queries = ticketColumnQueries(7, config(['todo', 'in-progress', 'done']), null);

    expect(queries.map((query) => query.state)).toEqual(['todo', 'in-progress', 'done']);
    for (const query of queries) {
      expect(query.workspaceId).toBe(7);
      expect(query.milestone).toBeUndefined();
    }
  });

  it('carries the milestone filter into every column read', () => {
    const queries = ticketColumnQueries(7, config(['todo', 'done']), 'm5');

    expect(queries.map((query) => [query.state, query.milestone])).toEqual([
      ['todo', 'm5'],
      ['done', 'm5'],
    ]);
  });
});

describe('buildTicketBoard', () => {
  it('lays out the columns in the order the settings list the states', () => {
    const overview = list([], { config: config(['todo', 'in-progress', 'done']) });
    const board = buildTicketBoard(
      overview,
      new Map([
        ['done', list([summary(1, 'done')])],
        ['todo', list([summary(2, 'todo')])],
        ['in-progress', list([summary(3, 'in-progress')])],
      ])
    );

    const columns = readyColumns(board);
    expect(columns.map((column) => column.state)).toEqual(['todo', 'in-progress', 'done']);
    expect(columns.map((column) => column.tickets.map((ticket) => ticket.number))).toEqual([
      [2],
      [3],
      [1],
    ]);
    expect(columns.every((column) => column.configured)).toBe(true);
  });

  it('keeps a ticket whose state the settings do not name, in a column of its own after the configured ones', () => {
    const overview = list(
      [summary(1, 'todo'), summary(4, 'review'), summary(5, 'archived'), summary(6, 'review')],
      { config: config(['todo', 'done']) }
    );
    const board = buildTicketBoard(
      overview,
      new Map([
        ['todo', list([summary(1, 'todo')])],
        ['done', list([])],
      ])
    );

    const columns = readyColumns(board);
    expect(columns.map((column) => [column.state, column.configured])).toEqual([
      ['todo', true],
      ['done', true],
      ['archived', false],
      ['review', false],
    ]);
    expect(columns.map((column) => column.tickets.map((ticket) => ticket.number))).toEqual([
      [1],
      [],
      [5],
      [4, 6],
    ]);
  });

  it('orders a column by milestone position, tickets without a milestone last, then by number', () => {
    const milestones = [milestone('m5', 2), milestone('m4', 1)];
    const overview = list([], { config: config(['todo']), milestones });
    const board = buildTicketBoard(
      overview,
      new Map([
        [
          'todo',
          list(
            [
              summary(5, 'todo'),
              summary(9, 'todo', 'm5'),
              summary(3, 'todo', 'm5'),
              summary(7, 'todo', 'm4'),
              summary(2, 'todo'),
            ],
            { milestones }
          ),
        ],
      ])
    );

    const todo = readyColumns(board)[0];
    expect(todo?.tickets.map((ticket) => ticket.number)).toEqual([7, 3, 9, 2, 5]);
  });

  it('marks only the columns whose own read was cut short', () => {
    const overview = list([], { config: config(['todo', 'done']) });
    const board = buildTicketBoard(
      overview,
      new Map([
        ['todo', list([summary(1, 'todo')], { truncated: false })],
        ['done', list([summary(2, 'done')], { truncated: true })],
      ])
    );

    const columns = readyColumns(board);
    expect(columns.map((column) => [column.state, column.truncated])).toEqual([
      ['todo', false],
      ['done', true],
    ]);
  });

  it('marks the columns of unnamed states as cut short when the read they came from was', () => {
    const overview = list([summary(4, 'review')], { config: config(['todo']), truncated: true });
    const board = buildTicketBoard(overview, new Map([['todo', list([], { truncated: false })]]));

    const columns = readyColumns(board);
    expect(columns.map((column) => [column.state, column.truncated])).toEqual([
      ['todo', false],
      ['review', true],
    ]);
  });

  it('shows a ticket once even when the overview read also returned it', () => {
    const overview = list([summary(1, 'todo'), summary(2, 'done')], {
      config: config(['todo', 'done']),
    });
    const board = buildTicketBoard(
      overview,
      new Map([
        ['todo', list([summary(1, 'todo')])],
        ['done', list([summary(2, 'done')])],
      ])
    );

    const numbers = readyColumns(board).flatMap((column) =>
      column.tickets.map((ticket) => ticket.number)
    );
    expect(numbers).toEqual([1, 2]);
  });

  it('shows the reason and no columns when the settings are missing or broken', () => {
    const overview = list([summary(1, 'todo'), summary(2, 'done')], {
      config: null,
      configError: 'tickets.toml was not found',
    });

    expect(buildTicketBoard(overview, new Map())).toEqual({
      kind: 'unconfigured',
      reason: 'tickets.toml was not found',
    });
  });
});

describe('ticketReloadFor', () => {
  it('reads the board again for a push about the workspace shown', () => {
    expect(ticketReloadFor({ workspace_id: 3 }, 3, [1, 3])).toEqual({
      workspaces: false,
      board: true,
    });
  });

  it('reads nothing for a push about another workspace already listed', () => {
    expect(ticketReloadFor({ workspace_id: 1 }, 3, [1, 3])).toEqual({
      workspaces: false,
      board: false,
    });
  });

  it('reads the workspace list again for a push about a workspace not yet listed', () => {
    expect(ticketReloadFor({ workspace_id: 9 }, 3, [1, 3])).toEqual({
      workspaces: true,
      board: false,
    });
    expect(ticketReloadFor({ workspace_id: 9 }, null, [])).toEqual({
      workspaces: true,
      board: false,
    });
  });
});
