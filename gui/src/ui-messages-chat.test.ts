/**
 * The chat facade's two renderings: the per-target delivery chip and the halt
 * banner. Both take a value the daemon chose, so both have to answer for a
 * string outside the known set as well — a room that stops for a reason this
 * build does not know must still say it stopped.
 *
 * Only the facade is imported, the same boundary the components read.
 */
import { describe, it, expect } from 'vitest';

import { describeDeliveryState, describeHaltReason } from './ui-messages';

/** Every delivery state the daemon can report for one target. */
const DELIVERY_STATES = [
  'queued',
  'delivered',
  'undelivered_unknown',
  'undelivered_halted',
  'undelivered_exhausted',
  'undelivered_restart',
] as const;

/** Every halt reason the daemon can report. */
const HALT_REASONS = ['repetition', 'monologue', 'turn_budget'];

describe('describeDeliveryState', () => {
  it.each(DELIVERY_STATES)('renders %s as a non-empty string', (state) => {
    expect(describeDeliveryState(state).length).toBeGreaterThan(0);
  });

  it('renders the six states as six distinct strings', () => {
    const rendered = DELIVERY_STATES.map((state) => describeDeliveryState(state));
    expect(new Set(rendered).size).toBe(DELIVERY_STATES.length);
  });

  it('renders every unrecognized state the same non-empty string, distinct from the six', () => {
    const fallback = describeDeliveryState('nonsense');
    expect(fallback.length).toBeGreaterThan(0);
    expect(describeDeliveryState('delivered_by_carrier_pigeon')).toBe(fallback);
    expect(describeDeliveryState('')).toBe(fallback);
    const rendered = DELIVERY_STATES.map((state) => describeDeliveryState(state));
    expect(rendered).not.toContain(fallback);
  });
});

describe('describeHaltReason', () => {
  it.each(HALT_REASONS)('renders %s as a non-empty string', (reason) => {
    expect(describeHaltReason(reason).length).toBeGreaterThan(0);
  });

  it('renders the three reasons as three distinct strings', () => {
    const rendered = HALT_REASONS.map((reason) => describeHaltReason(reason));
    expect(new Set(rendered).size).toBe(HALT_REASONS.length);
  });

  it('renders an unrecognized reason as a non-empty string, distinct from the three', () => {
    const fallback = describeHaltReason('sudden_silence');
    expect(fallback.length).toBeGreaterThan(0);
    const rendered = HALT_REASONS.map((reason) => describeHaltReason(reason));
    expect(rendered).not.toContain(fallback);
  });
});
