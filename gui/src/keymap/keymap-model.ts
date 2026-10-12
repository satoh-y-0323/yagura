/**
 * The keymap as the user sees it: which chord each action has. A keymap file
 * holds overrides per action; this module is the one place they are combined
 * with the defaults, edited by the Keyboard tab and turned back into overrides.
 */

import { validateBinding } from './binding-rules';
import type { KeyboardPlatform } from './keyboard-platform';
import { ACTION_IDS, type ActionId, type KeyMap } from './key-binding';

/** The chord of every action, or `null` for an action without one. */
export type Keys = Readonly<Record<ActionId, string | null>>;

type DefaultKeys = Readonly<Record<ActionId, string>>;

const KNOWN_ACTIONS: ReadonlySet<string> = new Set<string>(ACTION_IDS);

function keysFrom(chordOf: (action: ActionId) => string | null): Keys {
  return Object.fromEntries(ACTION_IDS.map((action) => [action, chordOf(action)])) as Keys;
}

function hasOwn(record: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(record, key);
}

/**
 * Combines the overrides of a keymap file with `defaults`.
 *
 * Overrides are taken in `ACTION_IDS` order, so the object order never decides
 * a tie. An override is discarded and counted when its action is unknown, when
 * `validateBinding` refuses its chord on `platform`, or when an earlier override
 * already took the chord. An empty chord leaves the action without a chord and
 * is not counted. An action without a usable override keeps its default chord
 * unless an override took that chord, in which case it has none.
 *
 * @returns The chord of every action (one to one) and the number of discarded overrides.
 */
export function effectiveKeys(
  defaults: DefaultKeys,
  overrides: Readonly<Record<string, string>>,
  platform: KeyboardPlatform
): { keys: Keys; discarded: number } {
  let discarded = Object.keys(overrides).filter((name) => !KNOWN_ACTIONS.has(name)).length;
  const overridden = new Map<ActionId, string | null>();
  const taken = new Set<string>();

  for (const action of ACTION_IDS) {
    const chord = hasOwn(overrides, action) ? overrides[action] : undefined;
    if (chord === undefined) continue;
    if (chord === '') {
      overridden.set(action, null);
    } else if (validateBinding(chord, platform) !== null || taken.has(chord)) {
      discarded += 1;
    } else {
      overridden.set(action, chord);
      taken.add(chord);
    }
  }

  const keys = keysFrom((action) => {
    const override = overridden.get(action);
    if (override !== undefined) return override;
    const chord = defaults[action];
    // A default an override took is left out; the default tables hold each chord once.
    return taken.has(chord) ? null : chord;
  });
  return { keys, discarded };
}

/** The chord → action map used to match key presses. Actions without a chord are left out. */
export function bindingsOf(keys: Keys): KeyMap {
  const bindings: KeyMap = new Map();
  for (const action of ACTION_IDS) {
    const chord = keys[action];
    if (chord !== null && !bindings.has(chord)) bindings.set(chord, action);
  }
  return bindings;
}

/**
 * Gives `chord` to `action`. The action that held the chord is left without one
 * and reported as `movedFrom`. The chord is not validated here: callers run
 * `validateBinding` first.
 */
export function assignChord(
  keys: Keys,
  action: ActionId,
  chord: string
): { keys: Keys; movedFrom: ActionId | null } {
  const movedFrom = ACTION_IDS.find((other) => other !== action && keys[other] === chord) ?? null;
  const next = keysFrom((each) => {
    if (each === action) return chord;
    if (each === movedFrom) return null;
    return keys[each];
  });
  return { keys: next, movedFrom };
}

/**
 * Restores the default chord of `action`, taking it from whichever action holds
 * it now, exactly as assigning that chord would.
 */
export function resetAction(
  keys: Keys,
  defaults: DefaultKeys,
  action: ActionId
): { keys: Keys; movedFrom: ActionId | null } {
  return assignChord(keys, action, defaults[action]);
}

/** Leaves `action` without a chord. */
export function removeAction(keys: Keys, action: ActionId): Keys {
  return keysFrom((each) => (each === action ? null : keys[each]));
}

/**
 * The overrides that bring back `keys` on top of `defaults`: only the actions
 * whose chord differs from the default, with `""` for an action without a chord.
 */
export function overridesOf(defaults: DefaultKeys, keys: Keys): Record<string, string> {
  const overrides: Record<string, string> = {};
  for (const action of ACTION_IDS) {
    const chord = keys[action];
    if (chord !== defaults[action]) overrides[action] = chord ?? '';
  }
  return overrides;
}
