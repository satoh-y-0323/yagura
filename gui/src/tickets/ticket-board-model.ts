/**
 * The ticket board's display logic as pure functions: which lists the board
 * reads, how the columns are laid out from the user's settings, the order
 * inside a column, what a ticket-changed push reads again, and when the plans
 * panel counts as showing the plan board.
 */
import type { TicketListQuery } from '../ipc/ticket-client';
import type {
  TicketChanged,
  TicketConfig,
  TicketList,
  TicketMilestone,
  TicketSummary,
} from '../ipc/ticket-boundary';

/** The tabs of the plans panel, in the order they are shown. */
export type PlansTab = 'tickets' | 'board' | 'roles' | 'models';

/** Whether the plans panel is open, and which tab it shows. */
export interface PlansPanelView {
  open: boolean;
  tab: PlansTab;
}

/** One column of the ticket board. */
export interface TicketColumn {
  state: string;
  /** False for a state the settings do not name, kept so its tickets stay visible. */
  configured: boolean;
  tickets: TicketSummary[];
  /** Whether more tickets are in this column than the read returned. */
  truncated: boolean;
}

/** The board as drawn: columns, or the reason there are none. */
export type TicketBoard =
  { kind: 'ready'; columns: TicketColumn[] } | { kind: 'unconfigured'; reason: string };

/** What a ticket-changed push reads again. */
export interface TicketReload {
  workspaces: boolean;
  board: boolean;
}

/**
 * One list read per configured state, in the settings' order, so a full
 * column never pushes another column's tickets out of the board.
 * @param workspaceId the workspace shown
 * @param config the user's ticket settings
 * @param milestone the milestone filter, `null` for all milestones
 */
export function ticketColumnQueries(
  workspaceId: number,
  config: TicketConfig,
  milestone: string | null
): TicketListQuery[] {
  return config.states.map((state) => ({
    workspaceId,
    state: state.name,
    ...(milestone === null ? {} : { milestone }),
  }));
}

/**
 * Lays out the board from an unfiltered read (`overview`: settings,
 * milestones, and tickets in states the settings no longer name) and one
 * read per configured state (`columnLists`, keyed by state name).
 * @param overview the read not filtered by state
 * @param columnLists the per-state reads
 */
export function buildTicketBoard(
  overview: TicketList,
  columnLists: ReadonlyMap<string, TicketList>
): TicketBoard {
  const config = overview.config;
  if (config === null) {
    return { kind: 'unconfigured', reason: overview.config_error ?? '' };
  }
  const order = milestoneOrder(overview.milestones);
  const configuredNames = new Set(config.states.map((state) => state.name));
  const configured = config.states.map((state): TicketColumn => {
    const read = columnLists.get(state.name);
    return {
      state: state.name,
      configured: true,
      tickets: sortColumn(read?.tickets ?? [], order),
      truncated: read?.truncated ?? false,
    };
  });
  const unnamed = new Map<string, TicketSummary[]>();
  for (const ticket of overview.tickets) {
    if (!configuredNames.has(ticket.state)) {
      unnamed.set(ticket.state, [...(unnamed.get(ticket.state) ?? []), ticket]);
    }
  }
  const extra = [...unnamed.keys()].sort(compareText).map((state): TicketColumn => ({
    state,
    configured: false,
    tickets: sortColumn(unnamed.get(state) ?? [], order),
    truncated: overview.truncated,
  }));
  return { kind: 'ready', columns: [...configured, ...extra] };
}

/**
 * Which reads a ticket-changed push calls for: the workspace list when the
 * push names a workspace not listed yet, the board when it names the one shown.
 * @param push the push received
 * @param shownWorkspaceId the workspace the board shows, `null` for none
 * @param knownWorkspaceIds the workspaces already listed
 */
export function ticketReloadFor(
  push: TicketChanged,
  shownWorkspaceId: number | null,
  knownWorkspaceIds: readonly number[]
): TicketReload {
  const isKnown = knownWorkspaceIds.includes(push.workspace_id);
  return {
    workspaces: !isKnown,
    board: isKnown && push.workspace_id === shownWorkspaceId,
  };
}

/**
 * Whether the plan board has just come into view: the panel's open state and
 * tab went from not showing the Board tab to showing it.
 * @param previous the panel before the change
 * @param next the panel after the change
 */
export function planBoardBecameVisible(previous: PlansPanelView, next: PlansPanelView): boolean {
  return !showsPlanBoard(previous) && showsPlanBoard(next);
}

/**
 * The name shown for a workspace: the last segment of its path.
 * @param path the workspace's path, already prepared for display
 */
export function ticketWorkspaceName(path: string): string {
  const segments = path.split(/[\\/]/).filter((segment) => segment !== '');
  return segments[segments.length - 1] ?? path;
}

function showsPlanBoard(view: PlansPanelView): boolean {
  return view.open && view.tab === 'board';
}

interface MilestoneRank {
  position: number;
  name: string;
}

function milestoneOrder(
  milestones: readonly TicketMilestone[]
): ReadonlyMap<string, MilestoneRank> {
  return new Map(
    milestones.map((entry) => [entry.name, { position: entry.position, name: entry.name }])
  );
}

// A milestone the list does not carry sorts with the tickets that have none.
function sortColumn(
  tickets: readonly TicketSummary[],
  order: ReadonlyMap<string, MilestoneRank>
): TicketSummary[] {
  const rankOf = (ticket: TicketSummary): MilestoneRank | undefined =>
    ticket.milestone === null ? undefined : order.get(ticket.milestone);
  return [...tickets].sort((left, right) => {
    const leftRank = rankOf(left);
    const rightRank = rankOf(right);
    if (leftRank !== undefined && rightRank !== undefined) {
      const byMilestone =
        leftRank.position - rightRank.position || compareText(leftRank.name, rightRank.name);
      if (byMilestone !== 0) {
        return byMilestone;
      }
    } else if (leftRank !== undefined || rightRank !== undefined) {
      return leftRank === undefined ? 1 : -1;
    }
    return left.number - right.number;
  });
}

function compareText(left: string, right: string): number {
  if (left === right) {
    return 0;
  }
  return left < right ? -1 : 1;
}
