<script lang="ts">
  /**
   * One ticket's details in the Tickets tab: its description, fields,
   * results and the plans made from it.
   *
   * It owns no IPC and offers no edits. Ticket text is written by agents; the
   * boundary prepares every part drawn here.
   */
  import Icon from './Icon.svelte';
  import type {
    Ticket,
    TicketConfig,
    TicketFieldName,
    TicketMilestone,
    TicketPlanRef,
  } from '../ipc/ticket-boundary';
  import type { UiText } from '../display/ui-text';
  import type { DisplayBlock, DisplayLine } from '../display/untrusted-text';
  import { describeRecordedTime, tickets as text } from '../ui-messages';

  /** The least of a plan-list entry a plan link reads. */
  interface PlanChoice {
    plan_id: string;
    title: DisplayLine;
  }

  /** `null` while the ticket is being read. */
  export let ticket: Ticket | null;
  /** The user's ticket settings, `null` when they could not be read. */
  export let config: TicketConfig | null;
  export let milestones: TicketMilestone[];
  export let planList: PlanChoice[];
  export let onBack: () => void;
  export let onOpenPlan: (planId: string) => void;

  interface FieldRow extends TicketFieldName {
    /** `null` for a field the state requires that has no value. */
    value: DisplayBlock | null;
  }

  $: fields = ticket === null ? [] : fieldRows(ticket, config);

  // The fields the state requires come first, in the settings' order, then
  // any other value the ticket still carries.
  function fieldRows(current: Ticket, settings: TicketConfig | null): FieldRow[] {
    const required = settings?.states.find((state) => state.name === current.state)?.requires ?? [];
    const requiredNames = new Set(required.map((field) => field.name));
    const valueOf = (name: string): DisplayBlock | null =>
      current.fields.find((field) => field.name === name)?.value ?? null;
    const others = current.fields
      .filter((field) => !requiredNames.has(field.name))
      .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
    return [...required.map((field) => ({ ...field, value: valueOf(field.name) })), ...others];
  }

  function milestoneLabel(name: string, shownName: DisplayLine): UiText | DisplayLine {
    const milestone = milestones.find((entry) => entry.name === name);
    return milestone === undefined
      ? shownName
      : text.milestoneWithTitle(shownName, milestone.title);
  }

  function planLabel(plan: TicketPlanRef): DisplayLine {
    return planList.find((choice) => choice.plan_id === plan.planId)?.title ?? plan.displayPlanId;
  }

  function resultTime(at: number): UiText | null {
    return describeRecordedTime(at / 1000);
  }
</script>

<div class="ticket-detail">
  <button type="button" class="ticket-detail__back" on:click={onBack}
    ><Icon name="arrow-left" size="xs" /> {text.backButton}</button
  >

  {#if ticket !== null}
    <div class="ticket-detail__head">
      <span class="ticket-detail__number">{text.ticketLabel(ticket.number)}</span>
      <span class="ticket-detail__title">{ticket.title}</span>
      <span class="ticket-detail__state">{ticket.displayState}</span>
      {#if ticket.milestone !== null && ticket.displayMilestone !== null}
        <span class="ticket-detail__chip"
          >{milestoneLabel(ticket.milestone, ticket.displayMilestone)}</span
        >
      {/if}
    </div>

    <section class="ticket-detail__section">
      <h4 class="ticket-detail__heading">{text.descriptionHeading}</h4>
      <p class="ticket-detail__text">{ticket.body}</p>
    </section>

    <section class="ticket-detail__section">
      <h4 class="ticket-detail__heading">{text.fieldsHeading}</h4>
      {#if fields.length === 0}
        <p class="ticket-detail__muted">{text.noRequiredFields}</p>
      {:else}
        <dl class="ticket-detail__fields">
          {#each fields as row (row.name)}
            <dt>{row.displayName}</dt>
            <dd class:ticket-detail__muted={row.value === null}>{row.value ?? text.fieldNotSet}</dd>
          {/each}
        </dl>
      {/if}
    </section>

    <section class="ticket-detail__section">
      <h4 class="ticket-detail__heading">{text.resultsHeading}</h4>
      {#if ticket.results.length === 0}
        <p class="ticket-detail__muted">{text.noResults}</p>
      {:else}
        <ul class="ticket-detail__results">
          {#each ticket.results as result (result.seq)}
            <li>
              <time class="ticket-detail__time">{resultTime(result.at)}</time>
              <span class="ticket-detail__text">{result.text}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <section class="ticket-detail__section">
      <h4 class="ticket-detail__heading">{text.plansHeading}</h4>
      {#if ticket.plans.length === 0}
        <p class="ticket-detail__muted">{text.noPlans}</p>
      {:else}
        <div class="ticket-detail__plans">
          {#each ticket.plans as plan (plan.planId)}
            <button
              type="button"
              class="ticket-detail__plan"
              on:click={() => onOpenPlan(plan.planId)}>{planLabel(plan)}</button
            >
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .ticket-detail {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }

  .ticket-detail__back,
  .ticket-detail__plan {
    padding: 2px var(--space-sm);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background-color: var(--color-surface);
    color: var(--color-text-primary);
    cursor: pointer;
  }

  .ticket-detail__back {
    align-self: flex-start;
  }

  .ticket-detail__plan {
    color: var(--color-primary);
    overflow-wrap: anywhere;
  }

  .ticket-detail__head {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-sm);
    align-items: baseline;
  }

  .ticket-detail__number,
  .ticket-detail__time {
    font-family: var(--font-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-detail__title {
    font-weight: 600;
    color: var(--color-text-primary);
    overflow-wrap: anywhere;
  }

  .ticket-detail__state {
    padding: 0 var(--space-sm);
    border-radius: var(--radius-sm);
    background-color: var(--color-border);
    font-size: var(--font-size-xs);
  }

  .ticket-detail__chip {
    padding: 0 var(--space-xs);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-detail__heading {
    margin: 0 0 var(--space-xs);
    font-size: var(--font-size-xs);
    font-weight: 600;
    color: var(--color-text-secondary);
  }

  .ticket-detail__text {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    line-height: 1.5;
    color: var(--color-text-primary);
  }

  .ticket-detail__muted {
    margin: 0;
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-detail__fields {
    margin: 0;
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px var(--space-sm);
  }

  .ticket-detail__fields dt {
    font-family: var(--font-mono);
    font-size: var(--font-size-xs);
    color: var(--color-text-secondary);
  }

  .ticket-detail__fields dd {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .ticket-detail__results {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .ticket-detail__results li {
    display: flex;
    flex-direction: column;
    padding-left: var(--space-xs);
    border-left: 3px solid var(--color-border);
  }

  .ticket-detail__plans {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
  }
</style>
