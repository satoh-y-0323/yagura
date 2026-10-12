/**
 * The terminal rule against a table the test writes, so each case names the one entry it turns on.
 * The table the daemon generates is measured against the real keys in `binding-rules.test.ts`.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const table = vi.hoisted(() => ({ chords: Object.create(null) as Record<string, string> }));

vi.mock('./terminal-key-bytes.generated.json', () => ({
  default: { generated_by: 'test', chords: table.chords },
}));

import { validateBinding } from './binding-rules';

beforeEach(() => {
  for (const key of Object.keys(table.chords)) {
    delete table.chords[key];
  }
});

function writes(entries: Record<string, string>): void {
  Object.assign(table.chords, entries);
}

describe('a Ctrl chord is free when Ctrl adds nothing to what the terminal receives', () => {
  it('is free when the table lacks the chord, which writes nothing', () => {
    expect(validateBinding('Ctrl+F9', 'windows')).toBeNull();
  });

  it('is refused when the chord writes something', () => {
    writes({ 'Ctrl+F9': '01' });

    expect(validateBinding('Ctrl+F9', 'windows')).toBe('reaches-terminal');
  });

  it('is free when the key writes the same without Ctrl', () => {
    writes({ 'Ctrl+F9': '01', F9: '01' });

    expect(validateBinding('Ctrl+F9', 'windows')).toBeNull();
  });

  it('is refused when the key writes something else without Ctrl', () => {
    writes({ 'Ctrl+F9': '01', F9: '02' });

    expect(validateBinding('Ctrl+F9', 'windows')).toBe('reaches-terminal');
  });

  it('is never refused with Meta held, since the terminal never receives Meta', () => {
    writes({ 'Ctrl+F9': '01', 'Ctrl+Meta+F9': '01' });

    expect(validateBinding('Ctrl+Meta+F9', 'windows')).toBeNull();
  });
});

describe('only the modifiers the chord holds count when comparing with the key without Ctrl', () => {
  it('does not take Alt+key as the same when Alt is not held', () => {
    writes({ 'Ctrl+F9': '01', 'Alt+F9': '01' });

    expect(validateBinding('Ctrl+F9', 'windows')).toBe('reaches-terminal');
  });

  it('does not take Shift+key as the same when Shift is not held', () => {
    writes({ 'Ctrl+F9': '01', 'Shift+F9': '01' });

    expect(validateBinding('Ctrl+F9', 'windows')).toBe('reaches-terminal');
  });

  it('does not take Alt+Shift+key as the same when only Alt is held', () => {
    writes({ 'Ctrl+Alt+F9': '01', 'Alt+Shift+F9': '01' });

    expect(validateBinding('Ctrl+Alt+F9', 'windows')).toBe('reaches-terminal');
  });

  it('does not take Alt+Shift+key as the same when only Shift is held', () => {
    writes({ 'Ctrl+Shift+F9': '01', 'Alt+Shift+F9': '01' });

    expect(validateBinding('Ctrl+Shift+F9', 'windows')).toBe('reaches-terminal');
  });

  it('takes Alt+key as the same when Alt is held', () => {
    writes({ 'Ctrl+Alt+F9': '01', 'Alt+F9': '01' });

    expect(validateBinding('Ctrl+Alt+F9', 'windows')).toBeNull();
  });

  it('takes Shift+key as the same when Shift is held', () => {
    writes({ 'Ctrl+Shift+F9': '01', 'Shift+F9': '01' });

    expect(validateBinding('Ctrl+Shift+F9', 'windows')).toBeNull();
  });

  it('takes Alt+Shift+key as the same when both are held', () => {
    writes({ 'Ctrl+Alt+Shift+F9': '01', 'Alt+Shift+F9': '01' });

    expect(validateBinding('Ctrl+Alt+Shift+F9', 'windows')).toBeNull();
  });

  it('takes the key with one of two held modifiers as the same', () => {
    writes({ 'Ctrl+Alt+Shift+F9': '01', 'Alt+F9': '01' });
    expect(validateBinding('Ctrl+Alt+Shift+F9', 'windows')).toBeNull();

    delete table.chords['Alt+F9'];
    writes({ 'Shift+F9': '01' });
    expect(validateBinding('Ctrl+Alt+Shift+F9', 'windows')).toBeNull();
  });

  it('takes the bare key as the same whatever modifiers are held', () => {
    writes({ 'Ctrl+Alt+Shift+F9': '01', F9: '01' });

    expect(validateBinding('Ctrl+Alt+Shift+F9', 'windows')).toBeNull();
  });
});
