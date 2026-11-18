/**
 * Which non-approval modals are open. Every field is required so that adding a
 * modal without deciding how it interacts with approvals fails to type-check.
 */
export interface OtherModalState {
  readonly isAddDialogOpen: boolean;
  readonly isConfirmDialogOpen: boolean;
  readonly isRenameDialogOpen: boolean;
  readonly isRestoreDialogOpen: boolean;
}

/**
 * An approval never stacks on top of another modal: it waits in the queue
 * until every other modal is closed. Whether a request exists is the caller's
 * concern.
 */
export function isApprovalDialogAllowed(state: OtherModalState): boolean {
  return (
    !state.isAddDialogOpen &&
    !state.isConfirmDialogOpen &&
    !state.isRenameDialogOpen &&
    !state.isRestoreDialogOpen
  );
}
