/**
 * The chat facade's two renderings: the per-target delivery chip and the halt
 * banner. Both take a value the daemon chose, so both have to answer for a
 * string outside the known set as well — a room that stops for a reason this
 * build does not know must still say it stopped.
 *
 * Only the facade is imported, the same boundary the components read.
 */
import { describe, it, expect } from 'vitest';

import {
  describeDeliveryState,
  describeHaltReason,
  describeUndeliveredUnknownReason,
} from './ui-messages';

/** Every delivery state the daemon can report for one target. */
const DELIVERY_STATES = [
  'pending',
  'confirmed',
  'unconfirmed',
  'not_observed',
  'not_written',
  'write_failed',
  'undelivered_unknown',
  'undelivered_halted',
  'undelivered_exhausted',
] as const;

/** The delivery states that can never carry a reason. */
const REASONLESS_DELIVERY_STATES = DELIVERY_STATES.filter(
  (state): state is Exclude<(typeof DELIVERY_STATES)[number], 'undelivered_unknown'> =>
    state !== 'undelivered_unknown'
);

/** Every halt reason the daemon can report. */
const HALT_REASONS = ['repetition', 'monologue', 'turn_budget'];

describe('describeDeliveryState', () => {
  it.each(DELIVERY_STATES)('renders %s as a non-empty string', (state) => {
    expect(describeDeliveryState(state).length).toBeGreaterThan(0);
  });

  it('renders the nine states as nine distinct strings', () => {
    const rendered = DELIVERY_STATES.map((state) => describeDeliveryState(state));
    expect(new Set(rendered).size).toBe(DELIVERY_STATES.length);
  });

  it('renders an unrecognized state as a non-empty string', () => {
    expect(describeDeliveryState('delivered_by_carrier_pigeon').length).toBeGreaterThan(0);
  });

  it('renders every unrecognized state the same way', () => {
    expect(describeDeliveryState('nonsense')).toBe(describeDeliveryState(''));
  });

  it('does not render an unrecognized state as any of the nine', () => {
    const rendered = DELIVERY_STATES.map((state) => describeDeliveryState(state));
    expect(rendered).not.toContain(describeDeliveryState('nonsense'));
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

  it('renders an unrecognized reason as a non-empty string', () => {
    expect(describeHaltReason('sudden_silence').length).toBeGreaterThan(0);
  });

  it('does not render an unrecognized reason as any of the three', () => {
    const rendered = HALT_REASONS.map((reason) => describeHaltReason(reason));
    expect(rendered).not.toContain(describeHaltReason('sudden_silence'));
  });
});

/**
 * A separate function, not a change to `describeDeliveryState` itself -- the
 * nine-state distinct set and its signature above are unchanged. It takes the
 * delivery outcome as one value, so the arm it names, not the presence of a
 * reason, decides which wording comes out.
 */
describe('describeUndeliveredUnknownReason', () => {
  it('falls back to describeDeliveryState("undelivered_unknown") for that arm with no reason', () => {
    expect(describeUndeliveredUnknownReason({ state: 'undelivered_unknown', reason: null })).toBe(
      describeDeliveryState('undelivered_unknown')
    );
  });

  it('renders a reason-specific string, distinct from the reason-less fallback', () => {
    const result = describeUndeliveredUnknownReason({
      state: 'undelivered_unknown',
      reason: 'screen_lost',
    });
    expect(result.length).toBeGreaterThan(0);
    expect(result).not.toBe(describeDeliveryState('undelivered_unknown'));
  });

  it('renders "Not sent (status unknown: screen lost)" for screen_lost', () => {
    expect(
      describeUndeliveredUnknownReason({ state: 'undelivered_unknown', reason: 'screen_lost' })
    ).toBe('Not sent (status unknown: screen lost)');
  });

  it.each(REASONLESS_DELIVERY_STATES)(
    'renders the %s arm exactly as the nine-state rendering does',
    (state) => {
      expect(describeUndeliveredUnknownReason({ state })).toBe(describeDeliveryState(state));
    }
  );
});
