import { describe, it, expect } from 'vitest';

/**
 * The state's field names are not fixed by the plan; they are read from the
 * module at the call site so a different spelling fails only these tests.
 */
const CLOSED = {
  isAddDialogOpen: false,
  isConfirmDialogOpen: false,
  isRenameDialogOpen: false,
  isRestoreDialogOpen: false,
};

type ModalKey = keyof typeof CLOSED;
const MODAL_KEYS = Object.keys(CLOSED) as ModalKey[];

async function isAllowed(state: typeof CLOSED): Promise<boolean> {
  const { isApprovalDialogAllowed } = await import('./modal-state');
  return isApprovalDialogAllowed(state);
}

describe('isApprovalDialogAllowed: an approval never stacks on another modal', () => {
  it('allows the approval dialog when every other modal is closed', async () => {
    expect(await isAllowed({ ...CLOSED })).toBe(true);
  });

  it.each(MODAL_KEYS)('refuses it while only %s is open', async (key) => {
    expect(await isAllowed({ ...CLOSED, [key]: true })).toBe(false);
  });

  it('refuses it while all of them are open', async () => {
    const open = Object.fromEntries(MODAL_KEYS.map((key) => [key, true])) as typeof CLOSED;
    expect(await isAllowed(open)).toBe(false);
  });
});
