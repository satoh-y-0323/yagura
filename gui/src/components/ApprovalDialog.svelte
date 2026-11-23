<script lang="ts">
  import type { UiText } from '../display/ui-text';
  import type { DisplayLine } from '../display/untrusted-text';
  import type { ApprovalRequest } from '../ipc/session-boundary';
  import { isWithinGuard } from '../session/approval-queue';
  import { describeApprovalRequester, describeApprovalTarget } from '../session/approval-target';
  import { buttons, dialogs } from '../ui-messages';
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

  const failureNoteId = 'approval-decision-failed';
  $: describedBy = decisionFailure !== null ? failureNoteId : undefined;

  $: requesterLabel = describeApprovalRequester(request, isRequesterOpen);
  $: toolLabel = request.displayTool;
  $: targetLabel = describeApprovalTarget(request);
  $: title = pendingCount > 1 ? dialogs.approvalTitleCount(pendingCount) : dialogs.approvalTitle;

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

<ModalDialog bind:isOpen onCancel={handleCancel} refocusKey={request.request_id}>
  <div class="dialog-content">
    <h2 class="dialog-title">{title}</h2>
    {#if decisionFailure !== null}
      <p class="approval-failure" id={failureNoteId}>
        <Icon name="circle-exclamation-point" size="sm" />
        {decisionFailure}
      </p>
    {/if}
    <p class="dialog-message">{dialogs.approvalRoute(requesterLabel, toolLabel, targetLabel)}</p>
    {#if request.text !== null && request.text !== ''}
      <pre class="approval-body">{request.text}</pre>
    {/if}
    <div class="dialog-actions">
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

  @media (forced-colors: active) {
    .approval-failure {
      border-left-color: CanvasText;
    }
  }

  .approval-body {
    margin: 0;
    max-height: 12rem;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--font-size-sm);
  }
</style>
