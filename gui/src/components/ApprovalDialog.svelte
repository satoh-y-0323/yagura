<script lang="ts">
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import type { ApprovalRequest } from '../ipc/session-boundary';
  import { isWithinGuard } from '../session/approval-queue';
  import { approvalFactsOf } from '../session/approval-target';
  import { buttons, dialogs } from '../ui-messages';
  import ApprovalHeadline from './ApprovalHeadline.svelte';
  import ApprovalParty from './ApprovalParty.svelte';
  import Icon from './Icon.svelte';
  import ModalDialog from './ModalDialog.svelte';

  export let isOpen: boolean = false;
  export let request: ApprovalRequest;
  /** Whether the requesting window is still in the window list App polls for. */
  export let isRequesterOpen: boolean = false;
  /** How many requests are queued, this one included. */
  export let pendingCount: number = 1;
  /** When the dialog was opened, for the click guard. */
  export let openedAt: number;
  export let onDecide: (requestId: string, approved: boolean) => void;
  /**
   * The line for the decision on the request shown now that could not be sent, `null` when it did not fail. The
   * shell answers it for this request only; the note never moves focus.
   */
  export let decisionFailure: UiText | DisplayLine | null = null;

  const headlineId = 'approval-headline';
  const fromId = 'approval-from';
  const toId = 'approval-to';
  const failureNoteId = 'approval-decision-failed';
  const bodyId = 'approval-body';

  $: facts = approvalFactsOf(request, isRequesterOpen);
  $: headline = facts.headline;
  $: hasBody = request.text !== null && request.text !== '';
  // What is sent decides the answer more than who sends it, so the buttons are described by the
  // body too; it comes last, so a long body does not bury the cards and the note.
  $: describedBy = [
    fromId,
    toId,
    ...(decisionFailure !== null ? [failureNoteId] : []),
    ...(hasBody ? [bodyId] : []),
  ].join(' ');
  $: eyebrow = pendingCount > 1 ? dialogs.approvalTitleCount(pendingCount) : dialogs.approvalTitle;

  function handleDeny(): void {
    if (isWithinGuard(openedAt, Date.now())) {
      return;
    }
    onDecide(request.request_id, false);
  }

  function handleApprove(): void {
    if (isWithinGuard(openedAt, Date.now())) {
      return;
    }
    onDecide(request.request_id, true);
  }

  /**
   * Escape and a backdrop click both arrive here. A request that opened
   * without being asked for must be answered rather than waved away, so this
   * deliberately does nothing and the dialog stays up until a decision or the
   * daemon's deadline.
   */
  function handleCancel(): void {
    // No dismissal path.
  }
</script>

<ModalDialog
  bind:isOpen
  onCancel={handleCancel}
  refocusKey={request.request_id}
  labelledBy={headlineId}
  maxWidth="560px"
>
  <div class="dialog-content approval">
    <div class="approval-head">
      <span class="approval-head__icon">
        <Icon name="shield-exclamation-point" size="md" />
      </span>
      <div class="approval-head__text">
        <span class="approval-head__eyebrow">{eyebrow}</span>
        <h2 class="dialog-title approval-head__headline" id={headlineId}>
          <ApprovalHeadline {headline} />
        </h2>
      </div>
    </div>
    <div class="approval-parties">
      <ApprovalParty party={facts.from} heading={dialogs.approvalFrom} id={fromId} />
      <span class="approval-parties__arrow">
        <Icon name="chevron-right" size="md" />
      </span>
      <ApprovalParty party={facts.to} heading={dialogs.approvalTo} id={toId} />
    </div>
    {#if hasBody}
      <div class="approval-body" id={bodyId}>
        {#if facts.bodyHeading !== null}
          <span class="approval-body__heading">{facts.bodyHeading}</span>
        {/if}
        <pre class="approval-body__text">{request.text}</pre>
      </div>
    {/if}
    {#if decisionFailure !== null}
      <p class="approval-failure" id={failureNoteId}>
        <Icon name="circle-exclamation-point" size="sm" />
        {decisionFailure}
      </p>
    {/if}
    <div class="dialog-actions approval-actions">
      <span class="approval-actions__tool">
        {dialogs.approvalToolLabel}
        <code class="approval-tool-name">{facts.tool}</code>
      </span>
      <!-- The dialog focusing steps pick the autofocus element, so initial focus is always the
           safe choice; markup order stays Deny first for when autofocus is not honored. -->
      <!-- svelte-ignore a11y_autofocus -->
      <button
        type="button"
        class="btn btn-secondary"
        autofocus
        aria-describedby={describedBy}
        on:click={handleDeny}
      >
        {buttons.deny}
      </button>
      <button
        type="button"
        class="btn btn-danger"
        aria-describedby={describedBy}
        on:click={handleApprove}
      >
        {buttons.approve}
      </button>
    </div>
  </div>
</ModalDialog>

<style>
  .approval {
    gap: var(--space-4);
  }

  .approval-head {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
  }

  .approval-head__icon {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border-radius: var(--radius-full);
    background-color: var(--st-approval-tint);
    color: var(--st-approval);
  }

  .approval-head__text {
    display: flex;
    flex-direction: column;
    gap: var(--space-0_5);
    min-width: 0;
  }

  .approval-head__eyebrow {
    color: var(--st-approval);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .approval-head__headline {
    font-size: var(--text-lg);
    line-height: var(--leading-md);
    overflow-wrap: anywhere;
  }

  .approval-tool-name {
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  .approval-parties {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    gap: var(--space-2);
    align-items: stretch;
  }

  .approval-parties__arrow {
    display: inline-flex;
    align-self: center;
    color: var(--fg-faint);
  }

  .approval-body {
    display: flex;
    flex-direction: column;
    gap: var(--space-1_5);
  }

  .approval-body__heading {
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
  }

  .approval-body__text {
    margin: 0;
    max-height: 12rem;
    overflow: auto;
    padding: var(--space-3);
    border-radius: var(--radius-md);
    background-color: var(--bg-hover);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: var(--leading-sm);
  }

  .approval-failure {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin: 0;
    padding: var(--space-1_5) var(--space-2);
    border-left: 3px solid var(--danger);
    background-color: var(--bg-hover);
    color: var(--fg-default);
    font-size: var(--text-sm);
  }

  .approval-failure :global(.icon) {
    color: var(--danger);
  }

  .approval-actions {
    align-items: center;
  }

  .approval-actions__tool {
    flex: 1 1 auto;
    min-width: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  @media (forced-colors: active) {
    .approval-failure {
      border-left-color: CanvasText;
    }
  }
</style>
