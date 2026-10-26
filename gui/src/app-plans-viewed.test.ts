/**
 * When the plans panel counts as the user having looked at the plan board:
 * the moment the Board tab becomes visible, not the moment the panel opens,
 * since the panel now opens on the Tickets tab.
 */
import { describe, expect, it } from 'vitest';

import { planBoardBecameVisible } from './tickets/ticket-board-model';

describe('planBoardBecameVisible', () => {
  it('counts opening the panel while the Board tab is the one shown', () => {
    expect(
      planBoardBecameVisible({ open: false, tab: 'board' }, { open: true, tab: 'board' })
    ).toBe(true);
  });

  it('does not count opening the panel on the Tickets tab', () => {
    expect(
      planBoardBecameVisible({ open: false, tab: 'tickets' }, { open: true, tab: 'tickets' })
    ).toBe(false);
  });

  it('counts switching from the Tickets tab to the Board tab, as a ticket plan link does', () => {
    expect(
      planBoardBecameVisible({ open: true, tab: 'tickets' }, { open: true, tab: 'board' })
    ).toBe(true);
  });

  it('counts switching to the Board tab from the Roles or Models tab', () => {
    expect(planBoardBecameVisible({ open: true, tab: 'roles' }, { open: true, tab: 'board' })).toBe(
      true
    );
    expect(
      planBoardBecameVisible({ open: true, tab: 'models' }, { open: true, tab: 'board' })
    ).toBe(true);
  });

  it('does not count leaving the Board tab or closing the panel', () => {
    expect(
      planBoardBecameVisible({ open: true, tab: 'board' }, { open: true, tab: 'tickets' })
    ).toBe(false);
    expect(
      planBoardBecameVisible({ open: true, tab: 'board' }, { open: false, tab: 'board' })
    ).toBe(false);
  });
});
