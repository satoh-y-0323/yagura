<script lang="ts">
  /**
   * The Tickets tab's board: the workspace and milestone pickers, and one
   * column per ticket state with a card per ticket.
   *
   * It owns no IPC and offers no edits: the shell reads the board and hands
   * it in. Ticket text is written by agents, so every string drawn here goes
   * through the ticket display functions first.
   */
  import { sanitizeForDisplay } from '../ipc/session-boundary';
  import {
    displayTicketSummary,
    displayTicketWorkspace,
    formatTicketText,
    type TicketMilestone,
    type TicketSummary,
    type TicketWorkspace,
  } from '../ipc/ticket-boundary';
  import {
    ticketWorkspaceName,
    type TicketBoard,
    type TicketColumn,
  } from '../tickets/ticket-board-model';
  import { tickets as text } from '../ui-messages';

  export let workspaces: TicketWorkspace[];
  export let selectedWorkspaceId: number | null;
  export let milestones: TicketMilestone[];
  /** The milestone the board is narrowed to, `null` for all of them. */
  export let milestoneFilter: string | null;
  /** `null` until the selected workspace has been read. */
  export let board: TicketBoard | null;
  export let onSelectWorkspace: (workspaceId: number) => void;
  export let onSelectMilestone: (milestone: string | null) => void;
  export let onOpenTicket: (ticket: number) => void;

  // An option value cannot be null, and no milestone name is empty.
  const ALL_MILESTONES = '';

  function workspaceOption(workspace: TicketWorkspace): string {
    const shown = displayTicketWorkspace(workspace);
    return `${ticketWorkspaceName(shown.path)} · ${shown.path}`;
  }

  function milestoneOption(milestone: TicketMilestone): string {
    return `${sanitizeForDisplay(milestone.name)} · ${sanitizeForDisplay(milestone.title)}`;
  }

  function fieldsOf(summary: TicketSummary): [string, string][] {
    return Object.entries(summary.fields).map(([name, value]) => [
      sanitizeForDisplay(name),
      formatTicketText(value),
    ]);
  }

  let columns: TicketColumn[];
  $: columns = board !== null && board.kind === 'ready' ? board.columns : [];
  let unconfiguredReason: string | null;
  $: unconfiguredReason = board !== null && board.kind === 'unconfigured' ? board.reason : null;
  $: ticketCount = columns.reduce((total, column) => total + column.tickets.length, 0);
</script>

<div class="ticket-board">
  {#if workspaces.length === 0}
    <p class="ticket-board__muted">{text.noWorkspaces}</p>
  {:else}
    <div class="ticket-board__picker">
      <select
        class="ticket-board__select ticket-board__workspace"
        aria-label={text.workspaceLabel}
        value={selectedWorkspaceId}
        on:change={(event) => onSelectWorkspace(Number(event.currentTarget.value))}
      >
        {#each workspaces as workspace (workspace.workspace_id)}
          <option value={workspace.workspace_id}>{workspaceOption(workspace)}</option>
        {/each}
      </select>
      <select
        class="ticket-board__select"
        aria-label={text.milestoneLabel}
        value={milestoneFilter ?? ALL_MILESTONES}
        on:change={(event) => {
          const value = event.currentTarget.value;
          onSelectMilestone(value === ALL_MILESTONES ? null : value);
        }}
      >
        <option value={ALL_MILESTONES}>{text.allMilestones}</option>
        {#each milestones as milestone (milestone.name)}
          <option value={milestone.name}>{milestoneOption(milestone)}</option>
        {/each}
      </select>
    </div>

    {#if unconfiguredReason !== null}
      <p class="ticket-board__problem" role="status">
        {text.unconfigured(sanitizeForDisplay(unconfiguredReason))}
      </p>
    {:else if board !== null}
      <p class="ticket-board__note">{text.boardNote(ticketCount, milestones.length)}</p>
      <div class="ticket-board__columns">
        {#each columns as column (column.state)}
          <section class="ticket-board__column" data-configured={column.configured}>
            <h3 class="ticket-board__state">
              <span>{sanitizeForDisplay(column.state)}</span>
              <span class="ticket-board__count">{column.tickets.length}</span>
            </h3>
            <ul class="ticket-board__cards">
              {#each column.tickets as summary (summary.number)}
                {@const shown = displayTicketSummary(summary)}
                <li>
                  <button
                    type="button"
                    class="ticket-card"
                    on:click={() => onOpenTicket(summary.number)}
                  >
                    <span class="ticket-card__head">
                      <span class="ticket-card__number">{shown.label}</span>
                      <span class="ticket-card__title">{shown.title}</span>
                    </span>
                    {#each fieldsOf(summary) as [name, value] (name)}
                      <span class="ticket-card__field"
                        ><span class="ticket-card__field-name">{name}</span>: {value}</span
                      >
                    {/each}
                    {#if shown.milestone !== null || summary.plan_count > 0}
                      <span class="ticket-card__meta">
                        {#if shown.milestone !== null}
                          <span class="ticket-card__chip">{shown.milestone}</span>
                        {/if}
                        {#if summary.plan_count > 0}
                          <span>{text.planCount(summary.plan_count)}</span>
                        {/if}
                      </span>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
            {#if column.truncated}
              <p class="ticket-board__more">{text.columnTruncated}</p>
            {/if}
          </section>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .ticket-board {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    min-height: 0;
    flex: 1 1 auto;
  }

  .ticket-board__picker {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
  }

  .ticket-board__select {
    min-width: 0;
  }

  .ticket-board__workspace {
    flex: 1 1 auto;
  }

  .ticket-board__note,
  .ticket-board__muted,
  .ticket-board__more {
    margin: 0;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-board__problem {
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-warning);
    color: var(--color-on-warning);
    font-size: var(--font-size-sm);
    overflow-wrap: anywhere;
  }

  .ticket-board__columns {
    flex: 1 1 auto;
    display: flex;
    gap: var(--space-sm);
    overflow: auto;
    min-height: 0;
  }

  .ticket-board__column {
    flex: 0 0 200px;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .ticket-board__state {
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: flex-start;
    gap: var(--space-xs);
    font-size: var(--font-size-xs);
    font-weight: 600;
    color: var(--color-text-secondary);
    overflow-wrap: anywhere;
  }

  .ticket-board__count {
    padding: 0 var(--space-xs);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    border: 1px solid var(--color-border);
    font-weight: 400;
    font-variant-numeric: tabular-nums;
  }

  .ticket-board__cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .ticket-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    width: 100%;
    padding: var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-background);
    color: inherit;
    font: inherit;
    font-size: var(--font-size-sm);
    text-align: left;
    cursor: pointer;
  }

  .ticket-card:hover {
    border-color: var(--color-text-secondary);
  }

  .ticket-card__head {
    display: flex;
    gap: var(--space-xs);
    align-items: baseline;
  }

  .ticket-card__number {
    flex: 0 0 auto;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-card__title {
    font-weight: 600;
    color: var(--color-text-primary);
    overflow-wrap: anywhere;
  }

  /* A field value may run to several lines; the card shows its start and the
     ticket's details show all of it. */
  .ticket-card__field {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    overflow: hidden;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-card__field-name {
    font-weight: 600;
    color: var(--color-text-primary);
  }

  .ticket-card__meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
    align-items: center;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-card__chip {
    padding: 0 var(--space-xs);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
  }
</style>
