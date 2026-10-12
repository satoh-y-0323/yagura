/**
 * TS-boundary contract for the model-alias wire shapes: the narrowing
 * functions that turn a daemon-shaped reply or push into a typed
 * `ModelChoicesRequested` / `ModelChoicesPayload` / `ModelAliasesSaved`,
 * plus the closed `ModelChoiceSource` vocabulary a
 * choices push carries (`values` / `command`, no catch-all -- mirroring the
 * Rust enum of the same name, which decodes nothing outside that set).
 *
 * Lives next to `model-boundary.ts`, the way `plan-boundary.test.ts` sits
 * next to `plan-boundary.ts`.
 */
import { describe, it, expect } from 'vitest';

import {
  BoundaryError,
  narrowModelAliasesSaved,
  narrowModelChoicesPayload,
  narrowModelChoicesRequested,
} from './model-boundary';

/** A well-formed `ModelChoicesRequested` wire value. */
function modelChoicesRequestedWire(
  overrides: Record<string, unknown> = {}
): Record<string, unknown> {
  return {
    request_id: 7,
    profile: 'claude-code',
    ...overrides,
  };
}

/** A well-formed `ModelChoices` push wire value. */
function modelChoicesPayloadWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    request_id: 7,
    profile: 'claude-code',
    source: { kind: 'values' },
    choices: [{ id: 'haiku', label: 'Haiku' }],
    error: null,
    ...overrides,
  };
}

/** A well-formed `LaunchOptionAlias` wire row. */
function launchOptionAliasWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    alias: 'fast',
    value: 'haiku',
    ...overrides,
  };
}

/** A well-formed `ModelAliasesSaved` wire value. */
function modelAliasesSavedWire(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    profile: 'claude-code',
    aliases: [launchOptionAliasWire()],
    ...overrides,
  };
}

describe('narrowModelChoicesRequested', () => {
  it('narrows a well-formed reply', () => {
    const result = narrowModelChoicesRequested(modelChoicesRequestedWire());
    expect(result).toEqual({ request_id: 7, profile: 'claude-code' });
  });

  it('rejects a non-integer request_id', () => {
    expect(() =>
      narrowModelChoicesRequested(modelChoicesRequestedWire({ request_id: '7' }))
    ).toThrow(BoundaryError);
  });

  it('rejects a missing profile', () => {
    const wire = modelChoicesRequestedWire();
    delete wire['profile'];
    expect(() => narrowModelChoicesRequested(wire)).toThrow(BoundaryError);
  });
});

describe('narrowModelChoicesPayload', () => {
  it('narrows the values source', () => {
    const result = narrowModelChoicesPayload(
      modelChoicesPayloadWire({ source: { kind: 'values' } })
    );
    expect(result.source).toEqual({ kind: 'values' });
  });

  it('narrows the command source with its argv', () => {
    const result = narrowModelChoicesPayload(
      modelChoicesPayloadWire({ source: { kind: 'command', argv: ['codex', 'debug', 'models'] } })
    );
    expect(result.source).toEqual({ kind: 'command', argv: ['codex', 'debug', 'models'] });
  });

  it('defaults source to null when the key is absent', () => {
    const wire = modelChoicesPayloadWire();
    delete wire['source'];
    const result = narrowModelChoicesPayload(wire);
    expect(result.source).toBeNull();
  });

  it('reads a null source as no source', () => {
    expect(narrowModelChoicesPayload(modelChoicesPayloadWire({ source: null })).source).toBeNull();
  });

  it('keeps every choice with its id and label', () => {
    const result = narrowModelChoicesPayload(
      modelChoicesPayloadWire({
        choices: [{ id: 'haiku', label: 'Haiku' }, { id: 'opus' }],
      })
    );
    expect(result.choices).toEqual([
      { id: 'haiku', label: 'Haiku' },
      { id: 'opus', label: null },
    ]);
  });

  it('defaults error to null when the key is absent', () => {
    const wire = modelChoicesPayloadWire();
    delete wire['error'];
    const result = narrowModelChoicesPayload(wire);
    expect(result.error).toBeNull();
  });

  it('defaults a choice label to null when the key is absent', () => {
    const wire = modelChoicesPayloadWire({ choices: [{ id: 'haiku' }] });
    const result = narrowModelChoicesPayload(wire);
    expect(result.choices).toEqual([{ id: 'haiku', label: null }]);
  });

  it('carries the error text through untouched', () => {
    const result = narrowModelChoicesPayload(
      modelChoicesPayloadWire({ source: undefined, choices: [], error: 'output too large' })
    );
    expect(result.error).toBe('output too large');
  });

  it('rejects an unrecognized source kind', () => {
    expect(() =>
      narrowModelChoicesPayload(modelChoicesPayloadWire({ source: { kind: 'bogus' } }))
    ).toThrow(BoundaryError);
  });

  it('rejects a non-integer request_id', () => {
    expect(() =>
      narrowModelChoicesPayload(modelChoicesPayloadWire({ request_id: 'seven' }))
    ).toThrow(BoundaryError);
  });

  it('rejects a choice missing its id', () => {
    expect(() =>
      narrowModelChoicesPayload(modelChoicesPayloadWire({ choices: [{ label: 'Haiku' }] }))
    ).toThrow(BoundaryError);
  });
});

describe('the failures name where the daemon went wrong', () => {
  it('names the unrecognized source kind', () => {
    expect(() =>
      narrowModelChoicesPayload(modelChoicesPayloadWire({ source: { kind: 'bogus' } }))
    ).toThrow(/source\.kind: unrecognized kind "bogus"/);
  });

  it('names the choices of a push that are not an array', () => {
    expect(() => narrowModelChoicesPayload(modelChoicesPayloadWire({ choices: 'x' }))).toThrow(
      /session:\/\/model-choices\.choices/
    );
  });

  it('names a source that is not an object', () => {
    expect(() => narrowModelChoicesPayload(modelChoicesPayloadWire({ source: 'x' }))).toThrow(
      /model-choices\.source: expected an object/
    );
  });

  it('names a label that is not a string', () => {
    expect(() =>
      narrowModelChoicesPayload(modelChoicesPayloadWire({ choices: [{ id: 'a', label: 3 }] }))
    ).toThrow(/label/);
  });

  it('names the command of a failed list_model_choices reply', () => {
    expect(() => narrowModelChoicesRequested('x')).toThrow(/^list_model_choices/);
  });

  it('names the command and the aliases of a failed save_model_aliases reply', () => {
    expect(() => narrowModelAliasesSaved(null)).toThrow(/^save_model_aliases/);
    expect(() => narrowModelAliasesSaved(modelAliasesSavedWire({ aliases: 'x' }))).toThrow(
      /^save_model_aliases\.aliases/
    );
  });
});

describe('narrowModelAliasesSaved', () => {
  it('narrows a well-formed reply', () => {
    const result = narrowModelAliasesSaved(modelAliasesSavedWire());
    expect(result).toEqual({
      profile: 'claude-code',
      aliases: [{ alias: 'fast', value: 'haiku' }],
    });
  });

  it('rejects an alias row missing its value', () => {
    const wire = modelAliasesSavedWire({ aliases: [{ alias: 'fast' }] });
    expect(() => narrowModelAliasesSaved(wire)).toThrow(BoundaryError);
  });
});
