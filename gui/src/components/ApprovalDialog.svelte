<script lang="ts">
  import {
    formatApprovalBody,
    sanitizeForDisplay,
    type ApprovalRequest,
  } from '../ipc/session-boundary';
  import { isWithinGuard } from '../session/approval-queue';
  import { describeUntargeted } from '../session/approval-target';
  import { buttons, dialogs, describeStatus } from '../ui-messages';
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

  $: requesterName = sanitizeForDisplay(request.requester.name ?? '');
  $: requesterLabel =
    request.requester.kind === 'window'
      ? isRequesterOpen
        ? requesterName
        : dialogs.approvalRequesterGone(requesterName)
      : dialogs.approvalRequesterExternal;
  $: targetLabel = describeTarget(request);
  // The body is lines of what will run; hidden characters are shown escaped.
  $: body = request.text === null ? '' : formatApprovalBody(request.text);
  $: title =
    pendingCount > 1 ? `${dialogs.approvalTitle} (${pendingCount})` : dialogs.approvalTitle;

  function describeTarget(pending: ApprovalRequest): string {
    const target = pending.target;
    if (target !== undefined) {
      return `${sanitizeForDisplay(target.name)} (${describeStatus(target.status)})`;
    }
    return describeUntargeted(pending);
  }

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

<ModalDialog bind:isOpen onCancel={handleCancel}>
  <div class="dialog-content">
    <h2 class="dialog-title">{title}</h2>
    <p class="dialog-message">{requesterLabel} → {request.tool} → {targetLabel}</p>
    {#if body !== ''}
      <pre class="approval-body">{body}</pre>
    {/if}
    <div class="dialog-actions">
      <!-- Deny comes first: it is the first focusable descendant, so the
           focus the browser gives the modal cannot approve anything. -->
      <button type="button" class="btn btn-secondary" on:click={handleDeny}>
        {buttons.deny}
      </button>
      <button type="button" class="btn btn-danger" on:click={handleApprove}>
        {buttons.approve}
      </button>
    </div>
  </div>
</ModalDialog>

<style>
  .approval-body {
    margin: 0;
    max-height: 12rem;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-family-mono);
    font-size: var(--font-size-sm);
  }
</style>
