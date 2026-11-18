import { describe, it, expect } from 'vitest';

import { isApprovalDialogAllowed } from './modal-state';

const CLOSED = {
  isAddDialogOpen: false,
  isConfirmDialogOpen: false,
  isRenameDialogOpen: false,
  isRestoreDialogOpen: false,
};

type ModalKey = keyof typeof CLOSED;
const MODAL_KEYS = Object.keys(CLOSED) as ModalKey[];

describe('isApprovalDialogAllowed: an approval never stacks on another modal', () => {
  it('allows the approval dialog when every other modal is closed', () => {
    expect(isApprovalDialogAllowed({ ...CLOSED })).toBe(true);
  });

  it.each(MODAL_KEYS)('refuses it while only %s is open', (key) => {
    expect(isApprovalDialogAllowed({ ...CLOSED, [key]: true })).toBe(false);
  });

  it('refuses it while all of them are open', () => {
    const open = Object.fromEntries(MODAL_KEYS.map((key) => [key, true])) as typeof CLOSED;
    expect(isApprovalDialogAllowed(open)).toBe(false);
  });
});
