/**
 * `normalizeDrop` maps a raw Tauri drag-drop event into the frozen shape the
 * add-window dialog reads. `enter` folds into `over`, since the drop hint
 * starts on the first `enter`, but keeps the same empty-paths gate as `drop`.
 */
import { describe, it, expect } from 'vitest';

import { normalizeDrop, type RawDropEvent } from './folder-drop-model';

describe('normalizeDrop: enter folds into { kind: "over" }', () => {
  it('folds an enter event carrying one path into { kind: "over" }', () => {
    const event: RawDropEvent = { type: 'enter', paths: ['/tmp/project'] };
    expect(normalizeDrop(event)).toEqual({ kind: 'over' });
  });

  it('folds an enter event carrying several paths into { kind: "over" } (the path itself is not carried)', () => {
    const event: RawDropEvent = { type: 'enter', paths: ['/tmp/a', '/tmp/b'] };
    expect(normalizeDrop(event)).toEqual({ kind: 'over' });
  });

  it('returns null for an enter event with an empty paths array', () => {
    const event: RawDropEvent = { type: 'enter', paths: [] };
    expect(normalizeDrop(event)).toBeNull();
  });
});

describe('normalizeDrop: over and leave carry no paths field', () => {
  it('maps an over event to { kind: "over" }', () => {
    const event: RawDropEvent = { type: 'over' };
    expect(normalizeDrop(event)).toEqual({ kind: 'over' });
  });

  it('maps a leave event to { kind: "leave" }', () => {
    const event: RawDropEvent = { type: 'leave' };
    expect(normalizeDrop(event)).toEqual({ kind: 'leave' });
  });
});

describe('normalizeDrop: drop carries the first path only', () => {
  it('maps a drop event with one path to { kind: "drop", path }', () => {
    const event: RawDropEvent = { type: 'drop', paths: ['/tmp/project'] };
    expect(normalizeDrop(event)).toEqual({ kind: 'drop', path: '/tmp/project' });
  });

  it('takes only the first path when a drop event carries several', () => {
    const event: RawDropEvent = { type: 'drop', paths: ['/tmp/a', '/tmp/b'] };
    expect(normalizeDrop(event)).toEqual({ kind: 'drop', path: '/tmp/a' });
  });

  it('returns null for a drop event with an empty paths array', () => {
    const event: RawDropEvent = { type: 'drop', paths: [] };
    expect(normalizeDrop(event)).toBeNull();
  });
});
